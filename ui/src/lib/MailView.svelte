<script lang="ts">
  /**
   * MailView — the right-hand reading pane.
   *
   * Given an account + folder + UID, calls `fetch_message` to pull the
   * full message (headers + body) from the IMAP server. Renders plain
   * text if we have it, otherwise falls back to inline-rendered HTML
   * sanitized by DOMPurify (scripts and dangerous attributes stripped)
   * with remote images blocked by default.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { save } from '@tauri-apps/plugin-dialog'
  import DOMPurify from 'dompurify'
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
    /** RFC 2392 Content-ID — present on attachments referenced inline
     *  via `<a href="cid:…">` in the body. Used to route those
     *  anchor clicks to the right attachment in `attachmentClicked`. */
    content_id?: string | null
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
    /** True when the currently selected folder is the account's
        Drafts mailbox. Swaps the toolbar over from the reply/forward
        cluster to a single "Edit" action, because Reply-to-a-draft
        doesn't model anything useful. */
    isDraftsFolder?: boolean
    /** Open the shown draft back in Compose for editing. Fires only
        from the "Edit" button inside the drafts toolbar. */
    oneditdraft?: (mail: Email) => void
    /** Fires after the message has been successfully archived or
        deleted on the server.  The removed UID is passed back so the
        parent can compute the "next" message to open (auto-advance
        behaviour) instead of forcing the user back to the empty
        reading-pane state. */
    onmessageremoved?: (removedUid: number) => void
    /** App-wide default for "render HTML email on a white canvas".
        When true the body wrapper gets a forced white background and
        dark text so emails designed for a light page stay readable in
        dark mode. The user can override per message via a toolbar
        toggle. */
    forceWhiteBackground?: boolean
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
    isDraftsFolder = false,
    oneditdraft,
    onmessageremoved,
    forceWhiteBackground = true,
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
    showImagesForMessage = false
    trustedSender = false
    whiteBackgroundOverride = null

    // Cache first — lets the reading pane paint instantly when the user
    // re-opens a previously read message (the common case).
    try {
      const cached = await invoke<Email | null>('get_cached_message', {
        accountId: id,
        folder: f,
        uid: u,
      })
      if (id === accountId && f === folder && u === uid && cached) {
        // Resolve trust state BEFORE assigning `email`, otherwise the
        // first render of the message runs with trustedSender=false,
        // briefly flashes the "Remote images blocked" banner, and only
        // then settles into the trusted state — looks like a bug for
        // senders the user has already approved.
        trustedSender = isSenderTrusted(cached.from)
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
        trustedSender = isSenderTrusted(fresh.from)
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

  // ── Per-sender image trust (persisted in localStorage) ──────────────
  // senders the user has chosen "always show images from" live here.
  // Key format: ["user@example.com", ...] — lower-cased bare addresses.

  const TRUSTED_SENDERS_KEY = 'nimbus-trusted-senders'

  function getSenderAddress(from: string): string {
    const m = from.match(/<([^>]+)>/)
    return (m ? m[1] : from).trim().toLowerCase()
  }

  function isSenderTrusted(from: string): boolean {
    try {
      const raw = localStorage.getItem(TRUSTED_SENDERS_KEY)
      const list: string[] = raw ? JSON.parse(raw) : []
      return list.includes(getSenderAddress(from))
    } catch {
      return false
    }
  }

  function addTrustedSender(from: string) {
    try {
      const raw = localStorage.getItem(TRUSTED_SENDERS_KEY)
      const list: string[] = raw ? JSON.parse(raw) : []
      const addr = getSenderAddress(from)
      if (!list.includes(addr)) {
        list.push(addr)
        localStorage.setItem(TRUSTED_SENDERS_KEY, JSON.stringify(list))
      }
    } catch {
      console.warn('Failed to persist trusted sender')
    }
  }

  // Per-message "Show images" toggle; reset to false on every new message.
  let showImagesForMessage = $state(false)
  // True when the sender is in the trusted list (set in load()).
  let trustedSender = $state(false)

  // Per-message override for the white-canvas default. `null` means
  // "use the app-wide preference"; `true` / `false` flip it just for
  // the open message. Reset on every new message in load().
  let whiteBackgroundOverride = $state<boolean | null>(null)
  let effectiveWhiteBackground = $derived(
    whiteBackgroundOverride ?? forceWhiteBackground,
  )

  // ── HTML sanitization + image blocking ───────────────────────────────
  //
  // DOMPurify strips scripts, event handlers, and any element that can
  // execute code or load external resources (iframe, object, form…).
  // We keep inline styles so newsletter formatting survives, but we
  // forbid <style> blocks — they can't be easily scoped and could
  // clobber the app's UI classes. Most real-world HTML email uses
  // inline styles anyway (Gmail strips <style> too, so senders know).
  //
  // After DOMPurify, a second pass with DOMParser:
  //   • annotates <a href> with a tooltip showing the raw URL (phishing
  //     guard — the Tauri webview hides the URL bar)
  //   • marks cid: anchors with data-nimbus-cid for the click handler
  //   • unless showImages is true, replaces every remote <img src> with
  //     a transparent 1×1 GIF and stashes the original in
  //     data-nimbus-blocked-src

  const BLOCKED_IMG_PLACEHOLDER =
    'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7'

  function processEmailHtml(
    html: string,
    showImages: boolean,
  ): { html: string; hadBlocked: boolean } {
    if (!html) return { html: '', hadBlocked: false }
    try {
      const clean = DOMPurify.sanitize(html, {
        FORBID_TAGS: [
          'script', 'noscript', 'object', 'embed', 'applet',
          'iframe', 'frame', 'frameset',
          'form', 'input', 'textarea', 'select', 'button',
          'base', 'meta', 'link', 'style',
        ],
        ADD_ATTR: ['target', 'data-nimbus-cid', 'data-nimbus-blocked-src', 'title'],
        FORCE_BODY: true,
      })

      const doc = new DOMParser().parseFromString(clean, 'text/html')

      // Annotate links with tooltip + handle cid: anchors
      doc.querySelectorAll('a[href]').forEach((a) => {
        const href = a.getAttribute('href') ?? ''
        if (!href) return
        const existing = a.getAttribute('title')
        a.setAttribute('title', existing ? `${existing} — ${href}` : href)
        if (href.toLowerCase().startsWith('cid:')) {
          const cid = href.slice(4).trim().replace(/^<|>$/g, '')
          a.setAttribute('data-nimbus-cid', cid)
          // Neutralise default `cid:` navigation; our click handler takes over.
          a.setAttribute('href', '#')
        } else {
          // External links open in the system browser via open_url command.
          a.setAttribute('target', '_blank')
          a.setAttribute('rel', 'noopener noreferrer')
        }
      })

      // Block remote images unless the user has opted in for this message/sender
      let hadBlocked = false
      if (!showImages) {
        doc.querySelectorAll('img').forEach((img) => {
          const src = img.getAttribute('src') ?? ''
          if (src && !src.toLowerCase().startsWith('data:') && !src.toLowerCase().startsWith('cid:')) {
            hadBlocked = true
            img.setAttribute('data-nimbus-blocked-src', src)
            img.setAttribute('src', BLOCKED_IMG_PLACEHOLDER)
            img.removeAttribute('srcset')
            const alt = img.getAttribute('alt') ?? ''
            if (!alt) img.setAttribute('alt', '(blocked image)')
            img.setAttribute('title', 'Remote image blocked — click "Show images" to load')
          }
        })
      }

      return { html: doc.body.innerHTML, hadBlocked }
    } catch (e) {
      console.warn('processEmailHtml failed:', e)
      return { html: '', hadBlocked: false }
    }
  }

  // Recompute whenever the email body, per-message toggle, or trust state changes.
  let processedHtml = $derived.by(() => {
    if (!email?.body_html) return { html: '', hadBlocked: false }
    return processEmailHtml(email.body_html, showImagesForMessage || trustedSender)
  })

  // ── Click handling for the inline HTML body div ───────────────────────
  //
  // cid: links open the matching attachment (same as before).
  // External http/https links are routed through the `open_url` Tauri
  // command so they open in the user's default system browser instead of
  // navigating inside the app's webview.

  function onBodyClick(e: MouseEvent) {
    const target = e.target as HTMLElement | null
    if (!target) return
    const anchor = target.closest('a') as HTMLAnchorElement | null
    if (!anchor) return

    const cid = anchor.getAttribute('data-nimbus-cid')
    if (cid) {
      e.preventDefault()
      e.stopPropagation()
      if (!email) return
      const att = email.attachments.find(
        (a) => a.content_id != null && a.content_id.toLowerCase() === cid.toLowerCase(),
      )
      if (!att) {
        console.warn(`MailView: cid:${cid} clicked but no matching attachment`)
        return
      }
      void attachmentClicked(att)
      return
    }

    const href = anchor.getAttribute('href') ?? ''
    if (href && href !== '#' && !href.startsWith('javascript:')) {
      e.preventDefault()
      void invoke('open_url', { url: href })
    }
  }

  /** MIME types Nextcloud Office (Collabora) opens in-browser via
   *  the `index.php/f/<id>` deep link. Plain `text/*` is NOT in
   *  the list — those open more cheaply in our existing reading
   *  pane and routing them through Office for view-only is overkill.
   *  When the type is missing / generic (`application/octet-stream`,
   *  common on incoming mail) we fall back to a filename-extension
   *  check below. */
  const OFFICE_MIME_TYPES = new Set([
    'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    'application/vnd.openxmlformats-officedocument.presentationml.presentation',
    'application/vnd.oasis.opendocument.text',
    'application/vnd.oasis.opendocument.spreadsheet',
    'application/vnd.oasis.opendocument.presentation',
    'application/msword',
    'application/vnd.ms-excel',
    'application/vnd.ms-powerpoint',
    'text/csv',
  ])
  const OFFICE_EXTENSIONS = new Set([
    'docx', 'xlsx', 'pptx', 'odt', 'ods', 'odp',
    'doc', 'xls', 'ppt', 'csv',
  ])

  function isOfficeAttachment(att: EmailAttachment): boolean {
    if (OFFICE_MIME_TYPES.has(att.content_type)) return true
    const dot = att.filename.lastIndexOf('.')
    if (dot < 0) return false
    return OFFICE_EXTENSIONS.has(att.filename.slice(dot + 1).toLowerCase())
  }

  function isPdfAttachment(att: EmailAttachment): boolean {
    if (att.content_type === 'application/pdf') return true
    return att.filename.toLowerCase().endsWith('.pdf')
  }

  /** Single dispatch point for any user-driven attachment open
   *  request (currently: cid:-anchor clicks; the attachment-tray
   *  buttons keep their explicit Download / Save-to-NC handlers).
   *  Branches by content type:
   *    - Office docs → upload-to-NC + open in a Collabora window
   *    - PDFs → upload-to-NC + open in Nextcloud's built-in PDF
   *      viewer
   *    - everything else → fall through to download */
  async function attachmentClicked(att: EmailAttachment) {
    if (isOfficeAttachment(att)) {
      await openInOfficeViewer(att)
      return
    }
    if (isPdfAttachment(att)) {
      await openInPdfViewer(att)
      return
    }
    await downloadAttachment(att)
  }

  /** Upload `att` to the user's first connected Nextcloud, ask the
   *  backend for the deep-link URL, and open it in a fresh webview
   *  window. On window close we DELETE the temp file so the user's
   *  Nextcloud doesn't accumulate every attachment they've ever
   *  previewed.
   *
   *  Multi-Nextcloud support is intentionally simple here: pick the
   *  first connected account. The Settings UI will let users choose
   *  a default once we have more than one user with two NCs. */
  async function openInOfficeViewer(att: EmailAttachment) {
    if (!email || uid == null) return
    setBusy(att.part_id, true)
    try {
      const ncAccounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
      if (ncAccounts.length === 0) {
        error =
          'Connect a Nextcloud account in Settings to open Office files in the embedded viewer.'
        return
      }
      const ncId = ncAccounts[0].id

      // Pull the bytes — `download_email_attachment` re-fetches the
      // raw MIME body and decodes the matching part. Fast on a
      // cached message, a single IMAP round-trip otherwise.
      const bytes = await invoke<number[]>('download_email_attachment', {
        accountId: email.account_id,
        folder: email.folder,
        uid,
        partId: att.part_id,
      })

      const result = await invoke<{ url: string; tempPath: string }>(
        'office_open_attachment',
        {
          ncId,
          filename: att.filename,
          data: bytes,
          contentType: att.content_type || null,
        },
      )

      // Open in a top-level Tauri webview window. Each viewer gets
      // a unique label so multiple attachments can be open at once
      // without colliding. The `tauri://destroyed` listener fires
      // exactly once per window — we use it to expunge the temp
      // file from the user's Nextcloud.
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow')
      const label = `office-${crypto.randomUUID().replaceAll('-', '')}`
      const win = new WebviewWindow(label, {
        url: result.url,
        title: att.filename,
        width: 1200,
        height: 800,
      })
      // Attach the cleanup listener BEFORE awaiting create — the
      // window emits `tauri://destroyed` once it's gone, and on a
      // fast close we'd otherwise miss the event. Errors are
      // swallowed; the startup sweeper picks up any orphans.
      void win.once('tauri://destroyed', async () => {
        try {
          await invoke('office_close_attachment', {
            ncId,
            tempPath: result.tempPath,
          })
        } catch (e) {
          console.warn('office_close_attachment failed:', e)
        }
      })
    } catch (e) {
      error = formatError(e) || 'Failed to open in Office'
    } finally {
      setBusy(att.part_id, false)
    }
  }

  /** PDF mirror of `openInOfficeViewer`. The backend uploads the
   *  bytes to a temp folder on the user's Nextcloud and returns a
   *  deep link into Nextcloud's built-in PDF viewer; the frontend
   *  opens that URL in a Tauri window and registers the same
   *  close-cleanup hook (DAV-deletes the temp file when the
   *  window is destroyed). */
  async function openInPdfViewer(att: EmailAttachment) {
    if (!email || uid == null) return
    setBusy(att.part_id, true)
    try {
      const ncAccounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
      if (ncAccounts.length === 0) {
        error =
          'Connect a Nextcloud account in Settings to open PDFs in the embedded viewer.'
        return
      }
      const ncId = ncAccounts[0].id

      const bytes = await invoke<number[]>('download_email_attachment', {
        accountId: email.account_id,
        folder: email.folder,
        uid,
        partId: att.part_id,
      })

      const result = await invoke<{ url: string; tempPath: string }>(
        'pdf_open_attachment',
        {
          ncId,
          filename: att.filename,
          data: bytes,
          contentType: att.content_type || null,
        },
      )

      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow')
      const label = `pdf-${crypto.randomUUID().replaceAll('-', '')}`
      const win = new WebviewWindow(label, {
        url: result.url,
        title: att.filename,
        width: 1200,
        height: 800,
      })
      void win.once('tauri://destroyed', async () => {
        try {
          await invoke('pdf_close_attachment', {
            ncId,
            tempPath: result.tempPath,
          })
        } catch (e) {
          console.warn('pdf_close_attachment failed:', e)
        }
      })
    } catch (e) {
      error = formatError(e) || 'Failed to open PDF'
    } finally {
      setBusy(att.part_id, false)
    }
  }

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

  /** Pull bytes for the attachment and hand them to the backend's
   *  `print_attachment`, which writes the file to OS temp and
   *  opens it with the user's default app for that file type
   *  (Word for .docx, Edge / Acrobat for .pdf, Photos for images,
   *  Notepad for text, etc.). The user then hits Ctrl/Cmd-P from
   *  inside that app to get the system printer-chooser dialog. */
  async function printAttachment(att: EmailAttachment) {
    if (!email || uid == null) return
    setBusy(att.part_id, true)
    try {
      const bytes = await invoke<number[]>('download_email_attachment', {
        accountId: email.account_id,
        folder: email.folder,
        uid,
        partId: att.part_id,
      })
      await invoke('print_attachment', {
        fileName: att.filename,
        bytes,
      })
    } catch (e) {
      error = formatError(e) || 'Failed to print attachment'
    } finally {
      setBusy(att.part_id, false)
    }
  }

  /** Copy the attachment filename to the clipboard. Useful when the
   *  user wants to paste it into another app (e.g. as a reference
   *  in a Talk message) without saving the file first. */
  async function copyFilename(att: EmailAttachment) {
    try {
      await navigator.clipboard.writeText(att.filename)
    } catch (e) {
      console.warn('clipboard write failed', e)
    }
  }

  // ── Per-attachment action menu (Outlook-style chevron dropdown) ──
  // One menu open at a time, keyed by `part_id`. `null` = closed.
  // Anchor + position are captured at click time so the popup floats
  // next to the row that owns it without needing a portal.
  let openMenuFor = $state<number | null>(null)

  function toggleMenu(att: EmailAttachment) {
    openMenuFor = openMenuFor === att.part_id ? null : att.part_id
  }
  function closeMenu() {
    openMenuFor = null
  }

  /** Click handler that runs an action and closes the menu in one
   *  go. `void`-wraps async handlers so the inline onclick stays
   *  synchronous (Svelte warns otherwise). */
  function runAndClose(fn: () => void | Promise<void>) {
    return () => {
      closeMenu()
      void fn()
    }
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

  // ---------------------------------------------------------------------
  // Archive / Delete — top-bar actions that remove the current message
  // from the visible folder. Both follow the same optimistic shape:
  // disable the buttons, run the Tauri command, and notify the parent
  // so it can deselect + refresh the mail list. Errors bubble back into
  // the same `error` banner the load path uses.
  // ---------------------------------------------------------------------
  let removing = $state(false)

  async function archiveMessage() {
    if (!email || uid == null) return
    removing = true
    try {
      await invoke('archive_message', {
        accountId: email.account_id,
        folder: email.folder,
        uid,
      })
      onmessageremoved?.(uid)
    } catch (e) {
      error = formatError(e) || 'Failed to archive'
    } finally {
      removing = false
    }
  }

  async function deleteMessage() {
    if (!email || uid == null) return
    // No confirm dialog yet — matches the "click = commit" shape of
    // the rest of the toolbar. A Trash-folder intermediate (and
    // undo) can come later; for now Delete is outright expunge.
    removing = true
    try {
      await invoke('delete_message', {
        accountId: email.account_id,
        folder: email.folder,
        uid,
      })
      onmessageremoved?.(uid)
    } catch (e) {
      error = formatError(e) || 'Failed to delete'
    } finally {
      removing = false
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

    <!-- Action bar. The Drafts folder shows an "Edit" action instead
         of the reply/forward/mark-read cluster — a draft is the user's
         own unfinished work, so re-opening it in Compose is the only
         gesture that makes sense. -->
    <div class="flex items-center gap-2 px-6 py-2 border-b border-surface-200 dark:border-surface-700 text-sm">
      {#if isDraftsFolder}
        <button
          class="btn btn-sm preset-filled-primary-500"
          onclick={() => email && oneditdraft?.(email)}
          title="Open this draft in Compose to keep editing"
        >✏️ Edit draft</button>
      {:else}
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
      {/if}
      <div class="flex-1"></div>
      {#if email.body_html}
        <!-- Per-message background toggle — flips the white-canvas
             default just for the open mail. The label always shows
             where a click will take you, not the current state. -->
        <button
          class="btn btn-sm preset-outlined-surface-500"
          onclick={() => (whiteBackgroundOverride = !effectiveWhiteBackground)}
          title={effectiveWhiteBackground
            ? "Switch this mail to the app's theme background"
            : 'Switch this mail to a white background'}
        >{effectiveWhiteBackground ? '🎨 Use mail theme' : '📄 White background'}</button>
      {/if}
      <button
        class="btn btn-sm preset-outlined-surface-500"
        disabled={removing}
        onclick={archiveMessage}
        title="Move this message to the Archive folder"
      >{removing ? '…' : 'Archive'}</button>
      <button
        class="btn btn-sm preset-outlined-surface-500"
        disabled={removing}
        onclick={deleteMessage}
        title="Move this message to Trash (permanently deletes if already in Trash or if the account has no Trash folder)"
      >{removing ? '…' : 'Delete'}</button>
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
            {@const isOffice = isOfficeAttachment(att)}
            {@const isPdf = isPdfAttachment(att)}
            {@const menuOpen = openMenuFor === att.part_id}
            <li class="relative flex items-center gap-2 pl-3 pr-1 py-1.5 rounded-md bg-surface-100 dark:bg-surface-800 text-sm">
              <span class="text-base">{attachmentIcon(att)}</span>
              <span class="font-medium truncate max-w-60" title={att.filename}>{att.filename}</span>
              {#if att.size != null}
                <span class="text-xs text-surface-500">{formatAttSize(att.size)}</span>
              {/if}

              <!-- Primary action — picks the most natural open
                   verb for the attachment type. Same as a click on
                   the chip itself; the dropdown to the right
                   exposes everything else (Print, Download, Save
                   to NC, Copy filename). Mirrors Outlook's
                   "click = open, ▾ = more". -->
              {#if isOffice}
                <button
                  class="btn btn-sm preset-filled-primary-500 text-xs"
                  disabled={busy}
                  onclick={() => openInOfficeViewer(att)}
                  title="Open in Nextcloud Office (Collabora)"
                >
                  {busy ? '…' : '📝 Open in Office'}
                </button>
              {:else if isPdf}
                <button
                  class="btn btn-sm preset-filled-primary-500 text-xs"
                  disabled={busy}
                  onclick={() => openInPdfViewer(att)}
                  title="Open in Nextcloud's built-in PDF viewer"
                >
                  {busy ? '…' : '📄 Open PDF'}
                </button>
              {:else}
                <button
                  class="btn btn-sm preset-filled-primary-500 text-xs"
                  disabled={busy}
                  onclick={() => downloadAttachment(att)}
                  title="Download to your computer"
                >
                  {busy ? '…' : '⬇ Download'}
                </button>
              {/if}

              <!-- Chevron toggle. Sits flush against the primary
                   button so they read as one pill with a split
                   click target. -->
              <button
                class="btn btn-sm preset-outlined-surface-500 text-xs px-2"
                disabled={busy}
                aria-haspopup="menu"
                aria-expanded={menuOpen}
                aria-label="More attachment actions"
                onclick={() => toggleMenu(att)}
                title="More actions"
              >▾</button>

              {#if menuOpen}
                <!-- Click-outside catcher. Sits behind the menu so
                     anywhere outside dismisses, but the menu itself
                     (z-50) stays above and receives clicks. -->
                <button
                  type="button"
                  class="fixed inset-0 z-40 cursor-default"
                  aria-label="Close menu"
                  onclick={closeMenu}
                ></button>
                <div
                  role="menu"
                  class="absolute right-0 top-full mt-1 z-50 min-w-52 rounded-md shadow-lg border border-surface-300 dark:border-surface-700 bg-surface-50 dark:bg-surface-900 py-1 text-sm"
                >
                  {#if isOffice}
                    <button
                      role="menuitem"
                      class="block w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
                      onclick={runAndClose(() => openInOfficeViewer(att))}
                    >📝 Open in Office</button>
                  {:else if isPdf}
                    <button
                      role="menuitem"
                      class="block w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
                      onclick={runAndClose(() => openInPdfViewer(att))}
                    >📄 Open PDF</button>
                  {/if}
                  <button
                    role="menuitem"
                    class="block w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
                    onclick={runAndClose(() => printAttachment(att))}
                    title="Open this attachment in its default desktop app (Ctrl/Cmd-P there to print)"
                  >🖥 Open in Desktop App</button>
                  <button
                    role="menuitem"
                    class="block w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
                    onclick={runAndClose(() => downloadAttachment(att))}
                  >⬇ Save to disk…</button>
                  <button
                    role="menuitem"
                    class="block w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
                    onclick={runAndClose(() => startSaveToNextcloud(att))}
                  >☁ Save to Nextcloud…</button>
                  <div class="my-1 border-t border-surface-200 dark:border-surface-700"></div>
                  <button
                    role="menuitem"
                    class="block w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
                    onclick={runAndClose(() => copyFilename(att))}
                  >📋 Copy filename</button>
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      </div>
    {/if}

    <!-- Email body. Prefer HTML when present — multipart/alternative
         senders (GitHub, newsletters, almost everything modern) include
         a plain-text fallback for clients that can't render HTML, but
         the HTML is what carries the real formatting (layout, links,
         brand styles). DOMPurify + remote-image blocking make this
         safe; we fall back to plain text only when no HTML part exists. -->
    <div class="flex-1 overflow-y-auto">
      {#if email.body_html}
        <!-- Image-blocking banner — only visible when at least one remote
             image was replaced with a placeholder and the user hasn't opted
             in for this message or trusted this sender. -->
        {#if processedHtml.hadBlocked && !showImagesForMessage && !trustedSender}
          <div class="flex flex-wrap items-center gap-3 px-6 py-2 bg-amber-50 dark:bg-amber-900/20 border-b border-amber-200 dark:border-amber-700 text-sm text-amber-800 dark:text-amber-300">
            <span class="shrink-0">🛡️ Remote images are blocked.</span>
            <button
              class="btn btn-sm preset-outlined-surface-500"
              onclick={() => (showImagesForMessage = true)}
            >Show images</button>
            {#if email.from}
              <button
                class="btn btn-sm preset-outlined-surface-500"
                onclick={() => {
                  addTrustedSender(email!.from)
                  trustedSender = true
                }}
              >Always show from {getSenderAddress(email.from)}</button>
            {/if}
          </div>
        {/if}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div
          class="email-html-body text-sm leading-relaxed overflow-x-auto {effectiveWhiteBackground
            ? 'email-html-body--white'
            : 'email-html-body--native p-6'}"
          role="region"
          aria-label="Email body"
          onclick={onBodyClick}
        >
          {@html processedHtml.html}
        </div>
      {:else if email.body_text}
        <!-- Plain-text fallback for messages without an HTML part. -->
        <pre class="whitespace-pre-wrap font-sans text-sm p-6">{email.body_text}</pre>
      {:else}
        <p class="text-sm text-surface-500 p-6">(This message has no visible body.)</p>
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
