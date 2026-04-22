<script lang="ts">
  /**
   * MailView — the right-hand reading pane.
   *
   * Given an account + folder + UID, calls `fetch_message` to pull the
   * full message (headers + body) from the IMAP server. Renders plain
   * text if we have it, otherwise falls back to the HTML body inside a
   * sandboxed iframe (to keep any remote-content / scripts in the mail
   * isolated from the app).
   */

  import { invoke } from '@tauri-apps/api/core'
  import { save } from '@tauri-apps/plugin-dialog'
  import { formatError } from './errors'
  import NextcloudFilePicker from './NextcloudFilePicker.svelte'

  interface EmailAttachment {
    filename: string
    content_type: string
    size: number | null
    /**
     * Stable index of this part inside the original MIME tree, used by
     * the backend to re-fetch and extract just this attachment's bytes
     * without retransmitting the rest of the message.
     */
    part_id: number
  }

  interface Email {
    id: string
    account_id: string
    folder: string
    from: string
    to: string[]
    cc: string[]
    subject: string
    body_text: string | null
    body_html: string | null
    date: string
    is_read: boolean
    is_starred: boolean
    has_attachments: boolean
    attachments: EmailAttachment[]
  }

  interface Props {
    accountId: string
    folder?: string
    uid: number | null
    onread?: (uid: number) => void
    onreply?: (mail: Email) => void
    onreplyall?: (mail: Email) => void
    onforward?: (mail: Email) => void
    /** Open the "Create Talk room" flow seeded from this email's
        subject + thread participants. Wired from `App.svelte` so the
        resulting Compose window stacks on top of the inbox view. */
    oncreatetalk?: (mail: Email) => void
  }
  let {
    accountId,
    folder = 'INBOX',
    uid,
    onread,
    onreply,
    onreplyall,
    onforward,
    oncreatetalk,
  }: Props = $props()

  let email = $state<Email | null>(null)
  let loading = $state(false)
  let refreshing = $state(false)
  let error = $state('')

  $effect(() => {
    if (uid == null) {
      email = null
      return
    }
    void load(accountId, folder, uid)
  })

  async function load(id: string, f: string, u: number) {
    loading = true
    refreshing = false
    error = ''
    email = null

    // Cache first — lets the reading pane paint instantly when the user
    // re-opens a previously read message (the common case).
    try {
      const cached = await invoke<Email | null>('get_cached_message', {
        accountId: id,
        folder: f,
        uid: u,
      })
      if (id === accountId && f === folder && u === uid && cached) {
        email = cached
        loading = false
      }
    } catch (e: any) {
      console.warn('get_cached_message failed:', e)
    }

    // Network refresh: pulls fresh flags / body in case the message
    // changed on the server (marked read elsewhere, updated draft, etc.).
    refreshing = email != null
    try {
      const fresh = await invoke<Email>('fetch_message', {
        accountId: id,
        folder: f,
        uid: u,
      })
      if (id === accountId && f === folder && u === uid) {
        email = fresh
      }
    } catch (e: any) {
      if (email == null) {
        error = formatError(e) || 'Failed to load message'
      } else {
        console.warn('fetch_message failed (showing cached):', e)
      }
    } finally {
      loading = false
      refreshing = false
    }

    // Mark as read — fire-and-forget. The MailList picked up an optimistic
    // cache update from the backend, and onread() lets the parent refresh
    // the envelope list so the unread styling clears immediately.
    if (email && !email.is_read && id === accountId && f === folder && u === uid) {
      try {
        await invoke('mark_as_read', { accountId: id, folder: f, uid: u })
        if (email) email.is_read = true
        onread?.(u)
      } catch (e: any) {
        console.warn('mark_as_read failed:', e)
      }
    }
  }

  /** Toggle the read state from the toolbar. Optimistic: flip the
      local flag so the button label flips immediately, then call the
      backend; revert if it fails. The parent's `onread` callback also
      fires so the mail list and sidebar badge update. */
  async function toggleRead() {
    if (!email || uid == null) return
    const next = !email.is_read
    email.is_read = next
    try {
      await invoke('set_message_read', {
        accountId,
        folder,
        uid,
        read: next,
      })
      onread?.(uid)
    } catch (e: any) {
      console.warn('set_message_read failed:', e)
      if (email) email.is_read = !next
    }
  }

  function formatFullDate(iso: string): string {
    return new Date(iso).toLocaleString()
  }

  /**
   * Add a `title` attribute containing the href to every `<a>` in the
   * message HTML, so hovering over a link reveals its URL as a native
   * browser tooltip.
   *
   * Why we need this: the message body renders inside a `sandbox=""`
   * iframe, which in most webviews hides the status bar that would
   * normally show `<a href>` on hover. Without a `title`, users see a
   * blue underlined phrase with no way to preview where it leads —
   * which also happens to be an easy phishing surface. Surfacing the
   * real URL on hover lets the user sanity-check before clicking.
   *
   * Implementation: parse the HTML with DOMParser (which treats scripts
   * as inert text nodes, so it's safe to use on untrusted mail) and
   * annotate each anchor. If the anchor already carries a `title`, keep
   * it and append the URL so we don't clobber author-provided tooltips.
   */
  function addLinkTooltips(html: string): string {
    if (!html) return html
    try {
      const doc = new DOMParser().parseFromString(html, 'text/html')
      doc.querySelectorAll('a[href]').forEach((a) => {
        const href = a.getAttribute('href') || ''
        if (!href) return
        const existing = a.getAttribute('title')
        a.setAttribute('title', existing ? `${existing} — ${href}` : href)
      })
      // `documentElement.outerHTML` gives us a full `<html>…</html>`,
      // which is exactly what `srcdoc` wants. If parsing somehow gives
      // us nothing useful, fall back to the original string below.
      const out = doc.documentElement?.outerHTML
      return out || html
    } catch {
      return html
    }
  }

  // Annotated copy of `email.body_html` with link tooltips. Derived so
  // we recompute only when the message changes, not on every render.
  let bodyHtmlWithTooltips = $derived(
    email?.body_html ? addLinkTooltips(email.body_html) : '',
  )

  // ---------------------------------------------------------------------
  // Attachments — download to disk or save into a Nextcloud folder.
  // ---------------------------------------------------------------------

  // Per-attachment in-flight flags, keyed by part_id. Lets us show a
  // spinner / disable just the row the user clicked instead of locking
  // the whole list.
  let busyParts = $state<Set<number>>(new Set())
  // Set when the user clicks "Save to Nextcloud" on an attachment —
  // mounts the file picker in folder-pick mode. Once a folder is picked
  // we upload the bytes there.
  let savingAttachment = $state<EmailAttachment | null>(null)

  function setBusy(partId: number, busy: boolean) {
    const next = new Set(busyParts)
    if (busy) next.add(partId)
    else next.delete(partId)
    busyParts = next
  }

  function formatAttSize(bytes: number | null): string {
    if (bytes == null) return ''
    if (bytes < 1024) return `${bytes} B`
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`
  }

  function attachmentIcon(att: EmailAttachment): string {
    const ct = att.content_type || ''
    if (ct.startsWith('image/')) return '🖼️'
    if (ct.startsWith('video/')) return '🎞️'
    if (ct.startsWith('audio/')) return '🎵'
    if (ct.includes('pdf')) return '📄'
    if (ct.includes('zip') || ct.includes('compressed')) return '🗜️'
    if (ct.startsWith('text/')) return '📝'
    return '📎'
  }

  /**
   * Download an attachment to a user-chosen location on disk.
   *
   * Flow:
   * 1. Open a native "Save As" dialog (prefilled with the attachment
   *    filename) via `@tauri-apps/plugin-dialog`.
   * 2. If the user cancels, bail without fetching bytes — no point
   *    pulling a multi-MB attachment just to throw it away.
   * 3. Otherwise re-fetch the bytes through `download_email_attachment`
   *    and write them to the chosen path via `save_bytes_to_path`.
   *
   * Why not a synthetic `<a download>` like the earlier version? The
   * WebView 2 / WebKit implementations that Tauri sits on top of don't
   * reliably prompt for a save location — the file either lands in the
   * system Downloads folder or the download fails silently. The native
   * dialog is the only consistent way to let the user pick a path.
   */
  async function downloadAttachment(att: EmailAttachment) {
    if (!email) return
    // Use the `uid` prop directly — `email.id` is a composite string
    // like `{account}-{folder}-{uid}` and parseInt'ing it gives NaN,
    // which serializes to null and fails Tauri's u32 validation.
    if (uid == null) return

    // Ask for a save location first. If the user hits Cancel, `save`
    // resolves to `null` and we stop — no network, no write, no noise.
    let chosenPath: string | null = null
    try {
      chosenPath = await save({
        defaultPath: att.filename,
        title: 'Save attachment',
      })
    } catch (e) {
      error = formatError(e) || 'Failed to open save dialog'
      return
    }
    if (!chosenPath) return

    setBusy(att.part_id, true)
    try {
      const bytes = await invoke<number[]>('download_email_attachment', {
        accountId: email.account_id,
        folder: email.folder,
        uid,
        partId: att.part_id,
      })
      await invoke('save_bytes_to_path', { path: chosenPath, data: bytes })
    } catch (e) {
      error = formatError(e) || 'Failed to download attachment'
    } finally {
      setBusy(att.part_id, false)
    }
  }

  /**
   * Open the Nextcloud picker in folder-pick mode. The picker calls
   * `onSavePicked` with the chosen folder; we then download the
   * attachment bytes and PUT them into that folder.
   */
  function startSaveToNextcloud(att: EmailAttachment) {
    savingAttachment = att
  }

  async function onSavePicked(ncId: string, folderPath: string) {
    const att = savingAttachment
    savingAttachment = null
    if (!email || !att) return
    setBusy(att.part_id, true)
    try {
      // Use the `uid` prop directly — `email.id` is a composite string
      // like `{account}-{folder}-{uid}` and parseInt'ing it gives NaN,
      // which serializes to null and fails Tauri's u32 validation.
      if (uid == null) return
      const bytes = await invoke<number[]>('download_email_attachment', {
        accountId: email.account_id,
        folder: email.folder,
        uid,
        partId: att.part_id,
      })
      // Join the folder path with the filename, avoiding double slashes
      // when folderPath is just '/'.
      const base = folderPath.endsWith('/') ? folderPath : `${folderPath}/`
      const target = `${base}${att.filename}`
      await invoke('upload_to_nextcloud', {
        ncId,
        path: target,
        data: bytes,
        contentType: att.content_type || null,
      })
    } catch (e) {
      error = formatError(e) || 'Failed to save to Nextcloud'
    } finally {
      setBusy(att.part_id, false)
    }
  }
</script>

<main class="flex-1 flex flex-col overflow-hidden">
  {#if uid == null}
    <div class="flex-1 flex items-center justify-center text-surface-500">
      Select a message to read.
    </div>
  {:else if loading}
    <div class="flex-1 flex items-center justify-center text-surface-500">Loading message…</div>
  {:else if error}
    <div class="p-6 text-sm text-red-500">{error}</div>
  {:else if email}
    <!-- Email header -->
    <div class="p-6 border-b border-surface-200 dark:border-surface-700">
      <div class="flex items-start justify-between mb-2 gap-4">
        <h2 class="text-xl font-semibold">{email.subject || '(no subject)'}</h2>
        <div class="flex items-center gap-3 shrink-0">
          {#if refreshing}
            <span class="text-xs text-surface-500">Refreshing…</span>
          {/if}
          <span class="text-sm text-surface-500">{formatFullDate(email.date)}</span>
        </div>
      </div>
      <div class="flex items-center gap-2 text-sm text-surface-600 dark:text-surface-400">
        <span class="font-medium">{email.from || '(unknown sender)'}</span>
      </div>
      {#if email.to.length > 0}
        <div class="text-xs text-surface-500 mt-1">
          To: {email.to.join(', ')}
        </div>
      {/if}
      {#if email.cc.length > 0}
        <div class="text-xs text-surface-500">
          Cc: {email.cc.join(', ')}
        </div>
      {/if}
    </div>

    <!-- Action bar -->
    <div class="flex items-center gap-2 px-6 py-2 border-b border-surface-200 dark:border-surface-700 text-sm">
      <button class="btn btn-sm preset-outlined-surface-500" onclick={() => email && onreply?.(email)}>Reply</button>
      <button class="btn btn-sm preset-outlined-surface-500" onclick={() => email && onreplyall?.(email)}>Reply All</button>
      <button class="btn btn-sm preset-outlined-surface-500" onclick={() => email && onforward?.(email)}>Forward</button>
      {#if oncreatetalk}
        <button
          class="btn btn-sm preset-outlined-primary-500"
          onclick={() => email && oncreatetalk?.(email)}
          title="Create a Nextcloud Talk room with the participants of this thread"
        >💬 Talk</button>
      {/if}
      <button
        class="btn btn-sm preset-outlined-surface-500"
        onclick={toggleRead}
        title={email.is_read ? 'Mark this message as unread' : 'Mark this message as read'}
      >{email.is_read ? 'Mark unread' : 'Mark read'}</button>
      <div class="flex-1"></div>
      <button class="btn btn-sm preset-outlined-surface-500">Archive</button>
      <button class="btn btn-sm preset-outlined-surface-500">Delete</button>
    </div>

    <!-- Attachments — only renders when the message actually has any. -->
    {#if email.attachments.length > 0}
      <div class="px-6 py-3 border-b border-surface-200 dark:border-surface-700">
        <div class="text-xs font-semibold text-surface-500 mb-2">
          {email.attachments.length} attachment{email.attachments.length === 1 ? '' : 's'}
        </div>
        <ul class="flex flex-wrap gap-2">
          {#each email.attachments as att (att.part_id)}
            {@const busy = busyParts.has(att.part_id)}
            <li class="flex items-center gap-2 px-3 py-1.5 rounded-md bg-surface-100 dark:bg-surface-800 text-sm">
              <span class="text-base">{attachmentIcon(att)}</span>
              <span class="font-medium truncate max-w-60" title={att.filename}>{att.filename}</span>
              {#if att.size != null}
                <span class="text-xs text-surface-500">{formatAttSize(att.size)}</span>
              {/if}
              <button
                class="btn btn-sm preset-outlined-surface-500 text-xs"
                disabled={busy}
                onclick={() => downloadAttachment(att)}
                title="Download to your computer"
              >
                {busy ? '…' : '⬇ Download'}
              </button>
              <button
                class="btn btn-sm preset-outlined-primary-500 text-xs"
                disabled={busy}
                onclick={() => startSaveToNextcloud(att)}
                title="Save this attachment to a folder in your Nextcloud"
              >
                {busy ? '…' : '☁ Save to Nextcloud'}
              </button>
            </li>
          {/each}
        </ul>
      </div>
    {/if}

    <!-- Email body -->
    <div class="flex-1 overflow-y-auto p-6">
      {#if email.body_text}
        <!-- Prefer plain text: safe, simple, no remote content. -->
        <pre class="whitespace-pre-wrap font-sans text-sm">{email.body_text}</pre>
      {:else if email.body_html}
        <!--
          HTML-only messages go in a sandboxed iframe. `sandbox=""` (no
          allow-* tokens) disables scripts, form submission, same-origin,
          and top-navigation — so even malicious mail can't attack the app.
        -->
        <iframe
          title="Message body"
          class="w-full h-full border-0 bg-white"
          sandbox=""
          srcdoc={bodyHtmlWithTooltips}
        ></iframe>
      {:else}
        <p class="text-sm text-surface-500">(This message has no visible body.)</p>
      {/if}
    </div>
  {/if}

  {#if savingAttachment}
    <!--
      The picker takes the usual onpicked/onclose pair, but we don't use
      onpicked here — the picker is opened in folder-pick mode (via
      onpickfolder), which short-circuits the per-file selection flow
      entirely. The empty onpicked is just to satisfy the prop.
    -->
    <NextcloudFilePicker
      onpicked={() => {}}
      onpickfolder={onSavePicked}
      onclose={() => (savingAttachment = null)}
    />
  {/if}
</main>
