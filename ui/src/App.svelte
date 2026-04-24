<script lang="ts">
  /**
   * App.svelte — root component and simple view router.
   *
   * On startup, it asks the Rust backend how many accounts exist:
   *   - 0 accounts → show the AccountSetup wizard
   *   - 1+ accounts → show the main inbox (3-panel layout)
   *
   * The user can also navigate to AccountSettings from the sidebar.
   * This is a simple state-based "router" — no URL routing needed
   * since this is a desktop app, not a website.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { listen, type UnlistenFn } from '@tauri-apps/api/event'
  import {
    isPermissionGranted,
    requestPermission,
    sendNotification,
  } from '@tauri-apps/plugin-notification'
  import IconRail, { type RailView } from './lib/IconRail.svelte'
  import Sidebar from './lib/Sidebar.svelte'
  import MailList from './lib/MailList.svelte'
  import MailView from './lib/MailView.svelte'
  import AccountSetup from './lib/AccountSetup.svelte'
  import AccountSettings from './lib/AccountSettings.svelte'
  import Compose, { type ComposeInitial } from './lib/Compose.svelte'
  import ContactsView from './lib/ContactsView.svelte'
  import CalendarView from './lib/CalendarView.svelte'
  import FilesView from './lib/FilesView.svelte'
  import TalkView from './lib/TalkView.svelte'
  import CreateTalkRoomModal, { type TalkRoom } from './lib/CreateTalkRoomModal.svelte'
  import SearchBar, {
    type SearchScope,
    type SearchFilters,
  } from './lib/SearchBar.svelte'
  import SearchResults from './lib/SearchResults.svelte'
  import { applyTheme, installSystemModeListener, type ThemeMode } from './lib/theme'

  // ── View state ──────────────────────────────────────────────
  // Which view is currently shown. Starts as 'loading' until we
  // check whether any accounts exist.
  type View =
    | 'loading'
    | 'setup'
    | 'inbox'
    | 'settings'
    | 'contacts'
    | 'calendar'
    | 'files'
    | 'talk'
  let currentView = $state<View>('loading')


  // ── Inbox state ─────────────────────────────────────────────
  // All configured mail accounts and which one the user is currently
  // looking at. Kept at the App level so Sidebar / MailList / MailView
  // stay in sync when the user switches accounts. `activeAccountEmail`
  // is derived from the list so it stays consistent if an account's
  // email is edited in settings.
  interface Account {
    id: string
    display_name: string
    email: string
    /** User-defined folder icon rules. The Sidebar reads this off
        the active account to apply per-account theming. Optional
        because older `accounts.json` files predate the field. */
    folder_icons?: { keyword: string; icon: string }[]
    /** Per-folder icon overrides (full path → emoji). Set via the
        Sidebar's right-click → Change icon picker; wins over
        special-use / keyword rules in `folderIcon`. Optional for
        the same back-compat reason as `folder_icons`. */
    folder_icon_overrides?: Record<string, string>
  }
  let accounts = $state<Account[]>([])
  let activeAccountId = $state<string | null>(null)
  const activeAccountEmail = $derived(
    accounts.find((a) => a.id === activeAccountId)?.email ?? '',
  )
  // Unified-inbox mode: when on, MailList aggregates INBOX across all
  // accounts. `activeAccountId` stays pointed at a real account so the
  // sidebar's folder tree, integrations, and default Compose-from
  // continue to have a sensible "current account" — the unified view
  // is an overlay on top, not a replacement for the active account.
  let unifiedMode = $state(false)
  // The account a clicked message belongs to. In single-account mode
  // this just shadows `activeAccountId`; in unified mode it's set from
  // the row's `account_id` so MailView opens the right message even
  // though the folder picker isn't pointing at that account.
  let selectedMessageAccountId = $state<string | null>(null)
  // Compose modal: `null` = closed. When open, carries a (possibly empty)
  // initial prefill for reply / reply-all / forward.
  let composeInitial = $state<ComposeInitial | null>(null)
  // Default to INBOX — the Sidebar replaces this as soon as the user
  // picks a folder, or could switch it automatically if INBOX is absent.
  let selectedFolder = $state<string>('INBOX')
  let selectedUid = $state<number | null>(null)
  // Bumped to force child lists to re-fetch (manual refresh, mark-as-read).
  let refreshToken = $state(0)

  // ── Search state ────────────────────────────────────────────
  // `searchQuery` drives the mail-list column: non-empty query OR
  // any active filter swaps MailList out for SearchResults.
  let searchQuery = $state('')
  let searchScope = $state<SearchScope>({})
  let searchFilters = $state<SearchFilters>({})
  // Derived: are we in "search mode"?
  const searchActive = $derived(
    searchQuery.trim().length > 0 ||
      !!searchFilters.unreadOnly ||
      !!searchFilters.flaggedOnly ||
      !!searchFilters.hasAttachment,
  )

  function onSearch(q: string, scope: SearchScope, filters: SearchFilters) {
    searchQuery = q
    searchScope = scope
    searchFilters = filters
  }

  // When a search hit is picked, follow its folder — the hit may
  // live in a different folder than the one currently selected
  // (e.g. "All folders" scope). Syncing the folder makes the
  // subsequent MailView fetch + sidebar highlight coherent.
  function onSelectSearchHit(uid: number, folder: string) {
    if (folder !== selectedFolder) {
      selectedFolder = folder
    }
    selectedUid = uid
  }

  // ── Check for existing accounts on startup ──────────────────
  // This runs once when the component mounts. It calls get_accounts
  // to see if the user has already configured an email account.
  $effect(() => {
    checkAccounts()
  })

  // ── Issue #16: background-sync events + desktop notifications ──
  //
  // Rust emits a `new-mail` event per newly-fetched envelope and an
  // `unread-count-updated` event after each poll cycle. The frontend
  // owns notification display so there's a single permission check
  // path and a single formatting path.
  //
  // Notification burst cap: if more than 3 `new-mail` events land
  // inside a 2-second window, the tail gets collapsed into one
  // summary toast — avoids a rain of toasts after a long offline
  // period or on first JMAP sync.
  let notificationsGranted = $state(false)
  let recentBurst: number[] = []
  let pendingSummaryTimer: ReturnType<typeof setTimeout> | null = null

  type NewMail = {
    account_id: string
    folder: string
    uid: number
    from: string
    subject: string
  }

  type AppPrefs = {
    minimize_to_tray: boolean
    background_sync_enabled: boolean
    background_sync_interval_secs: number
    notifications_enabled: boolean
    start_minimized: boolean
    theme_name: string
    theme_mode: ThemeMode
  }

  // Cached settings snapshot — refreshed when the settings command is
  // called, and consulted when a `new-mail` event arrives to decide
  // whether to show a toast.
  let appPrefs = $state<AppPrefs | null>(null)

  async function bootstrapNotifications() {
    try {
      const granted = await isPermissionGranted()
      if (granted) {
        notificationsGranted = true
        return
      }
      // Only prompt once the user is past setup — on the very first
      // launch the setup wizard should own the screen, not an OS
      // permission dialog.
      if (currentView === 'setup') return
      const res = await requestPermission()
      notificationsGranted = res === 'granted'
    } catch (err) {
      console.warn('notification permission bootstrap failed', err)
    }
  }

  function shouldNotify(): boolean {
    return (
      notificationsGranted && (appPrefs?.notifications_enabled ?? true)
    )
  }

  function fireToast(title: string, body: string) {
    try {
      sendNotification({ title, body })
    } catch (err) {
      console.warn('sendNotification failed', err)
    }
  }

  function handleNewMail(payload: NewMail) {
    // Refresh the list regardless of notification state — new mail
    // should appear in the inbox even if toasts are off.
    refreshToken++

    if (!shouldNotify()) return

    const now = Date.now()
    // Prune burst entries older than 2s — a pure sliding window.
    recentBurst = recentBurst.filter((t) => now - t < 2000)
    recentBurst.push(now)

    if (recentBurst.length <= 3) {
      fireToast(payload.from || 'New mail', payload.subject || '(no subject)')
      return
    }

    // 4th+ toast in the window — suppress individual toast and
    // schedule one summary toast for the end of the window.
    if (pendingSummaryTimer) clearTimeout(pendingSummaryTimer)
    const count = recentBurst.length
    pendingSummaryTimer = setTimeout(() => {
      fireToast('Nimbus Mail', `${count} new messages`)
      pendingSummaryTimer = null
    }, 600)
  }

  async function loadAppPrefs() {
    try {
      appPrefs = await invoke<AppPrefs>('get_app_settings')
    } catch (err) {
      console.warn('get_app_settings failed', err)
    }
  }

  /** Re-apply the theme + (re)install the OS-mode listener whenever
      the user's theme preferences change. The effect's cleanup
      function tears down the previous listener before the next run
      installs a new one, so we never leak `matchMedia` subscribers
      when the user toggles between System / Light / Dark. */
  $effect(() => {
    if (!appPrefs) return
    applyTheme(appPrefs.theme_name, appPrefs.theme_mode)
    return installSystemModeListener(appPrefs.theme_mode, appPrefs.theme_name)
  })

  $effect(() => {
    loadAppPrefs()
    bootstrapNotifications()

    let unlistenNewMail: UnlistenFn | null = null
    let unlistenCompose: UnlistenFn | null = null
    ;(async () => {
      unlistenNewMail = await listen<NewMail>('new-mail', (e) =>
        handleNewMail(e.payload),
      )
      unlistenCompose = await listen('open-compose', () => openCompose({}))
    })()
    return () => {
      unlistenNewMail?.()
      unlistenCompose?.()
      if (pendingSummaryTimer) clearTimeout(pendingSummaryTimer)
    }
  })

  async function checkAccounts() {
    try {
      const list = await invoke<Account[]>('get_accounts')
      accounts = list
      if (list.length > 0) {
        // Keep the current selection if it still exists (e.g. after
        // adding another account); otherwise fall back to the first.
        // This also handles the "active account was just removed"
        // case from AccountSettings.
        if (!activeAccountId || !list.some((a) => a.id === activeAccountId)) {
          activeAccountId = list[0].id
        }
        currentView = 'inbox'
      } else {
        activeAccountId = null
        currentView = 'setup'
      }
    } catch {
      // If we can't load accounts (e.g. first launch, file doesn't exist),
      // show the setup wizard
      accounts = []
      activeAccountId = null
      currentView = 'setup'
    }
  }

  // ── Navigation handlers ─────────────────────────────────────
  function goToInbox() {
    currentView = 'inbox'
    // The user may have added / removed accounts in settings; re-read
    // the list so the IconRail avatars and the active selection
    // reflect the current state.
    void checkAccounts()
  }

  /**
   * Switch the app to a different mail account. Called by the Sidebar
   * account picker. IMAP UIDs are per-account so keeping `selectedUid`
   * would point at a message that doesn't exist in the new account;
   * resetting folder → INBOX keeps the landing experience predictable.
   * Also clears search state because the query was scoped to the old
   * account.
   *
   * The sentinel `"__all__"` toggles `unifiedMode` instead of changing
   * the active account — `activeAccountId` stays pointed at whatever
   * the user had before so the sidebar folder tree and integrations
   * still have a sensible default. Pinging back into a real account
   * id automatically turns unified mode off.
   */
  function selectAccount(id: string) {
    if (id === '__all__') {
      if (unifiedMode) return
      unifiedMode = true
      selectedFolder = 'INBOX'
      selectedUid = null
      selectedMessageAccountId = null
      searchQuery = ''
      searchScope = {}
      searchFilters = {}
      refreshToken++
      return
    }
    if (!unifiedMode && id === activeAccountId) return
    unifiedMode = false
    activeAccountId = id
    selectedFolder = 'INBOX'
    selectedUid = null
    selectedMessageAccountId = null
    searchQuery = ''
    searchScope = {}
    searchFilters = {}
    refreshToken++
  }

  function goToSetup() {
    currentView = 'setup'
  }

  /** IconRail nav click. Maps the rail's view enum directly to the
   *  router's `currentView` — the old `onSelectIntegration` took
   *  string labels like "Contacts" / "Nextcloud Talk" because the
   *  Sidebar rendered those display names verbatim; the rail uses
   *  a typed `RailView` instead so this handler is just a
   *  structural pass-through with no case map. */
  function onSelectView(view: RailView) {
    currentView = view
  }

  /** Fire a `check_mail_now` whenever the user transitions into the
   *  mail view. The background sync loop already runs on its own
   *  cadence (`background_sync_interval_secs`, default 60s), but a
   *  fresh poll on view-switch matches what users expect — the
   *  mailbox you just opened should reflect the server, not whatever
   *  state the background loop last landed. The first run fires on
   *  initial load into `'inbox'`, which is redundant with the bg
   *  loop's startup poll but cheap and predictable. */
  $effect(() => {
    if (currentView === 'inbox') {
      void invoke('check_mail_now').catch((e) =>
        console.warn('auto check_mail_now on view switch failed:', e),
      )
    }
  })

  async function onSetupComplete() {
    // After adding an account, refresh the account list so we pick
    // up the new account's ID, then switch to the inbox.
    await checkAccounts()
    currentView = 'inbox'
  }

  function selectMessage(uid: number, accountId?: string) {
    selectedUid = uid
    // Unified mode: each row carries its owning account id so MailView
    // can fetch from the right account. Outside unified mode, the
    // active account is implicit.
    selectedMessageAccountId = accountId ?? null
  }

  // Changing the folder resets the open message — the UID that was
  // selected doesn't exist in the new folder, so showing it would be
  // stale at best.
  function selectFolder(name: string) {
    selectedFolder = name
    selectedUid = null
  }

  // MailView fires this after it successfully marks a message \Seen on
  // the server. Bumping the token makes MailList + Sidebar re-fetch so
  // the bold "unread" styling and the folder badge update immediately.
  function onMessageRead(_uid: number) {
    refreshToken++
  }

  /** The currently shown message has been archived or deleted on the
   *  server. Drop the selection so the reading pane returns to the
   *  "pick a message" placeholder, and bump `refreshToken` so MailList
   *  + Sidebar re-query the server and the row disappears. */
  function onMessageRemoved() {
    selectedUid = null
    selectedMessageAccountId = null
    refreshToken++
  }

  // Open the Compose modal. Called with no arg for a blank new message,
  // or with a prefill for reply/reply-all/forward.
  function openCompose(initial: ComposeInitial = {}) {
    composeInitial = initial
  }

  function closeCompose() {
    composeInitial = null
    // Force the mail list + sidebar to re-query the server. Compose's
    // save-draft / send paths modify the Drafts and Sent folders
    // (APPEND + expunge) without touching the envelope cache, so the
    // UI would otherwise stay on the pre-compose view until the user
    // clicked another folder.
    refreshToken++
  }

  /** Build a quoted reply body as HTML.
   *
   *  Output shape (the Compose editor's `initialBodyHtml` accepts
   *  literal HTML — it detects tags and passes the string straight
   *  through instead of escaping):
   *
   *    <p></p>
   *    <p></p>
   *    <p>On <date>, <from> wrote:</p>
   *    <blockquote>...original body, escaped or passed-through...</blockquote>
   *
   *  The two leading empty paragraphs give the user a visible cursor
   *  above the quote to start typing into. The `<blockquote>` is
   *  Tiptap-native, so the styling we already have (left bar, indent,
   *  muted colour) applies; when the message is sent, the HTML flows
   *  straight through to the wire so the recipient's client renders
   *  it the same way every other client does.
   */
  function quoteBody(from: string, date: string, body: string | null): string {
    const esc = (s: string) =>
      s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    const bodyHtml = htmlOrEscape(body ?? '')
    const when = new Date(date).toLocaleString()
    return (
      `<p></p><p></p>` +
      `<p>On ${esc(when)}, ${esc(from)} wrote:</p>` +
      `<blockquote>${bodyHtml}</blockquote>`
    )
  }

  /** If the input already looks like HTML, pass it through. Otherwise
   *  escape special chars and convert newlines to `<br>` so the plain
   *  text renders with its original line breaks inside the blockquote. */
  function htmlOrEscape(text: string): string {
    if (/<[a-z][\s\S]*>/i.test(text)) return text
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/\n/g, '<br>')
  }

  function replySubject(s: string): string {
    return /^re:/i.test(s) ? s : `Re: ${s}`
  }

  function forwardSubject(s: string): string {
    return /^fwd?:/i.test(s) ? s : `Fwd: ${s}`
  }

  type OpenMail = {
    from: string
    to: string[]
    cc: string[]
    subject: string
    body_text: string | null
    date: string
  }

  function onReply(mail: OpenMail) {
    openCompose({
      to: mail.from,
      subject: replySubject(mail.subject),
      body: quoteBody(mail.from, mail.date, mail.body_text),
    })
  }

  function onReplyAll(mail: OpenMail) {
    const others = [...mail.to, ...mail.cc].filter(
      (a) => a && a.toLowerCase() !== activeAccountEmail.toLowerCase(),
    )
    openCompose({
      to: mail.from,
      cc: others.join(', '),
      subject: replySubject(mail.subject),
      body: quoteBody(mail.from, mail.date, mail.body_text),
    })
  }

  /** Does the given folder name look like the account's Drafts folder?
   *  Mirrors the Rust-side `pick_drafts_folder` name-hint list (the
   *  authoritative `\Drafts` special-use attribute lives on the server
   *  and we don't propagate it to the frontend yet, so this is the
   *  pragmatic "good enough" heuristic). */
  const DRAFTS_NAME_HINTS = ['drafts', 'draft', 'entwürfe', 'entwurf', 'brouillons', 'brouillon']
  function isDraftsFolderName(name: string): boolean {
    const lower = name.toLowerCase()
    return DRAFTS_NAME_HINTS.some((h) => lower.includes(h))
  }
  const isDraftsFolder = $derived(isDraftsFolderName(selectedFolder))

  /** Open a draft from the Drafts folder back in Compose for editing.
   *  Mirrors the reply/forward entry points but additionally:
   *    - downloads every attachment's bytes so the user can re-send
   *      or re-save without the attachments silently dropping;
   *    - records the source UID/folder in `draftSource` so Compose
   *      can expunge the server-side copy once the edit is sent or
   *      re-saved (otherwise the Drafts mailbox accumulates one
   *      copy per edit).
   *  The reply-style guard fields (`in_reply_to`) stay unset: this is
   *  a continuation of the user's own work, not a response to someone
   *  else, so the signature effect correctly skips re-inserting. */
  type DraftMail = OpenMail & {
    account_id: string
    folder: string
    bcc?: string[]
    body_html: string | null
    attachments: { filename: string; content_type: string; part_id: number }[]
  }
  async function onEditDraft(mail: DraftMail) {
    if (selectedUid == null) return
    const uid = selectedUid
    // Pull every attachment's bytes. Parallel — even mid-size drafts
    // rarely have more than a couple of attachments, and the IMAP
    // backend already reuses one connection per `fetch_message`
    // command internally.
    const attachments = await Promise.all(
      mail.attachments.map(async (att) => ({
        filename: att.filename,
        content_type: att.content_type,
        data: await invoke<number[]>('download_email_attachment', {
          accountId: mail.account_id,
          folder: mail.folder,
          uid,
          partId: att.part_id,
        }),
        // Fresh content_id — the `/` editor shortcut references
        // attachments by this id, so each one needs a value even
        // when we're rehydrating a draft. Any `cid:` refs already
        // baked into the old draft body are intentionally broken
        // by this: fixing them up would mean parsing the stored
        // HTML and rewriting refs, which is scope-heavier and can
        // wait. For a freshly-edited draft you just re-pick the
        // attachment via `/` to relink.
        content_id: crypto.randomUUID().replaceAll('-', ''),
      })),
    )
    openCompose({
      to: mail.to.join(', '),
      cc: mail.cc.join(', '),
      bcc: (mail.bcc ?? []).join(', '),
      subject: mail.subject,
      // Prefer the HTML body — the editor is a rich-text editor and
      // will pass the HTML through unchanged (`textToHtml` detects
      // tags). Fall back to plain text for the rare HTML-less draft.
      body: mail.body_html ?? mail.body_text ?? '',
      attachments,
      draftSource: { accountId: mail.account_id, folder: mail.folder, uid },
    })
  }

  function onForward(mail: OpenMail) {
    // Forwards use the same blockquote treatment as replies so the
    // original message sits inside a visually distinct container.
    // Unlike reply, we prefix with a small header block that states
    // the original From/Date/Subject so the recipient can see the
    // chain even if they collapse the quote.
    const esc = (s: string) =>
      s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    const when = new Date(mail.date).toLocaleString()
    const header =
      `<p><strong>---------- Forwarded message ----------</strong></p>` +
      `<p>From: ${esc(mail.from)}<br>` +
      `Date: ${esc(when)}<br>` +
      `Subject: ${esc(mail.subject)}</p>`
    const body = htmlOrEscape(mail.body_text ?? '')
    openCompose({
      subject: forwardSubject(mail.subject),
      body:
        `<p></p><p></p>` +
        `<blockquote>${header}${body}</blockquote>`,
    })
  }

  // ── "Create Talk room from this thread" flow ────────────────
  // Triggered from MailView's 💬 Talk button. Opens a modal seeded
  // with the email's subject and the thread's participants; on
  // create, chains into Compose with the room link in the body and
  // the same recipients in the To field, satisfying issue #13's
  // "create a Talk room from an email thread" task in one user
  // gesture.
  let talkRoomDraft = $state<{
    ncId: string
    initialName: string
    initialParticipants: string[]
    /** Pre-fills Compose's `To` after the room is created — kept on
        the draft so we don't have to re-derive it from the email. */
    composeTo: string
  } | null>(null)

  /** Strip an `"Name" <addr>` wrapper down to the bare email. */
  function bareEmail(s: string): string | null {
    const t = s.trim()
    if (!t) return null
    const m = t.match(/^\s*(?:"[^"]*"|[^<]*?)\s*<([^>]+)>\s*$/)
    return m ? m[1].trim() : t
  }

  async function onCreateTalkFromMail(mail: OpenMail) {
    // For the MVP we use the first connected Nextcloud account. A
    // multi-account picker can land later — most users have one NC.
    let ncId = ''
    try {
      const list = await invoke<{ id: string }[]>('get_nextcloud_accounts')
      if (list.length === 0) {
        alert('Connect a Nextcloud account first (Settings → Nextcloud).')
        return
      }
      ncId = list[0].id
    } catch (e) {
      alert(`Failed to load Nextcloud accounts: ${e}`)
      return
    }

    // Build the participant set: From + To + Cc, deduped, minus the
    // current user (who's already in the room as the creator).
    const seen = new Set<string>()
    const participants: string[] = []
    for (const piece of [mail.from, ...mail.to, ...mail.cc]) {
      const addr = bareEmail(piece)
      if (!addr) continue
      const key = addr.toLowerCase()
      if (key === activeAccountEmail.toLowerCase()) continue
      if (seen.has(key)) continue
      seen.add(key)
      participants.push(addr)
    }

    // The Compose `To` field happily accepts the original
    // `"Name" <addr>` strings, so display names round-trip into the
    // sent invite without us having to re-format.
    const composeTo = [mail.from, ...mail.to, ...mail.cc]
      .filter((a) => {
        const e = bareEmail(a)
        return e && e.toLowerCase() !== activeAccountEmail.toLowerCase()
      })
      .join(', ')

    talkRoomDraft = {
      ncId,
      initialName: mail.subject || 'Talk',
      initialParticipants: participants,
      composeTo,
    }
  }

  function onTalkRoomCreatedFromMail(room: TalkRoom) {
    const draft = talkRoomDraft
    talkRoomDraft = null
    if (!draft) return
    openCompose({
      to: draft.composeTo,
      subject: `Join Talk: ${room.display_name}`,
      talkLink: { name: room.display_name, url: room.web_url },
    })
  }
</script>

<!-- Loading / Setup both run before the user has an account, so the
     IconRail (which is keyed by accounts) isn't mounted. -->
{#if currentView === 'loading'}
  <div class="h-full flex items-center justify-center bg-surface-50 dark:bg-surface-900">
    <p class="text-surface-500">Loading...</p>
  </div>
{:else if currentView === 'setup'}
  <AccountSetup oncomplete={onSetupComplete} />
{:else}
  <!-- Post-setup shell: IconRail is mounted *once* outside the
       currentView branches, so switching between Mail, Contacts,
       Calendar, Files, Talk, or Settings never remounts the rail
       (keeps the Talk unread poll warm, avatars stable, ring
       transitions smooth). Every view below sits inside the same
       flex row so the rail is always on the far left.

       Compose is also mounted here — it's an overlay modal, so it
       stacks on top of whichever view the user came from without
       the view needing to know about it. -->
  <div class="h-full flex">
    <IconRail
      accounts={accounts}
      accountId={activeAccountId}
      unified={unifiedMode}
      currentView={currentView}
      onselectaccount={selectAccount}
      onselectview={onSelectView}
    />

    {#if !activeAccountId}
      <div class="flex-1 flex items-center justify-center bg-surface-50 dark:bg-surface-900">
        <p class="text-surface-500">No account selected.</p>
      </div>
    {:else if currentView === 'settings'}
      <div class="flex-1 min-w-0 overflow-auto">
        <AccountSettings
          onclose={goToInbox}
          onaddaccount={goToSetup}
          onappprefschanged={(p) => (appPrefs = p)}
        />
      </div>
    {:else if currentView === 'contacts'}
      <div class="flex-1 min-w-0">
        <ContactsView onclose={goToInbox} />
      </div>
    {:else if currentView === 'calendar'}
      <div class="flex-1 min-w-0">
        <CalendarView onclose={goToInbox} />
      </div>
    {:else if currentView === 'files'}
      <div class="flex-1 min-w-0">
        <FilesView onclose={goToInbox} oncompose={openCompose} />
      </div>
    {:else if currentView === 'talk'}
      <div class="flex-1 min-w-0">
        <TalkView onclose={goToInbox} oncompose={openCompose} />
      </div>
    {:else}
      <!-- Mail view: Sidebar (folders) + mail-list column + MailView.
           Sidebar is now much leaner — just Compose + folder tree —
           since the shell chrome lives on the rail. -->
      <Sidebar
        accounts={accounts}
        accountId={activeAccountId}
        selectedFolder={selectedFolder}
        refreshToken={refreshToken}
        unified={unifiedMode}
        onselectfolder={selectFolder}
        oncompose={() => openCompose()}
        onaccountschanged={checkAccounts}
      />
      <!-- Mail-list column: SearchBar on top, then either MailList
           or SearchResults depending on whether the user is
           searching. Search isn't wired for unified mode yet —
           searching while unified is enabled scopes back to the
           active account, which is the safer default than silently
           returning nothing. -->
      <div class="flex flex-col w-80 shrink-0 border-r border-surface-200 dark:border-surface-700">
        <SearchBar
          accountId={activeAccountId}
          currentFolder={selectedFolder}
          onsearch={onSearch}
        />
        <div class="flex-1 min-h-0 flex">
          {#if searchActive}
            <SearchResults
              accountId={activeAccountId}
              currentFolder={selectedFolder}
              query={searchQuery}
              scope={searchScope}
              filters={searchFilters}
              selectedUid={selectedUid}
              onselect={onSelectSearchHit}
            />
          {:else}
            <MailList
              accounts={accounts}
              accountId={activeAccountId}
              folder={selectedFolder}
              unified={unifiedMode}
              selectedUid={selectedUid}
              refreshToken={refreshToken}
              onselect={selectMessage}
            />
          {/if}
        </div>
      </div>
      <MailView
        accountId={selectedMessageAccountId ?? activeAccountId}
        folder={selectedFolder}
        uid={selectedUid}
        onread={onMessageRead}
        onreply={onReply}
        onreplyall={onReplyAll}
        onforward={onForward}
        oncreatetalk={onCreateTalkFromMail}
        isDraftsFolder={isDraftsFolder}
        oneditdraft={onEditDraft}
        onmessageremoved={onMessageRemoved}
      />
    {/if}

    {#if composeInitial !== null}
      <Compose
        accounts={accounts}
        accountId={activeAccountId ?? ''}
        initial={composeInitial}
        onclose={closeCompose}
      />
    {/if}
  </div>
{/if}

<!-- Talk-room creation modal — mounted at the app level so it can
     overlay any view. Driven entirely by `talkRoomDraft`: setting it
     opens the modal pre-filled, clearing it (or `oncreated`)
     dismisses. -->
{#if talkRoomDraft}
  <CreateTalkRoomModal
    ncId={talkRoomDraft.ncId}
    initialName={talkRoomDraft.initialName}
    initialParticipants={talkRoomDraft.initialParticipants}
    onclose={() => (talkRoomDraft = null)}
    oncreated={onTalkRoomCreatedFromMail}
  />
{/if}
