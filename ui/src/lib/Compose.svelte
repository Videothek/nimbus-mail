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
  import CreateTalkRoomModal, { type TalkRoom } from './CreateTalkRoomModal.svelte'
  import EventEditor, { type SavedEvent } from './EventEditor.svelte'

  /** Slim Nextcloud account row — same shape `TalkView` / `Sidebar` use.
      We only need the id to pass to the Talk + Calendar commands. */
  interface NextcloudAccount {
    id: string
  }
  /** Mail account row for the From: picker. Mirrors the public fields
      of the Rust `Account` we actually consume here. */
  interface MailAccount {
    id: string
    display_name: string
    email: string
    signature?: string | null
  }
  /** Slim calendar summary — matches the Rust `CalendarSummary` Tauri
      return shape. We pass the full list to `EventEditor` so the user
      can pick which calendar the event lands in. */
  interface CalendarSummary {
    id: string
    nextcloud_account_id: string
    display_name: string
    color: string | null
    last_synced_at: string | null
  }

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
    /** Files to pre-attach. Used by `FilesView`'s "New mail with
        attachment" action so the user can pick files in the Files
        view and land in Compose with them already attached. */
    attachments?: Attachment[]
    /** Public Nextcloud share URLs to render into the body as a
        "Shared via Nextcloud" block. Used by `FilesView`'s "New mail
        with link" action — the same shape the in-Compose picker
        produces via `appendHtml`. */
    nextcloudLinks?: { filename: string; url: string }[]
    /** Nextcloud Talk room link to render into the body as a
        "Join the Talk room" block. Used by `TalkView`'s "Share link"
        action and by `MailView`'s "Create Talk room from this thread"
        action — the latter creates the room first and then opens
        Compose to invite the participants. */
    talkLink?: { name: string; url: string }
  }

  interface Props {
    /** Every configured mail account. Drives the From: picker; with
        a single account the picker collapses to a static label. */
    accounts: MailAccount[]
    /** The account this draft starts in — usually the active inbox
        account, or the receiving account for replies. The user can
        switch via the From: picker before sending. */
    accountId: string
    initial?: ComposeInitial
    onclose: () => void
  }
  let { accounts, accountId, initial, onclose }: Props = $props()

  // ── From: picker state ──────────────────────────────────────
  // The id is the canonical handle (used for `send_email`); the rest
  // of the form pulls the display name / email / signature from the
  // selected account. We re-resolve through `accounts` so that an
  // edit in settings (renamed account, updated signature) propagates
  // without remounting the modal.
  // svelte-ignore state_referenced_locally
  let fromAccountId = $state(accountId)
  const fromAccount = $derived(
    accounts.find((a) => a.id === fromAccountId) ?? accounts[0],
  )
  const fromAddress = $derived(fromAccount?.email ?? '')

  // Drafts are namespaced by the *initial* account so a reply draft
  // doesn't collide with the new-mail draft sitting on a different
  // account. Switching accounts mid-compose keeps using the same
  // draft key — the saved draft was started from this account.
  // svelte-ignore state_referenced_locally
  const DRAFT_KEY = `nimbus-draft:${accountId}`

  // ── Form state ──────────────────────────────────────────────
  // If we got an explicit initial (reply/forward), use it. Otherwise
  // try to rehydrate a locally saved draft so the user doesn't lose
  // work when they accidentally close the window. Props are snapshotted
  // at mount — the modal is remounted when the parent opens a new one.
  //
  // We discriminate "this is a reply/forward" by the presence of `body`
  // or `in_reply_to` rather than by `!initial`, because `openCompose()`
  // defaults `initial` to `{}` (truthy) for blank composes — a plain
  // `!initial` check would mis-classify every blank compose as a reply
  // and skip both draft restoration and signature insertion.
  // svelte-ignore state_referenced_locally
  const saved = !isReplyOrForward(initial) ? loadDraft() : null

  /** Does this initial prefill carry quoted reply / forward content?
      Other prefills (FilesView attachments, TalkView links) leave the
      body empty, so the user is effectively starting a blank compose
      with extras and should still get their saved draft + signature. */
  function isReplyOrForward(init?: ComposeInitial): boolean {
    return !!(init && (init.body || init.in_reply_to))
  }
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
  // svelte-ignore state_referenced_locally
  let attachments = $state<Attachment[]>(initial?.attachments ?? [])
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
  let bodyHtml = $state(initialBodyHtml())

  /** Build the editor's starting HTML — the body (plain text or
      already-HTML draft) with any pre-rendered Nextcloud share-link
      block appended. Same shape as the in-Compose picker emits when
      its `onlinks` callback fires. The signature is *not* added here:
      it's appended via the `$effect` below once both `editorApi` and
      `fromAccount` are settled, because `fromAccount` is a `$derived`
      that may not have a value yet at the time this `$state` initializer
      runs. */
  function initialBodyHtml(): string {
    let html = textToHtml(body)
    const esc = (s: string) =>
      s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    if (initial?.nextcloudLinks && initial.nextcloudLinks.length > 0) {
      const items = initial.nextcloudLinks
        .map((l) => `<p>📎 <a href="${l.url}">${esc(l.filename)}</a></p>`)
        .join('')
      html += `<p><strong>Shared via Nextcloud:</strong></p>${items}`
    }
    if (initial?.talkLink) {
      // Same block shape as the share-link block — keeps the rendered
      // mail consistent across "Share file" and "Share Talk room"
      // entry points.
      html +=
        `<p><strong>Join the Talk room:</strong></p>` +
        `<p>💬 <a href="${initial.talkLink.url}">${esc(initial.talkLink.name)}</a></p>`
    }
    return html
  }

  /** Whether the signature has already been inserted for this compose
      session. Guards against the `$effect` re-running and stacking
      duplicate signatures if `fromAccount` changes (e.g. user picks
      a different From: account). */
  let signatureApplied = false

  /** Insert the active account's signature once the editor is live.
      Done in an effect rather than baked into `initialBodyHtml` because
      `fromAccount` is a `$derived` that may evaluate to `undefined`
      during the `$state(initialBodyHtml())` synchronous init — the
      effect runs after that, by which time the props have flowed
      through and `fromAccount` carries the real account row (with its
      `signature` field). Skipped for replies / forwards / restored
      drafts: those already carry their intended body content. */
  $effect(() => {
    if (signatureApplied) return
    if (isReplyOrForward(initial) || saved) return
    if (!editorApi || !fromAccount) return
    const sig = signatureBlock(fromAccount.signature)
    if (!sig) return
    editorApi.appendHtml(sig)
    signatureApplied = true
  })

  /** Render a per-account signature as the standard `-- ` separator
      followed by the user's lines. Returns `''` when there's no
      signature configured so callers can append unconditionally. */
  function signatureBlock(sig: string | null | undefined): string {
    const text = (sig ?? '').trim()
    if (!text) return ''
    const esc = (s: string) =>
      s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    const lines = text.split('\n').map(esc).join('<br>')
    // The "-- " (with trailing space) prefix is the RFC 3676 signature
    // delimiter — well-behaved mail clients hide it from quoted replies.
    return `<p>-- <br>${lines}</p>`
  }
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

  // ── Talk room + Calendar event creation from Compose ────────
  // Both flows piggyback on the existing modals (`CreateTalkRoomModal`,
  // `EventEditor`) so the UX matches what the user already sees in
  // TalkView / CalendarView. We lazy-load the Nextcloud account list
  // and calendar list — neither is needed unless the user opens one
  // of these flows.
  let showTalkModal = $state(false)
  let showEventEditor = $state(false)
  let ncAccountId = $state('')
  let calendars = $state<CalendarSummary[]>([])
  /** The Talk room created during this compose session (if any). Used
      to seed the "URL" field of the Calendar event so the meeting
      invite carries the join link. */
  let createdTalkLink = $state<{ name: string; url: string } | null>(null)
  /** Token of the room behind `createdTalkLink` — needed by
      `add_talk_participant` when we sync event attendees back into
      the room after the event is saved. */
  let talkRoomToken: string | null = null
  /** Lower-cased bare addresses we've already POSTed to Talk's
      participant endpoint, so the post-save sync skips them. Includes
      the participants we passed at room-creation time. */
  const talkRoomParticipants = new Set<string>()
  /** Whether the "Join the Talk room" body block has been appended
      yet. The Talk button injects immediately; the Add-Event auto-
      create defers injection until the event is saved. */
  let talkLinkInjected = false
  let openingEvent = $state(false)

  /** Resolve a Nextcloud account id (cached for the rest of the
      session). Sets `error` and returns null if none configured. */
  async function ensureNextcloudAccount(): Promise<string | null> {
    if (ncAccountId) return ncAccountId
    try {
      const accounts = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      if (accounts.length === 0) {
        error = 'Connect a Nextcloud account first (Settings → Nextcloud).'
        return null
      }
      ncAccountId = accounts[0].id
      return ncAccountId
    } catch (e) {
      error = formatError(e) || 'Failed to load Nextcloud accounts'
      return null
    }
  }

  async function openTalkModal() {
    error = ''
    const id = await ensureNextcloudAccount()
    if (id) showTalkModal = true
  }

  /** Combined To + Cc list as bare/RFC-formatted address strings,
      ready to seed CreateTalkRoomModal / EventEditor's attendee inputs. */
  function recipients(): string[] {
    return [...splitAddrs(to), ...splitAddrs(cc)]
  }

  /** Append the "Join the Talk room" body block once. Used by both
      the immediate-injection path (Talk button) and the deferred path
      (Add-Event auto-create → injected on event save). The flag keeps
      callers from accidentally duplicating the block. */
  function injectTalkBlock(link: { name: string; url: string }) {
    if (talkLinkInjected) return
    const esc = (s: string) =>
      s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    const block =
      `<p><strong>Join the Talk room:</strong></p>` +
      `<p>💬 <a href="${link.url}">${esc(link.name)}</a></p>`
    editorApi?.appendHtml(block)
    talkLinkInjected = true
  }

  function onTalkRoomCreated(room: TalkRoom, participants: string[]) {
    createdTalkLink = { name: room.display_name, url: room.web_url }
    talkRoomToken = room.token
    for (const p of participants) {
      const k = bareAddr(p).toLowerCase()
      if (k) talkRoomParticipants.add(k)
    }
    // Keep the mail recipients in sync with the Talk invite: any
    // address the user typed into the modal that wasn't already on
    // To/Cc/Bcc gets added to To.
    mergeIntoRecipients(participants)
    // The Talk button is itself a "share this room now" gesture, so
    // inject the body block immediately. The Add-Event auto-create
    // path deliberately bypasses this callback (it uses
    // `createTalkRoomSilently`) so its block can be deferred.
    injectTalkBlock(createdTalkLink)
  }

  /**
   * Create a Talk room without going through CreateTalkRoomModal —
   * used by the Add-Event path so the auto-created room's URL is
   * available to prefill the event's URL field. The body block is
   * **not** injected here; that happens after the event is saved
   * (per the user-facing "after the event is saved, add the talk
   * link to the mail body" semantics).
   */
  async function createTalkRoomSilently() {
    const id = ncAccountId
    if (!id) return
    const seen = new Set<string>()
    const dedupd: { kind: 'email'; value: string }[] = []
    for (const r of recipients()) {
      const addr = bareAddr(r)
      if (!addr) continue
      const k = addr.toLowerCase()
      if (seen.has(k)) continue
      seen.add(k)
      dedupd.push({ kind: 'email', value: addr })
    }
    const room = await invoke<TalkRoom>('create_talk_room', {
      ncId: id,
      // Talk requires a non-empty name. Fall back to a generic label
      // when the user hasn't set a subject yet — they can rename in
      // Nextcloud later.
      roomName: subject.trim() || '(meeting)',
      participants: dedupd,
    })
    createdTalkLink = { name: room.display_name, url: room.web_url }
    talkRoomToken = room.token
    for (const p of dedupd) talkRoomParticipants.add(p.value.toLowerCase())
  }

  async function openEventEditor() {
    error = ''
    openingEvent = true
    try {
      // Calendars come from every connected Nextcloud account so the
      // user can drop the event into any of their calendars — same
      // aggregation CalendarView does on its own load.
      const accounts = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      if (accounts.length === 0) {
        error = 'Connect a Nextcloud account first (Settings → Nextcloud).'
        return
      }
      if (!ncAccountId) ncAccountId = accounts[0].id
      const all: CalendarSummary[] = []
      for (const acc of accounts) {
        try {
          const cs = await invoke<CalendarSummary[]>('get_cached_calendars', { ncId: acc.id })
          all.push(...cs)
        } catch (e) {
          console.warn('get_cached_calendars failed:', e)
        }
      }
      calendars = all
      if (all.length === 0) {
        error = 'No calendars cached yet. Open the Calendar tab once to sync.'
        return
      }
      // Auto-create a Talk room so the event's URL field carries the
      // join link from the start. We skip this if the user already
      // created one via the Talk button (or via an earlier click of
      // Add Event in the same Compose session). Failure is non-fatal:
      // surface the error and still open the editor — the user can
      // create the event without a link or paste one in by hand.
      if (!createdTalkLink) {
        try {
          await createTalkRoomSilently()
        } catch (e) {
          error = formatError(e) || 'Failed to create Talk room for event'
        }
      }
      showEventEditor = true
    } finally {
      openingEvent = false
    }
  }

  /** Build the create-mode draft passed to EventEditor. Default time
      window is the next half-hour for one hour — same behaviour the
      grid's "+ New event" button uses. Subject, attendees, and the
      Talk URL (if a room was just created) seed the editor so the
      user only has to confirm the time. */
  function eventDraft() {
    const start = new Date()
    // Round up to the next 30-minute mark so the prefilled slot is
    // visually clean (no "10:13" oddities).
    const minute = start.getMinutes()
    const bump = (30 - (minute % 30)) % 30 || 30
    start.setMinutes(minute + bump, 0, 0)
    const end = new Date(start)
    end.setHours(end.getHours() + 1)
    return {
      calendarId: calendars[0]?.id ?? '',
      start,
      end,
      summary: subject,
      attendees: recipients(),
      url: createdTalkLink?.url ?? '',
    }
  }

  /** After the EventEditor saves, do the post-save bookkeeping:
   *
   *   1. Add any new event attendees to the email's To field so the
   *      mail recipients track the invite list.
   *   2. Inject the deferred "Join the Talk room" body block (the
   *      auto-create path skips immediate injection so the link
   *      lands in the body once the meeting is actually scheduled).
   *   3. Append the "📅 Meeting" block with title / when / link.
   *   4. Best-effort: POST any new event attendees to the Talk room
   *      so the room's participant list stays aligned with the
   *      event's. EventEditor doesn't await this callback so the
   *      sync happens in the background; per-address failures
   *      (already-added, invalid email) are logged and skipped.
   */
  async function onEventSaved(saved?: SavedEvent) {
    if (!saved) return
    mergeIntoRecipients(saved.attendees)
    if (createdTalkLink) injectTalkBlock(createdTalkLink)
    const esc = (s: string) =>
      s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    const start = new Date(saved.start)
    const end = new Date(saved.end)
    const sameDay =
      start.getFullYear() === end.getFullYear() &&
      start.getMonth() === end.getMonth() &&
      start.getDate() === end.getDate()
    const dateStr = start.toLocaleDateString(undefined, {
      weekday: 'short',
      year: 'numeric',
      month: 'short',
      day: 'numeric',
    })
    const timeFmt: Intl.DateTimeFormatOptions = { hour: '2-digit', minute: '2-digit' }
    const when = sameDay
      ? `${dateStr}, ${start.toLocaleTimeString(undefined, timeFmt)}–${end.toLocaleTimeString(undefined, timeFmt)}`
      : `${start.toLocaleString(undefined, { ...timeFmt, dateStyle: 'medium' } as Intl.DateTimeFormatOptions)} – ${end.toLocaleString(undefined, { ...timeFmt, dateStyle: 'medium' } as Intl.DateTimeFormatOptions)}`
    const title = esc(saved.summary || '(untitled)')
    // Label the link as "Talk room" when the URL matches the one we
    // just created from the Talk button, otherwise call it "Meeting
    // link" so a user who just types a URL by hand doesn't get a
    // misleading "Talk room" label.
    const isTalkLink =
      !!saved.url && createdTalkLink?.url === saved.url
    const linkLabel = isTalkLink ? 'Talk room' : 'Meeting link'
    const linkLine = saved.url
      ? `<p>${linkLabel}: <a href="${saved.url}">${esc(saved.url)}</a></p>`
      : ''
    const block =
      `<p><strong>📅 Meeting:</strong> ${title}</p>` +
      `<p>When: ${esc(when)}</p>` +
      linkLine
    editorApi?.appendHtml(block)

    // Rename the auto-created Talk room to match the final event
    // title. The room was created up-front (see `createTalkRoomSilently`)
    // with the email subject as a placeholder name so we could prefill
    // its URL into the event editor; once the user has typed an actual
    // event title we rename the room so the Talk web UI shows
    // something sensible.
    if (
      talkRoomToken &&
      ncAccountId &&
      createdTalkLink &&
      saved.summary &&
      saved.summary !== createdTalkLink.name
    ) {
      try {
        await invoke('rename_talk_room', {
          ncId: ncAccountId,
          roomToken: talkRoomToken,
          newName: saved.summary,
        })
        createdTalkLink = { ...createdTalkLink, name: saved.summary }
      } catch (e) {
        console.warn('Failed to rename Talk room:', e)
      }
    }

    if (talkRoomToken && ncAccountId) {
      for (const addr of saved.attendees) {
        const key = addr.toLowerCase()
        if (!key || talkRoomParticipants.has(key)) continue
        try {
          await invoke('add_talk_participant', {
            ncId: ncAccountId,
            roomToken: talkRoomToken,
            participant: { kind: 'email', value: addr },
          })
          talkRoomParticipants.add(key)
        } catch (e) {
          console.warn('Failed to add Talk participant', addr, ':', e)
        }
      }
    }
  }

  // Autosave draft whenever the user edits a field. Skip the write
  // for replies/forwards because `loadDraft()` only restores blank
  // composes — autosaving a reply here would otherwise leak its
  // body into the next blank-compose's draft slot for this account.
  $effect(() => {
    const draft = { to, cc, bcc, subject, body: bodyHtml }
    if (isReplyOrForward(initial)) return
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

  /** Strip an `"Name" <addr>` wrapper down to the bare address. Same
      parser shape the EventEditor / CreateTalkRoomModal use. */
  function bareAddr(piece: string): string {
    const trimmed = piece.trim()
    if (!trimmed) return ''
    const m = trimmed.match(/^\s*(?:"[^"]*"|[^<]*?)\s*<([^>]+)>\s*$/)
    return m ? m[1].trim() : trimmed
  }

  /**
   * Merge newly invited addresses back into the email's To field.
   * Used by both the Talk-room-created and calendar-event-saved
   * callbacks so adding someone in either modal also adds them to
   * the mail recipients — keeping the invite and the email aligned.
   * Deduplication is case-insensitive on the bare address, and we
   * skip addresses that are already on To/Cc/Bcc so the user never
   * gets surprised by a recipient jumping from Cc back to To.
   */
  function mergeIntoRecipients(addresses: string[]) {
    const have = new Set<string>()
    for (const field of [to, cc, bcc]) {
      for (const a of splitAddrs(field)) {
        const bare = bareAddr(a).toLowerCase()
        if (bare) have.add(bare)
      }
    }
    const additions: string[] = []
    for (const a of addresses) {
      const bare = bareAddr(a).toLowerCase()
      if (!bare || have.has(bare)) continue
      have.add(bare)
      additions.push(a)
    }
    if (additions.length === 0) return
    const current = splitAddrs(to)
    to = [...current, ...additions].join(', ')
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
        accountId: fromAccountId,
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
      <!-- From: picker. Shown as a real <select> when the user has
           more than one account, otherwise collapsed to a static label
           so single-account users see no extra chrome. -->
      <div class="flex items-center gap-2">
        <label class="text-xs w-14 text-surface-500" for="compose-from">From</label>
        {#if accounts.length > 1}
          <select
            id="compose-from"
            class="select flex-1 px-3 py-2 text-sm rounded-md"
            bind:value={fromAccountId}
          >
            {#each accounts as a (a.id)}
              <option value={a.id}>{a.display_name || a.email} &lt;{a.email}&gt;</option>
            {/each}
          </select>
        {:else}
          <span id="compose-from" class="text-sm text-surface-700 dark:text-surface-300">
            {fromAccount?.display_name || ''} &lt;{fromAddress}&gt;
          </span>
        {/if}
      </div>

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
      <button
        type="button"
        class="btn preset-outlined-surface-500"
        title="Create a Nextcloud Talk room with the current recipients and add the join link to this email"
        onclick={openTalkModal}
      >💬 Talk room</button>
      <button
        type="button"
        class="btn preset-outlined-surface-500"
        disabled={openingEvent}
        title={
          createdTalkLink
            ? 'Create a calendar event with the current recipients as attendees (existing Talk link prefilled)'
            : 'Create a Talk room and a calendar event with the current recipients — the Talk link is added to the event URL and the email body'
        }
        onclick={openEventEditor}
      >{openingEvent ? (createdTalkLink ? 'Loading…' : 'Creating Talk room…') : '📅 Add event'}</button>
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

{#if showTalkModal && ncAccountId}
  <CreateTalkRoomModal
    ncId={ncAccountId}
    initialName={subject}
    initialParticipants={recipients()}
    onclose={() => (showTalkModal = false)}
    oncreated={onTalkRoomCreated}
  />
{/if}

{#if showEventEditor}
  <EventEditor
    mode="create"
    {calendars}
    draft={eventDraft()}
    onclose={() => (showEventEditor = false)}
    onsaved={onEventSaved}
  />
{/if}
