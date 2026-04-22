<script lang="ts">
  /**
   * Sidebar — account's folder list + integration shortcuts.
   *
   * Folders come from the IMAP server. We load the cached list first
   * so the sidebar paints instantly on launch, then refresh from the
   * network in parallel (same pattern as MailList / MailView).
   *
   * Clicking a folder calls `onselectfolder(name)` so App.svelte can
   * point MailList at the new folder.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { onDestroy } from 'svelte'
  import { formatError } from './errors'

  interface Folder {
    name: string
    delimiter: string | null
    attributes: string[]
    unread_count: number | null
  }

  /** One "folder name contains X → use icon Y" rule, mirror of the
      Rust `FolderIconRule` struct. Carried inside `Account` so the
      sidebar can apply per-account theming without a separate fetch. */
  interface FolderIconRule {
    keyword: string
    icon: string
  }

  /** Slim account row for the picker — matches the Rust `Account`
      struct's public fields that the switcher needs. Includes the
      user's custom folder-icon rules so the sidebar can apply them
      to the active account's folder list. */
  interface Account {
    id: string
    display_name: string
    email: string
    folder_icons?: FolderIconRule[]
  }

  /** Slim shape of a Talk room — only the fields we need for the
      aggregate unread badge. Avoids importing TalkView's type so the
      sidebar stays decoupled from the Talk view. */
  interface TalkRoomSummary {
    unread_messages: number
    unread_mention: boolean
  }

  interface Props {
    /** All configured mail accounts. Drives the account picker at the
        top of the sidebar; the picker is hidden when the list has
        fewer than two entries. */
    accounts?: Account[]
    accountId: string
    selectedFolder: string
    /** Bumped by the parent to force a network re-fetch (manual refresh). */
    refreshToken?: number
    /** Which integration tab (if any) is currently active. */
    activeIntegration?: string | null
    /** Whether the unified inbox is currently active. When true, the
        per-account folder list is hidden and a single "All Inboxes"
        entry takes its place. */
    unified?: boolean
    /** Called with a real account id to switch to that account, or with
        the sentinel `"__all__"` to enable unified-inbox mode. */
    onselectaccount?: (id: string) => void
    onselectfolder: (name: string) => void
    onsettings: () => void
    onrefresh?: () => void
    oncompose?: () => void
    onselectintegration?: (name: string) => void
  }
  let {
    accounts = [],
    accountId,
    selectedFolder,
    refreshToken = 0,
    activeIntegration = null,
    unified = false,
    onselectaccount,
    onselectfolder,
    onsettings,
    onrefresh,
    oncompose,
    onselectintegration,
  }: Props = $props()

  let folders = $state<Folder[]>([])
  let loading = $state(true)
  let refreshing = $state(false)
  let error = $state('')

  // Total unread across every account's INBOX — used as the badge on
  // the "All Inboxes" entry when unified mode is on. The number is the
  // same one the tray icon shows; an `unread-count-updated` event from
  // Rust nudges us to re-read it whenever a poll changes it.
  let unifiedUnread = $state(0)
  async function refreshUnifiedUnread() {
    try {
      unifiedUnread = await invoke<number>('get_total_unread')
    } catch (e) {
      console.warn('get_total_unread failed:', e)
    }
  }
  $effect(() => {
    void refreshUnifiedUnread()
    let unlisten: (() => void) | null = null
    ;(async () => {
      const { listen } = await import('@tauri-apps/api/event')
      unlisten = await listen('unread-count-updated', () => {
        void refreshUnifiedUnread()
        // Per-folder badges read from the cached `folders` table,
        // which `mark_envelope_read` and `bump_folder_unread` keep in
        // sync with mail activity. Re-read the cache here so the
        // sidebar picks up those changes the moment Rust signals
        // them — without re-running `fetch_folders` (which would do
        // one IMAP `STATUS` round-trip per folder on every mark-as-
        // read, swamping the server).
        void reloadCachedFolders(accountId)
      })
    })()
    return () => {
      unlisten?.()
    }
  })

  /** Lightweight cache-only re-read used by the unread-count event
      listener. The full `load()` also kicks off `fetch_folders`,
      which is expensive — that's reserved for explicit refresh
      paths (mount, account switch, refresh button). */
  async function reloadCachedFolders(id: string) {
    try {
      const cached = await invoke<Folder[]>('get_cached_folders', { accountId: id })
      if (id === accountId) folders = cached
    } catch (e) {
      console.warn('reloadCachedFolders failed:', e)
    }
  }

  /** The picker's `<select>` value: the account id, or the sentinel
      `"__all__"` when unified mode is active. Kept derived so the
      dropdown follows whatever the parent thinks is selected. */
  const pickerValue = $derived(unified ? '__all__' : accountId)

  // Aggregate unread state across all Talk rooms on the user's first
  // Nextcloud account. Drives the badge on the "Nextcloud Talk"
  // integration entry — no count means no badge, mention means the
  // badge gets the louder error tint. Polled on a 30s timer so the
  // user gets a relatively prompt nudge when something new arrives.
  let talkUnreadTotal = $state(0)
  let talkUnreadHasMention = $state(false)
  const TALK_POLL_MS = 30_000
  let talkPollTimer: number | null = null

  // Integrations are still hardcoded — those are planned features, not
  // derived from the mail server.
  const integrations = [
    { name: 'Calendar', icon: '\u{1F4C5}' },      // 📅
    { name: 'Contacts', icon: '\u{1F464}' },      // 👤
    { name: 'Nextcloud Talk', icon: '\u{1F4AC}' }, // 💬
    { name: 'Nextcloud Files', icon: '\u{1F4C1}' },// 📁
  ]

  // Full reload (cache + network `STATUS` per folder) on mount and
  // whenever the active account switches. We deliberately do *not*
  // tie this to `refreshToken`: that token also bumps on mark-as-read
  // and new-mail signals, and a STATUS round-trip per folder there
  // would (a) swamp the IMAP server on every read and (b) race with
  // our cache decrement — STATUS may return the pre-`\Seen` count if
  // the server hasn't finished propagating it, then `upsert_folders`
  // would overwrite our just-decremented cache count and the badge
  // would visibly snap back to the old number.
  $effect(() => {
    void load(accountId)
  })

  // Cache-only reload on every other refresh signal. The cache stays
  // correct via `mark_envelope_read` (decrements on read) and
  // `bump_folder_unread` (increments on poll), so re-reading from the
  // cache picks up those changes without a network round-trip. Manual
  // "refresh" requests (where the user does want fresh server state)
  // come in via the explicit refresh button below, which calls the
  // full `load(accountId)` path directly.
  $effect(() => {
    refreshToken
    void reloadCachedFolders(accountId)
  })

  // Talk-unread polling lives in its own lifecycle: started once on
  // mount and torn down on destroy. We don't tie it to `accountId`
  // because the Talk badge follows the *Nextcloud* account, not the
  // mail account — those can change independently.
  $effect(() => {
    void refreshTalkBadge()
    talkPollTimer = window.setInterval(refreshTalkBadge, TALK_POLL_MS)
    return () => {
      if (talkPollTimer !== null) window.clearInterval(talkPollTimer)
      talkPollTimer = null
    }
  })

  onDestroy(() => {
    if (talkPollTimer !== null) window.clearInterval(talkPollTimer)
  })

  /**
   * Pull the latest Talk room list from the first connected Nextcloud
   * account and aggregate the unread counts. Errors are swallowed —
   * a flaky badge shouldn't block the rest of the sidebar from working.
   */
  async function refreshTalkBadge() {
    try {
      const accounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
      if (accounts.length === 0) {
        talkUnreadTotal = 0
        talkUnreadHasMention = false
        return
      }
      const rooms = await invoke<TalkRoomSummary[]>('list_talk_rooms', {
        ncId: accounts[0].id,
      })
      let total = 0
      let mention = false
      for (const r of rooms) {
        total += r.unread_messages
        if (r.unread_mention && r.unread_messages > 0) mention = true
      }
      talkUnreadTotal = total
      talkUnreadHasMention = mention
    } catch (e) {
      console.warn('Talk unread poll failed:', e)
    }
  }

  async function load(id: string) {
    loading = true
    refreshing = false
    error = ''

    try {
      const cached = await invoke<Folder[]>('get_cached_folders', { accountId: id })
      if (id === accountId) {
        folders = cached
        if (cached.length > 0) loading = false
      }
    } catch (e) {
      console.warn('get_cached_folders failed:', e)
    }

    refreshing = folders.length > 0
    try {
      const fresh = await invoke<Folder[]>('fetch_folders', { accountId: id })
      if (id === accountId) {
        folders = fresh
      }
    } catch (e) {
      if (folders.length === 0) {
        error = formatError(e) || 'Failed to load folders'
      } else {
        console.warn('fetch_folders failed (showing cached):', e)
      }
    } finally {
      loading = false
      refreshing = false
    }
  }

  // Map IMAP folder attributes (\Sent, \Drafts, \Trash, etc.) to an
  // emoji. Falls back to a plain folder icon. Attribute names come from
  // async-imap's Debug formatting, so they look like `Sent`, `Drafts`,
  // etc. — we match case-insensitively.
  //
  // User-defined rules from the account's `folder_icons` are checked
  // *after* the built-in heuristics so a rule like "Bank → 🏦" wins
  // over the generic 📁 fallback but doesn't override INBOX/Sent/etc.
  // (those are part of the app's navigation language).
  //
  // We read the active account's rules directly from the `accounts`
  // prop on every call rather than going through a `$derived` — the
  // derived was reading stale data after the user added a rule and
  // came back to the inbox view, even though `accounts` had refreshed.
  // Reading inline lets Svelte's per-call dependency tracking pick up
  // changes the moment the prop updates.
  /** True when an IMAP folder is the trash (`\Trash` / `\Deleted`)
      or junk/spam (`\Junk`) bin. Used both for icon selection and
      for hiding the unread-count badge — those folders are
      typically full of stuff the user doesn't want surfaced as
      "unread to read", so a count there is just visual noise. */
  function isTrashOrJunk(f: Folder): boolean {
    const name = f.name.toLowerCase()
    const attrs = f.attributes.map((a) => a.toLowerCase())
    const has = (k: string) => attrs.some((a) => a.includes(k))
    return (
      has('trash') ||
      has('deleted') ||
      has('junk') ||
      has('spam') ||
      // Name-based fallback for servers that don't tag with the
      // special-use attributes — many German hosters return
      // "Trash" / "Spam" / "Papierkorb" without flags.
      name === 'trash' ||
      name === 'spam' ||
      name === 'junk' ||
      name === 'papierkorb'
    )
  }

  function folderIcon(f: Folder): string {
    const name = f.name.toLowerCase()
    const attrs = f.attributes.map((a) => a.toLowerCase())

    const has = (k: string) => attrs.some((a) => a.includes(k))
    if (name === 'inbox' || has('inbox')) return '\u{1F4E5}' // 📥
    if (has('sent')) return '\u{1F4E4}' // 📤
    if (has('draft')) return '\u{1F4DD}' // 📝
    if (has('trash') || has('deleted') || name === 'trash' || name === 'papierkorb') return '\u{1F5D1}' // 🗑️
    if (has('junk') || has('spam') || name === 'spam' || name === 'junk') return '\u{1F6AB}' // 🚫
    if (has('flagged') || has('starred')) return '\u{2B50}' // ⭐
    if (has('archive')) return '\u{1F5C3}' // 🗃️

    // User-defined rules. Match against the full folder name so
    // hierarchical names like "INBOX/Bank" also match a "bank"
    // keyword. First match wins — order in the settings list is
    // the user's stated priority.
    const account = accounts.find((a) => a.id === accountId)
    const rules = account?.folder_icons ?? []
    for (const rule of rules) {
      const kw = rule.keyword.trim().toLowerCase()
      if (kw && name.includes(kw)) return rule.icon
    }

    // Diagnostic log — kept only until we're certain the rules
    // pipeline is solid; remove once #63 fully verified in the wild.
    if (rules.length === 0 && import.meta.env.DEV) {
      console.debug(
        '[folderIcon] no custom rules for account',
        accountId,
        '— accounts had:',
        accounts.length,
        'entries; matching account:',
        account ? { id: account.id, hasIcons: 'folder_icons' in account, raw: account.folder_icons } : 'none',
      )
    }

    return '\u{1F4C1}' // 📁
  }

  // Short display name: strip the hierarchy prefix so "INBOX/Work" shows
  // as "Work". INBOX itself keeps its name but title-cased.
  function displayName(f: Folder): string {
    if (f.name.toUpperCase() === 'INBOX') return 'Inbox'
    const delim = f.delimiter ?? '/'
    const parts = f.name.split(delim)
    return parts[parts.length - 1] || f.name
  }
</script>

<aside class="w-56 shrink-0 border-r border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800 flex flex-col">
  <!-- App title -->
  <div class="p-4 border-b border-surface-200 dark:border-surface-700">
    <h1 class="text-lg font-bold text-primary-500">Nimbus Mail</h1>
  </div>

  <!-- Account picker: only rendered when the user has more than one
       account. With a single account the dropdown is pure chrome, so
       we hide it to keep the sidebar clean. -->
  {#if accounts.length > 1}
    <div class="px-3 pt-3">
      <label class="sr-only" for="sidebar-account-picker">Account</label>
      <select
        id="sidebar-account-picker"
        class="select w-full text-sm px-2 py-1.5 rounded-md"
        value={pickerValue}
        onchange={(e) => onselectaccount?.((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="__all__">All inboxes</option>
        {#each accounts as a (a.id)}
          <option value={a.id}>{a.display_name || a.email}</option>
        {/each}
      </select>
    </div>
  {/if}

  <!-- Compose button -->
  <div class="p-3">
    <button class="btn preset-filled-primary-500 w-full" onclick={() => oncompose?.()}>
      Compose
    </button>
  </div>

  <!-- Mail folders -->
  <nav class="flex-1 overflow-y-auto px-2">
    <div class="px-2 py-1 text-xs font-semibold text-surface-500 uppercase tracking-wider flex items-center justify-between">
      <span>Folders</span>
      <div class="flex items-center gap-2">
        {#if refreshing}
          <span class="text-[10px] font-normal normal-case tracking-normal text-surface-500">Refreshing…</span>
        {/if}
        <button
          class="text-surface-500 hover:text-primary-500 disabled:opacity-50 normal-case tracking-normal"
          title="Refresh"
          aria-label="Refresh"
          disabled={refreshing}
          onclick={() => {
            // Manual refresh: do the full network reload locally so
            // the user gets fresh STATUS counts. Notify the parent so
            // it can refresh sibling views (mail list etc.) — but the
            // `refreshToken` it bumps now triggers cache-only reloads
            // here, which is why we need this explicit `load()`.
            void load(accountId)
            onrefresh?.()
          }}
        >
          &#x21bb;
        </button>
      </div>
    </div>

    {#if unified}
      <!-- Unified mode: only INBOX is meaningful (the per-account
           folder tree doesn't compose across accounts), so collapse
           the list to a single highlighted entry. The badge mirrors
           the tray's total-unread count. -->
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm bg-primary-500/10 text-primary-500 font-medium"
        onclick={() => onselectfolder('INBOX')}
      >
        <span>📥</span>
        <span class="flex-1 text-left truncate">All Inboxes</span>
        {#if unifiedUnread > 0}
          <span class="badge preset-filled-primary-500 text-xs">{unifiedUnread}</span>
        {/if}
      </button>
    {:else if loading}
      <p class="px-3 py-2 text-xs text-surface-500">Loading folders…</p>
    {:else if error}
      <p class="px-3 py-2 text-xs text-red-500">{error}</p>
    {:else if folders.length === 0}
      <p class="px-3 py-2 text-xs text-surface-500">No folders.</p>
    {:else}
      {#each folders as folder (folder.name)}
        <button
          class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors
            {selectedFolder === folder.name
              ? 'bg-primary-500/10 text-primary-500 font-medium'
              : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
          onclick={() => onselectfolder(folder.name)}
        >
          <span>{folderIcon(folder)}</span>
          <span class="flex-1 text-left truncate">{displayName(folder)}</span>
          {#if folder.unread_count && folder.unread_count > 0 && !isTrashOrJunk(folder)}
            <span class="badge preset-filled-primary-500 text-xs">{folder.unread_count}</span>
          {/if}
        </button>
      {/each}
    {/if}

    <hr class="my-3 border-surface-200 dark:border-surface-700" />

    <p class="px-2 py-1 text-xs font-semibold text-surface-500 uppercase tracking-wider">Integrations</p>
    {#each integrations as item (item.name)}
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors
          {activeIntegration === item.name
            ? 'bg-primary-500/10 text-primary-500 font-medium'
            : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
        onclick={() => onselectintegration?.(item.name)}
      >
        <span>{item.icon}</span>
        <span class="flex-1 text-left">{item.name}</span>
        {#if item.name === 'Nextcloud Talk' && talkUnreadTotal > 0}
          <span
            class="badge text-xs
                   {talkUnreadHasMention ? 'preset-filled-error-500' : 'preset-filled-primary-500'}"
            title={talkUnreadHasMention ? 'You were mentioned' : 'Unread Talk messages'}
          >{talkUnreadTotal}</span>
        {/if}
      </button>
    {/each}
  </nav>

  <!-- Account / Settings -->
  <div class="p-3 border-t border-surface-200 dark:border-surface-700">
    <button
      class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm hover:bg-surface-200 dark:hover:bg-surface-700 transition-colors text-surface-500"
      onclick={onsettings}
    >
      <span>&#9881;</span>
      <span>Account Settings</span>
    </button>
  </div>
</aside>
