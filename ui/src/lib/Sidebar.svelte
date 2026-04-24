<script lang="ts">
  /**
   * Sidebar — mail-view only: Compose CTA + folder list for the
   * currently active account.
   *
   * The shell-level nav (account avatars, integrations, settings)
   * lives in `IconRail.svelte` now; this component is mounted by
   * App.svelte exclusively when the user is in the mail view
   * (`currentView === 'inbox'`). That lets the folder list extend
   * floor-to-ceiling in its column and keeps the sidebar focused on
   * a single job — telling the user where their mail lives.
   *
   * Manual refresh is gone from here too: the background sync loop
   * runs every `background_sync_interval_secs` and a poll fires
   * automatically whenever the user enters the mail view (see the
   * `currentView` effect in App.svelte). The explicit "Check Mail
   * Now" button now lives inside Settings, mirroring the
   * Contacts / Calendar sync-now buttons.
   */

  import { invoke } from '@tauri-apps/api/core'
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

  /** Slim account row — only the folder-icon rules matter to the
      Sidebar now that account switching has moved to the IconRail. */
  interface Account {
    id: string
    display_name: string
    email: string
    folder_icons?: FolderIconRule[]
  }

  interface Props {
    accounts?: Account[]
    accountId: string
    selectedFolder: string
    /** Bumped by the parent to force a cache-only re-read (manual
     *  refresh, mark-as-read, new-mail signal). */
    refreshToken?: number
    /** Unified-inbox mode. When true the per-account folder tree
     *  collapses to a single "All Inboxes" entry; toggled on / off
     *  via the IconRail's "ALL" bubble. */
    unified?: boolean
    onselectfolder: (name: string) => void
    oncompose?: () => void
  }
  let {
    accounts = [],
    accountId,
    selectedFolder,
    refreshToken = 0,
    unified = false,
    onselectfolder,
    oncompose,
  }: Props = $props()

  let folders = $state<Folder[]>([])
  let loading = $state(true)
  let error = $state('')

  // Total unread across every account's INBOX — used as the badge on
  // the "All Inboxes" entry when unified mode is on. Pulled + kept
  // fresh via the same `unread-count-updated` event the tray listens
  // to, so a poll that changes the total nudges us to re-read it.
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
        // sidebar picks up those changes without a fetch_folders
        // round-trip per poll.
        void reloadCachedFolders(accountId)
      })
    })()
    return () => {
      unlisten?.()
    }
  })

  /** Cache-only re-read used by the unread-count event listener.
      Full `load()` also fires `fetch_folders`, which is expensive —
      reserved for mount + account switch. */
  async function reloadCachedFolders(id: string) {
    try {
      const cached = await invoke<Folder[]>('get_cached_folders', { accountId: id })
      if (id === accountId) folders = cached
    } catch (e) {
      console.warn('reloadCachedFolders failed:', e)
    }
  }

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
  // cache picks up those changes without a network round-trip.
  $effect(() => {
    refreshToken
    void reloadCachedFolders(accountId)
  })

  async function load(id: string) {
    loading = true
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
    }
  }

  /** True when an IMAP folder is the trash or junk bin. Used both
      for icon selection and for hiding the unread-count badge —
      surfacing "unread" counts there is noise. Recognises the IMAP
      special-use attributes and common name fallbacks (many German
      hosters return `Trash` / `Spam` / `Papierkorb` without flags). */
  function isTrashOrJunk(f: Folder): boolean {
    const name = f.name.toLowerCase()
    const attrs = f.attributes.map((a) => a.toLowerCase())
    const has = (k: string) => attrs.some((a) => a.includes(k))
    return (
      has('trash') ||
      has('deleted') ||
      has('junk') ||
      has('spam') ||
      name === 'trash' ||
      name === 'spam' ||
      name === 'junk' ||
      name === 'papierkorb'
    )
  }

  /** Pick an icon for a folder. Special-use attributes (and a few
      name fallbacks) win first so INBOX/Sent/Drafts/etc. always show
      their canonical icons; user-defined `folder_icons` rules then
      apply to anything left over before the generic 📁 fallback. */
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

    const rules = accounts.find((a) => a.id === accountId)?.folder_icons ?? []
    for (const rule of rules) {
      const kw = rule.keyword.trim().toLowerCase()
      if (kw && name.includes(kw)) return rule.icon
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
  <!-- Compose CTA. Emoji makes the primary action visually anchored —
       matches Nick's ask for "nice emoji" on the button. -->
  <div class="p-3">
    <button class="btn preset-filled-primary-500 w-full" onclick={() => oncompose?.()}>
      <span class="mr-1">&#x270F;&#xFE0F;</span>Compose
    </button>
  </div>

  <!-- Folder tree. Takes every vertical pixel below the Compose
       button now that the refresh / unified toggle / integration
       nav / settings slot have all moved out of this component. -->
  <nav class="flex-1 overflow-y-auto px-2">
    {#if unified}
      <!-- Unified mode: only INBOX is meaningful across accounts, so
           the per-account tree collapses to this single entry. The
           badge mirrors the tray's total-unread count. -->
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
  </nav>
</aside>
