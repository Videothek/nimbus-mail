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
  import LockScreen from './lib/LockScreen.svelte'
  import Compose, {
    type ComposeInitial,
    type SendFailurePayload,
  } from './lib/Compose.svelte'
  import ContactsView from './lib/ContactsView.svelte'
  import CalendarView from './lib/CalendarView.svelte'
  import FilesView from './lib/FilesView.svelte'
  import TalkView from './lib/TalkView.svelte'
  import NotesView from './lib/NotesView.svelte'
  import EventEditor, { type SavedEvent } from './lib/EventEditor.svelte'
  import SearchBar, {
    type SearchScope,
    type SearchFilters,
  } from './lib/SearchBar.svelte'
  import SearchResults from './lib/SearchResults.svelte'
  import {
    applyTheme,
    installSystemModeListener,
    registerCustomThemePath,
    setCustomThemes,
    unregisterCustomThemePath,
    type ThemeMode,
    type ThemeOption,
  } from './lib/theme'

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
    | 'notes'
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
    /** Display order in the IconRail; lower = top.  Lets us pick
     *  the visually-first account on launch instead of the one
     *  that happens to be first in the DB's insertion order. */
    sort_order?: number
  }
  let accounts = $state<Account[]>([])
  let activeAccountId = $state<string | null>(null)

  // ── Database lock state (#164 Phase 1B) ─────────────────────
  // Cache may be in FIDO-only mode at boot; in that case the
  // lock screen mounts ahead of every other view and the rest
  // of the app stays inert until the user authenticates.
  interface DatabaseStatus {
    locked: boolean
    needsSetup: boolean
    methods: {
      kind: 'fido_prf' | 'passphrase'
      credentialId: string
      label: string
      salt: string
      createdAt: number
    }[]
    attemptsRemaining: number | null
  }
  let dbStatus = $state<DatabaseStatus | null>(null)
  let dbStatusError = $state('')
  $effect(() => {
    void invoke<DatabaseStatus>('database_status')
      .then((s) => (dbStatus = s))
      .catch((e) => {
        console.warn('database_status failed', e)
        dbStatusError = String(e)
        // Fail-open: assume unlocked so the user isn't trapped on
        // a blank screen if the IPC went wrong.  Real lock-state
        // bugs surface as "every other IPC errors with locked".
        dbStatus = { locked: false, needsSetup: false, methods: [], attemptsRemaining: null }
      })
  })
  function onUnlocked() {
    if (dbStatus) dbStatus = { ...dbStatus, locked: false }
  }
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

  // Bindable mirror of MailList's currently-rendered envelope rows.
  // Used by the auto-advance-after-delete flow (#99) to pick the
  // UID of the row directly below the one we just removed without
  // having to re-implement the cache fetch up here.  Shape mirrors
  // MailList's local `EmailEnvelope` interface — we only ever read
  // `uid` / `account_id` here, but the bind requires the full
  // shape to type-check both sides.
  type MailListEnvelope = {
    uid: number
    folder: string
    from: string
    subject: string
    date: string
    is_read: boolean
    is_starred: boolean
    account_id: string
  }
  let mailListEnvelopes = $state<MailListEnvelope[]>([])

  // Network-refresh state for the IconRail's active-account
  // avatar spinner (#161).  Each child component (MailList,
  // MailView) flips its own flag while a post-cache fetch is
  // in flight; we OR them so a refresh in either pane lights
  // the same indicator.
  let mailListRefreshing = $state(false)
  let mailViewRefreshing = $state(false)
  const mailRefreshing = $derived(mailListRefreshing || mailViewRefreshing)

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
  // Wait until the cache is actually unlocked before asking Rust
  // for the account list — `get_accounts` returns `Locked` while
  // the FIDO unlock screen is up, and that error path used to
  // route the user into the setup wizard even when accounts
  // existed.  Re-runs after `onUnlocked` flips `dbStatus.locked`.
  $effect(() => {
    if (dbStatus && !dbStatus.locked) {
      void checkAccounts()
    }
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
  // Absolute path to our app icon, fetched once at startup. Passed
  // to `sendNotification` so libnotify (Linux) / NSUserNotification
  // (macOS) / WinRT (Windows) show the Nimbus icon next to each
  // toast instead of a generic placeholder. Empty until the
  // backend `get_notification_icon_path` resolves.
  let notificationIconPath = $state<string>('')
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
    mail_html_white_background: boolean
    auto_advance_after_remove: boolean
    talk_reminder_enabled: boolean
    autostart_enabled: boolean
    /** User-imported Skeleton themes (#132 tier 2). */
    custom_themes?: CustomTheme[]
  }
  type CustomTheme = {
    id: string
    label: string
    description?: string
    path: string
  }

  // Issue #123: Talk-join reminders.  Rust scans upcoming
  // events on every sync tick and emits this event whenever an
  // event with a Talk URL hits one of its VALARM lead times.
  type TalkReminder = {
    uid: string
    summary: string
    start: string
    talkUrl: string
    minutesBefore: number
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

  /** Best-effort startup cleanup for the Office viewer's temp area
   *  on every connected Nextcloud. If Nimbus crashed mid-edit, or
   *  `office_close_attachment` errored on the way out last session,
   *  the user's `/Nimbus Mail/temp` folder accumulates orphan
   *  uploads. The Rust sweeper scopes by mtime so a still-open
   *  edit window in a parallel Nimbus instance doesn't get its
   *  file pulled out from under it. Failures are logged and
   *  swallowed — no toast, no UI block. */
  async function sweepNextcloudTempFiles() {
    try {
      const accounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
      for (const a of accounts) {
        try {
          await invoke('office_sweep_temp', { ncId: a.id })
        } catch (e) {
          console.warn('office_sweep_temp failed for', a.id, e)
        }
      }
    } catch (e) {
      console.warn('sweepNextcloudTempFiles: get_nextcloud_accounts failed', e)
    }
  }

  function shouldNotify(): boolean {
    return (
      notificationsGranted && (appPrefs?.notifications_enabled ?? true)
    )
  }

  async function fireToast(title: string, body: string) {
    // On Linux the native command sends through `notify-rust` with
    // the `DesktopEntry` hint set, so notifications land in the
    // notification center / history (GNOME Shell, KDE Plasma).  The
    // command returns `false` on non-Linux platforms so we fall
    // through to the Tauri plugin, whose macOS / Windows backends
    // already wire in the right OS hooks.
    try {
      const handled = await invoke<boolean>('send_native_notification', {
        title,
        body,
      })
      if (handled) return
    } catch (err) {
      console.warn('send_native_notification failed, falling back to plugin', err)
    }
    try {
      sendNotification({
        title,
        body,
        ...(notificationIconPath ? { icon: notificationIconPath } : {}),
      })
    } catch (err) {
      console.warn('sendNotification failed', err)
    }
  }

  /** Format "in 5 min" / "in 1 hour" / "now" given a positive
   *  lead-time in minutes — wording the body of a Talk reminder
   *  toast so the user knows how soon to drop into the call. */
  function formatLeadTime(min: number): string {
    if (min <= 0) return 'now'
    if (min < 60) return `in ${min} min`
    const hours = Math.floor(min / 60)
    const remainder = min % 60
    if (remainder === 0) return `in ${hours} hour${hours === 1 ? '' : 's'}`
    return `in ${hours}h ${remainder}m`
  }

  /** Emoji-prefixed clock label for the body line.  We don't
   *  rely on click-to-launch (Linux libnotify doesn't expose
   *  it through the plugin), so spelling the join URL into the
   *  body keeps the affordance visible even when the user has
   *  to copy/paste it. */
  function handleTalkReminder(payload: TalkReminder) {
    if (!shouldNotify()) return
    if (!appPrefs?.talk_reminder_enabled) return
    const lead = formatLeadTime(payload.minutesBefore)
    const startLocal = new Date(payload.start).toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
    })
    const title = `📅 ${payload.summary || 'Meeting'} — ${lead}`
    const body = `Starts at ${startLocal} · Click to join via Talk`
    void fireToast(title, body)
    // Best-effort: tell the OS handler to open the Talk room
    // when the user chooses to act on the reminder.  We open
    // immediately for "now"-bucket reminders (≤1 min lead) so
    // the user lands in the room without an extra click; for
    // earlier reminders we just toast and let them decide.
    if (payload.minutesBefore <= 1) {
      void invoke('open_url', { url: payload.talkUrl }).catch((err) =>
        console.warn('open_url for talk reminder failed', err),
      )
      void invoke('dismiss_talk_reminder', { uid: payload.uid }).catch(() => {})
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
      void fireToast(payload.from || 'New mail', payload.subject || '(no subject)')
      return
    }

    // 4th+ toast in the window — suppress individual toast and
    // schedule one summary toast for the end of the window.
    if (pendingSummaryTimer) clearTimeout(pendingSummaryTimer)
    const count = recentBurst.length
    pendingSummaryTimer = setTimeout(() => {
      void fireToast('Nimbus Mail', `${count} new messages`)
      pendingSummaryTimer = null
    }, 600)
  }

  async function loadAppPrefs() {
    try {
      appPrefs = await invoke<AppPrefs>('get_app_settings')
      // Seed the theme module's custom-theme registry so the
      // picker + the runtime <link> swap know about the user's
      // imported themes (#132).  Re-runs on every reload so
      // imports/removals from another window stay in sync.
      const list: CustomTheme[] = appPrefs.custom_themes ?? []
      const options: ThemeOption[] = list.map((t) => ({
        id: t.id,
        label: t.label,
        description: t.description ?? 'Imported theme',
        custom: true,
      }))
      setCustomThemes(options)
      for (const t of list) registerCustomThemePath(t.id, t.path)
      // Drop any stale entries from a previous load that the
      // user has since removed.
      const liveIds = new Set(list.map((t) => t.id))
      for (const id of Object.keys(prevCustomThemeIds)) {
        if (!liveIds.has(id)) unregisterCustomThemePath(id)
      }
      prevCustomThemeIds = Object.fromEntries(list.map((t) => [t.id, true]))
    } catch (err) {
      console.warn('get_app_settings failed', err)
    }
  }
  let prevCustomThemeIds: Record<string, boolean> = {}

  async function loadNotificationIconPath() {
    try {
      notificationIconPath = await invoke<string>('get_notification_icon_path')
    } catch (err) {
      console.warn('get_notification_icon_path failed', err)
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
    void loadNotificationIconPath()
    void sweepNextcloudTempFiles()

    let unlistenNewMail: UnlistenFn | null = null
    let unlistenTalkReminder: UnlistenFn | null = null
    let unlistenCustomThemes: UnlistenFn | null = null
    let unlistenCompose: UnlistenFn | null = null
    let unlistenComposeFromMail: UnlistenFn | null = null
    let unlistenEditDraftFromMail: UnlistenFn | null = null
    let unlistenMailtoFromMail: UnlistenFn | null = null
    ;(async () => {
      unlistenNewMail = await listen<NewMail>('new-mail', (e) =>
        handleNewMail(e.payload),
      )
      unlistenTalkReminder = await listen<TalkReminder>(
        'talk-join-reminder',
        (e) => handleTalkReminder(e.payload),
      )
      // #132: backend fires this whenever a custom theme is
      // imported / removed (in this window or another).  Re-pull
      // settings so the picker + the runtime <link> registry
      // both refresh without a full reload.
      unlistenCustomThemes = await listen('custom-themes-changed', () =>
        loadAppPrefs(),
      )
      unlistenCompose = await listen('open-compose', () => openCompose({}))
      // Standalone-mail windows (#104) emit these when the user
      // hits Reply / Reply All / Forward over there: we run the
      // existing compose flow here in the main window so the user
      // ends up with one Compose surface, with all autocomplete
      // and signature state already wired up.
      unlistenComposeFromMail = await listen<{
        kind: 'reply' | 'reply-all' | 'forward'
        mail: OpenMail
      }>('compose-from-mail', (e) => {
        const { kind, mail } = e.payload
        if (kind === 'reply') onReply(mail)
        else if (kind === 'reply-all') onReplyAll(mail)
        else if (kind === 'forward') onForward(mail)
      })
      unlistenEditDraftFromMail = await listen<{ mail: DraftMail }>(
        'edit-draft-from-mail',
        (e) => onEditDraft(e.payload.mail),
      )
      unlistenMailtoFromMail = await listen<{
        init: { to?: string; cc?: string; bcc?: string; subject?: string; body?: string }
      }>('mailto-from-mail', (e) => openCompose(e.payload.init))
    })()
    return () => {
      unlistenNewMail?.()
      unlistenTalkReminder?.()
      unlistenCustomThemes?.()
      unlistenCompose?.()
      unlistenComposeFromMail?.()
      unlistenEditDraftFromMail?.()
      unlistenMailtoFromMail?.()
      if (pendingSummaryTimer) clearTimeout(pendingSummaryTimer)
    }
  })

  async function checkAccounts() {
    try {
      const list = await invoke<Account[]>('get_accounts')
      accounts = list
      if (list.length > 0) {
        // Keep the current selection if it still exists (e.g. after
        // adding another account); otherwise fall back to the
        // visually-first account.  `list` is in insertion order; the
        // IconRail and sidebar render by `sort_order`, so we sort
        // here to match — otherwise the auto-picked account isn't
        // the one the user sees at the top of the rail (#161).
        if (!activeAccountId || !list.some((a) => a.id === activeAccountId)) {
          const sorted = [...list].sort((a, b) => {
            const ao = a.sort_order ?? 0
            const bo = b.sort_order ?? 0
            if (ao !== bo) return ao - bo
            return a.id.localeCompare(b.id)
          })
          activeAccountId = sorted[0].id
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
    // Picking an account from the IconRail always means "show me
    // mail for this account" — even from calendar / contacts /
    // settings, where the rail is still visible.  Flip the view
    // before any of the early-return paths so a click from
    // another view always lands you in the inbox (#161).
    const wasOnMail = currentView === 'inbox'
    if (currentView !== 'inbox') currentView = 'inbox'

    if (id === '__all__') {
      if (unifiedMode && wasOnMail) return
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
    if (!unifiedMode && id === activeAccountId && wasOnMail) return
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

  // MailView fires this after it successfully marks a message \Seen
  // on the server.  Used to bump `refreshToken` to force a full
  // MailList reload, but that races against the user's next click —
  // the `fetch_envelopes` IMAP call is in flight when the next
  // optimistic action runs, then lands and overwrites the local
  // list (#174 follow-up).  Flip the matching envelope's flag in
  // the bound list directly instead; the cache row was already
  // updated by the backend, and the per-account unread badge is
  // driven by its own `unread-count-by-account-updated` event.
  function onMessageRead(uid: number) {
    const idx = mailListEnvelopes.findIndex((e) => e.uid === uid)
    if (idx >= 0 && !mailListEnvelopes[idx].is_read) {
      mailListEnvelopes[idx].is_read = true
    }
  }

  /** The currently shown message has been archived or deleted on the
   *  server.  Auto-advances the reading pane to the row directly
   *  below the removed one (or the row above when the removed row
   *  was last) so triage flows don't bounce back to the empty
   *  "pick a message" placeholder after every delete / archive
   *  click.  Falls back to clearing the pane when the list is now
   *  empty, when we can't find the removed UID in the current
   *  rendered list, or when the user has explicitly opted out via
   *  `appPrefs.auto_advance_after_remove`. */
  function onMessageRemoved(removedUid: number) {
    // Auto-advance only fires when the removed message is the one
    // currently open in the reading pane.  For drag-and-drop moves
    // (#89) the user typically drags a non-selected row to a folder
    // — yanking the pane to that row's neighbour would be
    // disorienting, so we leave the current selection alone.
    const wasSelected = selectedUid === removedUid

    if (wasSelected) {
      let nextUid: number | null = null
      let nextAccountId: string | null = null

      if (appPrefs?.auto_advance_after_remove ?? true) {
        const idx = mailListEnvelopes.findIndex((e) => e.uid === removedUid)
        if (idx >= 0) {
          // Visually the list is sorted newest-first, so the row
          // "below" the current one is `idx + 1` (older message).
          // When the removed row was at the bottom, we step up to
          // `idx - 1` instead, matching what every mainstream
          // client does after deleting the oldest visible mail.
          const next = mailListEnvelopes[idx + 1] ?? mailListEnvelopes[idx - 1]
          if (next) {
            nextUid = next.uid
            nextAccountId = next.account_id || null
          }
        }
      }

      selectedUid = nextUid
      selectedMessageAccountId = nextAccountId
    }

    // Drop the matching envelope from the bound list (#174
    // follow-up).  MailList's own optimistic delete/move already
    // removed the row from its internal `envelopes`, in which
    // case this is a no-op.  The path that *needs* this is
    // Sidebar's drag-and-drop drop handler — it fires
    // `onmessagemoved` per UID but doesn't touch MailList's
    // state directly, so without this splice the moved row stays
    // visible until a folder switch, and clicking it lands on a
    // UID the cache has already dropped → "no message with UID".
    const idx = mailListEnvelopes.findIndex((e) => e.uid === removedUid)
    if (idx >= 0) {
      mailListEnvelopes = [
        ...mailListEnvelopes.slice(0, idx),
        ...mailListEnvelopes.slice(idx + 1),
      ]
    }

    // Deliberately *not* bumping `refreshToken` here.  After the
    // optimistic flow Phase 1 already dropped the row from
    // MailList's local list and Phase 2 tombstoned the cache row,
    // so any reload would just race a `fetch_envelopes` IMAP
    // call against the next click — making sequential deletes
    // feel laggy because the second click hits a list mid-
    // network-refresh.  Background sync's `new-mail` event drives
    // the genuine refresh path; this one's purely local.
  }

  // Open the Compose modal. Called with no arg for a blank new message,
  // or with a prefill for reply/reply-all/forward.
  function openCompose(initial: ComposeInitial = {}) {
    // A fresh open shouldn't carry over the error banner from a
    // previous failed background send (#156).
    composeSendError = ''
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

  // ── Background-send failure recovery (#156) ─────────────────
  // Compose now closes the modal as soon as the user clicks Send;
  // the IMAP submission runs in the background.  When that
  // submission fails after the modal is gone we surface the
  // error here AND re-open Compose pre-filled with the user's
  // draft so they can retry without retyping.
  let composeSendError = $state<string>('')
  function onComposeSendFailed(payload: SendFailurePayload) {
    composeSendError = payload.errorMessage
    // Re-open Compose with the original draft.  Setting
    // `composeInitial` triggers the mount in the same shell-
    // level branch the original Compose lived in.
    composeInitial = payload.draft
    // Try to also fire an OS-level notification so the user
    // notices the failure even if their attention has drifted
    // off the Nimbus window.  Best-effort — silently ignore on
    // platforms / permissions where it can't post.
    if (notificationsGranted) {
      try {
        sendNotification({
          title: 'Nimbus Mail — send failed',
          body: payload.errorMessage,
          icon: notificationIconPath || undefined,
        })
      } catch (e) {
        console.warn('send-failed notification failed', e)
      }
    }
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

  /** Same heuristic for the Sent folder — used to suppress the
   *  RSVP card on outbound invites the user themselves sent
   *  (you don't reply to your own meeting requests).  Same
   *  caveat as the Drafts hint: name-based until the backend
   *  surfaces `\Sent` special-use through the API. */
  const SENT_NAME_HINTS = ['sent', 'sent items', 'gesendet', 'envoyés', 'envoyes', 'inviati', 'enviados']
  function isSentFolderName(name: string): boolean {
    const lower = name.toLowerCase()
    return SENT_NAME_HINTS.some((h) => lower.includes(h))
  }
  const isSentFolder = $derived(isSentFolderName(selectedFolder))

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

  // ── "Respond with meeting" flow ────────────────────────────
  // Triggered from MailView's meeting button. Opens the full
  // EventEditor pre-filled with the email subject as the title, the
  // thread's From/To as required attendees, Cc as optional, and
  // (via `createTalkRoom: true`) auto-creates a Nextcloud Talk room
  // whose join URL lands in the event's location.  One gesture
  // turns an email into a calendar invite plus a meeting link.
  interface CalendarSummary {
    id: string
    nextcloud_account_id: string
    display_name: string
    color: string | null
    last_synced_at: string | null
    hidden?: boolean
    muted?: boolean
  }
  let meetingDraft = $state<{
    calendars: CalendarSummary[]
    draft: {
      calendarId: string
      start: Date
      end: Date
      summary: string
      requiredAttendees: string[]
      optionalAttendees: string[]
      createTalkRoom: boolean
    }
  } | null>(null)

  /** Strip an `"Name" <addr>` wrapper down to the bare email. */
  function bareEmail(s: string): string | null {
    const t = s.trim()
    if (!t) return null
    const m = t.match(/^\s*(?:"[^"]*"|[^<]*?)\s*<([^>]+)>\s*$/)
    return m ? m[1].trim() : t
  }

  /** Round a Date up to the next half-hour boundary.  Mirrors what
      a user would type when scheduling a fresh meeting "now-ish":
      11:07 → 11:30, 11:30 → 12:00. */
  /** Prefix the email subject with "Re: " to mark the event as a
      response to the thread.  Skips the prefix when the subject
      already starts with Re:/Aw:/Sv: (case-insensitive) so we don't
      stack "Re: Re: Re:" on a long reply chain. */
  function meetingSubject(subject: string): string {
    const s = subject.trim()
    if (!s) return 'Re: Meeting'
    if (/^(re|aw|sv)\s*:/i.test(s)) return s
    return `Re: ${s}`
  }

  function nextHalfHour(d: Date): Date {
    const out = new Date(d)
    out.setSeconds(0, 0)
    const m = out.getMinutes()
    out.setMinutes(m < 30 ? 30 : 60)
    return out
  }

  async function onRespondWithMeeting(mail: OpenMail) {
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

    let calendars: CalendarSummary[] = []
    try {
      calendars = await invoke<CalendarSummary[]>('get_cached_calendars', { ncId })
    } catch (e) {
      alert(`Failed to load calendars: ${e}`)
      return
    }
    const visible = calendars.filter((c) => !c.hidden)
    if (visible.length === 0) {
      alert('No writable calendars found on your Nextcloud account.')
      return
    }
    let initialCalendarId = visible[0].id
    try {
      const s = await invoke<{ default_calendar_id: string | null }>('get_app_settings')
      if (s.default_calendar_id && visible.some((c) => c.id === s.default_calendar_id)) {
        initialCalendarId = s.default_calendar_id!
      }
    } catch {}

    // Split the thread's participants — From + To go required,
    // Cc goes optional.  Skip the active account (the user is the
    // organizer; the editor adds them as CHAIR).  De-dupe across
    // buckets so an address that appears in both To and Cc only
    // shows up once in the higher-priority bucket.
    const self = activeAccountEmail.toLowerCase()
    const seen = new Set<string>()
    const required: string[] = []
    for (const piece of [mail.from, ...mail.to]) {
      const addr = bareEmail(piece)
      if (!addr) continue
      const key = addr.toLowerCase()
      if (key === self || seen.has(key)) continue
      seen.add(key)
      required.push(piece)
    }
    const optional: string[] = []
    for (const piece of mail.cc) {
      const addr = bareEmail(piece)
      if (!addr) continue
      const key = addr.toLowerCase()
      if (key === self || seen.has(key)) continue
      seen.add(key)
      optional.push(piece)
    }

    const start = nextHalfHour(new Date())
    const end = new Date(start.getTime() + 30 * 60 * 1000)

    meetingDraft = {
      calendars: visible,
      draft: {
        calendarId: initialCalendarId,
        start,
        end,
        summary: meetingSubject(mail.subject),
        requiredAttendees: required,
        optionalAttendees: optional,
        createTalkRoom: true,
      },
    }
  }

  function onMeetingEditorClose() {
    meetingDraft = null
  }
  function onMeetingEditorSaved(_saved?: SavedEvent) {
    meetingDraft = null
  }

  /** "Save as note" handler — issue #67's email→note bridge. Builds
      a markdown body that preserves the headers the user actually
      cares about (From / To / Date) so the note carries enough
      context to be useful when read months later. Body source
      preference: plain text first (already the right shape for
      markdown), falling back to a stripped HTML body so users on
      HTML-only senders still get readable note content. */
  async function onSaveMailAsNote(mail: OpenMail & { body_html?: string | null }) {
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

    const headerLines = [
      `**From:** ${mail.from}`,
      mail.to.length ? `**To:** ${mail.to.join(', ')}` : null,
      mail.cc.length ? `**Cc:** ${mail.cc.join(', ')}` : null,
      `**Date:** ${new Date(mail.date).toLocaleString()}`,
    ].filter(Boolean)

    let body = (mail.body_text ?? '').trim()
    if (!body && mail.body_html) {
      // Strip tags for the markdown note body — collapsing
      // whitespace afterwards keeps the result readable when the
      // sender's HTML had each block on its own line.
      const tmp = document.createElement('div')
      tmp.innerHTML = mail.body_html
      body = (tmp.textContent ?? '').trim()
    }

    const content = `${headerLines.join('  \n')}\n\n---\n\n${body}`
    const title = mail.subject || '(no subject)'

    try {
      await invoke('create_nextcloud_note', {
        ncId,
        title,
        content,
        category: 'Mail',
      })
      // Surface success via the same OS toast path new-mail uses,
      // when permission's been granted; otherwise fall back to a
      // plain alert so the user knows the save took.
      if (notificationsGranted) {
        fireToast('Saved to Notes', title)
      } else {
        alert(`Saved "${title}" to Nextcloud Notes.`)
      }
    } catch (e) {
      alert(`Failed to save note: ${e}`)
    }
  }
</script>

<!-- Lock screen (#164 Phase 1B) — when the cache is in FIDO-only
     mode at boot, the lock screen owns the whole viewport until
     the user authenticates.  Everything else (loading, setup,
     mail / calendar / contacts views) stays unmounted so no IPC
     fires with the cache still locked. -->
{#if dbStatus && dbStatus.locked}
  <LockScreen
    methods={dbStatus.methods}
    attemptsRemaining={dbStatus.attemptsRemaining}
    onattemptschange={(n) => {
      if (dbStatus) dbStatus = { ...dbStatus, attemptsRemaining: n }
    }}
    onunlock={onUnlocked}
  />
{:else if dbStatus === null}
  <!-- Brief flash while we wait for `database_status` to land —
       prevents the loading view from poking the cache before we
       know whether it's locked. -->
  <div class="h-full flex items-center justify-center bg-surface-50 dark:bg-surface-900">
    <p class="text-surface-500">Starting up…</p>
  </div>
{:else if currentView === 'loading'}
  <!-- Loading / Setup both run before the user has an account, so
       the IconRail (which is keyed by accounts) isn't mounted. -->
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
      mailRefreshing={mailRefreshing}
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
    {:else if currentView === 'notes'}
      <div class="flex-1 min-w-0">
        <NotesView onclose={goToInbox} oncompose={openCompose} />
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
        onmessagemoved={onMessageRemoved}
        onmovesfailed={() => refreshToken++}
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
              bind:envelopes={mailListEnvelopes}
              bind:refreshing={mailListRefreshing}
              onmessagemoved={onMessageRemoved}
            />
          {/if}
        </div>
      </div>
      <MailView
        accountId={selectedMessageAccountId ?? activeAccountId}
        folder={selectedFolder}
        uid={selectedUid}
        forceWhiteBackground={appPrefs?.mail_html_white_background ?? true}
        onread={onMessageRead}
        onreply={onReply}
        onreplyall={onReplyAll}
        onforward={onForward}
        onrespondwithmeeting={onRespondWithMeeting}
        onsavenote={onSaveMailAsNote}
        isDraftsFolder={isDraftsFolder}
        isSentFolder={isSentFolder}
        oneditdraft={onEditDraft}
        onmessageremoved={onMessageRemoved}
        onmailto={(init) => openCompose(init)}
        bind:refreshing={mailViewRefreshing}
      />
    {/if}

    {#if composeInitial !== null}
      <Compose
        accounts={accounts}
        accountId={activeAccountId ?? ''}
        initial={composeInitial}
        initialError={composeSendError}
        onclose={() => {
          composeSendError = ''
          closeCompose()
        }}
        onsendfailed={onComposeSendFailed}
      />
    {/if}
  </div>
{/if}

<!-- "Respond with meeting" event editor — mounted at the app level
     so it can overlay any view. Driven entirely by `meetingDraft`:
     setting it opens the editor pre-filled (subject as title,
     From/To as required attendees, Cc as optional, auto-created
     Talk room), clearing it dismisses. -->
{#if meetingDraft}
  <EventEditor
    mode="create"
    calendars={meetingDraft.calendars}
    draft={meetingDraft.draft}
    onclose={onMeetingEditorClose}
    onsaved={onMeetingEditorSaved}
  />
{/if}
