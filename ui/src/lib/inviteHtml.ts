/**
 * inviteHtml — email-safe HTML render helpers for the invitation
 * blocks that "Respond with meeting" and "Insert Talk link" drop
 * into an outgoing message body (#195).
 *
 * Constraints we design around:
 *   • Inline styles only.  Gmail / Outlook / Yahoo strip <style>
 *     blocks, classes carry no meaning across clients.
 *   • System font stack — no @import, no remote @font-face.
 *   • Width capped at 560 px so the card doesn't blow out a
 *     mobile inbox view.
 *   • Detail-row glyphs are emoji (📅 🕐 📍 📝) because every
 *     client renders them; SVG / icon fonts inside email are
 *     unreliable across Outlook desktop / Gmail.
 *   • Banner image referenced via the project's GitHub raw URL
 *     so the recipient's mail client fetches a real PNG of the
 *     Nimbus logo when they permit remote content.  Falls back
 *     gracefully to alt-text if blocked.
 *
 * The visual language across both cards is identical: same
 * gradient header, same surface card, same CTA button shape.
 * That keeps a thread carrying a Talk room *and* a meeting card
 * (the common case) reading as one consistent invitation,
 * not two unrelated stickers.
 */

/** The Nimbus brand logo as an inline `data:image/png;base64,...`
 *  URI — the storm 128px PNG, ~3.8 KB raw / ~5.2 KB base64.
 *  Embedded so the recipient's mail client renders the brand
 *  header without needing any remote fetch (or remote-content
 *  permission). Tradeoff vs. a public URL: every email is ~5 KB
 *  bigger, but reliability is bulletproof — works in Outlook
 *  desktop, Gmail, Apple Mail, Nextcloud Mail, etc., regardless
 *  of the recipient's "block remote images" setting.
 *
 *  The previous public-URL approach (`raw.githubusercontent.com`)
 *  failed for two reasons that combined: the path I picked
 *  (`nimbus-logo-v2/png/storm/...`) didn't actually exist on the
 *  v2 set (storm is a v1 style), and even with a corrected path
 *  the recipient's mail client would silently block remote
 *  images on first read. Inlining sidesteps both.
 *
 *  Compose's in-app preview can still pass a `logoUrl` override
 *  via `InviteRenderOptions` if it wants the editor to use a
 *  Tauri-served local URL — but the data URI works everywhere
 *  the editor itself runs, so the override isn't strictly
 *  needed any more. */
export const PUBLIC_NIMBUS_LOGO_URL =
  'data:image/png;base64,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'

/** Minimal HTML escape for the few values we splice into the
 *  template. Same characters DOMPurify-tier sanitisers cover —
 *  we don't need full sanitisation here because the inputs are
 *  app-side strings (event summary, room name, attendee list)
 *  rather than untrusted remote content. */
function esc(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

/** Format an ISO / Date pair as a single human line:
 *  "Tuesday, May 6 · 14:00 – 15:00".  Locale-aware via Intl,
 *  weekday-first because that's how every mature calendar
 *  invitation reads. */
function formatRange(start: Date, end: Date): string {
  const sameDay =
    start.getFullYear() === end.getFullYear() &&
    start.getMonth() === end.getMonth() &&
    start.getDate() === end.getDate()
  const dateFmt = new Intl.DateTimeFormat(undefined, {
    weekday: 'long',
    month: 'long',
    day: 'numeric',
  })
  const timeFmt = new Intl.DateTimeFormat(undefined, {
    hour: '2-digit',
    minute: '2-digit',
  })
  const datePart = dateFmt.format(start)
  if (sameDay) {
    return `${datePart} · ${timeFmt.format(start)} – ${timeFmt.format(end)}`
  }
  return `${datePart} ${timeFmt.format(start)} – ${dateFmt.format(end)} ${timeFmt.format(end)}`
}

// ── Shared design tokens (inlined into every style attribute) ──
//
// Tokens kept as string constants so a future tweak to the
// invite palette stays a single-file edit.  Values picked to
// read cleanly on both light and dark mail-client backgrounds —
// the card sits on its own white surface regardless.

const T = {
  card: 'background:#ffffff;border:1px solid #e2e8f0;border-radius:14px;box-shadow:0 1px 2px rgba(15,23,42,0.04),0 8px 24px rgba(15,23,42,0.06);overflow:hidden;',
  headerBg:
    'background:linear-gradient(135deg,#3b82f6 0%,#6366f1 100%);padding:20px 24px;color:#ffffff;',
  headerLogo:
    'width:36px;height:36px;border-radius:9px;background:rgba(255,255,255,0.18);padding:4px;vertical-align:middle;',
  headerWordmark:
    'font-size:13px;font-weight:600;letter-spacing:0.04em;color:rgba(255,255,255,0.92);text-transform:uppercase;vertical-align:middle;margin-left:10px;',
  body: 'padding:24px;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;color:#0f172a;',
  title:
    'margin:0 0 4px 0;font-size:20px;line-height:1.3;font-weight:600;color:#0f172a;',
  subtitle:
    'margin:0;font-size:14px;line-height:1.5;color:#475569;',
  detailLabel:
    'font-size:11px;font-weight:600;letter-spacing:0.06em;text-transform:uppercase;color:#64748b;',
  detailValue:
    'font-size:14px;line-height:1.5;color:#0f172a;margin:2px 0 0 0;',
  divider: 'height:1px;background:#e2e8f0;margin:20px 0;border:0;',
  ctaRow: 'margin-top:24px;',
  ctaButton:
    'display:inline-block;background:#3b82f6;color:#ffffff;text-decoration:none;font-weight:600;font-size:14px;padding:11px 22px;border-radius:10px;letter-spacing:0.01em;',
  footer:
    'margin:20px 0 0 0;padding:14px 16px;background:#f8fafc;border-radius:10px;font-size:12px;line-height:1.5;color:#475569;',
  footerStrong: 'color:#0f172a;font-weight:600;',
} as const

/** Wrap the card body inside the shared chrome (outer wrapper +
 *  branded header).  The header carries a logo and the "Nimbus"
 *  wordmark so the recipient instantly recognises the source
 *  even before reading the title.
 *
 *  The outer `data-nimbus-block` attribute is the marker the
 *  RichTextEditor's `NimbusBlock` extension parses into an atom
 *  node — that's how the styled card survives Tiptap's schema
 *  unwrapping when the HTML is loaded into the editor. */
function chrome(bodyHtml: string, logoUrl: string, kind: string): string {
  return `
<div data-nimbus-block="${kind}" style="max-width:560px;margin:16px 0;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;">
  <div style="${T.card}">
    <div style="${T.headerBg}">
      <img src="${logoUrl}" alt="" width="36" height="36" style="${T.headerLogo}" />
      <span style="${T.headerWordmark}">Nimbus Mail</span>
    </div>
    <div style="${T.body}">
      ${bodyHtml}
    </div>
  </div>
</div>
`.trim()
}

/** One detail row: emoji + label + value.  Stacked vertically
 *  so the wrap-around case (long location, long Talk URL) reads
 *  cleanly instead of forcing horizontal scroll. */
function detailRow(emoji: string, label: string, value: string): string {
  return `
<div style="display:flex;gap:12px;align-items:flex-start;margin:14px 0;">
  <div style="font-size:18px;line-height:1.4;flex-shrink:0;width:24px;text-align:center;">${emoji}</div>
  <div style="flex:1;min-width:0;">
    <div style="${T.detailLabel}">${esc(label)}</div>
    <div style="${T.detailValue}">${value}</div>
  </div>
</div>
`.trim()
}

/** Footer microcopy block — the "You'll be invited via
 *  Nextcloud" line lives here so it reads as a system-level
 *  note rather than competing with the meeting title. */
function nextcloudFooter(message: string): string {
  return `
<div style="${T.footer}">
  <span style="${T.footerStrong}">📨 ${esc(message)}</span><br />
  <span>Accept the invitation in your mail client or directly in Nextcloud — your calendar updates either way.</span>
</div>
`.trim()
}

// ── Public renderers ────────────────────────────────────────────

export interface TalkInvite {
  /** Display name of the Talk room. */
  name: string
  /** Full join URL the recipient will click. */
  url: string
}

/** Optional render-time options.  Compose passes a local
 *  `nimbus-logo://` URL for the in-app preview (dev webviews can
 *  be flaky with arbitrary HTTPS); the outbound submit path uses
 *  the default GitHub-hosted public PNG. */
export interface InviteRenderOptions {
  logoUrl?: string
}

/** Render a Talk-meeting invitation card.  Used by Compose's
 *  "Insert Talk link" flow when the user attaches a freshly-
 *  created Talk room to an outgoing message. */
export function talkInviteHtml(invite: TalkInvite, opts: InviteRenderOptions = {}): string {
  const inner = `
<h1 style="${T.title}">You're invited to a Talk meeting</h1>
<p style="${T.subtitle}">Click the button below to join the conversation in Nextcloud Talk — works in any modern browser, no install.</p>

<hr style="${T.divider}" />

${detailRow(
  '💬',
  'Talk room',
  `<strong style="font-weight:600;">${esc(invite.name)}</strong>`,
)}
${detailRow(
  '🔗',
  'Join link',
  `<a href="${esc(invite.url)}" style="color:#3b82f6;text-decoration:none;word-break:break-all;">${esc(invite.url)}</a>`,
)}

<div style="${T.ctaRow}">
  <a href="${esc(invite.url)}" style="${T.ctaButton}">Join Talk meeting →</a>
</div>

${nextcloudFooter("You'll also be added as a participant in Nextcloud Talk.")}
`.trim()
  return chrome(inner, opts.logoUrl ?? PUBLIC_NIMBUS_LOGO_URL, 'talk-invite')
}

export interface MeetingInvite {
  /** Event title / summary line. */
  summary: string
  /** Event start (Date or ISO string the constructor accepts). */
  start: Date | string
  /** Event end (Date or ISO string). */
  end: Date | string
  /** Optional location text (free-form — a room name, address, or
   *  Talk URL the user typed into the Location field). */
  location?: string | null
  /** Optional notes / description.  Plain text; we wrap-preserve
   *  newlines so a multi-line agenda survives the render. */
  description?: string | null
  /** Optional Talk room URL.  Rendered as its own row + a second
   *  CTA button so the recipient can join the conversation
   *  ahead of the meeting if they want. */
  talkUrl?: string | null
}

/** Render a calendar-meeting invitation card.  Used after the
 *  user saves an event from the "Respond with meeting" flow —
 *  Compose opens with this block pre-filled in the body. */
export function meetingInviteHtml(invite: MeetingInvite, opts: InviteRenderOptions = {}): string {
  const start = invite.start instanceof Date ? invite.start : new Date(invite.start)
  const end = invite.end instanceof Date ? invite.end : new Date(invite.end)

  const detailRows: string[] = [
    detailRow(
      '📅',
      'When',
      `<strong style="font-weight:600;">${esc(formatRange(start, end))}</strong>`,
    ),
  ]
  if (invite.location && invite.location.trim()) {
    detailRows.push(detailRow('📍', 'Where', esc(invite.location.trim())))
  }
  if (invite.talkUrl && invite.talkUrl.trim()) {
    detailRows.push(
      detailRow(
        '💬',
        'Talk room',
        `<a href="${esc(invite.talkUrl)}" style="color:#3b82f6;text-decoration:none;word-break:break-all;">${esc(invite.talkUrl)}</a>`,
      ),
    )
  }
  if (invite.description && invite.description.trim()) {
    // Preserve plain-text newlines as <br> so an agenda with
    // line breaks reads correctly.  The text content goes
    // through `esc` first so any `<` `>` `&` in the user's
    // notes don't accidentally break the surrounding HTML.
    const notesHtml = esc(invite.description.trim()).replace(/\r?\n/g, '<br />')
    detailRows.push(detailRow('📝', 'Notes', notesHtml))
  }

  const ctaHref = invite.talkUrl && invite.talkUrl.trim() ? invite.talkUrl : null
  const ctaBlock = ctaHref
    ? `
<div style="${T.ctaRow}">
  <a href="${esc(ctaHref)}" style="${T.ctaButton}">Join Talk meeting →</a>
</div>
`.trim()
    : ''

  const inner = `
<h1 style="${T.title}">${esc(invite.summary)}</h1>
<p style="${T.subtitle}">A calendar invitation has been created in Nextcloud and shared with everyone on this thread.</p>

<hr style="${T.divider}" />

${detailRows.join('\n')}

${ctaBlock}

${nextcloudFooter("You'll be invited via Nextcloud — accepting in your mail client adds the event to your calendar.")}
`.trim()
  return chrome(inner, opts.logoUrl ?? PUBLIC_NIMBUS_LOGO_URL, 'meeting-invite')
}

// ── Quoted-history wrapper (#195 follow-up) ─────────────────────

/** Wrap a reply's quoted history in a styled container.  Visually
 *  pushes the previous thread back so the user's fresh reply
 *  reads first: muted background, smaller type, soft left border,
 *  generous padding.  Same subdued grey palette across light and
 *  dark themes — designed to read as "old context" rather than
 *  competing with the new content.
 *
 *  The outer `data-nimbus-block` attribute lets the editor's
 *  `NimbusBlock` extension capture this as an atom node so the
 *  styled wrapper survives Tiptap's schema (which would otherwise
 *  unwrap the `<div>` and strip the inline styles). */
export function quotedHistoryHtml(args: {
  fromHeader: string
  whenText: string
  bodyHtml: string
}): string {
  const { fromHeader, whenText, bodyHtml } = args
  const escAttr = (s: string) =>
    s
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;')
  const meta = `On ${escAttr(whenText)}, ${escAttr(fromHeader)} wrote:`
  return `
<div data-nimbus-block="quoted-history" style="margin:24px 0 0 0;padding:14px 18px;background:#f1f5f9;border-left:3px solid #cbd5e1;border-radius:8px;color:#64748b;font-size:13px;line-height:1.55;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;">
  <div style="font-size:11px;font-weight:600;letter-spacing:0.06em;text-transform:uppercase;color:#94a3b8;margin-bottom:8px;">Previous conversation</div>
  <div style="font-size:12px;color:#475569;margin-bottom:10px;">${meta}</div>
  <div style="color:#64748b;">${bodyHtml}</div>
</div>
`.trim()
}
