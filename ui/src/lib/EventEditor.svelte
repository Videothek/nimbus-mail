<script lang="ts">
  /**
   * EventEditor — modal for creating, editing, and deleting calendar
   * events.
   *
   * Mirrors the Compose modal pattern: full-screen dim overlay, a
   * fixed-size centered card with a sticky header / scrolling body /
   * sticky footer. The parent (`CalendarView`) opens it in one of two
   * shapes:
   *
   *   - **create**: pass `mode="create"` plus a `draft` (calendar id +
   *     start/end seeded from a `+ New event` click or a click-and-drag
   *     gesture in the time grid). The editor PUTs via
   *     `create_calendar_event`.
   *   - **edit**: pass `mode="edit"` plus the existing `event` and the
   *     calendar list (so we can show the calendar name in read-only
   *     form — moving an event between calendars is a follow-up). The
   *     editor PUTs via `update_calendar_event` and offers a Delete
   *     button (`delete_calendar_event`).
   *
   * On a successful save or delete we call `onsaved()` so the parent
   * can re-query the cache and repaint the grid; the modal closes
   * itself via `onclose()` either way.
   *
   * # Field mapping
   *
   * The form field shapes match the Rust `CalendarEventInput` struct
   * (camelCase via serde): `summary`, `description`, `location`,
   * `start`, `end`, `allDay`, `url`, `transparency`, `attendees`,
   * `reminders`. The attendee list is edited as a comma-separated
   * email string — power users can paste a list, and the parsed shape
   * round-trips through `CalendarEvent.attendees`.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  // ── Types (kept local; these mirror the Rust models) ──────────
  interface EventAttendee {
    email: string
    common_name?: string | null
    status?: string | null
  }
  interface EventReminder {
    trigger_minutes_before: number
    action?: string | null
  }
  interface CalendarEvent {
    id: string
    summary: string
    description: string | null
    start: string
    end: string
    location: string | null
    rrule: string | null
    rdate: string[]
    exdate: string[]
    recurrence_id: string | null
    url?: string | null
    transparency?: string | null
    attendees?: EventAttendee[]
    reminders?: EventReminder[]
  }
  interface CalendarSummary {
    id: string
    nextcloud_account_id: string
    display_name: string
    color: string | null
    last_synced_at: string | null
  }

  type Mode = 'create' | 'edit'

  interface Props {
    mode: Mode
    calendars: CalendarSummary[]
    /** create-mode seed: which calendar + which start/end to prefill. */
    draft?: { calendarId: string; start: Date; end: Date; allDay?: boolean } | null
    /** edit-mode subject: the existing event being edited. */
    event?: CalendarEvent | null
    onclose: () => void
    onsaved: () => void
  }
  const { mode, calendars, draft, event, onclose, onsaved }: Props = $props()

  // ── Form state ──────────────────────────────────────────────
  // Initial values are computed once at mount from `draft` (create) or
  // `event` (edit). After that the form is fully owned by these
  // `$state` cells, so further changes to the prop don't clobber the
  // user's typing.
  // svelte-ignore state_referenced_locally
  let summary = $state(event?.summary ?? '')
  // svelte-ignore state_referenced_locally
  let description = $state(event?.description ?? '')
  // svelte-ignore state_referenced_locally
  let location = $state(event?.location ?? '')
  // svelte-ignore state_referenced_locally
  let url = $state(event?.url ?? '')
  // svelte-ignore state_referenced_locally
  let transparency = $state(event?.transparency ?? 'OPAQUE')

  // Determine the starting calendar id. In edit-mode we derive it from
  // the event id (`{nc_id}::{cal_path}::…`). In create-mode the parent
  // hands us one explicitly.
  // svelte-ignore state_referenced_locally
  let calendarId = $state(deriveInitialCalendarId())
  function deriveInitialCalendarId(): string {
    if (event) {
      const parts = event.id.split('::')
      return parts.slice(0, 2).join('::')
    }
    return draft?.calendarId ?? calendars[0]?.id ?? ''
  }

  // svelte-ignore state_referenced_locally
  const initialAllDay = inferAllDay()
  // svelte-ignore state_referenced_locally
  let allDay = $state(initialAllDay)

  // datetime-local inputs work in the user's local timezone — the
  // value shape is `YYYY-MM-DDTHH:MM` with no offset. We seed from the
  // event/draft (which carry UTC instants) and convert back to UTC at
  // save time. For all-day events we keep the date-only state in
  // `startDate` / `endDate` and let the save path fold them into
  // 00:00:00Z / 23:59:59Z.
  // svelte-ignore state_referenced_locally
  let startLocal = $state(toLocalInput(initialStart()))
  // svelte-ignore state_referenced_locally
  let endLocal = $state(toLocalInput(initialEnd()))
  // svelte-ignore state_referenced_locally
  let startDate = $state(toDateInput(initialStart()))
  // svelte-ignore state_referenced_locally
  let endDate = $state(toDateInput(initialEnd()))

  // Attendees are edited as a comma-separated email string. We parse
  // them into the structured shape on save. Existing CN / PARTSTAT data
  // round-trips opaquely (we hold it in `originalAttendees` and merge
  // back by email at save time).
  // svelte-ignore state_referenced_locally
  let attendeesText = $state(
    (event?.attendees ?? []).map((a) => a.email).join(', '),
  )
  // svelte-ignore state_referenced_locally
  const originalAttendees = event?.attendees ?? []

  // Reminder picker: a single dropdown that maps to the most common
  // VALARM offsets. "Custom" preserves whatever multi-alarm setup the
  // event came in with (we keep `originalReminders` for that case).
  type ReminderChoice = 'none' | '5' | '15' | '30' | '60' | '1440' | 'custom'
  // svelte-ignore state_referenced_locally
  let reminderChoice = $state<ReminderChoice>(deriveReminderChoice())
  // svelte-ignore state_referenced_locally
  const originalReminders = event?.reminders ?? []
  function deriveReminderChoice(): ReminderChoice {
    const list = event?.reminders ?? []
    if (list.length === 0) return 'none'
    if (list.length > 1) return 'custom'
    const m = list[0].trigger_minutes_before
    if (m === 5 || m === 15 || m === 30 || m === 60 || m === 1440) {
      return String(m) as ReminderChoice
    }
    return 'custom'
  }

  let saving = $state(false)
  let deleting = $state(false)
  let error = $state('')

  // ── Initial conversions ─────────────────────────────────────
  function initialStart(): Date {
    if (event) return new Date(event.start)
    if (draft) return draft.start
    const now = new Date()
    now.setMinutes(0, 0, 0)
    return now
  }
  function initialEnd(): Date {
    if (event) return new Date(event.end)
    if (draft) return draft.end
    const out = initialStart()
    out.setHours(out.getHours() + 1)
    return out
  }
  function inferAllDay(): boolean {
    if (draft?.allDay) return true
    if (!event) return false
    // Same heuristic as CalendarView.isAllDay — UTC midnight start,
    // span ≈ N×24h.
    const s = new Date(event.start)
    const e = new Date(event.end)
    if (s.getUTCHours() !== 0 || s.getUTCMinutes() !== 0) return false
    const hours = (e.getTime() - s.getTime()) / 3_600_000
    if (hours < 23) return false
    const remainder = hours % 24
    return remainder < 1 / 60 || remainder > 24 - 1 / 60
  }

  // ── datetime / date helpers ─────────────────────────────────
  /** `Date` → `YYYY-MM-DDTHH:MM` in the user's local zone. */
  function toLocalInput(d: Date): string {
    const pad = (n: number) => String(n).padStart(2, '0')
    return (
      `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}` +
      `T${pad(d.getHours())}:${pad(d.getMinutes())}`
    )
  }
  /** `Date` → `YYYY-MM-DD` in UTC (matches the all-day storage shape). */
  function toDateInput(d: Date): string {
    const pad = (n: number) => String(n).padStart(2, '0')
    return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`
  }
  /** `YYYY-MM-DDTHH:MM` (local) → ISO string in UTC. */
  function fromLocalInput(s: string): Date {
    // The browser parses `T`-separated strings without an offset as
    // local time, so this is the inverse of `toLocalInput`.
    return new Date(s)
  }
  /** `YYYY-MM-DD` (treated as a UTC calendar date) → midnight UTC. */
  function dateInputToUtcMidnight(s: string): Date {
    return new Date(`${s}T00:00:00Z`)
  }
  /** `YYYY-MM-DD` → 23:59:59.999 UTC of that date. */
  function dateInputToUtcEndOfDay(s: string): Date {
    return new Date(`${s}T23:59:59.999Z`)
  }

  // When the user toggles all-day, keep the visible inputs sane: copy
  // the timed value over to the date-only field on the way in, and
  // restore a sensible 1-hour timed window on the way out.
  function onToggleAllDay() {
    if (allDay) {
      const s = fromLocalInput(startLocal)
      const e = fromLocalInput(endLocal)
      startDate = toDateInput(s)
      endDate = toDateInput(e)
    } else {
      const s = dateInputToUtcMidnight(startDate)
      const out = new Date(s)
      out.setHours(9, 0, 0, 0)
      const end = new Date(out)
      end.setHours(end.getHours() + 1)
      startLocal = toLocalInput(out)
      endLocal = toLocalInput(end)
    }
  }

  // ── Save / delete ───────────────────────────────────────────
  function buildAttendees(): EventAttendee[] {
    const seen = new Map<string, EventAttendee>()
    for (const a of originalAttendees) seen.set(a.email.toLowerCase(), a)
    const out: EventAttendee[] = []
    for (const piece of attendeesText.split(',')) {
      const email = piece.trim()
      if (!email) continue
      const prior = seen.get(email.toLowerCase())
      out.push(
        prior
          ? { email, common_name: prior.common_name ?? null, status: prior.status ?? null }
          : { email, common_name: null, status: null },
      )
    }
    return out
  }

  function buildReminders(): EventReminder[] {
    if (reminderChoice === 'none') return []
    if (reminderChoice === 'custom') return originalReminders
    return [
      {
        trigger_minutes_before: parseInt(reminderChoice, 10),
        action: 'DISPLAY',
      },
    ]
  }

  function buildInput() {
    const start = allDay
      ? dateInputToUtcMidnight(startDate)
      : fromLocalInput(startLocal)
    const end = allDay
      ? dateInputToUtcEndOfDay(endDate)
      : fromLocalInput(endLocal)
    return {
      summary: summary.trim(),
      description: description.trim() ? description.trim() : null,
      location: location.trim() ? location.trim() : null,
      start: start.toISOString(),
      end: end.toISOString(),
      allDay,
      url: url.trim() ? url.trim() : null,
      transparency: transparency || null,
      attendees: buildAttendees(),
      reminders: buildReminders(),
    }
  }

  async function save() {
    error = ''
    if (!summary.trim()) {
      error = 'Title is required'
      return
    }
    if (!calendarId) {
      error = 'Pick a calendar'
      return
    }
    const input = buildInput()
    // Reject inverted ranges before bothering the backend.
    if (new Date(input.end).getTime() <= new Date(input.start).getTime()) {
      error = 'End must be after start'
      return
    }
    saving = true
    try {
      if (mode === 'create') {
        await invoke('create_calendar_event', { calendarId, input })
      } else if (event) {
        await invoke('update_calendar_event', { eventId: event.id, input })
      }
      onsaved()
      onclose()
    } catch (e) {
      error = formatError(e) || 'Failed to save event'
    } finally {
      saving = false
    }
  }

  async function remove() {
    if (mode !== 'edit' || !event) return
    if (!confirm(`Delete "${event.summary || '(no title)'}"?`)) return
    deleting = true
    error = ''
    try {
      await invoke('delete_calendar_event', { eventId: event.id })
      onsaved()
      onclose()
    } catch (e) {
      error = formatError(e) || 'Failed to delete event'
    } finally {
      deleting = false
    }
  }

  function currentCalendarLabel(): string {
    return calendars.find((c) => c.id === calendarId)?.display_name ?? '(unknown)'
  }
</script>

<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50" role="dialog" aria-modal="true">
  <div class="w-[640px] max-h-[90vh] bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl flex flex-col">
    <header class="px-5 py-3 border-b border-surface-200 dark:border-surface-700 flex items-center justify-between">
      <h2 class="text-base font-semibold">
        {mode === 'create' ? 'New event' : 'Edit event'}
      </h2>
      <button class="text-surface-500 hover:text-surface-900 dark:hover:text-surface-100" onclick={onclose} aria-label="Close">✕</button>
    </header>

    <div class="flex-1 overflow-y-auto p-5 space-y-3">
      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-summary">Title</label>
        <input
          id="event-summary"
          class="input flex-1 px-3 py-2 text-sm rounded-md"
          bind:value={summary}
          placeholder="Event title"
        />
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-calendar">Calendar</label>
        {#if mode === 'create'}
          <select
            id="event-calendar"
            class="select flex-1 px-3 py-2 text-sm rounded-md"
            bind:value={calendarId}
          >
            {#each calendars as c (c.id)}
              <option value={c.id}>{c.display_name}</option>
            {/each}
          </select>
        {:else}
          <span class="flex-1 px-3 py-2 text-sm text-surface-600 dark:text-surface-300">
            {currentCalendarLabel()}
          </span>
        {/if}
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-allday">All day</label>
        <input
          id="event-allday"
          type="checkbox"
          class="checkbox"
          bind:checked={allDay}
          onchange={onToggleAllDay}
        />
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-start">Starts</label>
        {#if allDay}
          <input
            id="event-start"
            type="date"
            class="input flex-1 px-3 py-2 text-sm rounded-md"
            bind:value={startDate}
          />
        {:else}
          <input
            id="event-start"
            type="datetime-local"
            class="input flex-1 px-3 py-2 text-sm rounded-md"
            bind:value={startLocal}
          />
        {/if}
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-end">Ends</label>
        {#if allDay}
          <input
            id="event-end"
            type="date"
            class="input flex-1 px-3 py-2 text-sm rounded-md"
            bind:value={endDate}
          />
        {:else}
          <input
            id="event-end"
            type="datetime-local"
            class="input flex-1 px-3 py-2 text-sm rounded-md"
            bind:value={endLocal}
          />
        {/if}
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-location">Location</label>
        <input
          id="event-location"
          class="input flex-1 px-3 py-2 text-sm rounded-md"
          bind:value={location}
          placeholder="Address, room, link…"
        />
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-url">URL</label>
        <input
          id="event-url"
          class="input flex-1 px-3 py-2 text-sm rounded-md"
          bind:value={url}
          placeholder="Meeting link, agenda doc, …"
        />
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-transp">Show as</label>
        <select
          id="event-transp"
          class="select flex-1 px-3 py-2 text-sm rounded-md"
          bind:value={transparency}
        >
          <option value="OPAQUE">Busy</option>
          <option value="TRANSPARENT">Free</option>
        </select>
      </div>

      <div class="flex items-center gap-2">
        <label class="text-xs w-20 text-surface-500" for="event-reminder">Reminder</label>
        <select
          id="event-reminder"
          class="select flex-1 px-3 py-2 text-sm rounded-md"
          bind:value={reminderChoice}
        >
          <option value="none">None</option>
          <option value="5">5 minutes before</option>
          <option value="15">15 minutes before</option>
          <option value="30">30 minutes before</option>
          <option value="60">1 hour before</option>
          <option value="1440">1 day before</option>
          {#if reminderChoice === 'custom'}
            <option value="custom">Custom (preserved from server)</option>
          {/if}
        </select>
      </div>

      <div class="flex items-start gap-2">
        <label class="text-xs w-20 text-surface-500 pt-2" for="event-attendees">Attendees</label>
        <input
          id="event-attendees"
          class="input flex-1 px-3 py-2 text-sm rounded-md"
          bind:value={attendeesText}
          placeholder="alice@example.com, bob@example.com"
        />
      </div>

      <div class="flex items-start gap-2">
        <label class="text-xs w-20 text-surface-500 pt-2" for="event-description">Notes</label>
        <textarea
          id="event-description"
          class="textarea flex-1 px-3 py-2 text-sm rounded-md min-h-[120px]"
          bind:value={description}
          placeholder="Description, agenda, notes…"
        ></textarea>
      </div>

      {#if error}
        <p class="text-sm text-red-500">{error}</p>
      {/if}
    </div>

    <footer class="px-5 py-3 border-t border-surface-200 dark:border-surface-700 flex items-center gap-2">
      <button class="btn preset-filled-primary-500" disabled={saving || deleting} onclick={save}>
        {saving ? 'Saving…' : mode === 'create' ? 'Create' : 'Save'}
      </button>
      {#if mode === 'edit'}
        <button class="btn preset-outlined-error-500" disabled={saving || deleting} onclick={remove}>
          {deleting ? 'Deleting…' : 'Delete'}
        </button>
      {/if}
      <div class="flex-1"></div>
      <button class="btn preset-outlined-surface-500" disabled={saving || deleting} onclick={onclose}>
        Cancel
      </button>
    </footer>
  </div>
</div>
