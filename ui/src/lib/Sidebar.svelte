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
      Sidebar now that account switching has moved to the IconRail.
      `folder_icon_overrides` is the per-folder picker state: maps
      the full folder path to the user's chosen emoji and beats every
      other icon source when it exists. */
  interface Account {
    id: string
    display_name: string
    email: string
    folder_icons?: FolderIconRule[]
    folder_icon_overrides?: Record<string, string>
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
    /** Called after the Sidebar has mutated an account record on
     *  the backend (currently only: `set_folder_icon`). The parent
     *  re-fetches its `accounts` state so the updated overrides
     *  map flows back into the Sidebar's `accounts` prop on the
     *  next render. */
    onaccountschanged?: () => void
    /** Fires after a drag-and-drop move (#89) succeeds.  Parent
     *  uses it to drop the source-folder envelope from `MailList`
     *  and trigger the auto-advance flow (#99) — the moved UID
     *  flows in here so it can pick the next neighbour. */
    onmessagemoved?: (removedUid: number) => void
  }
  let {
    accounts = [],
    accountId,
    selectedFolder,
    refreshToken = 0,
    unified = false,
    onselectfolder,
    oncompose,
    onaccountschanged,
    onmessagemoved,
  }: Props = $props()

  let folders = $state<Folder[]>([])
  let loading = $state(true)
  let error = $state('')

  // ── Drag-and-drop drop targets (#89) ───────────────────────────
  // Folder rows accept drags from `MailList` carrying our private
  // `application/x-nimbus-mail` payload.  `dragOverFolder` drives a
  // subtle highlight on the hovered row.  We swallow drops onto the
  // current source folder (`folder === payload.folder`) so a misfire
  // doesn't trip the IMAP server with a move-to-self request.
  let dragOverFolder = $state<string | null>(null)

  function isMailDrag(e: DragEvent): boolean {
    return !!e.dataTransfer?.types.includes('application/x-nimbus-mail')
  }

  function onFolderDragOver(e: DragEvent, target: Folder) {
    if (!isMailDrag(e)) return
    e.preventDefault() // allow drop
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    dragOverFolder = target.name
  }

  function onFolderDragLeave(target: Folder) {
    if (dragOverFolder === target.name) dragOverFolder = null
  }

  async function onFolderDrop(e: DragEvent, target: Folder) {
    dragOverFolder = null
    if (!isMailDrag(e)) return
    e.preventDefault()
    const raw = e.dataTransfer?.getData('application/x-nimbus-mail')
    if (!raw) return
    // Payload is always an array; multi-select drags carry several
    // entries, single-row drags carry one.
    let payload: { accountId: string; folder: string; uid: number }[]
    try {
      const parsed = JSON.parse(raw)
      payload = Array.isArray(parsed) ? parsed : [parsed]
    } catch {
      return
    }
    // Run every move first, *then* fire `onmessagemoved` for the
    // successes — emitting the callback per-iteration triggers the
    // App-level refreshToken bump, which schedules a re-fetch in
    // MailList that races the next iteration's `invoke`.  In the
    // multi-select case the race was eating the last move's
    // server-side write (the cache + IMAP ended up in disagreeing
    // states).  Deferring the callbacks guarantees every move
    // completes against a stable cache snapshot before the UI
    // starts reacting.
    const succeeded: number[] = []
    const failures: unknown[] = []
    for (const item of payload) {
      if (target.name === item.folder) continue // move-to-self
      try {
        await invoke('move_message', {
          accountId: item.accountId,
          folder: item.folder,
          uid: item.uid,
          destFolder: target.name,
        })
        succeeded.push(item.uid)
      } catch (err) {
        console.warn('move_message via drag-and-drop failed', err)
        failures.push(err)
      }
    }
    for (const uid of succeeded) {
      onmessagemoved?.(uid)
    }
    if (failures.length > 0) {
      error =
        failures.length === payload.length
          ? formatError(failures[0]) || 'Failed to move message'
          : `Moved ${succeeded.length} of ${payload.length} messages — ${failures.length} failed.`
    }
  }

  // ── Folder-management state ─────────────────────────────────
  // Each of the three actions (new, rename, delete) owns a single
  // `$state` slot that's either null (idle) or a small object
  // describing the in-flight operation. Only one operation can be
  // active at a time — triggering any of them nulls out the others.
  //
  // Keeping this inline beats a separate component: the operations
  // mutate the same `folders` array the sidebar already owns, the
  // context menu's positioning is trivial, and the confirm dialog
  // is a handful of lines. Extract if / when a third surface needs
  // the same machinery.

  /** Right-click context menu. Null = hidden; otherwise
   *  `{folder, x, y}` anchors the popup at the click position. */
  let contextMenu = $state<{ folder: Folder; x: number; y: number } | null>(null)

  /** Which folder is currently being renamed inline. `null` = no
   *  rename in progress. The row's text swaps to an input while
   *  this matches `folder.name`. */
  let renamingFolder = $state<string | null>(null)
  let renameValue = $state('')

  /** "Create new folder" input. `parent = null` = top-level,
   *  `parent = "INBOX/Projects"` = subfolder under that. The input
   *  renders at the end of the folder list while this is non-null. */
  let newFolderInput = $state<{ parent: string | null; value: string } | null>(null)

  /** "Are you sure?" modal for destructive delete. Null when
   *  hidden. */
  let deleteConfirm = $state<{ folder: Folder } | null>(null)

  /** Busy flag shared across the three mutations — disables the
   *  context-menu actions and the confirm button while an IMAP
   *  command is in flight to keep the user from double-submitting. */
  let folderOpBusy = $state(false)
  let folderOpError = $state('')

  /** Emoji-picker modal state. `null` = hidden; otherwise the
   *  folder whose icon is being changed. The picker's free-text
   *  input lives in its own `$state` so a cancel/close reliably
   *  wipes it regardless of how the modal is dismissed. */
  let iconPicker = $state<{ folder: Folder } | null>(null)
  let iconPickerCustom = $state('')

  /** Curated palette for the picker grid. Ordered by rough
   *  intent-group (mail basics, work, storage, life, signals)
   *  so the visual scan beats raw alphabetical / random order.
   *  Users who want something outside this set can type any
   *  emoji in the free-text input — the Rust command accepts any
   *  non-empty string. */
  const ICON_PRESETS: string[] = [
    // mail-adjacent defaults
    '\u{1F4C1}', '\u{1F4C2}', '\u{1F4E5}', '\u{1F4E4}', '\u{1F4EC}', '\u{1F4EE}',
    '\u{1F4E7}', '\u{1F4E8}', '\u{1F4DD}', '\u{2B50}', '\u{1F3F7}\u{FE0F}', '\u{1F4CC}',
    // storage / security
    '\u{1F5C3}\u{FE0F}', '\u{1F5D1}\u{FE0F}', '\u{1F512}', '\u{1F511}', '\u{1F6AB}', '\u{2764}\u{FE0F}',
    // work / project
    '\u{1F4BC}', '\u{1F4CA}', '\u{1F3AF}', '\u{1F4A1}', '\u{1F4C5}', '\u{1F514}',
    // life / topics
    '\u{1F3E6}', '\u{1F6D2}', '\u{1F3E0}', '\u{2708}\u{FE0F}', '\u{2615}', '\u{1F393}',
    // signals / fun
    '\u{1F4F7}', '\u{1F3B5}', '\u{1F3AC}', '\u{1F3A8}', '\u{26A1}', '\u{1F680}',
  ]

  function openIconPicker(folder: Folder) {
    iconPickerCustom = ''
    iconPicker = { folder }
  }

  function closeIconPicker() {
    iconPicker = null
    iconPickerCustom = ''
  }

  /** Persist the icon choice via `set_folder_icon`. `emoji === null`
   *  clears any existing override, restoring the folder to the
   *  default resolution chain (special-use → keyword rule → 📁).
   *  On success the parent re-fetches accounts so the new override
   *  flows back into the `accounts` prop and the next render
   *  paints it. */
  async function commitFolderIcon(folder: Folder, emoji: string | null) {
    folderOpBusy = true
    try {
      await invoke('set_folder_icon', {
        accountId,
        folderName: folder.name,
        icon: emoji,
      })
      onaccountschanged?.()
      closeIconPicker()
    } catch (e) {
      folderOpError = formatError(e) || 'Failed to set folder icon'
    } finally {
      folderOpBusy = false
    }
  }

  /** Close the context menu. Safe to call when already closed.
   *  Also clears any transient error left over from a prior
   *  operation's feedback so the next right-click starts clean. */
  function closeContextMenu() {
    contextMenu = null
    folderOpError = ''
  }

  /** Close-on-click-outside for the context menu. Attached at the
   *  document level while the menu is open; torn down as soon as
   *  it closes so we're not holding a listener during idle time. */
  $effect(() => {
    if (!contextMenu) return
    const onDocMouseDown = (e: MouseEvent) => {
      // Clicks *inside* the menu get `stopPropagation` on the
      // menu's own `onmousedown`, so anything reaching document
      // is by definition outside.
      closeContextMenu()
      void e
    }
    const onDocKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeContextMenu()
    }
    document.addEventListener('mousedown', onDocMouseDown)
    document.addEventListener('keydown', onDocKey)
    return () => {
      document.removeEventListener('mousedown', onDocMouseDown)
      document.removeEventListener('keydown', onDocKey)
    }
  })

  function openContextMenu(e: MouseEvent, folder: Folder) {
    e.preventDefault()
    // Cancel any other in-flight edits — opening the menu from a
    // fresh row should clear the noise.
    renamingFolder = null
    newFolderInput = null
    contextMenu = { folder, x: e.clientX, y: e.clientY }
  }

  /** Join a parent path with a child segment using the parent's
   *  delimiter (or `/` if the server didn't advertise one).
   *  Handles the `parent == null` case for top-level creations. */
  function joinPath(parent: Folder | null, child: string): string {
    if (!parent) return child
    const delim = parent.delimiter ?? '/'
    return `${parent.name}${delim}${child}`
  }

  async function commitRename() {
    // Re-entrancy guard: Enter→commit + the subsequent blur (when
    // unmounting the input or when the user clicks away during the
    // in-flight invoke) can each fire commit, producing two
    // parallel RENAMEs — the first succeeds, the second hits a
    // stale target and errors. `folderOpBusy` stays `true` for
    // the duration of the first call, which short-circuits the
    // duplicate cleanly.
    if (!renamingFolder || folderOpBusy) return
    const oldName = renamingFolder
    const newLeaf = renameValue.trim()
    if (!newLeaf || newLeaf === displayNameFromPath(oldName)) {
      // Nothing changed (or empty) — just bail, no IMAP round-trip.
      renamingFolder = null
      renameValue = ''
      return
    }
    // Rename preserves the parent path; only the last segment
    // changes. That matches what every mail client does and keeps
    // the server-side move simple.
    const parent = parentPath(oldName)
    const newName = parent ? `${parent}${delimiterFor(oldName)}${newLeaf}` : newLeaf
    folderOpBusy = true
    try {
      await invoke('rename_folder', {
        accountId,
        oldName,
        newName,
      })
      // Follow the selection if the user was standing on the
      // renamed folder — otherwise the mail-list column would
      // silently snap to an empty view.
      if (selectedFolder === oldName) onselectfolder(newName)
      renamingFolder = null
      renameValue = ''
      await load(accountId)
    } catch (e) {
      folderOpError = formatError(e) || 'Failed to rename folder'
    } finally {
      folderOpBusy = false
    }
  }

  function cancelRename() {
    renamingFolder = null
    renameValue = ''
  }

  async function commitNewFolder() {
    // Same re-entrancy guard as `commitRename` — Enter + blur can
    // both fire commit during a single in-flight invoke, yielding
    // a second CREATE that gets the server's ALREADYEXISTS error
    // even though the first CREATE succeeded. `folderOpBusy`
    // short-circuits the second call until the first settles.
    if (!newFolderInput || folderOpBusy) return
    const leaf = newFolderInput.value.trim()
    if (!leaf) {
      newFolderInput = null
      return
    }
    const parentFolder =
      newFolderInput.parent === null
        ? null
        : folders.find((f) => f.name === newFolderInput!.parent) ?? null
    const name = joinPath(parentFolder, leaf)
    folderOpBusy = true
    try {
      await invoke('create_folder', { accountId, name })
      newFolderInput = null
      await load(accountId)
    } catch (e) {
      folderOpError = formatError(e) || 'Failed to create folder'
    } finally {
      folderOpBusy = false
    }
  }

  function cancelNewFolder() {
    newFolderInput = null
  }

  async function confirmDelete() {
    if (!deleteConfirm) return
    const { folder } = deleteConfirm
    folderOpBusy = true
    try {
      await invoke('delete_folder', { accountId, name: folder.name })
      // If the user was viewing the folder they just deleted, bounce
      // them to INBOX — otherwise MailList keeps trying to fetch
      // from a mailbox the server no longer has.
      if (selectedFolder === folder.name) onselectfolder('INBOX')
      deleteConfirm = null
      await load(accountId)
    } catch (e) {
      folderOpError = formatError(e) || 'Failed to delete folder'
    } finally {
      folderOpBusy = false
    }
  }

  function cancelDelete() {
    deleteConfirm = null
    folderOpError = ''
  }

  /** Extract just the last segment of an IMAP folder path, using
   *  the folder's own delimiter when we have it. For the INBOX
   *  case the display name is already a single token. */
  function displayNameFromPath(name: string): string {
    if (name.toUpperCase() === 'INBOX') return 'Inbox'
    const f = folders.find((x) => x.name === name)
    const delim = f?.delimiter ?? '/'
    const parts = name.split(delim)
    return parts[parts.length - 1] || name
  }

  /** The parent path portion of a folder name, or `null` for
   *  top-level. `"INBOX/Projects/2026"` → `"INBOX/Projects"`,
   *  `"INBOX"` → `null`. */
  function parentPath(name: string): string | null {
    const f = folders.find((x) => x.name === name)
    const delim = f?.delimiter ?? '/'
    const idx = name.lastIndexOf(delim)
    return idx < 0 ? null : name.slice(0, idx)
  }

  /** Best-guess delimiter for a folder's subtree. Falls back to
   *  `/` when the server didn't advertise one on the LIST response
   *  (rare but possible for a freshly-created top-level folder). */
  function delimiterFor(name: string): string {
    const f = folders.find((x) => x.name === name)
    return f?.delimiter ?? '/'
  }

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

  /** Pick an icon for a folder. Resolution chain, highest priority
      first:
        1. Per-folder override set via the emoji picker — absolute
           winner so the user's explicit "I picked 📮 for my Inbox"
           beats the special-use default.
        2. IMAP special-use attributes (and a few name fallbacks)
           so INBOX/Sent/Drafts/etc. get canonical icons without
           the user having to pick anything.
        3. Keyword rules from Account.folder_icons (the older
           "folder name contains X → use Y" mechanism).
        4. Generic 📁 fallback.
     */
  function folderIcon(f: Folder): string {
    const account = accounts.find((a) => a.id === accountId)
    const override = account?.folder_icon_overrides?.[f.name]
    if (override) return override

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

    const rules = account?.folder_icons ?? []
    for (const rule of rules) {
      const kw = rule.keyword.trim().toLowerCase()
      if (kw && name.includes(kw)) return rule.icon
    }

    return '\u{1F4C1}' // 📁
  }

  /** True if an override is currently in effect for this folder —
   *  drives whether the picker's "Reset to default" button is
   *  enabled. */
  function hasIconOverride(f: Folder): boolean {
    const account = accounts.find((a) => a.id === accountId)
    return !!account?.folder_icon_overrides?.[f.name]
  }

  // Short display name: strip the hierarchy prefix so "INBOX/Work" shows
  // as "Work". INBOX itself keeps its name but title-cased.
  function displayName(f: Folder): string {
    if (f.name.toUpperCase() === 'INBOX') return 'Inbox'
    const delim = f.delimiter ?? '/'
    const parts = f.name.split(delim)
    return parts[parts.length - 1] || f.name
  }

  /** Rank each folder into the "standard" tier (Inbox / Drafts / Sent /
      Flagged / Archive / Junk / Trash) or the "user" tier. Standard
      folders get a numeric rank that drives the top-of-list order;
      user folders get -1 and are sorted alphabetically instead. The
      ordering mirrors what every major mail client shows — Inbox is
      where mail arrives, then the user's own outgoing queues, then
      the storage-ish folders at the bottom. */
  function standardRank(f: Folder): number {
    const name = f.name.toLowerCase()
    const attrs = f.attributes.map((a) => a.toLowerCase())
    const has = (k: string) => attrs.some((a) => a.includes(k))

    if (name === 'inbox' || has('inbox')) return 0
    if (has('draft')) return 1
    if (has('sent')) return 2
    if (has('flagged') || has('starred')) return 3
    if (has('archive')) return 4
    if (
      has('junk') ||
      has('spam') ||
      name === 'spam' ||
      name === 'junk'
    )
      return 5
    if (
      has('trash') ||
      has('deleted') ||
      name === 'trash' ||
      name === 'papierkorb'
    )
      return 6
    return -1
  }

  // Split the flat server-returned list into the two tiers so the
  // template renders them in distinct `{#each}` blocks with a
  // divider in between. `$derived` so the sort work only re-runs when
  // `folders` actually changes.
  const standardFolders = $derived(
    folders
      .filter((f) => standardRank(f) !== -1)
      .sort((a, b) => standardRank(a) - standardRank(b)),
  )
  const customFolders = $derived(
    folders
      .filter((f) => standardRank(f) === -1)
      // `localeCompare` so non-ASCII folder names (Entwürfe, Übersicht…)
      // sort the way the user's locale expects instead of by code point.
      .sort((a, b) =>
        displayName(a).localeCompare(displayName(b), undefined, {
          sensitivity: 'base',
          numeric: true,
        }),
      ),
  )
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
       nav / settings slot have all moved out of this component.
       Folder-management (new / rename / delete) is surfaced via a
       subtle header "+" for top-level creates and a right-click
       context menu on each row for subfolder / rename / delete. -->

  {#snippet folderRow(folder: Folder)}
    {#if renamingFolder === folder.name}
      <!-- Inline rename. `bind:this` + `autofocus` on the input
           is set from the `$effect` on `renamingFolder` below so
           the caret lands in the field the moment the menu's
           "Rename" click settles. Escape bails, Enter commits,
           blur also commits (matches most file managers). -->
      <div class="flex items-center gap-2 px-3 py-1.5">
        <span>{folderIcon(folder)}</span>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          type="text"
          class="input flex-1 text-sm px-2 py-1 rounded-md"
          bind:value={renameValue}
          disabled={folderOpBusy}
          autofocus
          onkeydown={(e) => {
            if (e.key === 'Enter') { e.preventDefault(); void commitRename() }
            else if (e.key === 'Escape') { e.preventDefault(); cancelRename() }
          }}
          onblur={() => { if (renamingFolder) void commitRename() }}
        />
      </div>
    {:else}
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors
          {selectedFolder === folder.name
            ? 'bg-primary-500/10 text-primary-500 font-medium'
            : dragOverFolder === folder.name
              ? 'bg-primary-500/20 ring-2 ring-primary-500'
              : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
        onclick={() => onselectfolder(folder.name)}
        oncontextmenu={(e) => openContextMenu(e, folder)}
        ondragover={(e) => onFolderDragOver(e, folder)}
        ondragleave={() => onFolderDragLeave(folder)}
        ondrop={(e) => void onFolderDrop(e, folder)}
      >
        <span>{folderIcon(folder)}</span>
        <span class="flex-1 text-left truncate">{displayName(folder)}</span>
        {#if folder.unread_count && folder.unread_count > 0 && !isTrashOrJunk(folder)}
          <span class="badge preset-filled-primary-500 text-xs">{folder.unread_count}</span>
        {/if}
      </button>
    {/if}
  {/snippet}

  <!-- Subtle header. "Folders" label + a `+` for adding a new
       top-level folder. Hidden in unified mode because a top-level
       folder would land on one account but the user's looking at
       all of them at once. -->
  {#if !unified}
    <div class="flex items-center justify-between px-3 pt-2 pb-1">
      <span class="text-[10px] font-semibold text-surface-500 uppercase tracking-wider">
        Folders
      </span>
      <button
        class="w-5 h-5 rounded-md flex items-center justify-center text-surface-500 hover:bg-surface-200 dark:hover:bg-surface-700 disabled:opacity-50"
        title="New folder"
        aria-label="New folder"
        disabled={folderOpBusy}
        onclick={() => {
          renamingFolder = null
          contextMenu = null
          newFolderInput = { parent: null, value: '' }
        }}
      >+</button>
    </div>
  {/if}

  <nav class="flex-1 overflow-y-auto px-2">
    {#if unified}
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm bg-primary-500/10 text-primary-500 font-medium"
        onclick={() => onselectfolder('INBOX')}
      >
        <!-- Fixed-size icon box so the emoji centers consistently
             regardless of the system font's emoji metrics. The
             absolute-positioned 🌐 corner badge signals "across all
             accounts" at a glance — same visual idiom as a notification
             dot on an app icon. -->
        <span
          class="relative inline-flex items-center justify-center w-5 h-5 leading-none shrink-0"
          aria-hidden="true"
        >
          <span class="text-base leading-none">📥</span>
          <span
            class="absolute -bottom-1 -right-1 text-[0.65rem] leading-none drop-shadow"
          >🌐</span>
        </span>
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
      {#each standardFolders as folder (folder.name)}
        {@render folderRow(folder)}
      {/each}

      {#if standardFolders.length > 0 && customFolders.length > 0}
        <hr class="my-2 mx-2 border-surface-200 dark:border-surface-700" />
      {/if}

      {#each customFolders as folder (folder.name)}
        {@render folderRow(folder)}
      {/each}

      <!-- New-folder inline input. Appears at the bottom of the
           folder list regardless of whether it's a top-level or
           subfolder create — the `parent` label makes the context
           clear without routing the input into the middle of the
           tree (which would be nice but gets fiddly with the
           two-tier standard/custom split). -->
      {#if newFolderInput}
        <div class="flex items-center gap-2 px-3 py-1.5 mt-1">
          <span>{newFolderInput.parent ? '\u{1F4C2}' : '\u{1F4C1}'}</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            type="text"
            class="input flex-1 text-sm px-2 py-1 rounded-md"
            placeholder={newFolderInput.parent
              ? `New subfolder in ${displayNameFromPath(newFolderInput.parent)}`
              : 'New folder'}
            bind:value={newFolderInput.value}
            disabled={folderOpBusy}
            autofocus
            onkeydown={(e) => {
              if (e.key === 'Enter') { e.preventDefault(); void commitNewFolder() }
              else if (e.key === 'Escape') { e.preventDefault(); cancelNewFolder() }
            }}
            onblur={() => {
              // Commit on blur only if there's actually text —
              // tabbing away from an empty input should just close.
              if (newFolderInput && newFolderInput.value.trim()) {
                void commitNewFolder()
              } else {
                cancelNewFolder()
              }
            }}
          />
        </div>
      {/if}

      <!-- Non-blocking feedback for the last folder-management
           operation's error. Clears when the next menu opens or the
           user starts a new operation. -->
      {#if folderOpError}
        <p class="px-3 py-1.5 mt-1 text-xs text-red-500 wrap-break-word">{folderOpError}</p>
      {/if}
    {/if}
  </nav>
</aside>

<!-- Right-click context menu. `position: fixed` anchored at the
     click point; z-60 to clear the IconRail (z-ordering of the
     sidebar's `aside`). Rename / Delete are disabled for
     special-use folders — most servers refuse to rename or delete
     the canonical Inbox / Sent / Drafts / etc., and even when they
     don't the account's special-use attributes then point at a
     folder that no longer exists, which breaks `pick_*_folder`
     resolution in save_draft / archive / trash flows. -->
{#if contextMenu}
  {@const stdFolder = standardRank(contextMenu.folder) !== -1}
  <div
    class="fixed z-60 min-w-44 rounded-md border border-surface-200 dark:border-surface-700 bg-surface-50 dark:bg-surface-900 shadow-lg py-1 text-sm"
    style="left: {Math.min(contextMenu.x, window.innerWidth - 200)}px; top: {Math.min(contextMenu.y, window.innerHeight - 150)}px;"
    role="menu"
    tabindex="-1"
    onmousedown={(e) => e.stopPropagation()}
  >
    <button
      class="w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800 disabled:opacity-50 disabled:hover:bg-transparent"
      disabled={folderOpBusy}
      onclick={() => {
        const parent = contextMenu!.folder.name
        contextMenu = null
        newFolderInput = { parent, value: '' }
      }}
    >New subfolder</button>
    <button
      class="w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800 disabled:opacity-50 disabled:hover:bg-transparent"
      disabled={folderOpBusy}
      onclick={() => {
        const f = contextMenu!.folder
        contextMenu = null
        openIconPicker(f)
      }}
    >Change icon…</button>
    <button
      class="w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800 disabled:opacity-50 disabled:hover:bg-transparent"
      disabled={folderOpBusy || stdFolder}
      title={stdFolder ? "Standard folders can't be renamed" : ''}
      onclick={() => {
        const f = contextMenu!.folder
        contextMenu = null
        renamingFolder = f.name
        renameValue = displayName(f)
      }}
    >Rename</button>
    <button
      class="w-full text-left px-3 py-1.5 hover:bg-red-500/10 text-red-600 dark:text-red-400 disabled:opacity-50 disabled:hover:bg-transparent"
      disabled={folderOpBusy || stdFolder}
      title={stdFolder ? "Standard folders can't be deleted" : ''}
      onclick={() => {
        const f = contextMenu!.folder
        contextMenu = null
        deleteConfirm = { folder: f }
      }}
    >Delete</button>
  </div>
{/if}

<!-- Delete confirmation modal. Destructive ops always pass through
     an explicit confirm — IMAP DELETE usually refuses non-empty
     folders but a freshly-created / emptied one disappears without
     a peep, and rebuilding it isn't possible if it carried custom
     subfolders. -->
{#if deleteConfirm}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onmousedown={(e) => { if (e.target === e.currentTarget) cancelDelete() }}
  >
    <div class="bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl w-96 max-w-full p-5">
      <h3 class="text-base font-semibold mb-2">Delete folder?</h3>
      <p class="text-sm text-surface-700 dark:text-surface-300 mb-4">
        Delete <span class="font-medium">{displayName(deleteConfirm.folder)}</span>?
        This can't be undone.
      </p>
      {#if folderOpError}
        <p class="text-xs text-red-500 mb-3 wrap-break-word">{folderOpError}</p>
      {/if}
      <div class="flex justify-end gap-2">
        <button
          class="btn preset-outlined-surface-500"
          disabled={folderOpBusy}
          onclick={cancelDelete}
        >Cancel</button>
        <button
          class="btn preset-filled-error-500"
          disabled={folderOpBusy}
          onclick={() => void confirmDelete()}
        >{folderOpBusy ? 'Deleting…' : 'Delete'}</button>
      </div>
    </div>
  </div>
{/if}

<!-- Emoji picker for "Change icon". Grid of curated folder
     emojis at the top, a free-text input below for anything not
     in the grid, and a "Reset to default" that clears the override
     on the backend. Clicking any grid emoji commits immediately —
     snappier than a click-then-confirm flow and matches the way
     GitHub / Slack / Linear reaction pickers work. -->
{#if iconPicker}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onmousedown={(e) => { if (e.target === e.currentTarget) closeIconPicker() }}
  >
    <div class="bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl w-104 max-w-full p-5">
      <h3 class="text-base font-semibold mb-1">Choose an icon</h3>
      <p class="text-xs text-surface-500 mb-4">
        For <span class="font-medium text-surface-700 dark:text-surface-300">{displayName(iconPicker.folder)}</span>
      </p>

      <!-- Preset grid: 6 cols × 6 rows. Each cell is a square
           button so the emoji centers cleanly regardless of font
           metrics. -->
      <div class="grid grid-cols-6 gap-1 mb-4">
        {#each ICON_PRESETS as emoji (emoji)}
          <button
            class="w-12 h-12 rounded-md text-xl flex items-center justify-center
                   hover:bg-surface-200 dark:hover:bg-surface-800 disabled:opacity-50
                   transition-colors"
            disabled={folderOpBusy}
            title={emoji}
            onclick={() => void commitFolderIcon(iconPicker!.folder, emoji)}
          >{emoji}</button>
        {/each}
      </div>

      <!-- Free-text escape hatch. Any single emoji the user
           pastes / types here goes through the same command. -->
      <div class="flex items-center gap-2 mb-4">
        <label class="text-xs text-surface-500 shrink-0" for="icon-picker-custom">Or any emoji:</label>
        <input
          id="icon-picker-custom"
          type="text"
          class="input flex-1 text-sm px-2 py-1 rounded-md"
          placeholder="🎨"
          bind:value={iconPickerCustom}
          disabled={folderOpBusy}
          onkeydown={(e) => {
            if (e.key === 'Enter' && iconPickerCustom.trim()) {
              e.preventDefault()
              void commitFolderIcon(iconPicker!.folder, iconPickerCustom.trim())
            }
          }}
        />
        <button
          class="btn btn-sm preset-filled-primary-500"
          disabled={folderOpBusy || !iconPickerCustom.trim()}
          onclick={() => void commitFolderIcon(iconPicker!.folder, iconPickerCustom.trim())}
        >Set</button>
      </div>

      {#if folderOpError}
        <p class="text-xs text-red-500 mb-3 wrap-break-word">{folderOpError}</p>
      {/if}

      <div class="flex justify-between">
        <button
          class="btn preset-outlined-surface-500 text-xs"
          disabled={folderOpBusy || !hasIconOverride(iconPicker.folder)}
          title={hasIconOverride(iconPicker.folder)
            ? 'Remove the custom icon — fall back to the default.'
            : 'No custom icon set.'}
          onclick={() => void commitFolderIcon(iconPicker!.folder, null)}
        >Reset to default</button>
        <button
          class="btn preset-outlined-surface-500"
          disabled={folderOpBusy}
          onclick={closeIconPicker}
        >Cancel</button>
      </div>
    </div>
  </div>
{/if}
