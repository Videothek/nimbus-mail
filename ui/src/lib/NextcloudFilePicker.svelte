<script lang="ts">
  /**
   * NextcloudFilePicker — modal that lets the user browse their Nextcloud
   * and pick files to attach to an outgoing email.
   *
   * Flow:
   * 1. On mount, load the list of Nextcloud accounts. If there's exactly
   *    one, select it automatically. If none, show an "add one in
   *    settings" message and close-only.
   * 2. On account selection, load the root folder via `list_nextcloud_files`.
   * 3. Clicking a folder navigates into it (loads children). Clicking a
   *    file toggles its selection (multi-select via checkbox).
   * 4. "Attach" downloads each selected file via `download_nextcloud_file`,
   *    shapes it into the `{filename, content_type, data}` triple Compose
   *    expects, and calls `onpicked(attachments)`. Downloads run in
   *    parallel — one slow file doesn't block the rest.
   *
   * We keep the full path (`/Documents/Work/report.pdf`) as the canonical
   * identifier; the UI shows the last segment as a label.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  interface NextcloudCapabilities {
    version?: string | null
    talk: boolean
    files: boolean
    caldav: boolean
    carddav: boolean
  }
  interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
    capabilities?: NextcloudCapabilities | null
  }
  interface FileEntry {
    name: string
    path: string
    is_dir: boolean
    size: number | null
    content_type: string | null
    modified: string | null
  }
  interface Attachment {
    filename: string
    content_type: string
    data: number[]
  }
  /** A "share as link" result — name + the public Nextcloud URL. */
  interface ShareLink {
    filename: string
    url: string
  }

  interface Props {
    /** Called when the user attaches the selected files as bytes. */
    onpicked: (attachments: Attachment[]) => void
    /**
     * Called when the user shares the selected files as public links.
     * Optional — callers that don't want the share action just leave
     * it undefined and the button won't render.
     */
    onlinks?: (links: ShareLink[]) => void
    /**
     * If set, the picker switches to **folder-pick mode**. The user
     * navigates the tree as usual but instead of selecting files, they
     * pick the *current folder* as a destination — this is what the
     * "Save to Nextcloud" flow needs (pick a folder, the caller then
     * uploads bytes into it). When this is set, the per-file checkboxes
     * and Attach/Share buttons are hidden and a "Save here" button
     * appears in the footer.
     */
    onpickfolder?: (accountId: string, folderPath: string) => void
    onclose: () => void
  }
  let { onpicked, onlinks, onpickfolder, onclose }: Props = $props()

  // True when the picker is being used to choose a destination folder
  // rather than to attach/share existing files.
  let pickFolderMode = $derived(onpickfolder != null)

  let accounts = $state<NextcloudAccount[]>([])
  let accountId = $state<string>('')
  let currentPath = $state<string>('/')
  let entries = $state<FileEntry[]>([])
  let selected = $state<Set<string>>(new Set())
  let loading = $state(false)
  let downloading = $state(false)
  let sharing = $state(false)
  let error = $state('')
  // "New folder" inline input — hidden until the user clicks the button.
  // We keep the name in component state rather than driving it via a
  // prompt() so we can validate, show errors inline, and select the new
  // folder on success.
  let creatingFolder = $state(false)
  let newFolderName = $state('')
  let creatingFolderInFlight = $state(false)
  // Bound to the inline input so we can focus it the moment the user
  // clicks "+ New folder". Using `autofocus` directly would trip the
  // svelte a11y rule (autofocus surprises screen-reader users); a
  // post-click programmatic focus is the recommended replacement.
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
      if (list.length === 1) {
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
      // Folders first, then files. Within each group we use "natural"
      // ordering so numbered names sort as a human expects:
      //   1, 2, 9, 10, 11   (not 1, 10, 11, 2, 9).
      // `numeric: true` on localeCompare reads runs of digits as whole
      // numbers instead of character-by-character. `sensitivity: 'base'`
      // makes the sort case-insensitive so "apple" doesn't land after
      // "Banana" — matches what the Nextcloud web UI does.
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

  // Build breadcrumb segments from the current path — each becomes a
  // clickable step back up the tree.
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

  /**
   * Selection split by entry type. Folders can be shared as public
   * links but not attached as bytes (there's nothing meaningful to
   * pull down — Nextcloud has no zip-folder endpoint). Files can be
   * either. We compute counts so the footer can label buttons
   * correctly and disable Attach when the selection is folders-only.
   */
  let selectedFileCount = $derived.by(() => {
    let n = 0
    for (const e of entries) if (!e.is_dir && selected.has(e.path)) n++
    return n
  })
  let selectedFolderCount = $derived(selected.size - selectedFileCount)

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
    // Svelte 5 Sets: reassign to trigger reactivity.
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

  function iconFor(entry: FileEntry): string {
    if (entry.is_dir) return '📁'
    const ct = entry.content_type ?? ''
    if (ct.startsWith('image/')) return '🖼️'
    if (ct.startsWith('video/')) return '🎞️'
    if (ct.startsWith('audio/')) return '🎵'
    if (ct.includes('pdf')) return '📄'
    if (ct.includes('zip') || ct.includes('compressed')) return '🗜️'
    if (ct.startsWith('text/')) return '📝'
    return '📎'
  }

  /** Last-segment of a `/foo/bar/baz.ext` path — the human filename. */
  function basename(path: string): string {
    return path.split('/').filter(Boolean).pop() ?? path
  }

  async function attachSelected() {
    // Folders in the selection are silently skipped — the footer
    // disables this button when the selection is folders-only, so
    // reaching here means at least one file is selected.
    const filePaths = entries
      .filter((e) => !e.is_dir && selected.has(e.path))
      .map((e) => e.path)
    if (filePaths.length === 0) return
    downloading = true
    error = ''
    try {
      // Run all downloads concurrently. Tauri bridges each invoke to a
      // separate async task so this genuinely parallelises.
      const results = await Promise.all(
        filePaths.map(async (p) => {
          const bytes = await invoke<number[]>('download_nextcloud_file', {
            ncId: accountId,
            path: p,
          })
          const ct =
            entries.find((e) => e.path === p)?.content_type ??
            'application/octet-stream'
          return {
            filename: basename(p),
            content_type: ct,
            data: bytes,
          } satisfies Attachment
        }),
      )
      onpicked(results)
      onclose()
    } catch (e) {
      error = formatError(e) || 'Failed to download file(s)'
    } finally {
      downloading = false
    }
  }

  async function shareSelected() {
    if (selected.size === 0 || !onlinks) return
    sharing = true
    error = ''
    try {
      const paths = Array.from(selected)
      // Same parallelisation rationale as downloads. Each invoke is
      // an independent OCS POST against Nextcloud.
      const results = await Promise.all(
        paths.map(async (p) => {
          const url = await invoke<string>('create_nextcloud_share', {
            ncId: accountId,
            path: p,
          })
          return { filename: basename(p), url } satisfies ShareLink
        }),
      )
      onlinks(results)
      onclose()
    } catch (e) {
      error = formatError(e) || 'Failed to create share link(s)'
    } finally {
      sharing = false
    }
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
   * then refresh the listing and pre-select the new folder so the user
   * can immediately hit "Share as link".
   */
  async function confirmCreateFolder() {
    const trimmed = newFolderName.trim()
    if (!trimmed) {
      error = 'Folder name is required.'
      return
    }
    // Filenames containing path separators would silently change which
    // folder we created in. Block them here rather than letting the
    // server return a confusing 409.
    if (trimmed.includes('/') || trimmed.includes('\\')) {
      error = "Folder name can't contain '/' or '\\'."
      return
    }
    creatingFolderInFlight = true
    error = ''
    try {
      // Join currentPath + name without doubling slashes. currentPath is
      // always either '/' or '/foo/bar' (no trailing slash except root).
      const base = currentPath.endsWith('/') ? currentPath : `${currentPath}/`
      const fullPath = `${base}${trimmed}`
      await invoke('create_nextcloud_directory', {
        ncId: accountId,
        path: fullPath,
      })
      creatingFolder = false
      newFolderName = ''
      // Re-list so the new folder appears, then pre-select it (with the
      // trailing slash the parser uses on folder paths).
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

<div
  class="fixed inset-0 z-60 flex items-center justify-center bg-black/50"
  role="dialog"
  aria-modal="true"
>
  <div class="w-160 max-h-[80vh] bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl flex flex-col">
    <header class="px-5 py-3 border-b border-surface-200 dark:border-surface-700 flex items-center justify-between">
      <h2 class="text-base font-semibold">
        {pickFolderMode ? 'Save to Nextcloud' : 'Attach from Nextcloud'}
      </h2>
      <button
        class="text-surface-500 hover:text-surface-900 dark:hover:text-surface-100"
        onclick={onclose}
        aria-label="Close"
      >✕</button>
    </header>

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
            Inline name input. We render it as its own row (rather than
            inside the breadcrumb bar) so a long folder name doesn't
            squeeze the breadcrumbs off-screen on narrow widths.
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
                      Folders also get a checkbox so the user can select
                      a folder to share as a public link. Clicking the
                      checkbox toggles selection without bubbling to the
                      row's onclick (which would navigate into the folder).
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
                    <span class="text-lg">{iconFor(entry)}</span>
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

    {#if error}
      <p class="px-5 py-2 text-sm text-red-500 border-t border-surface-200 dark:border-surface-700">
        {error}
      </p>
    {/if}

    <footer class="px-5 py-3 border-t border-surface-200 dark:border-surface-700 flex items-center gap-2">
      <span class="text-xs text-surface-500">
        {#if pickFolderMode}
          Saving to <span class="font-mono">{currentPath}</span>
        {:else if selected.size === 0}
          Nothing selected
        {:else if selectedFolderCount === 0}
          {selectedFileCount} file{selectedFileCount === 1 ? '' : 's'} selected
        {:else if selectedFileCount === 0}
          {selectedFolderCount} folder{selectedFolderCount === 1 ? '' : 's'} selected
        {:else}
          {selectedFileCount} file{selectedFileCount === 1 ? '' : 's'},
          {selectedFolderCount} folder{selectedFolderCount === 1 ? '' : 's'} selected
        {/if}
      </span>
      <div class="flex-1"></div>
      <button class="btn preset-outlined-surface-500" onclick={onclose}>Cancel</button>
      {#if pickFolderMode}
        <button
          class="btn preset-filled-primary-500"
          disabled={!accountId}
          onclick={() => {
            onpickfolder?.(accountId, currentPath)
            onclose()
          }}
          title="Save the file into this folder"
        >
          💾 Save here
        </button>
      {:else}
        {#if onlinks}
          <button
            class="btn preset-outlined-primary-500"
            disabled={selected.size === 0 || sharing || downloading}
            onclick={shareSelected}
            title="Insert public download links into the email body"
          >
            {sharing ? 'Sharing…' : '🔗 Share as link'}
          </button>
        {/if}
        <button
          class="btn preset-filled-primary-500"
          disabled={selectedFileCount === 0 || downloading || sharing}
          onclick={attachSelected}
          title={selectedFileCount === 0 && selectedFolderCount > 0
            ? 'Folders can be shared as a link, but not attached as bytes'
            : 'Download selected files and attach them to the email'}
        >
          {downloading ? 'Downloading…' : '📎 Attach'}
        </button>
      {/if}
    </footer>
  </div>
</div>
