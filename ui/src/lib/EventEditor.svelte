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

  import { convertFileSrc, invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  // ── Types (kept local; these mirror the Rust models) ──────────
  interface EventAttendee {
    email: string
    common_name?: string | null
    status?: string | null
    /** RFC 5545 ROLE: REQ-PARTICIPANT (Required) /
     *  OPT-PARTICIPANT (Optional) / CHAIR / NON-PARTICIPANT.
     *  Missing => Required. */
    role?: string | null
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

  /** Payload `onsaved` carries back after a successful create. Edit /
      delete still fire `onsaved()` with no argument — callers that
      don't care about the payload keep their `() => void` handler
      unchanged. */
  export interface SavedEvent {
    /** App-side composite event id returned by `create_calendar_event`. */
    uid: string
    summary: string
    start: string
    end: string
    url: string | null
    /** Bare-address list of everyone invited.  Kept on the payload
        so callers can refresh related UI (badges, lists). */
    attendees: string[]
  }

  interface Props {
    mode: Mode
    calendars: CalendarSummary[]
    /**
     * create-mode seed. Always carries the calendar id + start/end
     * (from a `+ New event` click or a click-and-drag on the grid).
     * The remaining content fields are optional prefills used by
     * callers that want to open the editor with more than just a
     * time slot — e.g. Compose's "Add event" action seeds summary
     * (from the email subject), attendees (from To/Cc), and url
     * (from a freshly created Talk room).
     */
    draft?: {
      calendarId: string
      start: Date
      end: Date
      allDay?: boolean
      summary?: string
      description?: string
      location?: string
      url?: string
      /** Each entry is a bare address or `"Name" <addr>` — same
          shape `parseAddress` accepts everywhere else. */
      attendees?: string[]
    } | null
    /** edit-mode subject: the existing event being edited. */
    event?: CalendarEvent | null
    onclose: () => void
    onsaved: (saved?: SavedEvent) => void
  }
  const {
    mode,
    calendars,
    draft,
    event,
    onclose,
    onsaved,
  }: Props = $props()

  // ── Form state ──────────────────────────────────────────────
  // Initial values are computed once at mount from `draft` (create) or
  // `event` (edit). After that the form is fully owned by these
  // `$state` cells, so further changes to the prop don't clobber the
  // user's typing.
  // svelte-ignore state_referenced_locally
  let summary = $state(event?.summary ?? draft?.summary ?? '')
  // svelte-ignore state_referenced_locally
  let description = $state(event?.description ?? draft?.description ?? '')
  // svelte-ignore state_referenced_locally
  let location = $state(event?.location ?? draft?.location ?? '')
  // svelte-ignore state_referenced_locally
  let transparency = $state(event?.transparency ?? 'OPAQUE')

  // Determine the starting calendar id. In edit-mode we derive it from
  // the event id (`{nc_id}::{cal_path}::…`). In create-mode the parent
  // hands us one explicitly; we override that with the user's
  // `default_calendar_id` setting if one is configured AND it points
  // at a calendar the editor knows about.
  // svelte-ignore state_referenced_locally
  let calendarId = $state(deriveInitialCalendarId())
  function deriveInitialCalendarId(): string {
    if (event) {
      const parts = event.id.split('::')
      return parts.slice(0, 2).join('::')
    }
    return draft?.calendarId ?? calendars[0]?.id ?? ''
  }

  // Async-load the default-calendar setting and switch to it if the
  // user hasn't manually changed the picker yet.  In create-mode
  // this is the single biggest UX nicety — Nick's "primary"
  // calendar (NC default) gets used reliably instead of whichever
  // calendar happened to come back first from the cache.
  $effect(() => {
    if (mode !== 'create') return
    void invoke<{ default_calendar_id: string | null }>('get_app_settings')
      .then((s) => {
        const def = s.default_calendar_id
        if (def && calendars.some((c) => c.id === def)) {
          calendarId = def
        }
      })
      .catch(() => {})
  })

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

  // ── Attendees ─────────────────────────────────────────────
  // Three role-bucketed lists drive the chip-row UI; the input
  // beneath each bucket adds *new* attendees with the matching
  // ROLE.  Existing attendees from `event` are seeded into
  // whichever bucket their ROLE points at, defaulting to
  // Required when the property is missing (RFC 5545 §3.2.18).
  // Each seed preserves its `common_name` / `status` (PARTSTAT)
  // so a save round-trip doesn't reset accepted/declined badges.
  type Role = 'REQ-PARTICIPANT' | 'OPT-PARTICIPANT' | 'CHAIR'
  function bucketFor(att: EventAttendee): Role {
    const r = (att.role ?? 'REQ-PARTICIPANT').toUpperCase()
    if (r === 'OPT-PARTICIPANT') return 'OPT-PARTICIPANT'
    if (r === 'CHAIR') return 'CHAIR'
    return 'REQ-PARTICIPANT'
  }
  // svelte-ignore state_referenced_locally
  let requiredAttendees = $state<EventAttendee[]>(
    event ? (event.attendees ?? []).filter((a) => bucketFor(a) === 'REQ-PARTICIPANT')
          : (draft?.attendees ?? []).map((s) => parseAddressToAttendee(s, 'REQ-PARTICIPANT')).filter((a): a is EventAttendee => !!a),
  )
  // svelte-ignore state_referenced_locally
  let optionalAttendees = $state<EventAttendee[]>(
    event ? (event.attendees ?? []).filter((a) => bucketFor(a) === 'OPT-PARTICIPANT') : [],
  )
  // svelte-ignore state_referenced_locally
  let chairAttendees = $state<EventAttendee[]>(
    event ? (event.attendees ?? []).filter((a) => bucketFor(a) === 'CHAIR') : [],
  )

  // One pending input per role.  Each commits to its bucket on
  // Enter / comma / blur.  Datalist suggestions come from the
  // shared address book cache (loaded lazily once on mount).
  let requiredInput = $state('')
  let optionalInput = $state('')
  let chairInput = $state('')

  /** Contact row mirroring the Rust `search_contacts` payload —
   *  `id` is what the `contact-photo://` URI scheme keys off,
   *  `email[]` is the typed-and-kinded list of vCard EMAIL
   *  values, and `photo_mime` tells us whether to render the
   *  photo or fall back to an initials bubble. */
  interface ContactEmail {
    kind: string
    value: string
  }
  interface Contact {
    id: string
    display_name: string
    email: ContactEmail[]
    organization: string | null
    photo_mime: string | null
  }
  /** Cached contact list keyed by lowercase email so chips —
   *  which only know the email — can render the matching photo
   *  + display name without an extra IPC round-trip per chip. */
  let contactsByEmail = $state<Map<string, Contact>>(new Map())
  $effect(() => {
    void invoke<Contact[]>('search_contacts', { query: '', limit: 500 })
      .then((rows) => {
        const map = new Map<string, Contact>()
        for (const c of rows) {
          for (const e of c.email) {
            if (e.value) map.set(e.value.toLowerCase(), c)
          }
        }
        contactsByEmail = map
      })
      .catch(() => {})
  })

  /** Pick the first non-empty email from a Contact. */
  function primaryEmail(c: Contact): string {
    return c.email.find((e) => e.value.length > 0)?.value ?? ''
  }

  /** `<img src>` against the custom `contact-photo://` scheme.
   *  Falls back to `null` when the contact doesn't have a vCard
   *  photo so chips/dropdown render an initials bubble. */
  function photoUrl(c: Contact | undefined): string | null {
    if (!c || !c.photo_mime) return null
    return convertFileSrc(c.id, 'contact-photo')
  }

  function initials(name: string): string {
    const parts = name.trim().split(/\s+/).filter(Boolean)
    if (parts.length === 0) return '?'
    if (parts.length === 1) return parts[0][0].toUpperCase()
    return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase()
  }

  // ── Internal-vs-external resolution ──────────────────────
  // Each attendee email is looked up once against NC's
  // `sharees` endpoint; a hit means the address belongs to a
  // local NC principal, which:
  //   - flips the chip's "internal" badge on,
  //   - lets the save flow add them to the Talk room as a
  //     `users` participant (in-NC notification) rather than
  //     an `emails` guest,
  //   - drives the room-type decision (all-internal => private).
  // `null` means "external"; `undefined` means "haven't asked
  // yet" — the UI stays neutral until the lookup resolves.
  interface InternalUser {
    user_id: string
    display_name: string
  }
  let internalLookup = $state<Map<string, InternalUser | null>>(new Map())

  /** Resolve any unknown attendee emails against NC's
   *  user-search.  Best-effort: failures cache as `null`
   *  (treated as external) so the lookup doesn't retry every
   *  effect cycle.  Fires whenever the bucket lists change. */
  $effect(() => {
    const all = [...requiredAttendees, ...optionalAttendees, ...chairAttendees]
    const cal = calendars.find((c) => c.id === calendarId)
    if (!cal) return
    const ncId = cal.nextcloud_account_id
    for (const att of all) {
      const key = att.email.toLowerCase()
      if (internalLookup.has(key)) continue
      // Mark as in-flight (null sentinel — replaced when the
      // OCS reply lands).  Without this guard, a re-render
      // before the promise resolves would re-fire the lookup.
      internalLookup.set(key, null)
      void invoke<InternalUser | null>('find_nextcloud_user_by_email', {
        ncId,
        email: att.email,
      })
        .then((m) => {
          internalLookup.set(key, m ?? null)
          // Trigger reactivity: replacing the Map ref forces
          // chip rows to re-render with the new badge state.
          internalLookup = new Map(internalLookup)
        })
        .catch((e) => {
          console.warn('find_nextcloud_user_by_email failed', e)
          internalLookup.set(key, null)
        })
    }
  })

  function isInternal(email: string): boolean {
    return !!internalLookup.get(email.toLowerCase())
  }

  // ── Per-role inline suggestion dropdown ──────────────────
  // Each role's input has its own debounced `search_contacts`
  // query and its own dropdown state — same plumbing
  // `AddressAutocomplete` uses on Compose's To/Cc/Bcc, but
  // adapted to the chip-based commit model: clicking a
  // suggestion adds the contact directly to the role bucket
  // instead of stuffing its address into a comma-separated
  // string.
  let activeSuggestionRole = $state<Role | null>(null)
  let dropdownSuggestions = $state<Contact[]>([])
  let activeIndex = $state(0)
  const SEARCH_DEBOUNCE_MS = 150
  const SUGGESTION_LIMIT = 8
  let searchDebounce: number | null = null

  function runSuggestionSearch(role: Role, query: string) {
    if (query.trim().length < 2) {
      dropdownSuggestions = []
      activeSuggestionRole = null
      return
    }
    if (searchDebounce !== null) window.clearTimeout(searchDebounce)
    searchDebounce = window.setTimeout(async () => {
      try {
        const rows = await invoke<Contact[]>('search_contacts', {
          query,
          limit: SUGGESTION_LIMIT,
        })
        // Stale-response guard: only commit if this role is
        // still the focused one.
        if (activeSuggestionRole === role) {
          dropdownSuggestions = rows
          activeIndex = 0
        }
      } catch (e) {
        console.warn('search_contacts failed', e)
        dropdownSuggestions = []
      }
    }, SEARCH_DEBOUNCE_MS)
  }

  /** Add a contact directly to a role bucket via the
   *  suggestion dropdown.  Skips if the email is already in
   *  any bucket. */
  function pickSuggestion(role: Role, c: Contact) {
    const addr = primaryEmail(c)
    if (!addr) return
    const exists = [...requiredAttendees, ...optionalAttendees, ...chairAttendees].some(
      (a) => a.email.toLowerCase() === addr.toLowerCase(),
    )
    // Always clear the input + close the dropdown — even when
    // the contact's already added, so the user gets clear
    // feedback ("nothing happened, but the field reset").
    if (role === 'REQ-PARTICIPANT') requiredInput = ''
    else if (role === 'OPT-PARTICIPANT') optionalInput = ''
    else chairInput = ''
    activeSuggestionRole = null
    dropdownSuggestions = []
    if (exists) return
    const att: EventAttendee = {
      email: addr,
      common_name: c.display_name || null,
      role,
    }
    if (role === 'REQ-PARTICIPANT') {
      requiredAttendees = [...requiredAttendees, att]
    } else if (role === 'OPT-PARTICIPANT') {
      optionalAttendees = [...optionalAttendees, att]
    } else {
      chairAttendees = [...chairAttendees, att]
    }
    // Mirror into the by-email cache so the chip avatar
    // resolves immediately (don't wait for the cache reload).
    contactsByEmail.set(addr.toLowerCase(), c)
    contactsByEmail = new Map(contactsByEmail)
  }

  /** Parse a single piece into an attendee with the given role.
   *  Accepts `"Name" <addr>`, `Name <addr>`, or a bare `addr`.
   *  Returns null for empty / malformed pieces. */
  function parseAddressToAttendee(
    piece: string,
    role: Role,
  ): EventAttendee | null {
    const trimmed = piece.trim()
    if (!trimmed) return null
    const m = trimmed.match(/^\s*(?:"([^"]*)"|([^<]*?))\s*<([^>]+)>\s*$/)
    if (m) {
      const name = (m[1] ?? m[2] ?? '').trim().replace(/\\"/g, '"')
      return {
        email: m[3].trim(),
        common_name: name || null,
        role,
      }
    }
    // Bare email — also accept lookups against the cached
    // contacts so a user who types just the display name and
    // then commits (Enter / comma / blur) without using the
    // dropdown still picks up the matching email automatically.
    for (const c of contactsByEmail.values()) {
      if ((c.display_name ?? '').toLowerCase() === trimmed.toLowerCase()) {
        return { email: primaryEmail(c), common_name: c.display_name, role }
      }
    }
    return { email: trimmed, common_name: null, role }
  }

  /** Add a comma- / semicolon-separated batch of pieces from
   *  one of the three role inputs.  Splits on `,` / `;`, dedupes
   *  by lowercase email across *all* buckets so the same
   *  address can't be both Required and Optional. */
  function commitInput(role: Role) {
    const text =
      role === 'REQ-PARTICIPANT'
        ? requiredInput
        : role === 'OPT-PARTICIPANT'
          ? optionalInput
          : chairInput
    if (!text.trim()) return
    const seen = new Set(
      [...requiredAttendees, ...optionalAttendees, ...chairAttendees].map((a) =>
        a.email.toLowerCase(),
      ),
    )
    const adds: EventAttendee[] = []
    for (const piece of text.split(/[,;]/)) {
      const att = parseAddressToAttendee(piece, role)
      if (!att) continue
      const key = att.email.toLowerCase()
      if (seen.has(key)) continue
      seen.add(key)
      adds.push(att)
    }
    if (adds.length === 0) {
      // Field had only invalid input — clear it so the user can
      // try again.
      if (role === 'REQ-PARTICIPANT') requiredInput = ''
      else if (role === 'OPT-PARTICIPANT') optionalInput = ''
      else chairInput = ''
      return
    }
    if (role === 'REQ-PARTICIPANT') {
      requiredAttendees = [...requiredAttendees, ...adds]
      requiredInput = ''
    } else if (role === 'OPT-PARTICIPANT') {
      optionalAttendees = [...optionalAttendees, ...adds]
      optionalInput = ''
    } else {
      chairAttendees = [...chairAttendees, ...adds]
      chairInput = ''
    }
  }

  function removeAttendee(role: Role, email: string) {
    if (role === 'REQ-PARTICIPANT') {
      requiredAttendees = requiredAttendees.filter((a) => a.email !== email)
    } else if (role === 'OPT-PARTICIPANT') {
      optionalAttendees = optionalAttendees.filter((a) => a.email !== email)
    } else {
      chairAttendees = chairAttendees.filter((a) => a.email !== email)
    }
  }

  /** Render an attendee for the chip label — prefers display
   *  name when present, otherwise the email. */
  function chipLabel(a: EventAttendee): string {
    if (a.common_name && a.common_name.trim() && a.common_name !== a.email) {
      return a.common_name
    }
    return a.email
  }

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
  /** Flatten the three role-bucketed attendee lists into the
   *  single array the backend expects.  Each bucket already
   *  carries the right `role`; PARTSTAT (`status`) is preserved
   *  on existing rows because the buckets were seeded from the
   *  inbound event verbatim.  Any pending text in the inputs is
   *  flushed first so an unsaved last word doesn't get dropped. */
  function buildAttendees(): EventAttendee[] {
    if (requiredInput.trim()) commitInput('REQ-PARTICIPANT')
    if (optionalInput.trim()) commitInput('OPT-PARTICIPANT')
    if (chairInput.trim()) commitInput('CHAIR')
    return [...requiredAttendees, ...optionalAttendees, ...chairAttendees]
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

  // ── Talk meeting button ───────────────────────────────────
  // Mints a fresh Nextcloud Talk room on the calendar's parent
  // Nextcloud account, then writes the room URL into the
  // event's LOCATION field — same destination Nextcloud
  // Calendar's own "Make it a Talk conversation" button uses
  // (`AddTalkModal.vue` $emits `updateLocation`).  Putting the
  // URL on LOCATION instead of URL is what triggers NC's
  // Calendar UI to render the "Join Talk conversation"
  // affordance, and what makes NC's iMIP template surface the
  // call link in the invite mail's body.
  //
  // Falls back to appending the URL to DESCRIPTION when
  // LOCATION is already set, again matching NC's modal
  // (lines 247-257 in AddTalkModal.vue).  Best-effort: a
  // failure surfaces in the local `error` banner but doesn't
  // block saving the event.
  /** Sync the editor's attendee list with the Talk room
   *  created from this editor session.  For each attendee we
   *  resolve internal-vs-external (using the cached lookup
   *  from `internalLookup`, falling back to a fresh OCS query
   *  for any address whose lookup hasn't landed yet) and POST
   *  to Talk's `participants` endpoint with `users` or
   *  `emails` source accordingly:
   *    - `users` → in-NC notification, the participant joins
   *      authenticated.
   *    - `emails` → Talk emails them a guest URL alongside
   *      the calendar invite NC's iMIP plugin already sends.
   *  Then, if every attendee turned out internal, downgrade
   *  the room from public to private — externals don't need
   *  the guest-URL escape hatch any more.  Best-effort: each
   *  POST that fails is logged (per-attendee) and we keep
   *  going. */
  async function syncTalkParticipants(attendees: EventAttendee[]) {
    const room = pendingTalkRoom
    if (!room) return
    const cal = calendars.find((c) => c.id === calendarId)
    if (!cal) return
    const ncId = cal.nextcloud_account_id

    // Fill any gaps in `internalLookup` synchronously here so
    // the room-type decision is based on the *full* answer set
    // — without this guard, an attendee added seconds before
    // Save would still register as undefined and we'd
    // pessimistically leave the room public.
    const lookups: Promise<void>[] = []
    for (const att of attendees) {
      const key = att.email.toLowerCase()
      if (internalLookup.has(key)) continue
      internalLookup.set(key, null)
      lookups.push(
        invoke<InternalUser | null>('find_nextcloud_user_by_email', {
          ncId,
          email: att.email,
        })
          .then((m) => {
            internalLookup.set(key, m ?? null)
          })
          .catch(() => {
            internalLookup.set(key, null)
          }),
      )
    }
    await Promise.all(lookups)

    let allInternal = attendees.length > 0
    for (const att of attendees) {
      const match = internalLookup.get(att.email.toLowerCase())
      const participant = match
        ? { kind: 'user' as const, value: match.user_id }
        : { kind: 'email' as const, value: att.email }
      if (!match) allInternal = false
      try {
        await invoke('add_talk_participant', {
          ncId,
          roomToken: room.token,
          participant,
        })
      } catch (e) {
        // Non-fatal — Talk returns 4xx when the participant is
        // already on the room (e.g. the user re-saved an event
        // they'd already invited people to), which we treat
        // as a no-op.
        console.warn('add_talk_participant failed', att.email, e)
      }
    }

    // Toggle visibility iff the desired state differs from the
    // last-known one.  An all-internal attendee set means we
    // can switch to private (the URL-only join is no longer
    // needed); any external attendee keeps it public.
    const desiredPublic = !allInternal
    if (desiredPublic !== room.isPublic) {
      try {
        await invoke('set_talk_room_public', {
          ncId,
          roomToken: room.token,
          public: desiredPublic,
        })
        pendingTalkRoom = { ...room, isPublic: desiredPublic }
      } catch (e) {
        console.warn('set_talk_room_public failed', e)
      }
    }
  }

  // Tracks the Talk room created from this editor session so
  // the save flow can post per-attendee participants and (when
  // every attendee turned out internal) downgrade the room
  // from public to private.  Cleared when the URL is no longer
  // present in LOCATION (the user manually wiped it).
  interface PendingTalkRoom {
    token: string
    web_url: string
    /** Last-known visibility we set on the server so the save
     *  flow only PATCHes when the desired state actually
     *  changed — avoids spurious round-trips on every save. */
    isPublic: boolean
  }
  // svelte-ignore state_referenced_locally
  let pendingTalkRoom = $state<PendingTalkRoom | null>(null)
  let creatingTalkRoom = $state(false)
  async function addTalkLink() {
    error = ''
    const cal = calendars.find((c) => c.id === calendarId)
    if (!cal) {
      error = 'Pick a calendar before creating a Talk room.'
      return
    }
    creatingTalkRoom = true
    try {
      // Room name = event title when present, falling back to a
      // generic label.  Talk rejects empty names and clamps long
      // ones; "Meeting" is the same default the NC Calendar app
      // uses for unnamed rooms.
      const roomName = summary.trim() || 'Meeting'
      const room = await invoke<{ token: string; web_url: string }>('create_talk_room', {
        ncId: cal.nextcloud_account_id,
        roomName,
        // No participants up-front — they're resolved + added
        // on save once the user has finished typing the
        // attendee list.
        participants: [],
        // Mirror Nextcloud Calendar's "Make it a Talk
        // conversation" button: tag the room as event-bound so
        // Talk's UI categorises it as a meeting room (filtered
        // out of "select existing conversation" lists for other
        // events).  The id is random — NC Calendar itself uses
        // md5(Date.now()), not the iCal UID, so there's no real
        // foreign-key here, just a tag.
        objectType: 'event',
        objectId: crypto.randomUUID(),
        // Public by default so externals invited to the event
        // can join via the calendar URL without an NC login.
        // The save flow downgrades to private when every
        // attendee resolves internal.
        roomType: 3,
      })
      pendingTalkRoom = { token: room.token, web_url: room.web_url, isPublic: true }
      if (!location.trim()) {
        location = room.web_url
      } else {
        // NC's modal does the same: separator + the URL appended
        // to the existing description.  Avoids clobbering any
        // location the user already typed.
        description = description.trim()
          ? `${description}\n\n${room.web_url}`
          : room.web_url
      }
    } catch (e) {
      error = `Failed to create Talk room: ${formatError(e) || e}`
    } finally {
      creatingTalkRoom = false
    }
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
      // URL field on iCalendar events isn't surfaced as a
      // first-class control any more — Talk meetings write
      // their join link into LOCATION (matching what
      // Nextcloud Calendar's "Make it a Talk conversation"
      // button does).  Pass null so the backend doesn't carry
      // a stale value forward in edit mode either.
      url: null,
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
    // ORGANIZER on the CalDAV PUT is resolved server-side from
    // the user's NC profile email (see `create_calendar_event`
    // in main.rs).  That address is what NC's Mail Provider
    // matches against the configured Mail-app account to pick
    // the SMTP for outbound iMIP — so it MUST be the user's
    // real address, not a synthetic hostname-based one.  No
    // override threading from the editor any more; the editor
    // doesn't know which mail account is "primary" and the
    // backend has authoritative information.
    try {
      if (mode === 'create') {
        const created = await invoke<{ id: string }>('create_calendar_event', {
          calendarId,
          input,
        })
        onsaved({
          uid: created.id,
          summary: input.summary,
          start: input.start,
          end: input.end,
          url: null,
          attendees: input.attendees.map((a) => a.email),
        })
      } else if (event) {
        await invoke('update_calendar_event', {
          eventId: event.id,
          input,
        })
        onsaved()
      }

      // Sync Talk participants + room visibility once the
      // CalDAV save has stuck.  Only fires when the user
      // created a Talk room from this editor session and
      // there's at least one attendee — pure "Talk-but-no-
      // attendees" rooms behave like personal scratch rooms
      // and don't need any of this.
      if (pendingTalkRoom && input.attendees.length > 0) {
        await syncTalkParticipants(input.attendees)
      }

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
    <header class="px-5 py-3 border-b border-surface-200 dark:border-surface-700 flex items-center justify-between gap-3">
      <h2 class="text-base font-semibold shrink-0">
        {mode === 'create' ? 'New event' : 'Edit event'}
      </h2>
      <div class="flex items-center gap-2">
        <!-- Talk meeting shortcut.  Mints a fresh Nextcloud Talk
             room on the calendar's parent NC account and writes
             its URL into the event's LOCATION field — same path
             Nextcloud Calendar's "Make it a Talk conversation"
             button uses, which is what NC's Calendar UI keys off
             to render the "Join Talk" affordance.  Disabled
             before a calendar is picked. -->
        <button
          type="button"
          class="btn btn-sm preset-outlined-primary-500 whitespace-nowrap"
          disabled={creatingTalkRoom || !calendarId}
          title="Create a Nextcloud Talk room and use its link as the location"
          onclick={() => void addTalkLink()}
        >
          {creatingTalkRoom ? 'Creating…' : '💬 Talk meeting'}
        </button>
        <button
          class="text-surface-500 hover:text-surface-900 dark:hover:text-surface-100"
          onclick={onclose}
          aria-label="Close"
        >✕</button>
      </div>
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

      <!-- Per-role attendee section.  Each role gets its own row:
           a chip list of already-added participants (large chips
           with the CardDAV photo when known, and a × remove
           button) above an input that adds new ones.  Each input
           drives its own debounced `search_contacts` dropdown —
           same plumbing AddressAutocomplete uses on Compose,
           adapted so a click adds the contact straight into the
           bucket instead of into a comma-separated string. -->

      {#snippet attendeeRow(label: string, role: Role, list: EventAttendee[], placeholder: string)}
        <div class="flex items-start gap-2">
          <div class="text-xs w-20 text-surface-500 pt-2">{label}</div>
          <div class="flex-1 min-w-0">
            {#if list.length > 0}
              <div class="flex flex-wrap gap-2 mb-2">
                {#each list as a (a.email)}
                  {@const c = contactsByEmail.get(a.email.toLowerCase())}
                  {@const photo = photoUrl(c)}
                  <span class="inline-flex items-center gap-2 pl-1 pr-2 py-1 rounded-full text-sm bg-surface-200 dark:bg-surface-700 max-w-full">
                    {#if photo}
                      <img
                        src={photo}
                        alt=""
                        loading="lazy"
                        class="w-7 h-7 rounded-full object-cover flex-shrink-0"
                      />
                    {:else}
                      <div class="w-7 h-7 rounded-full bg-surface-300 dark:bg-surface-600 flex items-center justify-center text-[11px] font-semibold flex-shrink-0">
                        {initials(chipLabel(a))}
                      </div>
                    {/if}
                    <span class="flex flex-col min-w-0">
                      <span class="flex items-center gap-1.5 max-w-[260px]">
                        <span class="truncate leading-tight font-medium" title={a.email}>{chipLabel(a)}</span>
                        {#if isInternal(a.email)}
                          <!-- Tag for NC-internal users — set when
                               `find_nextcloud_user_by_email` returns
                               a hit.  Drives the participant-add flow
                               (internals go in as `users` source) and
                               the room-visibility downgrade (private
                               when *every* attendee is internal). -->
                          <span
                            class="text-[9px] uppercase tracking-wide font-semibold px-1 py-px rounded bg-primary-500/20 text-primary-700 dark:text-primary-300 leading-tight shrink-0"
                            title="Nextcloud user on this server"
                          >internal</span>
                        {/if}
                      </span>
                      {#if a.status && a.status.toUpperCase() !== 'NEEDS-ACTION'}
                        <span class="text-[10px] uppercase tracking-wide text-surface-500 leading-tight" title="Response status">
                          {a.status.toLowerCase()}
                        </span>
                      {:else if c && c.organization}
                        <span class="text-[11px] text-surface-500 leading-tight truncate max-w-[220px]">{c.organization}</span>
                      {/if}
                    </span>
                    <button
                      type="button"
                      class="text-surface-500 hover:text-red-500 ml-1 text-base leading-none"
                      title="Remove"
                      aria-label={`Remove ${a.email}`}
                      onclick={() => removeAttendee(role, a.email)}
                    >×</button>
                  </span>
                {/each}
              </div>
            {/if}
            <div class="relative">
              <input
                type="text"
                class="input w-full px-3 py-2 text-sm rounded-md"
                {placeholder}
                autocomplete="off"
                value={role === 'REQ-PARTICIPANT'
                  ? requiredInput
                  : role === 'OPT-PARTICIPANT'
                    ? optionalInput
                    : chairInput}
                oninput={(e) => {
                  const v = (e.currentTarget as HTMLInputElement).value
                  if (role === 'REQ-PARTICIPANT') requiredInput = v
                  else if (role === 'OPT-PARTICIPANT') optionalInput = v
                  else chairInput = v
                  activeSuggestionRole = role
                  runSuggestionSearch(role, v)
                }}
                onfocus={() => {
                  activeSuggestionRole = role
                  const v =
                    role === 'REQ-PARTICIPANT'
                      ? requiredInput
                      : role === 'OPT-PARTICIPANT'
                        ? optionalInput
                        : chairInput
                  if (v.trim().length >= 2) runSuggestionSearch(role, v)
                }}
                onblur={() => {
                  // Defer the close so a click on a suggestion
                  // can fire its onmousedown handler first.
                  setTimeout(() => {
                    if (activeSuggestionRole === role) {
                      activeSuggestionRole = null
                      dropdownSuggestions = []
                    }
                    commitInput(role)
                  }, 120)
                }}
                onkeydown={(e) => {
                  const open = activeSuggestionRole === role && dropdownSuggestions.length > 0
                  if (open && e.key === 'ArrowDown') {
                    e.preventDefault()
                    activeIndex = (activeIndex + 1) % dropdownSuggestions.length
                  } else if (open && e.key === 'ArrowUp') {
                    e.preventDefault()
                    activeIndex =
                      (activeIndex - 1 + dropdownSuggestions.length) %
                      dropdownSuggestions.length
                  } else if (open && (e.key === 'Enter' || e.key === 'Tab')) {
                    e.preventDefault()
                    pickSuggestion(role, dropdownSuggestions[activeIndex])
                  } else if (e.key === 'Enter' || e.key === ',') {
                    e.preventDefault()
                    commitInput(role)
                  } else if (e.key === 'Escape') {
                    activeSuggestionRole = null
                    dropdownSuggestions = []
                  }
                }}
              />

              {#if activeSuggestionRole === role && dropdownSuggestions.length > 0}
                <ul
                  class="absolute left-0 right-0 top-full mt-1 z-50 max-h-72 overflow-y-auto bg-surface-50 dark:bg-surface-900 border border-surface-300 dark:border-surface-700 rounded-md shadow-lg"
                  role="listbox"
                >
                  {#each dropdownSuggestions as c, i (c.id)}
                    {@const url = photoUrl(c)}
                    <li
                      role="option"
                      aria-selected={i === activeIndex}
                      class="flex items-center gap-3 px-3 py-2 cursor-pointer text-sm {i === activeIndex
                        ? 'bg-primary-500/15'
                        : 'hover:bg-surface-200 dark:hover:bg-surface-800'}"
                      onmousedown={(e) => {
                        e.preventDefault()
                        pickSuggestion(role, c)
                      }}
                      onmouseenter={() => (activeIndex = i)}
                    >
                      {#if url}
                        <img
                          src={url}
                          alt=""
                          loading="lazy"
                          class="w-8 h-8 rounded-full object-cover flex-shrink-0"
                        />
                      {:else}
                        <div class="w-8 h-8 rounded-full bg-surface-300 dark:bg-surface-700 flex items-center justify-center text-xs font-semibold flex-shrink-0">
                          {initials(c.display_name)}
                        </div>
                      {/if}
                      <div class="flex-1 min-w-0">
                        <p class="font-medium truncate">{c.display_name}</p>
                        <p class="text-xs text-surface-500 truncate">
                          {primaryEmail(c)}
                          {#if c.organization}· {c.organization}{/if}
                        </p>
                      </div>
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          </div>
        </div>
      {/snippet}

      {@render attendeeRow('Required', 'REQ-PARTICIPANT', requiredAttendees, 'alice@example.com')}
      {@render attendeeRow('Optional', 'OPT-PARTICIPANT', optionalAttendees, 'bob@example.com')}
      {@render attendeeRow('Chair', 'CHAIR', chairAttendees, 'meeting host (usually one)')}

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
