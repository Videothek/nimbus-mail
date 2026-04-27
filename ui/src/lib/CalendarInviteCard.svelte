<script lang="ts">
  /**
   * CalendarInviteCard — RSVP UI for inbound mail carrying a
   * `text/calendar; method=REQUEST` attachment (#58, iMIP).
   *
   * Surfaced by `MailView` whenever the open message has at least
   * one `text/calendar` attachment.  The card shows the invite's
   * title + time slot + location/url so the user can sanity-check
   * the slot before responding, then sends an RSVP via
   * `send_event_rsvp` (silent — no Compose modal).
   *
   * The full ICS bytes ride through `raw_ics` so the REPLY can
   * preserve the original UID and propagate the user's PARTSTAT
   * back without re-fetching anything.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  /** Mirrors the Rust `EventAttendee` struct.  Optional fields
   *  are `null` over IPC; we treat them as missing. */
  interface InviteAttendee {
    email: string
    common_name?: string | null
    status?: string | null
  }

  /** Mirrors the Rust `InviteSummary` returned by
   *  `parse_event_invite`.  `rawIcs` is the original ICS body —
   *  passed back verbatim to `send_event_rsvp` so the REPLY
   *  carries the right UID / DTSTAMP / SEQUENCE. */
  export interface InviteSummary {
    /** Calendar-level METHOD (REQUEST / REPLY / CANCEL …).
     *  `MailView` filters on this to suppress the card for
     *  non-REQUEST messages. */
    method: string | null
    uid: string
    summary: string
    start: string
    end: string
    location: string | null
    url: string | null
    organizerEmail: string | null
    organizerName: string | null
    attendees: InviteAttendee[]
    rawIcs: string
  }

  let {
    invite,
    accountId,
    accountEmail,
    fromAddress,
    onresponded,
  }: {
    invite: InviteSummary
    /** Current mail account id — used by `send_event_rsvp` to
     *  pick the SMTP server for the REPLY. */
    accountId: string
    /** Current account's email — written into the REPLY's
     *  matching ATTENDEE row when its PARTSTAT flips. */
    accountEmail: string
    /** Message's `From:` header — fallback for the organiser
     *  address when the parsed invite doesn't carry one (the
     *  parser doesn't surface ORGANIZER as a typed field today,
     *  see `parse_event_invite` in main.rs). */
    fromAddress: string
    /** Fires after a successful RSVP so the parent can
     *  optimistically update the mail's read state, hide the
     *  card, etc.  Carries the chosen PARTSTAT for any
     *  follow-up the parent wants to do. */
    onresponded?: (partstat: 'ACCEPTED' | 'DECLINED' | 'TENTATIVE') => void
  } = $props()

  type Partstat = 'ACCEPTED' | 'DECLINED' | 'TENTATIVE'
  let busy = $state<Partstat | null>(null)
  let respondedAs = $state<Partstat | null>(null)
  let error = $state('')

  /** Human-readable past-tense verb for the chosen response.
   *  Used both in the "You replied: …" callout and as the label
   *  on the highlighted button after the user has answered. */
  function verbFor(p: Partstat): string {
    if (p === 'ACCEPTED') return 'Accepted'
    if (p === 'DECLINED') return 'Declined'
    return 'Tentative'
  }

  /** Pre-reply (or "change to") imperative label for each option. */
  function actionLabel(p: Partstat): string {
    if (p === 'ACCEPTED') return '✓ Accept'
    if (p === 'DECLINED') return '✗ Decline'
    return '? Tentative'
  }

  /** Pull the bare email out of an RFC 5322 address string ("Name
   *  <user@host>" → "user@host").  Same idea as Compose's helper —
   *  inlined here so the card doesn't pull in the whole address
   *  parser dep. */
  function bareAddr(s: string): string {
    const m = s.match(/<([^>]+)>/)
    return (m ? m[1] : s).trim()
  }

  /** Format the meeting slot the user is being asked to commit to.
   *  Same shape as the body block Compose injects (sameDay → one
   *  date, otherwise two full timestamps) so the visual language
   *  stays consistent across "I'm sending one" and "I got one". */
  let timeRange = $derived.by(() => {
    const start = new Date(invite.start)
    const end = new Date(invite.end)
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
      ? `${dateStr} · ${start.toLocaleTimeString(undefined, timeFmt)} – ${end.toLocaleTimeString(undefined, timeFmt)}`
      : `${start.toLocaleString(undefined, { dateStyle: 'medium', ...timeFmt } as Intl.DateTimeFormatOptions)} – ${end.toLocaleString(undefined, { dateStyle: 'medium', ...timeFmt } as Intl.DateTimeFormatOptions)}`
  })

  /** Approximate duration string ("1h 30m") for the time slot —
   *  helps the user decide quickly whether they can fit the
   *  meeting in. */
  let durationLabel = $derived.by(() => {
    const ms = new Date(invite.end).getTime() - new Date(invite.start).getTime()
    if (!isFinite(ms) || ms <= 0) return ''
    const totalMin = Math.round(ms / 60_000)
    const hours = Math.floor(totalMin / 60)
    const mins = totalMin % 60
    if (hours === 0) return `${mins}m`
    if (mins === 0) return `${hours}h`
    return `${hours}h ${mins}m`
  })

  async function rsvp(partstat: Partstat) {
    // Allow clicking the same response again as a no-op, but
    // skip while a request's already in flight so a double-click
    // doesn't fire two REPLY emails.
    if (busy) return
    if (respondedAs === partstat) return
    error = ''
    busy = partstat
    const organizer = invite.organizerEmail || bareAddr(fromAddress)
    try {
      await invoke('send_event_rsvp', {
        accountId,
        organizerEmail: organizer,
        attendeeEmail: accountEmail,
        summary: invite.summary,
        rawIcs: invite.rawIcs,
        partstat,
      })
      respondedAs = partstat
      onresponded?.(partstat)
    } catch (e) {
      error = formatError(e) || 'Failed to send RSVP'
    } finally {
      busy = null
    }
  }
</script>

<div class="rounded-md border border-primary-500/40 bg-primary-500/5 p-4 mb-3 text-sm">
  <div class="flex items-start justify-between gap-3 mb-2">
    <div class="flex items-center gap-2">
      <span class="text-lg">📅</span>
      <span class="font-semibold">{invite.summary || '(untitled meeting)'}</span>
    </div>
  </div>

  <!-- Time slot.  Front-and-centre so the user can decide
       "does this fit" before clicking anything. -->
  <div class="text-surface-700 dark:text-surface-300 mb-1">
    🕐 <span class="font-medium">{timeRange}</span>
    {#if durationLabel}
      <span class="text-surface-500"> · {durationLabel}</span>
    {/if}
  </div>

  {#if invite.location}
    <div class="text-surface-700 dark:text-surface-300 mb-1">
      📍 {invite.location}
    </div>
  {/if}
  {#if invite.url}
    <div class="text-surface-700 dark:text-surface-300 mb-1 truncate">
      🔗 <a class="text-primary-500 hover:underline" href={invite.url}>{invite.url}</a>
    </div>
  {/if}
  {#if invite.attendees.length > 0}
    <div class="text-xs text-surface-500 mt-1">
      {invite.attendees.length} attendee{invite.attendees.length === 1 ? '' : 's'}
    </div>
  {/if}

  {#if error}
    <p class="text-xs text-red-500 mt-2">{error}</p>
  {/if}

  {#if respondedAs}
    <!-- Post-reply state.  Chosen option = past-tense label
         ("Accepted") with a primary-coloured *border* (no fill)
         to distinguish it from the unanswered "click to commit"
         pre-reply state.  Alternatives become outlined "Change
         to …" so the user can flip their mind without losing
         the visual confirmation of "what did I answer?". -->
    <p class="text-sm text-surface-700 dark:text-surface-300 mt-3">
      <span class="font-medium">You replied:</span> {verbFor(respondedAs)}
    </p>
    <div class="flex flex-wrap items-center gap-2 mt-2">
      {#each (['ACCEPTED', 'TENTATIVE', 'DECLINED'] as Partstat[]) as p}
        {@const isCurrent = respondedAs === p}
        <button
          class="btn btn-sm disabled:opacity-50 {isCurrent
            ? 'border-2 border-primary-500 text-primary-500 bg-transparent hover:bg-primary-500/5 font-semibold'
            : 'preset-outlined-surface-500'}"
          disabled={busy !== null}
          onclick={() => void rsvp(p)}
          title={isCurrent ? 'Your current response' : `Change response to ${verbFor(p)}`}
        >
          {busy === p
            ? 'Sending…'
            : isCurrent
              ? verbFor(p)
              : `Change to ${actionLabel(p)}`}
        </button>
      {/each}
    </div>
  {:else}
    <div class="flex flex-wrap items-center gap-2 mt-3">
      <button
        class="btn btn-sm preset-filled-primary-500 disabled:opacity-50"
        disabled={busy !== null}
        onclick={() => void rsvp('ACCEPTED')}
      >
        {busy === 'ACCEPTED' ? 'Sending…' : '✓ Accept'}
      </button>
      <button
        class="btn btn-sm preset-outlined-surface-500 disabled:opacity-50"
        disabled={busy !== null}
        onclick={() => void rsvp('TENTATIVE')}
      >
        {busy === 'TENTATIVE' ? 'Sending…' : '? Tentative'}
      </button>
      <button
        class="btn btn-sm preset-outlined-surface-500 disabled:opacity-50"
        disabled={busy !== null}
        onclick={() => void rsvp('DECLINED')}
      >
        {busy === 'DECLINED' ? 'Sending…' : '✗ Decline'}
      </button>
    </div>
  {/if}
</div>
