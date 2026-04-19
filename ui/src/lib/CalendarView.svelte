<script lang="ts">
  /**
   * CalendarView — upcoming events across all Nextcloud calendars.
   *
   * Mirrors ContactsView: paint from cache first (`get_cached_calendars`
   * + `get_cached_events`) so the panel is instantly usable, then run
   * `sync_nextcloud_calendars` in the background for anything new.
   *
   * Scope: a flat list grouped by day, showing date / time / title /
   * location. Recurring series are fully expanded server-side — the
   * backend returns one row per concrete occurrence in the window, so
   * this component doesn't need to know about RRULE/EXDATE at all.
   * No month grid and no event editing yet; those live in follow-up
   * issues.
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

  // ── Config ──────────────────────────────────────────────────
  // How far forward to pull events on load. 60 days covers "the
  // next couple of months" without dragging a year of historical
  // rows into the panel. The expander (slice 4) will widen this
  // once recurring series stop being implicit.
  const WINDOW_DAYS_AHEAD = 60
  // Small look-behind so an event that started an hour ago is still
  // visible (e.g. an all-day conference happening right now).
  const WINDOW_HOURS_BEHIND = 2

  // ── State ───────────────────────────────────────────────────
  let accounts = $state<NextcloudAccount[]>([])
  let calendars = $state<CalendarSummary[]>([])
  let events = $state<CalendarEvent[]>([])
  let loading = $state(true)
  let syncing = $state(false)
  let error = $state('')
  // Per-calendar errors from the last sync run. Keyed by calendar
  // path so the UI can attach each message to its calendar (or just
  // dump them in a banner). A slice-2 `SyncCalendarsReport` carries
  // these as `errors: string[]`; we don't try to parse the path out.
  let syncErrors = $state<string[]>([])

  // Derived: map calendar id → color so event rows can paint a dot.
  const colorById = $derived.by(() => {
    const m = new Map<string, string>()
    for (const c of calendars) {
      if (c.color) m.set(c.id, c.color)
    }
    return m
  })

  // Derived: group visible events by day bucket for the rendered list.
  // We bucket by *local-time* calendar date — a meeting at 23:30 and
  // one at 00:30 the next day belong in separate groups even though
  // they're only an hour apart in UTC.
  type DayGroup = { key: string; label: string; rows: CalendarEvent[] }
  const groupedEvents = $derived.by<DayGroup[]>(() => {
    const groups = new Map<string, DayGroup>()
    for (const e of events) {
      const day = dayKey(e.start)
      const g = groups.get(day) ?? {
        key: day,
        label: dayLabel(e.start),
        rows: [],
      }
      g.rows.push(e)
      groups.set(day, g)
    }
    // Map preserves insertion order — events arrive pre-sorted by start.
    return Array.from(groups.values())
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
      await reloadFromCache()
    } catch (e) {
      error = formatError(e) || 'Failed to load calendars'
    } finally {
      loading = false
    }
    // Background refresh — picks up events added from other devices
    // without forcing the user to click "Sync now".
    void syncInBackground()
  }

  async function reloadFromCache() {
    // Collect cached calendars across every connected NC account
    // so one panel shows the user's entire calendar life.
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
      return
    }

    const now = new Date()
    const start = new Date(now.getTime() - WINDOW_HOURS_BEHIND * 3600_000)
    const end = new Date(now.getTime() + WINDOW_DAYS_AHEAD * 86400_000)
    try {
      events = await invoke<CalendarEvent[]>('get_cached_events', {
        calendarIds: allCalendars.map((c) => c.id),
        rangeStart: start.toISOString(),
        rangeEnd: end.toISOString(),
      })
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
      await reloadFromCache()
    } finally {
      syncing = false
    }
  }

  // ── Formatting helpers ──────────────────────────────────────

  // YYYY-MM-DD in local time, used as the group map key.
  function dayKey(iso: string): string {
    const d = new Date(iso)
    const y = d.getFullYear()
    const m = String(d.getMonth() + 1).padStart(2, '0')
    const dd = String(d.getDate()).padStart(2, '0')
    return `${y}-${m}-${dd}`
  }

  // Human-friendly heading for the group. "Today" / "Tomorrow" for
  // the common cases, then day-of-week + date for everything further
  // out. Matches how calendar apps normally read.
  function dayLabel(iso: string): string {
    const d = new Date(iso)
    const today = new Date()
    today.setHours(0, 0, 0, 0)
    const date = new Date(d.getFullYear(), d.getMonth(), d.getDate())
    const diffDays = Math.round((date.getTime() - today.getTime()) / 86400_000)
    if (diffDays === 0) return 'Today'
    if (diffDays === 1) return 'Tomorrow'
    if (diffDays === -1) return 'Yesterday'
    if (diffDays > 1 && diffDays < 7) {
      return date.toLocaleDateString(undefined, { weekday: 'long' })
    }
    return date.toLocaleDateString(undefined, {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
    })
  }

  // "09:30 – 10:00" for same-day events; "Aug 5, 09:30 – Aug 6, 10:00"
  // for multi-day. All-day events (midnight-to-midnight exactly) get a
  // special "All day" marker so they don't shout meaningless zeros.
  function timeRange(ev: CalendarEvent): string {
    const s = new Date(ev.start)
    const e = new Date(ev.end)
    if (isAllDay(s, e)) return 'All day'
    const sameDay =
      s.getFullYear() === e.getFullYear() &&
      s.getMonth() === e.getMonth() &&
      s.getDate() === e.getDate()
    const fmt = (d: Date) =>
      d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
    if (sameDay) {
      return `${fmt(s)} – ${fmt(e)}`
    }
    const longFmt = (d: Date) =>
      d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) +
      ', ' +
      fmt(d)
    return `${longFmt(s)} – ${longFmt(e)}`
  }

  function isAllDay(s: Date, e: Date): boolean {
    // Rough heuristic: iCalendar all-day events have start at local
    // midnight and an integer-day duration. Our parser coerces to UTC
    // at end-of-day, which when rendered back in local time looks like
    // "24h apart, both at 00:00" — that's what we match here.
    if (s.getHours() !== 0 || s.getMinutes() !== 0) return false
    if (e.getHours() !== 0 || e.getMinutes() !== 0) return false
    const spanHours = (e.getTime() - s.getTime()) / 3600_000
    return spanHours >= 23 && spanHours % 24 < 1
  }
</script>

<div class="h-full flex flex-col bg-surface-50 dark:bg-surface-900">
  <!-- Header -->
  <div
    class="flex items-center justify-between px-6 py-4 border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800"
  >
    <div class="flex items-center gap-3">
      <h2 class="text-xl font-semibold">Calendar</h2>
      {#if syncing}
        <span class="text-xs text-surface-500">Syncing…</span>
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
  <div class="flex-1 overflow-y-auto">
    {#if loading}
      <p class="px-6 py-4 text-sm text-surface-500">Loading calendars…</p>
    {:else if error}
      <p class="px-6 py-4 text-sm text-red-500">{error}</p>
    {:else if calendars.length === 0}
      <p class="px-6 py-4 text-sm text-surface-500">
        No calendars cached yet. Click <strong>Sync now</strong> to pull them
        from your Nextcloud account.
      </p>
    {:else if events.length === 0}
      <p class="px-6 py-4 text-sm text-surface-500">
        No events in the next {WINDOW_DAYS_AHEAD} days.
      </p>
    {:else}
      <ul class="px-6 py-4 space-y-6">
        {#each groupedEvents as group (group.key)}
          <li>
            <h3
              class="text-xs font-semibold uppercase tracking-wider text-surface-500 mb-2"
            >
              {group.label}
            </h3>
            <ul class="space-y-2">
              {#each group.rows as ev (ev.id)}
                <li
                  class="flex gap-3 p-3 rounded-md bg-surface-100 dark:bg-surface-800 border border-surface-200 dark:border-surface-700"
                >
                  <!-- Calendar colour dot. Falls back to the theme
                       primary when the calendar didn't advertise one. -->
                  <span
                    class="w-2.5 h-2.5 rounded-full mt-1.5 shrink-0"
                    style:background-color={colorById.get(
                      ev.id.split('::').slice(0, 2).join('::'),
                    ) ?? '#2bb0ed'}
                  ></span>
                  <div class="flex-1 min-w-0">
                    <div class="flex items-baseline gap-2">
                      <span class="text-sm font-medium truncate">
                        {ev.summary || '(no title)'}
                      </span>
                      <span class="text-xs text-surface-500 shrink-0">
                        {timeRange(ev)}
                      </span>
                    </div>
                    {#if ev.location}
                      <p class="text-xs text-surface-500 truncate mt-0.5">
                        {ev.location}
                      </p>
                    {/if}
                  </div>
                </li>
              {/each}
            </ul>
          </li>
        {/each}
      </ul>
    {/if}

    {#if syncErrors.length > 0}
      <div
        class="mx-6 mb-4 p-3 rounded-md border border-red-200 dark:border-red-700 bg-red-50 dark:bg-red-950 text-xs text-red-700 dark:text-red-200"
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
