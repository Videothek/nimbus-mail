/**
 * Shared construction of an outbound iTIP / iMIP meeting invite
 * (#58).  Used by:
 *   - **Compose's "Add Event" flow** — appends the HTML block
 *     into the editor body and stages the `text/calendar`
 *     attachment so it rides out with the user's drafted mail.
 *   - **CalendarView's event-creation flow** — the user creates
 *     an event with attendees from the calendar grid (no Compose
 *     surface in play), and we ship a freshly-composed mail with
 *     the same layout straight to the attendees.
 *
 * Putting both in one helper keeps the two surfaces aligned —
 * recipients see the same look + same iMIP attachment regardless
 * of where the organiser created the event.
 */

import { invoke } from '@tauri-apps/api/core'

export interface BuildInviteOpts {
  /** UID returned by `create_calendar_event`.  The iMIP attachment
   *  uses this so the eventual REPLY pairs back to the right
   *  CalDAV event. */
  uid: string
  summary: string
  /** RFC 3339 timestamps. */
  start: string
  end: string
  /** Optional join / meeting URL.  Talk URLs render as a
   *  primary-filled "Join Talk room" CTA; anything else gets
   *  labelled "Meeting link". */
  url: string | null
  /** Bare email addresses of everyone invited. */
  attendees: string[]
  /** Organiser's email — written into the iMIP `ORGANIZER` line
   *  and used as the SMTP `from` for the invite mail. */
  fromAddress: string
  /** Organiser's display name (for the iMIP `ORGANIZER;CN=` and
   *  the email's plain-text greeting).  Falls back to the email
   *  when missing. */
  fromName: string | null
  /** True when `url` was minted by Nimbus's Talk integration —
   *  flips the CTA copy from "Meeting link" to "Join Talk room". */
  isTalkLink: boolean
}

export interface BuiltInvite {
  /** Suggested subject line for the outbound mail.  Compose lets
   *  the user override (the subject field is editable); CalendarView
   *  uses this verbatim. */
  subject: string
  /** Inline-styled HTML card.  Both surfaces append this to the
   *  outgoing mail's HTML body. */
  html: string
  /** Plain-text fallback for clients that don't render HTML. */
  text: string
  /** iMIP REQUEST `text/calendar` attachment — `data` is a JS
   *  number array because that's what the SMTP layer expects on
   *  the IPC boundary. */
  attachment: {
    filename: string
    content_type: string
    data: number[]
  }
}

const esc = (s: string) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

/** Render the meeting time range the same way both surfaces do —
 *  same-day collapses to "Wed, Jan 1, 09:00 – 10:00", multi-day
 *  spans expand to two full timestamps. */
function formatTimeRange(startStr: string, endStr: string): string {
  const start = new Date(startStr)
  const end = new Date(endStr)
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
  return sameDay
    ? `${dateStr}, ${start.toLocaleTimeString(undefined, timeFmt)} – ${end.toLocaleTimeString(undefined, timeFmt)}`
    : `${start.toLocaleString(undefined, { dateStyle: 'medium', ...timeFmt } as Intl.DateTimeFormatOptions)} – ${end.toLocaleString(undefined, { dateStyle: 'medium', ...timeFmt } as Intl.DateTimeFormatOptions)}`
}

function formatDuration(startStr: string, endStr: string): string {
  const ms = new Date(endStr).getTime() - new Date(startStr).getTime()
  if (!isFinite(ms) || ms <= 0) return ''
  const totalMin = Math.round(ms / 60_000)
  const h = Math.floor(totalMin / 60)
  const m = totalMin % 60
  if (h === 0) return `${m}m`
  if (m === 0) return `${h}h`
  return `${h}h ${m}m`
}

export async function buildInviteEmail(opts: BuildInviteOpts): Promise<BuiltInvite> {
  const title = opts.summary || '(untitled meeting)'
  const when = formatTimeRange(opts.start, opts.end)
  const duration = formatDuration(opts.start, opts.end)
  const linkLabel = opts.isTalkLink ? 'Join Talk room' : 'Meeting link'
  const linkIcon = opts.isTalkLink ? '💬' : '🔗'

  // ── Inline-styled HTML card.  Mail clients strip stylesheet
  // blocks entirely, so every visual rule lives on the element
  // itself.  Table-based layout (vs flexbox) is the robust play:
  // Outlook for Windows uses the Word renderer and ignores
  // `display: flex` / `gap`, but renders tables flawlessly.
  const linkBlock = opts.url
    ? `<tr>
          <td style="padding-top:14px;">
            <a href="${opts.url}" style="display:inline-block; padding:9px 18px; background:#3b82f6; color:#ffffff; border-radius:6px; text-decoration:none; font-weight:600; font-size:14px;">
              ${linkIcon} ${esc(linkLabel)}
            </a>
            <div style="margin-top:6px; font-size:12px; color:#6b7280;">
              ${esc(opts.url)}
            </div>
          </td>
        </tr>`
    : ''

  const durationLine = duration
    ? `<span style="color:#9ca3af;"> · ${esc(duration)}</span>`
    : ''

  const html = `
    <table role="presentation" cellpadding="0" cellspacing="0" border="0" style="border-collapse:separate; border:1px solid #d1d5db; border-radius:10px; padding:18px 20px; max-width:560px; margin-top:14px; font-family:-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background:#f9fafb;">
      <tr>
        <td style="padding-bottom:8px; font-size:12px; font-weight:600; letter-spacing:0.05em; text-transform:uppercase; color:#3b82f6;">
          📅 Meeting invitation
        </td>
      </tr>
      <tr>
        <td style="font-size:18px; font-weight:600; color:#111827; padding-bottom:8px;">
          ${esc(title)}
        </td>
      </tr>
      <tr>
        <td style="font-size:14px; color:#374151; padding-bottom:4px;">
          <strong style="color:#111827;">🕐 When:</strong> ${esc(when)}${durationLine}
        </td>
      </tr>
      ${linkBlock}
    </table>
  `.replace(/\n\s*/g, '')

  // Plain-text fallback — every iCalendar-compliant mail client
  // also accepts `text/plain` so well-built invites carry both.
  // The lines mirror the HTML so users on text-only clients see
  // the same information.
  const textLines: string[] = [
    `📅 Meeting invitation`,
    ``,
    `Title:  ${title}`,
    `When:   ${when}${duration ? ` (${duration})` : ''}`,
  ]
  if (opts.url) textLines.push(`${linkLabel}: ${opts.url}`)
  textLines.push('')
  const text = textLines.join('\n')

  // iMIP REQUEST attachment.  The Rust side renders the iCalendar
  // and we serialise to a JS number array for the SMTP IPC.
  const ics = await invoke<string>('build_event_invite_ics', {
    uid: opts.uid,
    event: {
      summary: opts.summary,
      description: null,
      location: null,
      start: opts.start,
      end: opts.end,
      allDay: false,
      url: opts.url,
      transparency: null,
      attendees: opts.attendees.map((email) => ({ email })),
      reminders: [],
    },
    organizerEmail: opts.fromAddress,
    organizerName: opts.fromName,
    method: 'REQUEST',
  })

  return {
    subject: `Invitation: ${title}`,
    html,
    text,
    attachment: {
      filename: 'invite.ics',
      content_type: 'text/calendar; method=REQUEST; charset=utf-8',
      data: Array.from(new TextEncoder().encode(ics)),
    },
  }
}
