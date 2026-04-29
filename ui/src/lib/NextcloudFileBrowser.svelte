<script lang="ts">
  /**
   * NextcloudFileBrowser — the inner "browse Nextcloud" UI shared by
   * the modal picker (`NextcloudFilePicker`) and the sidebar-routed
   * full-pane view (`FilesView`).
   *
   * Owns the *browse* concerns and nothing else:
   *   - account loading + selection (only shown when N > 1)
   *   - folder navigation + breadcrumbs
   *   - listing + sort (folders first, natural sort within each group)
   *   - multi-select (unless `pickFolderMode` hides the checkboxes)
   *   - inline "+ New folder" with validation
   *
   * Action buttons (Attach / Share / Send via mail / …) are rendered
   * by the parent — the parent is the only one that knows what should
   * happen when the user commits a selection. We expose enough state
   * via `bind:`-able props for that footer to be smart.
   *
   * # Bindable state
   *
   *   bind:accountId    — currently active account id ('' = none)
   *   bind:currentPath  — folder we're currently looking at ('/' = root)
   *   bind:selected     — Set of paths the user has ticked
   *   bind:entries      — current folder's children (for content_type lookup)
   *   bind:accounts     — list of NC accounts (so the parent can detect
   *                       "no accounts connected" without re-fetching)
   *   bind:error        — last error string ('' = none) — parent renders
   *                       it wherever it wants
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import FileTypeIcon from './FileTypeIcon.svelte'

  interface NextcloudCapabilities {
    version?: string | null
    talk: boolean
    files: boolean
    caldav: boolean
    carddav: boolean
  }
  export interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
    capabilities?: NextcloudCapabilities | null
  }
  export interface FileEntry {
    name: string
    path: string
    is_dir: boolean
    size: number | null
    content_type: string | null
    modified: string | null
  }

  interface Props {
    /** Hide checkboxes — used by the picker's "save here" mode where
        the user is choosing a destination folder, not files. */
    pickFolderMode?: boolean
    accountId?: string
    currentPath?: string
    selected?: Set<string>
    entries?: FileEntry[]
    accounts?: NextcloudAccount[]
    error?: string
  }
  let {
    pickFolderMode = false,
    accountId = $bindable(''),
    currentPath = $bindable('/'),
    selected = $bindable(new Set<string>()),
    entries = $bindable<FileEntry[]>([]),
    accounts = $bindable<NextcloudAccount[]>([]),
    error = $bindable(''),
  }: Props = $props()

  let loading = $state(false)
  // "+ New folder" inline input — hidden until the user clicks the
  // button. Driven by component state so we can validate and show
  // errors inline rather than via a blocking `prompt()`.
  let creatingFolder = $state(false)
  let newFolderName = $state('')
  let creatingFolderInFlight = $state(false)
  let newFolderInput = $state<HTMLInputElement | null>(null)
  $effect(() => {
    if (creatingFolder && newFolderInput) {
      newFolderInput.focus()
    }
  })

  $effect(() => {
    loadAccounts()
  })

  async function loadAccounts() {
    try {
      const list = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      accounts = list
      if (list.length === 1 && !accountId) {
        accountId = list[0].id
        await loadFolder('/')
      }
    } catch (e) {
      error = formatError(e) || 'Failed to load Nextcloud accounts'
    }
  }

  async function selectAccount(id: string) {
    accountId = id
    selected = new Set()
    await loadFolder('/')
  }

  async function loadFolder(path: string) {
    if (!accountId) return
    loading = true
    error = ''
    try {
      const list = await invoke<FileEntry[]>('list_nextcloud_files', {
        ncId: accountId,
        path,
      })
      // Folders first, then files. Within each group, natural sort so
      // numbered names (1, 2, 9, 10, 11) come out the way humans expect
      // — `numeric: true` reads runs of digits as whole numbers, and
      // `sensitivity: 'base'` makes it case-insensitive. Matches the
      // Nextcloud web UI ordering.
      list.sort((a, b) => {
        if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1
        return a.name.localeCompare(b.name, undefined, {
          numeric: true,
          sensitivity: 'base',
        })
      })
      entries = list
      currentPath = path
    } catch (e) {
      error = formatError(e) || 'Failed to list folder'
    } finally {
      loading = false
    }
  }

  // Breadcrumb segments — each one is a clickable jump back up the
  // tree.
  let breadcrumbs = $derived.by(() => {
    const segs = currentPath.split('/').filter(Boolean)
    const out = [{ label: 'Home', path: '/' }]
    let acc = ''
    for (const s of segs) {
      acc += '/' + s
      out.push({ label: s, path: acc })
    }
    return out
  })

  function onEntryClick(entry: FileEntry) {
    if (entry.is_dir) {
      loadFolder(entry.path)
    } else if (!pickFolderMode) {
      // In folder-pick mode files aren't selectable — they're shown
      // only as a preview of what's already in the destination folder.
      toggleSelected(entry.path)
    }
  }

  function toggleSelected(path: string) {
    // Svelte 5 Sets need reassignment to trigger reactivity.
    const next = new Set(selected)
    if (next.has(path)) next.delete(path)
    else next.add(path)
    selected = next
  }

  function formatSize(bytes: number | null): string {
    if (bytes == null) return ''
    if (bytes < 1024) return `${bytes} B`
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`
  }

  /** Emoji glyph for entries that read better as pictographs
   *  (folders + media types).  Documents fall through to `null`
   *  so the row renders the typed `FileTypeIcon` SVG instead. */
  function iconEmojiFor(entry: FileEntry): string | null {
    if (entry.is_dir) return '📁'
    const ct = entry.content_type ?? ''
    if (ct.startsWith('image/')) return '🖼️'
    if (ct.startsWith('video/')) return '🎞️'
    if (ct.startsWith('audio/')) return '🎵'
    if (ct.startsWith('text/')) return '📝'
    return null
  }

  function navigateTo(path: string) {
    loadFolder(path)
  }

  function startCreateFolder() {
    newFolderName = ''
    error = ''
    creatingFolder = true
  }

  function cancelCreateFolder() {
    creatingFolder = false
    newFolderName = ''
  }

  /**
   * Create the folder named in `newFolderName` inside `currentPath`,
   * refresh the listing, and pre-select the new folder so the parent's
   * "Share as link" / "Send via mail" footer button immediately
   * targets it.
   */
  async function confirmCreateFolder() {
    const trimmed = newFolderName.trim()
    if (!trimmed) {
      error = 'Folder name is required.'
      return
    }
    // Names with path separators would silently change which folder
    // the create lands in — block here rather than let the server
    // return a confusing 409.
    if (trimmed.includes('/') || trimmed.includes('\\')) {
      error = "Folder name can't contain '/' or '\\'."
      return
    }
    creatingFolderInFlight = true
    error = ''
    try {
      const base = currentPath.endsWith('/') ? currentPath : `${currentPath}/`
      const fullPath = `${base}${trimmed}`
      await invoke('create_nextcloud_directory', {
        ncId: accountId,
        path: fullPath,
      })
      creatingFolder = false
      newFolderName = ''
      await loadFolder(currentPath)
      const folderPath = `${fullPath}/`
      const next = new Set(selected)
      next.add(folderPath)
      selected = next
    } catch (e) {
      error = formatError(e) || 'Failed to create folder'
    } finally {
      creatingFolderInFlight = false
    }
  }
</script>

{#if accounts.length === 0}
  <div class="p-6 text-sm text-surface-500">
    No Nextcloud account connected. Add one under
    <strong>Settings → Nextcloud</strong> first.
  </div>
{:else}
  {#if accounts.length > 1}
    <div class="px-5 py-2 border-b border-surface-200 dark:border-surface-700 flex items-center gap-2">
      <label for="nc-files-account" class="text-xs text-surface-500">Account</label>
      <select
        id="nc-files-account"
        class="select text-sm"
        value={accountId}
        onchange={(e) => selectAccount((e.target as HTMLSelectElement).value)}
      >
        <option value="" disabled>Choose an account</option>
        {#each accounts as acc (acc.id)}
          <option value={acc.id}>{acc.display_name ?? acc.username} ({acc.server_url})</option>
        {/each}
      </select>
    </div>
  {/if}

  {#if accountId}
    <nav class="px-5 py-2 border-b border-surface-200 dark:border-surface-700 flex items-center gap-1 text-xs overflow-x-auto">
      {#each breadcrumbs as crumb, i (crumb.path)}
        {#if i > 0}<span class="text-surface-400">/</span>{/if}
        <button
          class="hover:underline {i === breadcrumbs.length - 1 ? 'font-semibold' : 'text-primary-500'}"
          onclick={() => navigateTo(crumb.path)}
          disabled={i === breadcrumbs.length - 1}
        >{crumb.label}</button>
      {/each}
      <div class="flex-1"></div>
      {#if !creatingFolder}
        <button
          class="text-primary-500 hover:underline"
          onclick={startCreateFolder}
          title="Create a new folder in this directory"
        >+ New folder</button>
      {/if}
    </nav>

    {#if creatingFolder}
      <!--
        Inline name input rendered as its own row so a long folder name
        doesn't squeeze the breadcrumbs off-screen on narrow widths.
      -->
      <div class="px-5 py-2 border-b border-surface-200 dark:border-surface-700 flex items-center gap-2">
        <label for="nc-new-folder" class="text-xs text-surface-500">New folder name</label>
        <input
          id="nc-new-folder"
          class="input flex-1 px-3 py-1.5 text-sm rounded-md"
          bind:this={newFolderInput}
          bind:value={newFolderName}
          placeholder="My folder"
          onkeydown={(e) => {
            if (e.key === 'Enter') confirmCreateFolder()
            else if (e.key === 'Escape') cancelCreateFolder()
          }}
        />
        <button
          class="btn preset-filled-primary-500 text-xs"
          disabled={creatingFolderInFlight || !newFolderName.trim()}
          onclick={confirmCreateFolder}
        >{creatingFolderInFlight ? 'Creating…' : 'Create'}</button>
        <button
          class="btn preset-outlined-surface-500 text-xs"
          disabled={creatingFolderInFlight}
          onclick={cancelCreateFolder}
        >Cancel</button>
      </div>
    {/if}

    <div class="flex-1 overflow-y-auto">
      {#if loading}
        <div class="p-6 text-sm text-surface-500">Loading…</div>
      {:else if entries.length === 0}
        <div class="p-6 text-sm text-surface-500">This folder is empty.</div>
      {:else}
        <ul class="divide-y divide-surface-200 dark:divide-surface-800">
          {#each entries as entry (entry.path)}
            <li>
              <button
                class="w-full flex items-center gap-3 px-5 py-2 text-left hover:bg-surface-100 dark:hover:bg-surface-800"
                onclick={() => onEntryClick(entry)}
              >
                <!--
                  Folders also get a checkbox so the user can select a
                  folder to share as a public link. Click stops
                  propagation so the row's onclick (navigate-into) doesn't
                  also fire.
                -->
                {#if !pickFolderMode}
                  <input
                    type="checkbox"
                    class="checkbox"
                    checked={selected.has(entry.path)}
                    onclick={(e) => e.stopPropagation()}
                    onchange={() => toggleSelected(entry.path)}
                  />
                {/if}
                {@const emoji = iconEmojiFor(entry)}
                {#if emoji}
                  <span class="text-lg">{emoji}</span>
                {:else}
                  <FileTypeIcon contentType={entry.content_type} filename={entry.name} class="w-5 h-5" />
                {/if}
                <span class="flex-1 truncate text-sm">{entry.name}</span>
                <span class="text-xs text-surface-500">{formatSize(entry.size)}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
{/if}
