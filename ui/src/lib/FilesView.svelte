<script lang="ts">
  /**
   * FilesView — sidebar-routed full-pane Nextcloud file browser.
   *
   * The complement to `NextcloudFilePicker` (which only opens from
   * inside Compose). This view is what the user lands on when they
   * click "Nextcloud Files" in the sidebar's Integrations list.
   *
   * # Per-issue actions
   *
   * The two "browser-context" actions issue #53 calls for:
   *   - **Send as link in mail** — works for files *and* folders. We
   *     call `create_nextcloud_share` for each selection and then
   *     open Compose with the resulting URLs pre-rendered into the
   *     body (Compose's existing share-link block).
   *   - **Send as attachment in mail** — files only (Nextcloud has
   *     no zip-folder endpoint). We download each file's bytes and
   *     hand them to Compose as pre-filled `attachments`.
   *
   * Both actions accept a multi-select (the footer counts feed the
   * button labels) so the user can build "one mail, three files"
   * without juggling separate Compose windows.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import NextcloudFileBrowser, {
    type FileEntry,
    type NextcloudAccount,
  } from './NextcloudFileBrowser.svelte'
  import type { ComposeInitial } from './Compose.svelte'

  interface Attachment {
    filename: string
    content_type: string
    data: number[]
  }

  interface Props {
    onclose: () => void
    /** Open Compose with the given prefill (attachments / share links).
        Wired by App.svelte to the same handler that drives every other
        Compose entry point. */
    oncompose: (initial: ComposeInitial) => void
  }
  let { onclose, oncompose }: Props = $props()

  // Bound from the inner browser. The view's own footer reads these
  // to label/disable buttons and to drive the action commands.
  let accountId = $state('')
  let currentPath = $state('/')
  let selected = $state<Set<string>>(new Set())
  let entries = $state<FileEntry[]>([])
  let accounts = $state<NextcloudAccount[]>([])
  let error = $state('')

  let attaching = $state(false)
  let sharing = $state(false)

  let selectedFileCount = $derived.by(() => {
    let n = 0
    for (const e of entries) if (!e.is_dir && selected.has(e.path)) n++
    return n
  })
  let selectedFolderCount = $derived(selected.size - selectedFileCount)

  function basename(path: string): string {
    return path.split('/').filter(Boolean).pop() ?? path
  }

  /** Download every selected file (folders are skipped — Nextcloud has
      no zip-folder endpoint) and open Compose with them pre-attached. */
  async function sendAsAttachment() {
    const filePaths = entries
      .filter((e) => !e.is_dir && selected.has(e.path))
      .map((e) => e.path)
    if (filePaths.length === 0) return
    attaching = true
    error = ''
    try {
      // Same parallelisation rationale as the picker: each invoke is
      // an independent task, so this scales with file count rather
      // than serialising.
      const attachments = await Promise.all(
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
      oncompose({ attachments })
    } catch (e) {
      error = formatError(e) || 'Failed to download file(s)'
    } finally {
      attaching = false
    }
  }

  /** Mint a public link for every selection (files *and* folders) and
      open Compose with them rendered into the body as a "Shared via
      Nextcloud" block — same shape Compose already produces when the
      share button inside the picker is used. */
  async function sendAsLink() {
    if (selected.size === 0) return
    sharing = true
    error = ''
    try {
      const paths = Array.from(selected)
      const links = await Promise.all(
        paths.map(async (p) => {
          const url = await invoke<string>('create_nextcloud_share', {
            ncId: accountId,
            path: p,
          })
          return { filename: basename(p), url }
        }),
      )
      oncompose({ nextcloudLinks: links })
    } catch (e) {
      error = formatError(e) || 'Failed to create share link(s)'
    } finally {
      sharing = false
    }
  }
</script>

<div class="h-full flex flex-col bg-surface-50 dark:bg-surface-900">
  <!-- Header — same shape as the Calendar / Contacts views so the
       sidebar-routed integrations all feel like one app, not three. -->
  <div
    class="flex items-center justify-between px-6 py-3 border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800"
  >
    <div class="flex items-center gap-3">
      <h2 class="text-xl font-semibold">Nextcloud Files</h2>
      {#if currentPath !== '/'}
        <span class="text-sm text-surface-500 font-mono">{currentPath}</span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      <button class="btn preset-tonal-surface text-sm" onclick={onclose}>
        Close
      </button>
    </div>
  </div>

  <!-- The shared browser fills the rest. The browser itself owns
       account picking, breadcrumbs, listing, and "+ New folder" — we
       only consume its bound state for the action footer. -->
  <div class="flex-1 min-h-0 flex flex-col">
    <NextcloudFileBrowser
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

    <!-- Action footer. Both buttons are bulk-aware: select N items and
         the resulting Compose window carries all N attachments / links
         in a single new message. -->
    {#if accounts.length > 0 && accountId}
      <footer class="px-5 py-3 border-t border-surface-200 dark:border-surface-700 flex items-center gap-2">
        <span class="text-xs text-surface-500">
          {#if selected.size === 0}
            Tick a file or folder to share via mail
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
        <button
          class="btn preset-outlined-primary-500"
          disabled={selected.size === 0 || sharing || attaching}
          onclick={sendAsLink}
          title="Open a new mail with public download links inserted into the body"
        >
          {sharing ? 'Sharing…' : '🔗 New mail with link'}
        </button>
        <button
          class="btn preset-filled-primary-500"
          disabled={selectedFileCount === 0 || attaching || sharing}
          onclick={sendAsAttachment}
          title={selectedFileCount === 0 && selectedFolderCount > 0
            ? 'Folders can be shared as a link, but not attached as bytes'
            : 'Open a new mail with the selected files attached'}
        >
          {attaching ? 'Downloading…' : '📎 New mail with attachment'}
        </button>
      </footer>
    {/if}
  </div>
</div>
