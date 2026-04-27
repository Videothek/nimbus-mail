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
    onresponded,
  }: {
    invite: InviteSummary
    /** Fires after a successful RSVP so the parent can
     *  optimistically update the mail's read state, hide the
     *  card, etc. */
    onresponded?: (partstat: 'ACCEPTED' | 'DECLINED' | 'TENTATIVE') => void
  } = $props()

  /** Calendar picker rows mirroring `CalendarSummary` — fetched
   *  from every connected Nextcloud account so the user can drop
   *  the accepted event into any of their calendars. */
  interface CalendarRow {
    id: string
    nextcloud_account_id: string
    display_name: string
    color: string | null
    last_synced_at: string | null
    hidden?: boolean
  }
  let calendars = $state<CalendarRow[]>([])
  let selectedCalendarId = $state<string | null>(null)
  let calendarsLoading = $state(true)
  /** Best-effort hint for the backend: the address from the
   *  inbound ATTENDEE list that matches one of the user's
   *  configured mail accounts.  Backend treats it as the
   *  highest-priority candidate when picking the row to mutate
   *  + identify with on Sabre's principal CUA.  Stays `null`
   *  when the invite uses an address Nimbus doesn't have a
   *  matching account for; backend falls back to NC profile
   *  email + mail-account list. */
  let attendeeHint = $state<string | null>(null)

  // Fetch calendars + the user's default-calendar setting on
  // mount.  Both go through Tauri.  We don't gate the card on
  // these completing (the user can read the invite while the
  // picker spins up), but we do disable the buttons until the
  // selection is established.
  $effect(() => {
    calendarsLoading = true
    void Promise.all([
      invoke<{ default_calendar_id: string | null }>('get_app_settings'),
      (async () => {
        const accounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
        const all: CalendarRow[] = []
        for (const acc of accounts) {
          try {
            const cs = await invoke<CalendarRow[]>('get_cached_calendars', {
              ncId: acc.id,
            })
            all.push(...cs)
          } catch (e) {
            console.warn('CalendarInviteCard: get_cached_calendars failed', e)
          }
        }
        return all.filter((c) => !c.hidden)
      })(),
      (async () => {
        // Match the invite's ATTENDEE list against the user's
        // configured mail accounts so the backend has a verified
        // address-that-is-actually-in-the-invite to use as the
        // mutation target.  Skips silently when the lookup
        // fails — backend has its own fallbacks.
        try {
          const list = await invoke<{ email: string }[]>('get_accounts')
          const owned = new Set(
            list.map((a) => a.email.toLowerCase()).filter(Boolean),
          )
          return (
            invite.attendees
              .map((a) => a.email)
              .find((addr) => owned.has(addr.toLowerCase())) ?? null
          )
        } catch {
          return null
        }
      })(),
    ])
      .then(([settings, list, hint]) => {
        calendars = list
        attendeeHint = hint
        const def = settings.default_calendar_id
        if (def && list.some((c) => c.id === def)) {
          selectedCalendarId = def
        } else if (list.length > 0) {
          selectedCalendarId = list[0].id
        }
      })
      .catch((e) => {
        console.warn('CalendarInviteCard: failed to load calendars', e)
      })
      .finally(() => {
        calendarsLoading = false
      })
  })

  type Partstat = 'ACCEPTED' | 'DECLINED' | 'TENTATIVE'
  let busy = $state<Partstat | null>(null)
  let respondedAs = $state<Partstat | null>(null)
  let error = $state('')
  /** True until the partstat lookup for the *current* invite
   *  has completed at least once.  The action row is suppressed
   *  while this is true so the user never sees the fresh
   *  Accept/Decline buttons flash and snap to the post-reply
   *  state — the row appears in its final shape directly.
   *
   *  Tracked per-UID so navigating to a different invite
   *  re-gates rendering, but in-place reactivity (e.g.
   *  `attendeeHint` arriving from a parallel effect) does NOT
   *  re-trigger the gate: the previously-resolved state stays
   *  visible while we re-query in the background, no flicker. */
  let partstatLoadedUid = $state<string | null>(null)
  /** Tracks completion of the parallel cancellation probe
   *  (`is_invite_cancelled` + `is_event_in_calendar`).  Paired
   *  with `partstatLoadedUid` to gate the *entire card* — not
   *  just the action row.  Without that broader gate the
   *  outer flavour (blue REQUEST border vs red CANCEL border,
   *  the title strikethrough, the leading icon) paints in
   *  REQUEST mode for a moment and then snaps to CANCEL once
   *  `cancelledLater` lands, which the user reads as a
   *  flicker.  Holding the card until both probes are in lets
   *  it appear in its final flavour from the first paint. */
  let cancellationProbedUid = $state<string | null>(null)
  let cardReady = $derived(
    !!invite.uid &&
      partstatLoadedUid === invite.uid &&
      cancellationProbedUid === invite.uid,
  )
  /** Set when MailView has previously seen a `METHOD:CANCEL`
   *  mail for this UID — flips the card to the cancelled
   *  flavour even when the user is viewing the original
   *  REQUEST.  Probed once on mount via `is_invite_cancelled`;
   *  null until the lookup resolves (treated as "not cancelled
   *  yet" — the card renders the regular RSVP UI in that
   *  window so we never *hide* the buttons by mistake while
   *  waiting). */
  let cancelledLater = $state(false)
  // Card flavour:
  //   - REQUEST → Accept / Tentative / Decline RSVP UI (unless
  //     post-hoc cancellation has been recorded for the UID,
  //     in which case it flips to the cancelled banner so the
  //     user can't unwittingly answer a cancelled meeting).
  //   - CANCEL  → "Remove from my calendar" affordance.
  let isCancel = $derived(
    (invite.method ?? '').toUpperCase() === 'CANCEL' || cancelledLater,
  )
  let dismissingCancel = $state(false)
  let dismissed = $state(false)
  /** Whether a calendar entry for this UID currently exists
   *  locally — drives the CANCEL flavour's affordance.  Probed
   *  on mount via `is_event_in_calendar`; null until the lookup
   *  resolves.  When the event isn't in any cached calendar
   *  (user never accepted, or already removed it), we hide the
   *  "Remove from my calendar" button and show a passive line
   *  instead — there's nothing to remove. */
  let eventInCalendar = $state<boolean | null>(null)
  $effect(() => {
    const uid = invite.uid
    // Same logic as the partstat effect: only reset on UID
    // changes, never on incidental re-fires — so the resolved
    // state stays visible while we re-probe.
    if (cancellationProbedUid !== uid) {
      eventInCalendar = null
      cancelledLater = false
    }
    if (!uid) return
    void Promise.all([
      invoke<boolean>('is_event_in_calendar', { uid }),
      invoke<boolean>('is_invite_cancelled', { uid }),
    ])
      .then(([present, cancelled]) => {
        if (invite.uid !== uid) return
        eventInCalendar = present
        cancelledLater = cancelled
      })
      .catch((e) => {
        console.warn('cancellation/in-calendar probe failed', e)
      })
      .finally(() => {
        if (invite.uid === uid) cancellationProbedUid = uid
      })
  })
  async function dismissCancelledEvent() {
    if (dismissingCancel || dismissed) return
    error = ''
    dismissingCancel = true
    try {
      await invoke('dismiss_cancelled_event', { uid: invite.uid })
      dismissed = true
    } catch (e) {
      error = formatError(e) || 'Failed to remove the cancelled event'
    } finally {
      dismissingCancel = false
    }
  }

  // Re-hydrate the post-reply state whenever the invite changes
  // (the user clicked through to a different mail).  Source of
  // truth = the user's ATTENDEE PARTSTAT on the cached calendar
  // event (synced from CalDAV), which captures changes made via
  // NC web UI / phone / any other client too.  Falls back to the
  // local `rsvp_responses` table when the event isn't in the
  // calendar cache yet (e.g. first-time render before the
  // background sync completes).  Keyed by UID since a single
  // invite can show up in multiple folders / accounts and
  // should agree everywhere.
  // Hydrate the post-reply state whenever the open invite
  // changes.  Two sources of truth, in priority order:
  //   1. The cached calendar event's user-PARTSTAT (after a
  //      CalDAV delta sync).  Authoritative server-side state
  //      — captures changes made via NC web UI / phone / any
  //      other CalDAV client.
  //   2. The local `rsvp_responses` table — last-resort
  //      fallback used only when (1) returns null (the event
  //      isn't on any calendar any more — common after a CANCEL
  //      flow or after the user manually removed the event).
  //      Without this, the card would forget the user's
  //      previous response the moment the calendar entry is
  //      gone.  We only trust it when the cached event is
  //      genuinely missing — never when it's present with a
  //      different PARTSTAT — so stale rsvp_responses entries
  //      don't override authoritative server state.
  // Render is NOT gated on either lookup resolving:
  // `respondedAs` starts null so fresh Accept/Decline buttons
  // render immediately and only get replaced when a valid
  // prior PARTSTAT comes back.
  $effect(() => {
    const uid = invite.uid
    // Only reset the post-reply state when the user actually
    // navigated to a different invite — not on incidental
    // reactive re-fires (e.g. `attendeeHint` arriving later).
    // That keeps the resolved state visible while a background
    // re-query runs, and prevents the "fresh buttons flash and
    // snap" the user complained about.
    if (partstatLoadedUid !== uid) {
      respondedAs = null
      error = ''
    }
    if (!uid) return
    void (async () => {
      try {
        const valid = (s: string | null): s is Partstat =>
          s === 'ACCEPTED' || s === 'DECLINED' || s === 'TENTATIVE'
        const partstat = await invoke<string | null>(
          'get_event_partstat_for_user',
          { uid, attendeeHint: attendeeHint },
        )
        if (invite.uid !== uid) return
        if (valid(partstat)) {
          respondedAs = partstat
        } else {
          // No PARTSTAT on a calendar event — try the local
          // persistence table.
          const local = await invoke<string | null>('get_rsvp_response', { uid })
          if (invite.uid !== uid) return
          respondedAs = valid(local) ? local : null
        }
      } catch (e) {
        console.warn('partstat hydration failed', e)
      } finally {
        if (invite.uid === uid) partstatLoadedUid = uid
      }
    })()
  })

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
    // Multi-day events: build the format from explicit fields,
    // not `dateStyle` — `Intl.DateTimeFormat` rejects mixing
    // `dateStyle`/`timeStyle` with field-level options like
    // `hour`/`minute` (`TypeError`).  Fields-only is the safe
    // shape and matches the same-day branch's resolution.
    const dateTimeFmt: Intl.DateTimeFormatOptions = {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    }
    return sameDay
      ? `${dateStr} · ${start.toLocaleTimeString(undefined, timeFmt)} – ${end.toLocaleTimeString(undefined, timeFmt)}`
      : `${start.toLocaleString(undefined, dateTimeFmt)} – ${end.toLocaleString(undefined, dateTimeFmt)}`
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
    // doesn't double-PUT.
    if (busy) return
    if (respondedAs === partstat) return
    if (!selectedCalendarId) {
      error = 'Pick a calendar before responding.'
      return
    }
    error = ''
    busy = partstat
    try {
      // The backend writes the user's PARTSTAT into the chosen
      // calendar via CalDAV.  Nextcloud's iMIP plugin (NC 30+
      // Mail Provider) handles the REPLY mail to the organiser
      // automatically — the client never touches SMTP for RSVPs.
      // For DECLINED, the backend PUT-then-DELETEs so the
      // organiser is notified but the entry doesn't clutter the
      // user's calendar.
      await invoke('respond_to_invite', {
        calendarId: selectedCalendarId,
        rawIcs: invite.rawIcs,
        partstat,
        attendeeHint,
      })
      respondedAs = partstat
      onresponded?.(partstat)
    } catch (e) {
      error = formatError(e) || 'Failed to record RSVP'
    } finally {
      busy = null
    }
  }
</script>

{#if cardReady}
<div class="rounded-md p-4 mb-3 text-sm
            {isCancel
              ? 'border border-red-500/40 bg-red-500/5'
              : 'border border-primary-500/40 bg-primary-500/5'}">
  <div class="flex items-start justify-between gap-3 mb-2">
    <div class="flex items-center gap-2">
      <span class="text-lg">{isCancel ? '🚫' : '📅'}</span>
      <span class="font-semibold {isCancel ? 'line-through' : ''}">
        {invite.summary || '(untitled meeting)'}
      </span>
      {#if isCancel}
        <span class="text-[10px] uppercase tracking-wide font-semibold px-1.5 py-px rounded bg-red-500 text-white">
          Cancelled
        </span>
      {/if}
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

  <!-- Calendar picker.  Defaults to the user's "default
       calendar" app setting; falls back to the first non-hidden
       calendar across all connected Nextcloud accounts.  Hidden
       once the user has answered (the event is now in the
       chosen calendar — moving it elsewhere would need a
       separate UI), and entirely suppressed for CANCEL flavour
       (no calendar is being added to). -->
  {#if !isCancel && !respondedAs && calendars.length > 1}
    <div class="flex items-center gap-2 mt-3 text-xs">
      <label class="text-surface-500" for="rsvp-calendar">Add to</label>
      <select
        id="rsvp-calendar"
        class="select px-2 py-1 text-xs rounded-md flex-1 max-w-[260px]"
        bind:value={selectedCalendarId}
        disabled={calendarsLoading}
      >
        {#each calendars as c (c.id)}
          <option value={c.id}>{c.display_name}</option>
        {/each}
      </select>
    </div>
  {/if}

  {#if error}
    <p class="text-xs text-red-500 mt-2">{error}</p>
  {/if}

  {#if isCancel}
    <!-- CANCEL flavour.  The organiser dropped the meeting; we
         offer a single button to remove the local copy.  Once
         dismissed (or if the event isn't in the user's calendar
         to begin with), the button collapses to a confirmation
         line so the card communicates "you're done here". -->
    <p class="text-sm text-surface-700 dark:text-surface-300 mt-3">
      The organiser cancelled this meeting.
    </p>
    <div class="flex flex-wrap items-center gap-2 mt-2">
      {#if dismissed}
        <span class="text-xs text-surface-500 italic">
          ✓ Removed from your calendar
        </span>
      {:else if eventInCalendar === false}
        <!-- The meeting isn't on any of the user's calendars
             — nothing to remove.  Show a passive line so the
             card still communicates state without surfacing a
             button that would no-op. -->
        <span class="text-xs text-surface-500 italic">
          Not in your calendar — nothing to remove.
        </span>
      {:else}
        <button
          class="btn btn-sm preset-filled-error-500 disabled:opacity-50"
          disabled={dismissingCancel || eventInCalendar === null}
          onclick={() => void dismissCancelledEvent()}
        >
          {dismissingCancel ? 'Removing…' : '🗑 Remove from my calendar'}
        </button>
      {/if}
    </div>
  {:else if respondedAs}
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
{/if}
