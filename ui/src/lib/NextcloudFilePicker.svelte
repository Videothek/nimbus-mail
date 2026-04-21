<script lang="ts">
  /**
   * NextcloudFilePicker — modal wrapper around `NextcloudFileBrowser`.
   *
   * Two callers today, both via Compose:
   *   - **Attach mode** (default): the user picks files; we download
   *     each one and hand the bytes back via `onpicked`.
   *   - **Share-as-link mode** (when `onlinks` is set): the user picks
   *     files or folders; we ask the server to mint public share URLs
   *     and return them via `onlinks`.
   *   - **Folder-pick mode** (when `onpickfolder` is set): the user
   *     navigates the tree and picks the *current* folder as a target
   *     (used by "Save attachment to Nextcloud").
   *
   * The browse UI itself lives in `NextcloudFileBrowser` so the
   * sidebar-routed `FilesView` can reuse it without dragging in modal
   * chrome or attach-specific actions.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import NextcloudFileBrowser, {
    type FileEntry,
    type NextcloudAccount,
  } from './NextcloudFileBrowser.svelte'

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
     * If set, the picker switches to **folder-pick mode**: the user
     * navigates the tree and picks the *current folder* as a
     * destination (the per-file checkboxes and Attach/Share buttons
     * are hidden, and a "Save here" button appears in the footer).
     */
    onpickfolder?: (accountId: string, folderPath: string) => void
    onclose: () => void
  }
  let { onpicked, onlinks, onpickfolder, onclose }: Props = $props()

  let pickFolderMode = $derived(onpickfolder != null)

  // Bound from the inner browser — we read these to drive the footer
  // buttons and the download/share actions.
  let accountId = $state('')
  let currentPath = $state('/')
  let selected = $state<Set<string>>(new Set())
  let entries = $state<FileEntry[]>([])
  let accounts = $state<NextcloudAccount[]>([])
  let error = $state('')

  let downloading = $state(false)
  let sharing = $state(false)

  // Selection split by entry type. Folders can be shared as public
  // links but not attached as bytes (Nextcloud has no zip-folder
  // endpoint, so there's nothing meaningful to download). The footer
  // uses these counts to label and disable buttons appropriately.
  let selectedFileCount = $derived.by(() => {
    let n = 0
    for (const e of entries) if (!e.is_dir && selected.has(e.path)) n++
    return n
  })
  let selectedFolderCount = $derived(selected.size - selectedFileCount)

  function basename(path: string): string {
    return path.split('/').filter(Boolean).pop() ?? path
  }

  async function attachSelected() {
    const filePaths = entries
      .filter((e) => !e.is_dir && selected.has(e.path))
      .map((e) => e.path)
    if (filePaths.length === 0) return
    downloading = true
    error = ''
    try {
      // Run all downloads in parallel — Tauri bridges each invoke to
      // its own async task so this genuinely parallelises.
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

    <NextcloudFileBrowser
      {pickFolderMode}
      bind:accountId
      bind:currentPath
      bind:selected
      bind:entries
      bind:accounts
      bind:error
    />

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
