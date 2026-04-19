<script lang="ts">
  /**
   * CalendarView — Outlook-style week grid.
   *
   * Layout: seven day columns with a 24h time axis, events positioned
   * absolutely inside each column, plus an all-day strip above the
   * grid. Navigation is week-at-a-time with a "Today" shortcut.
   *
   * # Fetch strategy
   *
   * On mount we pull a broad window (±6 months) so week navigation
   * feels instant. When the user scrolls past the edge of the loaded
   * window (within `EXTEND_THRESHOLD_DAYS` of either boundary), we
   * extend that direction by `EXTEND_CHUNK_DAYS` and re-query the
   * backend — recurrence expansion is per-query server-side, so this
   * falls out cleanly without any client-side expansion.
   *
   * # Data flow
   *
   * - `init()` → accounts → `reloadFromCache(window)` → background sync.
   * - Week nav → adjust `currentWeekStart` → derived `weekEvents`
   *   filters the in-memory `events` array.
   * - Out-of-window nav → `ensureWindowCovers()` re-fetches with a
   *   widened window.
   *
   * # What's deliberately out of scope
   *
   * Event editing, month/day toggle, click-to-create, drag-to-resize
   * — each of those is its own issue. This component stays a
   * read-only week renderer.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  interface Props {
    onclose: () => void
  }
  const { onclose }: Props = $props()

  // ── Types (mirror the Rust models) ──────────────────────────
  interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
  }
  interface CalendarSummary {
    id: string
    nextcloud_account_id: string
    display_name: string
    color: string | null
    last_synced_at: string | null
  }
  interface CalendarEvent {
    id: string
    summary: string
    description: string | null
    start: string // RFC3339
    end: string
    location: string | null
    rrule: string | null
    rdate: string[]
    exdate: string[]
    recurrence_id: string | null
  }

  // ── Layout constants ────────────────────────────────────────
  // 48px per hour gives each 30-minute slot a readable 24px band.
  // Pixel-per-minute math elsewhere derives from this single number
  // so tweaking density is a one-line change.
  const HOUR_HEIGHT_PX = 48
  const PX_PER_MINUTE = HOUR_HEIGHT_PX / 60
  // ISO-day number for Monday. Users expect a Mon-start week for a
  // European-flavoured app; the backend is already storing in UTC so
  // switching to Sunday later is purely a UI change.
  const WEEK_STARTS_ON = 1
  // Top/bottom strips for all-day events — each row is a colored pill.
  // If more than `ALL_DAY_VISIBLE_ROWS` events fall on the same day,
  // the surplus collapses into a "+N more" affordance rather than
  // pushing the time grid off-screen.
  const ALL_DAY_ROW_HEIGHT_PX = 24
  const ALL_DAY_VISIBLE_ROWS = 3

  // ── Fetch-window constants ──────────────────────────────────
  // Initial load covers ±6 months — almost every week-nav stays
  // inside this span without hitting the backend. Picked empirically:
  // Nextcloud parsing overhead for a year of events is fine (<200ms
  // on the test account) but memory in Svelte gets chatty past that.
  const INITIAL_PAST_DAYS = 180
  const INITIAL_FUTURE_DAYS = 180
  // When the user navigates close to the edge of the loaded window
  // we extend it. The threshold is intentionally larger than a single
  // week so the extension happens one week *before* they'd see an
  // empty week, not as they're looking at it.
  const EXTEND_THRESHOLD_DAYS = 21
  const EXTEND_CHUNK_DAYS = 120

  // ── State ───────────────────────────────────────────────────
  let accounts = $state<NextcloudAccount[]>([])
  let calendars = $state<CalendarSummary[]>([])
  let events = $state<CalendarEvent[]>([])
  let loading = $state(true)
  let syncing = $state(false)
  let error = $state('')
  let syncErrors = $state<string[]>([])

  // Current navigation focus: the Monday of the visible week.
  let currentWeekStart = $state<Date>(startOfWeek(new Date()))
  // What range of data we currently have fetched. Tracked so week
  // nav can tell whether it's crossing into uncached territory and
  // trigger an extension.
  let loadedRangeStart = $state<Date>(new Date(0))
  let loadedRangeEnd = $state<Date>(new Date(0))

  // Per-calendar visibility — a Set of calendar ids that are
  // currently *hidden*. Outlook-style "uncheck a calendar to remove
  // its events from the grid". A Set rather than a positive list so
  // newly-discovered calendars default to visible without needing a
  // separate "show new calendars" workflow.
  let hiddenCalendarIds = $state<Set<string>>(new Set())

  // Derived: calendar id → colour (for the coloured stripe on each
  // event block and the all-day pills).
  const colorById = $derived.by(() => {
    const m = new Map<string, string>()
    for (const c of calendars) {
      if (c.color) m.set(c.id, c.color)
    }
    return m
  })

  // Derived: the seven Date objects (one per column) for the current
  // week. Monday → Sunday.
  const weekDays = $derived.by<Date[]>(() => {
    const out: Date[] = []
    for (let i = 0; i < 7; i++) {
      const d = new Date(currentWeekStart)
      d.setDate(d.getDate() + i)
      out.push(d)
    }
    return out
  })

  // Derived: events filtered to the current week, bucketed by day
  // and by all-day-ness. `timed` events get absolute layout inside
  // the grid; `allDay` events stack in the header strip.
  type PlacedEvent = {
    event: CalendarEvent
    topPx: number
    heightPx: number
    lane: number
    laneCount: number
  }
  type WeekBucket = {
    date: Date
    dayKey: string
    timed: PlacedEvent[]
    allDay: CalendarEvent[]
  }
  const weekBuckets = $derived.by<WeekBucket[]>(() => {
    const buckets: WeekBucket[] = weekDays.map((d) => ({
      date: d,
      dayKey: dayKey(d),
      timed: [],
      allDay: [],
    }))
    // Index for O(1) push-by-day-key.
    const byKey = new Map<string, WeekBucket>()
    for (const b of buckets) byKey.set(b.dayKey, b)

    for (const ev of events) {
      // Outlook-style "uncheck a calendar to hide its events" — drop
      // events from any calendar the user has toggled off before the
      // (more expensive) date math.
      if (hiddenCalendarIds.has(eventCalendarId(ev))) continue
      const start = new Date(ev.start)
      const end = new Date(ev.end)
      if (isAllDay(start, end)) {
        // All-day events live on UTC calendar days (the backend stores
        // them as `00:00:00Z`–`23:59:59Z`). Use `end - 1ms` as the
        // inclusive-end boundary so a DURATION:P1D event (end at
        // midnight UTC of the next day, exclusive per RFC 5545) doesn't
        // leak onto that next day's bucket. Also covers the DTEND
        // fix-pending case if a sync hasn't refreshed the cache yet.
        // dayKey is `YYYY-MM-DD`, so lexicographic compare doubles as
        // date compare.
        const startKey = utcDayKey(start)
        const endKey = utcDayKey(new Date(end.getTime() - 1))
        for (const bucket of buckets) {
          if (bucket.dayKey >= startKey && bucket.dayKey <= endKey) {
            bucket.allDay.push(ev)
          }
        }
        continue
      }
      // Timed event: place into the bucket that holds its start day.
      // Events that cross midnight just render in the starting day
      // up to 23:59 for now — multi-day non-all-day events are rare
      // enough (overnight shifts, flights) that a dedicated follow-up
      // can refine this without changing the data model.
      const bucket = byKey.get(dayKey(start))
      if (!bucket) continue
      const dayStart = startOfDay(bucket.date)
      const dayEnd = endOfDay(bucket.date)
      const clampedStart = start < dayStart ? dayStart : start
      const clampedEnd = end > dayEnd ? dayEnd : end
      const topPx =
        (minutesBetween(dayStart, clampedStart)) * PX_PER_MINUTE
      const heightPx = Math.max(
        // Enforce a minimum height so a 0-minute point event still
        // has a visible block to click on.
        20,
        minutesBetween(clampedStart, clampedEnd) * PX_PER_MINUTE,
      )
      bucket.timed.push({
        event: ev,
        topPx,
        heightPx,
        lane: 0,
        laneCount: 1,
      })
    }

    // Lane assignment per day for overlap handling. Same algorithm
    // Google/Outlook use: sort by start, assign each event to the
    // lowest-numbered lane whose last event has already ended. The
    // day's total lane count then becomes the divisor for block
    // widths. Simple first-pass: one lane count per day — two
    // overlapping clusters share the widest-cluster's width even if
    // they're not mutually overlapping. Good enough for V1; can
    // narrow later with cluster-aware layout.
    for (const bucket of buckets) {
      bucket.timed.sort((a, b) => a.topPx - b.topPx)
      const laneEnds: number[] = [] // each lane's current bottom (px)
      for (const p of bucket.timed) {
        const bottom = p.topPx + p.heightPx
        let assigned = -1
        for (let i = 0; i < laneEnds.length; i++) {
          if (laneEnds[i] <= p.topPx) {
            assigned = i
            laneEnds[i] = bottom
            break
          }
        }
        if (assigned === -1) {
          assigned = laneEnds.length
          laneEnds.push(bottom)
        }
        p.lane = assigned
      }
      const laneCount = Math.max(1, laneEnds.length)
      for (const p of bucket.timed) {
        p.laneCount = laneCount
      }
    }

    return buckets
  })

  $effect(() => {
    void init()
  })

  async function init() {
    loading = true
    error = ''
    try {
      accounts = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      if (accounts.length === 0) {
        error = 'Connect a Nextcloud account first to sync calendars.'
        loading = false
        return
      }
      const now = new Date()
      const start = addDays(now, -INITIAL_PAST_DAYS)
      const end = addDays(now, INITIAL_FUTURE_DAYS)
      await reloadFromCache(start, end)
    } catch (e) {
      error = formatError(e) || 'Failed to load calendars'
    } finally {
      loading = false
    }
    // Background sync for anything new server-side. Completes silently
    // except for the banner below the header when errors occur.
    void syncInBackground()
  }

  async function reloadFromCache(windowStart: Date, windowEnd: Date) {
    // Collect cached calendars across every connected NC account so a
    // user with multiple Nextclouds sees everything overlaid on one
    // grid.
    const allCalendars: CalendarSummary[] = []
    for (const a of accounts) {
      try {
        const cs = await invoke<CalendarSummary[]>('get_cached_calendars', {
          ncId: a.id,
        })
        allCalendars.push(...cs)
      } catch (e) {
        console.warn('get_cached_calendars failed for', a.id, e)
      }
    }
    calendars = allCalendars
    if (allCalendars.length === 0) {
      events = []
      loadedRangeStart = windowStart
      loadedRangeEnd = windowEnd
      return
    }
    try {
      events = await invoke<CalendarEvent[]>('get_cached_events', {
        calendarIds: allCalendars.map((c) => c.id),
        rangeStart: windowStart.toISOString(),
        rangeEnd: windowEnd.toISOString(),
      })
      loadedRangeStart = windowStart
      loadedRangeEnd = windowEnd
    } catch (e) {
      console.warn('get_cached_events failed:', e)
      events = []
    }
  }

  async function syncInBackground() {
    if (syncing) return
    syncing = true
    syncErrors = []
    try {
      for (const a of accounts) {
        try {
          const report = await invoke<{ errors: string[] }>(
            'sync_nextcloud_calendars',
            { ncId: a.id },
          )
          if (report.errors.length > 0) {
            syncErrors.push(...report.errors)
          }
        } catch (e) {
          console.warn('sync_nextcloud_calendars failed for', a.id, e)
          syncErrors.push(formatError(e) || `Sync failed for ${a.id}`)
        }
      }
      // Re-query the same window — fresh events from the sync appear
      // without the user doing anything.
      await reloadFromCache(loadedRangeStart, loadedRangeEnd)
    } finally {
      syncing = false
    }
  }

  // ── Navigation ──────────────────────────────────────────────
  function goToToday() {
    void navigateTo(startOfWeek(new Date()))
  }
  function prevWeek() {
    void navigateTo(addDays(currentWeekStart, -7))
  }
  function nextWeek() {
    void navigateTo(addDays(currentWeekStart, 7))
  }

  /** Move the visible week, extending the cached window on demand. */
  async function navigateTo(target: Date) {
    currentWeekStart = target
    const weekEnd = addDays(target, 7)
    // Extend if we're within the threshold of either edge — this runs
    // *before* the user can notice a gap, because the derived
    // `weekBuckets` already filters `events` to the visible week.
    const needsPast =
      daysBetween(loadedRangeStart, target) < EXTEND_THRESHOLD_DAYS
    const needsFuture =
      daysBetween(weekEnd, loadedRangeEnd) < EXTEND_THRESHOLD_DAYS
    if (!needsPast && !needsFuture) return
    const newStart = needsPast
      ? addDays(loadedRangeStart, -EXTEND_CHUNK_DAYS)
      : loadedRangeStart
    const newEnd = needsFuture
      ? addDays(loadedRangeEnd, EXTEND_CHUNK_DAYS)
      : loadedRangeEnd
    await reloadFromCache(newStart, newEnd)
  }

  // ── Formatting & geometry helpers ───────────────────────────

  function startOfWeek(d: Date): Date {
    const out = new Date(d.getFullYear(), d.getMonth(), d.getDate())
    // JS `getDay()` returns 0 for Sunday. We want ISO (Mon=1), so
    // shift back to the previous Monday — or today if it *is* Monday.
    const jsDay = out.getDay() // 0=Sun … 6=Sat
    const isoDay = jsDay === 0 ? 7 : jsDay // 1=Mon … 7=Sun
    const diff = isoDay - WEEK_STARTS_ON
    out.setDate(out.getDate() - diff)
    return out
  }
  function startOfDay(d: Date): Date {
    return new Date(d.getFullYear(), d.getMonth(), d.getDate())
  }
  function endOfDay(d: Date): Date {
    return new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1)
  }
  function addDays(d: Date, n: number): Date {
    const out = new Date(d)
    out.setDate(out.getDate() + n)
    return out
  }
  function daysBetween(a: Date, b: Date): number {
    return Math.round((b.getTime() - a.getTime()) / 86400_000)
  }
  function minutesBetween(a: Date, b: Date): number {
    return (b.getTime() - a.getTime()) / 60_000
  }
  function dayKey(d: Date): string {
    const y = d.getFullYear()
    const m = String(d.getMonth() + 1).padStart(2, '0')
    const dd = String(d.getDate()).padStart(2, '0')
    return `${y}-${m}-${dd}`
  }
  function isAllDay(s: Date, e: Date): boolean {
    // The backend (`nimbus-caldav::ical`) stores `VALUE=DATE` events
    // as midnight UTC → 23:59:59 UTC of the last covered day. Detect
    // on the UTC fields, not local ones — otherwise a user east of UTC
    // sees the event start at e.g. 02:00 (CEST) and isAllDay returns
    // false, dropping the event into the timed grid as a 22-hour block.
    if (s.getUTCHours() !== 0 || s.getUTCMinutes() !== 0) return false
    const hours = (e.getTime() - s.getTime()) / 3600_000
    if (hours < 23) return false
    // All-day shapes we actually observe in nimbus-caldav output:
    //   - DURATION:P1D×N  → span is exactly N×24h
    //   - DTSTART only    → span is 23h59m59s (start + 86399s fallback)
    //   - DTEND snapped   → span is N×24h - 1s (ical.rs 23:59:59 snap)
    // All three sit within a minute of a whole-day boundary, so check
    // that the remainder is close to 0 *or* close to 24.
    const remainder = hours % 24
    return remainder < 1 / 60 || remainder > 24 - 1 / 60
  }

  /** UTC `YYYY-MM-DD` for `d`. Use when matching all-day events (UTC
      midnight by convention) against a bucket's calendar day. */
  function utcDayKey(d: Date): string {
    const y = d.getUTCFullYear()
    const m = String(d.getUTCMonth() + 1).padStart(2, '0')
    const dd = String(d.getUTCDate()).padStart(2, '0')
    return `${y}-${m}-${dd}`
  }

  function fmtTime(iso: string): string {
    return new Date(iso).toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
    })
  }

  function weekRangeLabel(): string {
    const first = weekDays[0]
    const last = weekDays[6]
    const sameMonth = first.getMonth() === last.getMonth()
    const sameYear = first.getFullYear() === last.getFullYear()
    const fmtFull = (d: Date) =>
      d.toLocaleDateString(undefined, {
        month: 'short',
        day: 'numeric',
        year: 'numeric',
      })
    const fmtShort = (d: Date) =>
      d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
    const fmtDayOnly = (d: Date) =>
      d.toLocaleDateString(undefined, { day: 'numeric' })
    if (sameYear && sameMonth) {
      return `${fmtShort(first)} – ${fmtDayOnly(last)}, ${first.getFullYear()}`
    }
    if (sameYear) {
      return `${fmtShort(first)} – ${fmtShort(last)}, ${first.getFullYear()}`
    }
    return `${fmtFull(first)} – ${fmtFull(last)}`
  }

  function isToday(d: Date): boolean {
    const now = new Date()
    return (
      d.getDate() === now.getDate() &&
      d.getMonth() === now.getMonth() &&
      d.getFullYear() === now.getFullYear()
    )
  }

  function eventCalendarId(ev: CalendarEvent): string {
    // Event ids come out of the store as `{nc_id}::{cal_path}::{uid}`
    // or the expanded `{…}::occ::{epoch}` form. Calendar ids are the
    // first two `::`-delimited segments. Slice those off either shape
    // to recover the owning calendar id.
    const parts = ev.id.split('::')
    return parts.slice(0, 2).join('::')
  }
  function eventColor(ev: CalendarEvent): string {
    return colorById.get(eventCalendarId(ev)) ?? '#2bb0ed'
  }

  function toggleCalendar(id: string) {
    // Svelte 5 reactivity for Sets/Maps requires reassignment — mutating
    // in place won't trigger derived recomputation. Rebuild the Set
    // each toggle so `weekBuckets` re-runs.
    const next = new Set(hiddenCalendarIds)
    if (next.has(id)) {
      next.delete(id)
    } else {
      next.add(id)
    }
    hiddenCalendarIds = next
  }

  // All-day overflow: if a day has more than the visible cap, show
  // the first N and a "+M more" affordance. Collapsed detail UI is a
  // follow-up — today this just prevents the strip from growing
  // unbounded.
  function allDayVisible(list: CalendarEvent[]): CalendarEvent[] {
    return list.slice(0, ALL_DAY_VISIBLE_ROWS)
  }
  function allDayOverflow(list: CalendarEvent[]): number {
    return Math.max(0, list.length - ALL_DAY_VISIBLE_ROWS)
  }
</script>

<div class="h-full flex flex-col bg-surface-50 dark:bg-surface-900">
  <!-- Header -->
  <div
    class="flex items-center justify-between px-6 py-3 border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800"
  >
    <div class="flex items-center gap-3">
      <h2 class="text-xl font-semibold">Calendar</h2>
      <div class="flex items-center gap-1 ml-2">
        <button
          class="btn preset-tonal-surface text-sm px-2"
          onclick={prevWeek}
          aria-label="Previous week"
        >
          ‹
        </button>
        <button
          class="btn preset-tonal-surface text-sm px-3"
          onclick={goToToday}
        >
          Today
        </button>
        <button
          class="btn preset-tonal-surface text-sm px-2"
          onclick={nextWeek}
          aria-label="Next week"
        >
          ›
        </button>
      </div>
      <span class="text-sm font-medium ml-2">{weekRangeLabel()}</span>
      {#if syncing}
        <span class="text-xs text-surface-500 ml-2">Syncing…</span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      <button
        class="btn preset-tonal-surface text-sm"
        disabled={syncing}
        onclick={() => void syncInBackground()}
      >
        Sync now
      </button>
      <button class="btn preset-tonal-surface text-sm" onclick={onclose}>
        Close
      </button>
    </div>
  </div>

  <!-- Body -->
  {#if loading}
    <p class="px-6 py-4 text-sm text-surface-500">Loading calendars…</p>
  {:else if error}
    <p class="px-6 py-4 text-sm text-red-500">{error}</p>
  {:else if calendars.length === 0}
    <p class="px-6 py-4 text-sm text-surface-500">
      No calendars cached yet. Click <strong>Sync now</strong> to pull them
      from your Nextcloud account.
    </p>
  {:else}
    <div class="flex flex-1 min-h-0">
      <!-- Sidebar: per-calendar visibility toggles (Outlook-style).
           Click a calendar to hide/show its events on the grid. The
           coloured swatch is the calendar's own colour from Nextcloud,
           so it matches the event blocks 1:1. -->
      <aside
        class="w-56 shrink-0 border-r border-surface-200 dark:border-surface-700 bg-surface-100/60 dark:bg-surface-800/40 overflow-y-auto p-3"
      >
        <div class="text-xs font-semibold uppercase tracking-wider text-surface-500 mb-2 px-1">
          Calendars
        </div>
        <ul class="space-y-1">
          {#each calendars as c (c.id)}
            {@const visible = !hiddenCalendarIds.has(c.id)}
            <li>
              <label
                class="flex items-center gap-2 px-2 py-1 rounded hover:bg-surface-200/60 dark:hover:bg-surface-700/40 cursor-pointer text-sm"
              >
                <input
                  type="checkbox"
                  class="checkbox"
                  checked={visible}
                  onchange={() => toggleCalendar(c.id)}
                />
                <span
                  class="w-3 h-3 rounded-sm shrink-0"
                  style="background-color: {c.color ?? '#2bb0ed'};"
                ></span>
                <span class="truncate" title={c.display_name}>
                  {c.display_name}
                </span>
              </label>
            </li>
          {/each}
        </ul>
      </aside>

      <!-- Main grid area. Both header and time grid live in a single
           scrollable container with the header pinned via `sticky` so
           their column tracks share the same width and stay perfectly
           aligned no matter what the scrollbar does. `scrollbar-gutter:
           stable` reserves the scrollbar slot up front so the grid
           never reflows when content height crosses the viewport. -->
      <div class="flex-1 flex flex-col min-w-0">
        <div
          class="flex-1 overflow-y-auto"
          style="scrollbar-gutter: stable;"
        >
          <!-- Sticky day-of-week header. Each day cell carries the
               weekday label, the date number, and the all-day pills
               for that day stacked directly underneath — matching
               Outlook's "all-day events live with their date" layout. -->
          <div
            class="grid sticky top-0 z-10 border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800"
            style="grid-template-columns: 56px repeat(7, minmax(0, 1fr));"
          >
            <div></div>
            {#each weekBuckets as b (b.dayKey)}
              <div
                class="px-2 py-2 text-center text-xs font-medium border-l border-surface-200 dark:border-surface-700 flex flex-col gap-1"
                class:bg-primary-50={isToday(b.date)}
                class:dark:bg-primary-950={isToday(b.date)}
              >
                <div
                  class="uppercase tracking-wider text-surface-500"
                  class:text-primary-600={isToday(b.date)}
                >
                  {b.date.toLocaleDateString(undefined, { weekday: 'short' })}
                </div>
                <div
                  class="text-lg font-semibold leading-none"
                  class:text-primary-600={isToday(b.date)}
                >
                  {b.date.getDate()}
                </div>
                {#if b.allDay.length > 0}
                  <div class="flex flex-col gap-0.5 mt-1 text-left">
                    {#each allDayVisible(b.allDay) as ev (ev.id)}
                      <div
                        class="text-[11px] truncate rounded px-1.5 text-white"
                        style="background-color: {eventColor(ev)}; height: {ALL_DAY_ROW_HEIGHT_PX}px; line-height: {ALL_DAY_ROW_HEIGHT_PX}px;"
                        title={ev.summary}
                      >
                        {ev.summary || '(no title)'}
                      </div>
                    {/each}
                    {#if allDayOverflow(b.allDay) > 0}
                      <div class="text-[10px] text-surface-500">
                        +{allDayOverflow(b.allDay)} more
                      </div>
                    {/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>

          <!-- 24-hour time grid. -->
          <div
            class="relative grid"
            style="grid-template-columns: 56px repeat(7, minmax(0, 1fr)); height: {HOUR_HEIGHT_PX * 24}px;"
          >
            <!-- Hours axis. -->
            <div class="relative border-r border-surface-200 dark:border-surface-700">
              {#each Array.from({ length: 24 }, (_, i) => i) as h}
                <div
                  class="absolute left-0 right-0 text-[11px] text-surface-400 pr-2 text-right"
                  style="top: {h * HOUR_HEIGHT_PX}px;"
                >
                  {String(h).padStart(2, '0')}:00
                </div>
              {/each}
            </div>

            <!-- Seven day columns. Each one is its own relative container
                 so event blocks can absolutely-position against the
                 24-hour axis without interfering with each other. -->
            {#each weekBuckets as b (b.dayKey)}
              <div
                class="relative border-l border-surface-200 dark:border-surface-700"
                class:bg-primary-50={isToday(b.date)}
                class:dark:bg-primary-950={isToday(b.date)}
              >
                <!-- Hour gridlines — pure visual rhythm. -->
                {#each Array.from({ length: 24 }, (_, i) => i) as h}
                  <div
                    class="absolute left-0 right-0 border-t border-surface-200/60 dark:border-surface-700/60"
                    style="top: {h * HOUR_HEIGHT_PX}px;"
                  ></div>
                {/each}

                <!-- Event blocks. -->
                {#each b.timed as p (p.event.id)}
                  <div
                    class="absolute rounded-md text-[11px] text-white overflow-hidden px-1.5 py-1 shadow-sm cursor-default"
                    style="top: {p.topPx}px; height: {p.heightPx}px; left: calc({(p.lane / p.laneCount) * 100}% + 2px); width: calc({(1 / p.laneCount) * 100}% - 4px); background-color: {eventColor(p.event)};"
                    title={p.event.summary + (p.event.location ? ' — ' + p.event.location : '')}
                  >
                    <div class="font-medium truncate">
                      {p.event.summary || '(no title)'}
                    </div>
                    <div class="opacity-90 truncate">
                      {fmtTime(p.event.start)} – {fmtTime(p.event.end)}
                    </div>
                    {#if p.event.location && p.heightPx > 60}
                      <div class="opacity-90 truncate">{p.event.location}</div>
                    {/if}
                  </div>
                {/each}
              </div>
            {/each}
          </div>
        </div>

        {#if syncErrors.length > 0}
          <div
            class="mx-6 my-2 p-3 rounded-md border border-red-200 dark:border-red-700 bg-red-50 dark:bg-red-950 text-xs text-red-700 dark:text-red-200 shrink-0"
          >
            <p class="font-semibold mb-1">Some calendars failed to sync:</p>
            <ul class="list-disc list-inside space-y-0.5">
              {#each syncErrors as msg}
                <li>{msg}</li>
              {/each}
            </ul>
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>
