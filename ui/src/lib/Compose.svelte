<script lang="ts">
  /**
   * Compose — a modal for writing and sending an email.
   *
   * Handles new messages, replies, reply-all, and forwards. The parent
   * opens it with an optional `initial` prefill (used for reply / forward
   * to carry over To/Subject/quoted body).
   *
   * Body is plain text for now — a rich-text/Markdown editor is a later
   * enhancement. Attachments are read in the browser via FileReader so
   * we don't need the Tauri dialog plugin; the raw bytes are shipped to
   * the backend as a byte array.
   *
   * Drafts are saved to localStorage under a per-account key. That's
   * intentionally minimal — proper "Save to Drafts folder via IMAP APPEND"
   * is tracked separately.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import RichTextEditor, { type EditorApi } from './RichTextEditor.svelte'
  import AddressAutocomplete from './AddressAutocomplete.svelte'
  import NextcloudFilePicker from './NextcloudFilePicker.svelte'

  interface Attachment {
    filename: string
    content_type: string
    data: number[]
  }

  export interface ComposeInitial {
    to?: string
    cc?: string
    bcc?: string
    subject?: string
    body?: string
    in_reply_to?: number | null
  }

  interface Props {
    accountId: string
    fromAddress: string
    initial?: ComposeInitial
    onclose: () => void
  }
  let { accountId, fromAddress, initial, onclose }: Props = $props()

  // svelte-ignore state_referenced_locally
  const DRAFT_KEY = `nimbus-draft:${accountId}`

  // ── Form state ──────────────────────────────────────────────
  // If we got an explicit initial (reply/forward), use it. Otherwise
  // try to rehydrate a locally saved draft so the user doesn't lose
  // work when they accidentally close the window. Props are snapshotted
  // at mount — the modal is remounted when the parent opens a new one.
  // svelte-ignore state_referenced_locally
  const saved = !initial ? loadDraft() : null
  // svelte-ignore state_referenced_locally
  let to = $state(initial?.to ?? saved?.to ?? '')
  // svelte-ignore state_referenced_locally
  let cc = $state(initial?.cc ?? saved?.cc ?? '')
  // svelte-ignore state_referenced_locally
  let bcc = $state(initial?.bcc ?? saved?.bcc ?? '')
  // svelte-ignore state_referenced_locally
  let subject = $state(initial?.subject ?? saved?.subject ?? '')
  // svelte-ignore state_referenced_locally
  let body = $state(initial?.body ?? saved?.body ?? '')
  let attachments = $state<Attachment[]>([])
  // Whether the Nextcloud file picker modal is mounted. Picker is lazy
  // so we don't hit `get_nextcloud_accounts` / PROPFIND until the user
  // actually clicks "Attach from Nextcloud".
  let showNcPicker = $state(false)
  // Imperative handle into the rich-text editor — populated once the
  // editor mounts. We use it to append Nextcloud share links into the
  // body without disturbing the user's cursor or undo history.
  let editorApi: EditorApi | null = null
  // The editor content as HTML — kept in sync via the RichTextEditor's
  // onchange callback. The initial body (from reply/forward/draft) is
  // plain text, so we convert newlines to <br> for the WYSIWYG view.
  // svelte-ignore state_referenced_locally
  let bodyHtml = $state(textToHtml(body))
  // svelte-ignore state_referenced_locally
  let showCcBcc = $state(!!cc || !!bcc)

  /** Naively convert plain text (with newlines) into minimal HTML. */
  function textToHtml(text: string): string {
    if (!text) return ''
    // If it already looks like HTML (from a draft/forward), return as-is.
    if (/<[a-z][\s\S]*>/i.test(text)) return text
    // Escape & < > so they render literally, then convert line breaks.
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/\n/g, '<br>')
  }

  /** Strip HTML tags to produce a plain-text fallback for `body_text`. */
  function htmlToText(html: string): string {
    const tmp = document.createElement('div')
    tmp.innerHTML = html
    return tmp.textContent ?? tmp.innerText ?? ''
  }

  let sending = $state(false)
  let error = $state('')
  let savedHint = $state('')

  // Autosave draft whenever the user edits a field.
  $effect(() => {
    const draft = { to, cc, bcc, subject, body: bodyHtml }
    try {
      localStorage.setItem(DRAFT_KEY, JSON.stringify(draft))
    } catch {
      // localStorage full or disabled — silently ignore.
    }
  })

  function loadDraft(): null | { to: string; cc: string; bcc: string; subject: string; body: string } {
    try {
      const raw = localStorage.getItem(DRAFT_KEY)
      return raw ? JSON.parse(raw) : null
    } catch {
      return null
    }
  }

  function clearDraft() {
    try {
      localStorage.removeItem(DRAFT_KEY)
    } catch {
      // ignore
    }
  }

  function saveDraft() {
    // The effect already wrote it — just give the user confirmation.
    savedHint = 'Draft saved'
    setTimeout(() => (savedHint = ''), 1500)
  }

  // Split a comma/semicolon-separated address list into trimmed addresses.
  function splitAddrs(s: string): string[] {
    return s
      .split(/[,;]/)
      .map((a) => a.trim())
      .filter(Boolean)
  }

  async function onPickFiles(e: Event) {
    const input = e.target as HTMLInputElement
    if (!input.files) return
    const picked: Attachment[] = []
    for (const file of Array.from(input.files)) {
      const buf = await file.arrayBuffer()
      picked.push({
        filename: file.name,
        content_type: file.type || 'application/octet-stream',
        data: Array.from(new Uint8Array(buf)),
      })
    }
    attachments = [...attachments, ...picked]
    input.value = ''
  }

  function removeAttachment(i: number) {
    attachments = attachments.filter((_, idx) => idx !== i)
  }

  async function send() {
    error = ''
    const toList = splitAddrs(to)
    if (toList.length === 0) {
      error = 'At least one recipient is required.'
      return
    }
    sending = true
    try {
      await invoke('send_email', {
        accountId,
        email: {
          from: fromAddress,
          to: toList,
          cc: splitAddrs(cc),
          bcc: splitAddrs(bcc),
          reply_to: null,
          subject,
          body_text: htmlToText(bodyHtml),
          body_html: bodyHtml || null,
          attachments,
        },
      })
      clearDraft()
      onclose()
    } catch (e: any) {
      error = formatError(e) || 'Failed to send'
    } finally {
      sending = false
    }
  }

  function cancel() {
    // Draft is kept in localStorage so the user can resume later.
    onclose()
  }
</script>

<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50" role="dialog" aria-modal="true">
  <div class="w-[720px] max-h-[90vh] bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl flex flex-col">
    <header class="px-5 py-3 border-b border-surface-200 dark:border-surface-700 flex items-center justify-between">
      <h2 class="text-base font-semibold">New message</h2>
      <button class="text-surface-500 hover:text-surface-900 dark:hover:text-surface-100" onclick={cancel} aria-label="Close">✕</button>
    </header>

    <div class="flex-1 overflow-y-auto p-5 space-y-3">
      <div class="flex items-center gap-2">
        <label class="text-xs w-14 text-surface-500" for="compose-to">To</label>
        <AddressAutocomplete
          id="compose-to"
          bind:value={to}
          placeholder="alice@example.com, bob@example.com"
        />
        {#if !showCcBcc}
          <button class="text-xs text-primary-500 hover:underline" onclick={() => (showCcBcc = true)}>Cc/Bcc</button>
        {/if}
      </div>

      {#if showCcBcc}
        <div class="flex items-center gap-2">
          <label class="text-xs w-14 text-surface-500" for="compose-cc">Cc</label>
          <AddressAutocomplete id="compose-cc" bind:value={cc} />
        </div>
        <div class="flex items-center gap-2">
          <label class="text-xs w-14 text-surface-500" for="compose-bcc">Bcc</label>
          <AddressAutocomplete id="compose-bcc" bind:value={bcc} />
        </div>
      {/if}

      <div class="flex items-center gap-2">
        <label class="text-xs w-14 text-surface-500" for="compose-subject">Subject</label>
        <input id="compose-subject" class="input flex-1 px-3 py-2 text-sm rounded-md" bind:value={subject} />
      </div>

      <RichTextEditor
        content={bodyHtml}
        onchange={(html) => { bodyHtml = html }}
        onready={(api) => { editorApi = api }}
      />

      {#if attachments.length > 0}
        <div class="flex flex-wrap gap-2">
          {#each attachments as att, i (i)}
            <span class="inline-flex items-center gap-2 px-2 py-1 rounded-md bg-surface-200 dark:bg-surface-800 text-xs">
              <span>📎 {att.filename}</span>
              <button class="text-surface-500 hover:text-red-500" onclick={() => removeAttachment(i)} aria-label="Remove">✕</button>
            </span>
          {/each}
        </div>
      {/if}

      {#if error}
        <p class="text-sm text-red-500">{error}</p>
      {/if}
    </div>

    <footer class="px-5 py-3 border-t border-surface-200 dark:border-surface-700 flex items-center gap-2">
      <button class="btn preset-filled-primary-500" disabled={sending} onclick={send}>
        {sending ? 'Sending…' : 'Send'}
      </button>
      <label class="btn preset-outlined-surface-500 cursor-pointer">
        📎 Attach
        <input type="file" multiple class="hidden" onchange={onPickFiles} />
      </label>
      <button
        type="button"
        class="btn preset-outlined-surface-500"
        onclick={() => (showNcPicker = true)}
      >☁️ From Nextcloud</button>
      <button class="btn preset-outlined-surface-500" onclick={saveDraft}>Save draft</button>
      {#if savedHint}
        <span class="text-xs text-surface-500">{savedHint}</span>
      {/if}
      <div class="flex-1"></div>
      <button class="btn preset-outlined-surface-500" onclick={cancel}>Cancel</button>
    </footer>
  </div>
</div>

{#if showNcPicker}
  <NextcloudFilePicker
    onpicked={(picked) => { attachments = [...attachments, ...picked] }}
    onlinks={(links) => {
      // Drop a small "Shared via Nextcloud" block at the end of the
      // message body. Each link is its own paragraph so it survives
      // mail clients that strip styling. We escape the filename text
      // (URLs themselves only need href-quoting, not body-escaping).
      const esc = (s: string) =>
        s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      const items = links
        .map(
          (l) =>
            `<p>📎 <a href="${l.url}">${esc(l.filename)}</a></p>`,
        )
        .join('')
      const block = `<p><strong>Shared via Nextcloud:</strong></p>${items}`
      editorApi?.appendHtml(block)
    }}
    onclose={() => (showNcPicker = false)}
  />
{/if}
