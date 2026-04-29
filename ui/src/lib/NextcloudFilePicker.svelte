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
  /** A "share as link" result.
   *
   *  - `filename` / `url` — what the body block needs.
   *  - `id` / `ncId`     — what `update_nextcloud_share_label`
   *    needs so Compose can re-PUT the label whenever the
   *    recipient list changes after the share was minted (#91).
   */
  export interface ShareLink {
    filename: string
    url: string
    id: string
    ncId: string
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
    /**
     * Optional human-readable label that gets attached to every
     * share created from this picker (#91).  Compose passes the
     * mail's recipient string so each share lands in Nextcloud's
     * "Shared with others" list under "who got this link" rather
     * than the default auto-generated name.  Empty / undefined
     * leaves Nextcloud's auto-naming intact.
     */
    shareLabel?: string
    onclose: () => void
  }
  let {
    onpicked,
    onlinks,
    onpickfolder,
    shareLabel,
    onclose,
  }: Props = $props()

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

  // Password-protect-the-share modal. `null` = the prompt isn't open;
  // `paths` is the snapshot of selection at the moment the user
  // clicked "Share as link" so toggling the file tree behind the
  // modal can't change what gets shared. `password` is the in-flight
  // input — empty means "no password" (omitted from the OCS request,
  // which keeps the share open as before).  `permissions` is the
  // OCS bitfield value chosen via the dropdown — defaults to
  // read-only so existing flows behave the same when the user
  // clicks straight through.
  let sharePrompt = $state<{
    paths: string[]
    password: string
    permissions: number
    /** Whether any of the selected paths is a folder.  Drives which
     *  permission options the dropdown shows — "Upload + edit" and
     *  "File drop" only make sense for folder shares (file shares
     *  can't have new files dropped into them by definition). */
    hasFolders: boolean
  } | null>(null)

  /** Common public-link permission combinations Nextcloud's own
   *  share UI exposes.  The bitfield (1 read, 2 update, 4 create,
   *  8 delete, 16 share) gets sent to the OCS endpoint verbatim. */
  /** Permission combinations Nextcloud's own share UI exposes.
   *  `folderOnly` entries are filtered out when the selection is
   *  pure files — "Upload + edit" and "File drop" semantically
   *  only make sense for folder shares; offering them for a file
   *  share would surface a Nextcloud-side rejection ("invalid
   *  permissions"). */
  const PERMISSION_OPTIONS = [
    {
      value: 1,
      label: 'View only',
      hint: 'Recipient can read / download.',
      folderOnly: false,
    },
    {
      value: 3,
      label: 'View and edit',
      hint: 'Recipient can edit the file in Nextcloud.',
      folderOnly: false,
    },
    {
      value: 15,
      label: 'View, edit, upload, delete',
      hint: 'Folder share with full read-write — recipient can drop files in and modify existing ones.',
      folderOnly: true,
    },
    {
      value: 4,
      label: 'File drop (upload only)',
      hint: 'Folder share where recipients can upload but not see the contents.',
      folderOnly: true,
    },
  ] as const

  function visiblePermissionOptions(hasFolders: boolean) {
    return PERMISSION_OPTIONS.filter((o) => !o.folderOnly || hasFolders)
  }

  function permHint(value: number): string {
    return PERMISSION_OPTIONS.find((o) => o.value === value)?.hint ?? ''
  }

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

  /** Open the password prompt instead of jumping straight to OCS.
      The modal lets the user opt into a password (or skip with
      Enter / "Share without password") before any link is minted —
      no way to forget the password gate, no need to delete + recreate
      a share if the user changes their mind mid-click. */
  function shareSelected() {
    if (selected.size === 0 || !onlinks) return
    // Snapshot whether any of the selected entries is a folder so
    // the dropdown can hide the folder-only permission options for
    // pure-file shares.  Looked up against the current `entries`
    // listing — the user can only select within one folder at a
    // time, so this is always the right source of truth.
    const hasFolders = entries.some(
      (e) => e.is_dir && selected.has(e.path),
    )
    sharePrompt = {
      paths: Array.from(selected),
      password: '',
      permissions: 1, // View-only by default — matches Nextcloud's own picker.
      hasFolders,
    }
    error = ''
  }

  /** Run the actual create_nextcloud_share calls with the password
      the user picked (empty string = no password, omitted from the
      OCS form on the Rust side). Same error-surface as the previous
      direct flow. */
  async function commitShare() {
    if (!sharePrompt || !onlinks) return
    const { paths, password, permissions } = sharePrompt
    sharing = true
    error = ''
    try {
      const pw = password.trim() ? password : null
      const results = await Promise.all(
        paths.map(async (p) => {
          const r = await invoke<{ id: string; url: string }>(
            'create_nextcloud_share',
            {
              ncId: accountId,
              path: p,
              password: pw,
              label: shareLabel?.trim() || null,
              permissions,
            },
          )
          return {
            filename: basename(p),
            url: r.url,
            id: r.id,
            ncId: accountId,
          } satisfies ShareLink
        }),
      )
      sharePrompt = null
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

<!-- Password prompt for the public share link. Layered on top of
     the picker's own modal (z-70 vs z-60) so dismissing it returns
     focus to the picker without unmounting the selection. The
     "Share without password" path commits with an empty password,
     which the Rust side translates to omitting the OCS `password`
     param entirely — keeps the previous "no-password share" flow
     reachable in one click. -->
{#if sharePrompt}
  <div
    class="fixed inset-0 z-70 flex items-center justify-center bg-black/50"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onmousedown={(e) => { if (e.target === e.currentTarget && !sharing) sharePrompt = null }}
  >
    <div class="bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl w-96 max-w-full p-5">
      <h3 class="text-base font-semibold mb-1">Password-protect link?</h3>
      <p class="text-xs text-surface-500 mb-3">
        {sharePrompt.paths.length === 1
          ? 'Anyone with the link can open the file.'
          : `Anyone with each link can open ${sharePrompt.paths.length} files.`}
        Setting a password gates the recipient behind it; leave it empty
        to share without one.
      </p>

      <label class="block text-xs text-surface-500 mb-1" for="share-pw">Password (optional)</label>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        id="share-pw"
        type="password"
        class="input w-full text-sm px-2 py-1.5 rounded-md mb-3"
        placeholder="Leave blank for no password"
        bind:value={sharePrompt.password}
        disabled={sharing}
        autofocus
        onkeydown={(e) => {
          if (e.key === 'Enter') { e.preventDefault(); void commitShare() }
          else if (e.key === 'Escape' && !sharing) { e.preventDefault(); sharePrompt = null }
        }}
      />

      <!-- Permissions dropdown — mirrors Nextcloud's own share UI.
           The bitmask values map to OCS's `permissions` form field.
           File-only flows (where the picker is used to attach a
           single document) practically only use 1 / 3; the upload
           variants ride along for folder shares. -->
      <label class="block text-xs text-surface-500 mb-1" for="share-perms">Permissions</label>
      <select
        id="share-perms"
        class="input w-full text-sm px-2 py-1.5 rounded-md mb-1"
        bind:value={sharePrompt.permissions}
        disabled={sharing}
      >
        {#each visiblePermissionOptions(sharePrompt.hasFolders) as opt}
          <option value={opt.value}>{opt.label}</option>
        {/each}
      </select>
      <p class="text-[11px] text-surface-500 mb-3">
        {permHint(sharePrompt.permissions)}
      </p>

      {#if error}
        <p class="text-xs text-red-500 mb-3 wrap-break-word">{error}</p>
      {/if}

      <div class="flex justify-end gap-2">
        <button
          class="btn preset-outlined-surface-500"
          disabled={sharing}
          onclick={() => (sharePrompt = null)}
        >Cancel</button>
        <button
          class="btn preset-filled-primary-500"
          disabled={sharing}
          onclick={() => void commitShare()}
        >
          {#if sharing}Sharing…
          {:else if sharePrompt.password.trim()}Create with password
          {:else}Share without password{/if}
        </button>
      </div>
    </div>
  </div>
{/if}
