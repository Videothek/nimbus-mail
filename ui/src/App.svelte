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
  import Sidebar from './lib/Sidebar.svelte'
  import MailList from './lib/MailList.svelte'
  import MailView from './lib/MailView.svelte'
  import AccountSetup from './lib/AccountSetup.svelte'
  import AccountSettings from './lib/AccountSettings.svelte'
  import Compose, { type ComposeInitial } from './lib/Compose.svelte'
  import ContactsView from './lib/ContactsView.svelte'
  import CalendarView from './lib/CalendarView.svelte'
  import SearchBar, {
    type SearchScope,
    type SearchFilters,
  } from './lib/SearchBar.svelte'
  import SearchResults from './lib/SearchResults.svelte'

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
  let currentView = $state<View>('loading')

  // Which integration tab is active in the sidebar. Lives next to
  // `currentView` because the inbox shell and the integration views
  // share the same sidebar, and the sidebar needs to show the active
  // tab even when we're not in the mail-list view anymore.
  let activeIntegration = $state<string | null>(null)

  // ── Inbox state ─────────────────────────────────────────────
  // The active account (first configured one for now — multi-account
  // switching comes later) and the currently selected message UID.
  // Kept at the App level so MailList and MailView stay in sync.
  let activeAccountId = $state<string | null>(null)
  let activeAccountEmail = $state<string>('')
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
      const accounts = await invoke<Array<{ id: string; email: string }>>('get_accounts')
      if (accounts.length > 0) {
        activeAccountId = accounts[0].id
        activeAccountEmail = accounts[0].email
        currentView = 'inbox'
      } else {
        currentView = 'setup'
      }
    } catch {
      // If we can't load accounts (e.g. first launch, file doesn't exist),
      // show the setup wizard
      currentView = 'setup'
    }
  }

  // ── Navigation handlers ─────────────────────────────────────
  function goToInbox() {
    currentView = 'inbox'
    activeIntegration = null
  }

  function goToSetup() {
    currentView = 'setup'
  }

  function goToSettings() {
    currentView = 'settings'
  }

  // Sidebar "Integrations" click. Routes Contacts / Calendar to their
  // dedicated views; the other entries fall through until their
  // feature lands.
  function onSelectIntegration(name: string) {
    if (name === 'Contacts') {
      activeIntegration = name
      currentView = 'contacts'
    } else if (name === 'Calendar') {
      activeIntegration = name
      currentView = 'calendar'
    }
  }

  async function onSetupComplete() {
    // After adding an account, refresh the account list so we pick
    // up the new account's ID, then switch to the inbox.
    await checkAccounts()
    currentView = 'inbox'
  }

  function selectMessage(uid: number) {
    selectedUid = uid
  }

  // Changing the folder resets the open message — the UID that was
  // selected doesn't exist in the new folder, so showing it would be
  // stale at best.
  function selectFolder(name: string) {
    selectedFolder = name
    selectedUid = null
  }

  // Triggered by the sidebar's refresh button.
  function refreshAll() {
    refreshToken++
  }

  // MailView fires this after it successfully marks a message \Seen on
  // the server. Bumping the token makes MailList + Sidebar re-fetch so
  // the bold "unread" styling and the folder badge update immediately.
  function onMessageRead(_uid: number) {
    refreshToken++
  }

  // Open the Compose modal. Called with no arg for a blank new message,
  // or with a prefill for reply/reply-all/forward.
  function openCompose(initial: ComposeInitial = {}) {
    composeInitial = initial
  }

  function closeCompose() {
    composeInitial = null
  }

  // Build a quoted reply body — RFC 3676 style "> " prefix on each line.
  function quoteBody(from: string, date: string, body: string | null): string {
    const quoted = (body ?? '')
      .split('\n')
      .map((l) => `> ${l}`)
      .join('\n')
    return `\n\nOn ${new Date(date).toLocaleString()}, ${from} wrote:\n${quoted}`
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

  function onForward(mail: OpenMail) {
    openCompose({
      subject: forwardSubject(mail.subject),
      body: `\n\n---------- Forwarded message ----------\nFrom: ${mail.from}\nDate: ${new Date(mail.date).toLocaleString()}\nSubject: ${mail.subject}\n\n${mail.body_text ?? ''}`,
    })
  }
</script>

<!-- Loading state: shown briefly while we check for accounts -->
{#if currentView === 'loading'}
  <div class="h-full flex items-center justify-center bg-surface-50 dark:bg-surface-900">
    <p class="text-surface-500">Loading...</p>
  </div>

<!-- Setup wizard: first-time experience -->
{:else if currentView === 'setup'}
  <AccountSetup oncomplete={onSetupComplete} />

<!-- Account settings -->
{:else if currentView === 'settings'}
  <AccountSettings onclose={goToInbox} onaddaccount={goToSetup} />

<!-- Contacts view: sidebar stays put so the user can jump back to mail. -->
{:else if currentView === 'contacts' && activeAccountId}
  <div class="h-full flex">
    <Sidebar
      accountId={activeAccountId}
      selectedFolder={selectedFolder}
      refreshToken={refreshToken}
      activeIntegration={activeIntegration}
      onselectfolder={(f) => {
        selectFolder(f)
        goToInbox()
      }}
      onsettings={goToSettings}
      onrefresh={refreshAll}
      oncompose={() => openCompose()}
      onselectintegration={onSelectIntegration}
    />
    <div class="flex-1">
      <ContactsView onclose={goToInbox} />
    </div>
  </div>

<!-- Calendar view: same split as Contacts — sidebar on the left, the
     integration fills the rest of the window. -->
{:else if currentView === 'calendar' && activeAccountId}
  <div class="h-full flex">
    <Sidebar
      accountId={activeAccountId}
      selectedFolder={selectedFolder}
      refreshToken={refreshToken}
      activeIntegration={activeIntegration}
      onselectfolder={(f) => {
        selectFolder(f)
        goToInbox()
      }}
      onsettings={goToSettings}
      onrefresh={refreshAll}
      oncompose={() => openCompose()}
      onselectintegration={onSelectIntegration}
    />
    <div class="flex-1">
      <CalendarView onclose={goToInbox} />
    </div>
  </div>

<!-- Main inbox: the 3-panel mail client layout -->
{:else if activeAccountId}
  <div class="h-full flex">
    <Sidebar
      accountId={activeAccountId}
      selectedFolder={selectedFolder}
      refreshToken={refreshToken}
      activeIntegration={activeIntegration}
      onselectfolder={selectFolder}
      onsettings={goToSettings}
      onrefresh={refreshAll}
      oncompose={() => openCompose()}
      onselectintegration={onSelectIntegration}
    />
    <!-- Mail-list column: SearchBar on top, then either MailList
         or SearchResults depending on whether the user is searching. -->
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
            accountId={activeAccountId}
            folder={selectedFolder}
            selectedUid={selectedUid}
            refreshToken={refreshToken}
            onselect={selectMessage}
          />
        {/if}
      </div>
    </div>
    <MailView
      accountId={activeAccountId}
      folder={selectedFolder}
      uid={selectedUid}
      onread={onMessageRead}
      onreply={onReply}
      onreplyall={onReplyAll}
      onforward={onForward}
    />
    {#if composeInitial !== null}
      <Compose
        accountId={activeAccountId}
        fromAddress={activeAccountEmail}
        initial={composeInitial}
        onclose={closeCompose}
      />
    {/if}
  </div>
{:else}
  <div class="h-full flex items-center justify-center bg-surface-50 dark:bg-surface-900">
    <p class="text-surface-500">No account selected.</p>
  </div>
{/if}
