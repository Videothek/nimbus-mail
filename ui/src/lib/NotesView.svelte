<script lang="ts">
  /**
   * NotesView — sidebar-routed full-pane Nextcloud Notes browser + editor.
   *
   * Two-column layout: list pane on the left (notes sorted by modified
   * desc), editor on the right. Same shell as TalkView / FilesView so
   * the integration views feel coherent.
   *
   * # Why no local cache
   *
   * Same reason TalkView skips it: the Notes API is cheap, the Notes
   * web UI is the canonical editor, and reconciling a local copy with
   * server-side edits via etags is complexity we don't need for the
   * MVP. We refetch on demand and on a slow background timer.
   *
   * # Cross-actions
   *
   * `Send as email` opens Compose with the note's title as subject and
   * the markdown body as the message body — the same `ComposeInitial`
   * shape every other "share to mail" entry point uses, so the editor
   * doesn't need a Notes-specific code path.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { onDestroy, onMount } from 'svelte'
  import { formatError } from './errors'
  import type { ComposeInitial } from './Compose.svelte'

  interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
  }

  /** Mirror of the Rust `nimbus_nextcloud::Note` struct. */
  interface Note {
    id: number
    etag: string
    modified: number
    title: string
    category: string
    content: string
    favorite: boolean
  }

  interface Props {
    onclose: () => void
    /** Open Compose with the given prefill (used for "Send as email"). */
    oncompose: (initial: ComposeInitial) => void
  }
  const { onclose, oncompose }: Props = $props()

  // ── State ───────────────────────────────────────────────────
  let accounts = $state<NextcloudAccount[]>([])
  let accountId = $state('')
  let notes = $state<Note[]>([])
  let loading = $state(false)
  let error = $state('')

  /** Currently selected note id, or `null` for the empty-state pane.
      We hold the id (not the row) so the right-pane editor stays
      bound to the freshest version after a refresh. */
  let selectedId = $state<number | null>(null)
  /** Working copy of the selected note's editable fields. Kept
      separate from the list row so a half-finished edit doesn't
      pollute the list rendering, and so we can diff against the
      server copy at save time. */
  let draftTitle = $state('')
  let draftContent = $state('')
  /** etag we last loaded the open note at — sent back on update so
      the server can reject (412) if a concurrent edit landed. */
  let draftEtag = $state('')
  /** "Saving…" / "Saved" / "Save failed" pill status next to the
      title input. Driven by the debounced auto-save loop. */
  let saveStatus = $state<'' | 'saving' | 'saved' | 'error'>('')

  /** The currently-selected row, looked up at read-time so it stays
      in sync with the list after a refresh. */
  const selected = $derived(
    selectedId == null ? null : notes.find((n) => n.id === selectedId) ?? null,
  )

  // Periodic refresh — slower than Talk's 30s because notes change
  // far less often than chat messages. Two minutes keeps the list
  // fresh enough that a note edited in the web UI shows up before
  // the user gets confused, without spamming the server.
  const REFRESH_INTERVAL_MS = 120_000
  let pollTimer: number | null = null

  onMount(async () => {
    await loadAccounts()
  })

  onDestroy(() => {
    if (pollTimer !== null) window.clearInterval(pollTimer)
    if (saveTimer !== null) clearTimeout(saveTimer)
  })

  async function loadAccounts() {
    try {
      const list = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      accounts = list
      if (list.length >= 1 && !accountId) {
        accountId = list[0].id
        await refresh()
        startPolling()
      }
    } catch (e) {
      error = formatError(e) || 'Failed to load Nextcloud accounts'
    }
  }

  async function selectAccount(id: string) {
    accountId = id
    notes = []
    selectedId = null
    await refresh()
    startPolling()
  }

  function startPolling() {
    if (pollTimer !== null) window.clearInterval(pollTimer)
    pollTimer = window.setInterval(() => {
      void refresh({ silent: true })
    }, REFRESH_INTERVAL_MS)
  }

  async function refresh(opts: { silent?: boolean } = {}) {
    if (!accountId) return
    if (!opts.silent) loading = true
    if (!opts.silent) error = ''
    try {
      const list = await invoke<Note[]>('list_nextcloud_notes', { ncId: accountId })
      // Newest modification on top. Notes with `modified=0` (rare —
      // would mean the server didn't stamp the field) sort to the
      // bottom rather than confusing the order.
      list.sort((a, b) => b.modified - a.modified)
      notes = list
      // If the open note is gone (deleted from another client), drop
      // the editor pane back to its empty state.
      if (selectedId != null && !list.some((n) => n.id === selectedId)) {
        selectedId = null
      }
    } catch (e) {
      if (!opts.silent) error = formatError(e) || 'Failed to load notes'
    } finally {
      if (!opts.silent) loading = false
    }
  }

  function openNote(note: Note) {
    selectedId = note.id
    draftTitle = note.title
    draftContent = note.content
    draftEtag = note.etag
    saveStatus = ''
    if (saveTimer !== null) {
      clearTimeout(saveTimer)
      saveTimer = null
    }
  }

  async function newNote() {
    if (!accountId) return
    try {
      // Empty title + content lets the server stamp a real id and
      // pick "New note" as the title — same default the Notes web
      // UI uses for its "+ New note" button.
      const created = await invoke<Note>('create_nextcloud_note', {
        ncId: accountId,
        title: '',
        content: '',
        category: '',
      })
      notes = [created, ...notes]
      openNote(created)
    } catch (e) {
      error = formatError(e) || 'Failed to create note'
    }
  }

  async function deleteSelected() {
    if (!accountId || selectedId == null) return
    const note = selected
    if (!note) return
    const label = note.title.trim() || 'this note'
    if (!confirm(`Delete ${label}? This cannot be undone.`)) return
    try {
      await invoke('delete_nextcloud_note', { ncId: accountId, noteId: note.id })
      notes = notes.filter((n) => n.id !== note.id)
      selectedId = null
      saveStatus = ''
    } catch (e) {
      error = formatError(e) || 'Failed to delete note'
    }
  }

  // ── Auto-save ───────────────────────────────────────────────
  // Debounced 800ms — long enough that a fast typist doesn't fire
  // a request on every keystroke, short enough that the user feels
  // the save tracking what they typed.
  let saveTimer: ReturnType<typeof setTimeout> | null = null

  function scheduleSave() {
    if (selectedId == null || !accountId) return
    saveStatus = 'saving'
    if (saveTimer !== null) clearTimeout(saveTimer)
    saveTimer = setTimeout(saveNow, 800)
  }

  async function saveNow() {
    if (selectedId == null || !accountId) return
    const id = selectedId
    const titleNow = draftTitle
    const contentNow = draftContent
    try {
      const updated = await invoke<Note>('update_nextcloud_note', {
        ncId: accountId,
        noteId: id,
        etag: draftEtag,
        title: titleNow,
        content: contentNow,
        category: null,
        favorite: null,
      })
      // Splice the freshly-saved row into the list so its updated
      // modified time floats it back to the top on the next sort.
      const rest = notes.filter((n) => n.id !== updated.id)
      notes = [updated, ...rest].sort((a, b) => b.modified - a.modified)
      // Only refresh the etag — the user may already have typed
      // more characters since we kicked off the save, and overwriting
      // their `draftTitle` / `draftContent` would clobber that.
      if (id === selectedId) draftEtag = updated.etag
      saveStatus = 'saved'
      setTimeout(() => {
        if (saveStatus === 'saved') saveStatus = ''
      }, 1500)
    } catch (e) {
      console.warn('save note failed', e)
      saveStatus = 'error'
    }
  }

  /** "Send as email" action — opens Compose with the note as the
      seed of a new message. Title becomes the subject; markdown
      content goes into the body. The body is plain text (the
      RichTextEditor wraps it in <br>s automatically), which keeps
      the markdown legible at the receiving end. */
  function sendAsEmail() {
    const note = selected
    if (!note) return
    oncompose({
      subject: note.title || '(untitled note)',
      body: note.content,
    })
  }

  function fmtDate(epochSecs: number): string {
    if (!epochSecs) return ''
    const d = new Date(epochSecs * 1000)
    const now = new Date()
    const sameDay = d.toDateString() === now.toDateString()
    if (sameDay) {
      return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    }
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
  }

  /** Short preview of the note body for the list row — first
      non-empty line after the title. We strip the title line
      because it's already shown above the snippet, and an
      auto-titled note would otherwise echo the same text twice. */
  function preview(note: Note): string {
    const lines = note.content.split('\n').map((l) => l.trim()).filter(Boolean)
    const title = note.title.trim()
    const body = title && lines[0] === title ? lines.slice(1) : lines
    return body[0] ?? ''
  }
</script>

<div class="h-full flex flex-col bg-surface-50 dark:bg-surface-900">
  <!-- Header -->
  <header class="px-4 py-3 border-b border-surface-200 dark:border-surface-700 flex items-center gap-3">
    <h1 class="text-lg font-semibold flex-1">Notes</h1>

    {#if accounts.length > 1}
      <select
        class="select text-sm py-1 px-2 rounded-md"
        value={accountId}
        onchange={(e) => selectAccount((e.currentTarget as HTMLSelectElement).value)}
      >
        {#each accounts as a (a.id)}
          <option value={a.id}>{a.display_name || a.username}</option>
        {/each}
      </select>
    {/if}

    <button
      class="btn btn-sm preset-outlined-surface-500"
      onclick={() => refresh()}
      disabled={loading || !accountId}
      title="Refresh"
    >
      &#x21bb;
    </button>
    <button
      class="btn btn-sm preset-filled-primary-500"
      onclick={newNote}
      disabled={!accountId}
    >
      + New note
    </button>
    <button class="btn btn-sm preset-outlined-surface-500" onclick={onclose}>
      Back
    </button>
  </header>

  <!-- Body: list pane | editor pane. Falls back to a single-column
       message when there's no NC account configured. -->
  <div class="flex-1 min-h-0 flex">
    {#if accounts.length === 0 && !loading}
      <div class="flex-1 flex items-center justify-center text-sm text-surface-500 p-8 text-center">
        Connect a Nextcloud account first (Settings → Nextcloud) to use Notes.
      </div>
    {:else}
      <!-- List pane -->
      <div class="w-72 shrink-0 border-r border-surface-200 dark:border-surface-700 overflow-y-auto">
        {#if loading && notes.length === 0}
          <div class="p-6 text-center text-sm text-surface-500">Loading…</div>
        {:else if error && notes.length === 0}
          <div class="p-4 text-sm text-red-500">{error}</div>
        {:else if notes.length === 0}
          <div class="p-6 text-center text-sm text-surface-500">
            No notes yet. Click <strong>+ New note</strong> to create one.
          </div>
        {:else}
          {#each notes as n (n.id)}
            <button
              class="w-full text-left px-4 py-3 border-b border-surface-100 dark:border-surface-800 transition-colors
                {selectedId === n.id
                  ? 'bg-primary-500/10'
                  : 'hover:bg-surface-100 dark:hover:bg-surface-800'}"
              onclick={() => openNote(n)}
            >
              <div class="flex items-center justify-between mb-1">
                <span class="text-sm font-medium truncate pr-2">
                  {n.title || '(untitled)'}
                </span>
                <span class="text-xs text-surface-500 shrink-0">{fmtDate(n.modified)}</span>
              </div>
              {#if preview(n)}
                <p class="text-xs text-surface-500 truncate">{preview(n)}</p>
              {/if}
              {#if n.category}
                <p class="text-[10px] text-surface-400 mt-1 truncate">📂 {n.category}</p>
              {/if}
            </button>
          {/each}
        {/if}
      </div>

      <!-- Editor pane -->
      <div class="flex-1 min-w-0 flex flex-col">
        {#if !selected}
          <div class="flex-1 flex items-center justify-center text-sm text-surface-500">
            Select a note from the list, or create a new one.
          </div>
        {:else}
          <div class="px-5 py-3 border-b border-surface-200 dark:border-surface-700 flex items-center gap-2">
            <input
              class="input flex-1 text-base font-semibold px-3 py-2 rounded-md"
              placeholder="Title (optional — derived from the first line if empty)"
              bind:value={draftTitle}
              oninput={scheduleSave}
            />
            {#if saveStatus === 'saving'}
              <span class="text-xs text-surface-400">Saving…</span>
            {:else if saveStatus === 'saved'}
              <span class="text-xs text-success-500">Saved</span>
            {:else if saveStatus === 'error'}
              <span class="text-xs text-error-500">Save failed</span>
            {/if}
            <button
              class="btn btn-sm preset-outlined-primary-500"
              onclick={sendAsEmail}
              title="Open Compose with this note as the message body"
            >
              ✉ Send as email
            </button>
            <button
              class="btn btn-sm preset-outlined-error-500"
              onclick={deleteSelected}
            >
              Delete
            </button>
          </div>

          <textarea
            class="flex-1 resize-none p-5 bg-surface-50 dark:bg-surface-900 outline-none font-mono text-sm leading-relaxed"
            placeholder="Start writing — markdown is preserved on the server."
            bind:value={draftContent}
            oninput={scheduleSave}
          ></textarea>
        {/if}
      </div>
    {/if}
  </div>
</div>
