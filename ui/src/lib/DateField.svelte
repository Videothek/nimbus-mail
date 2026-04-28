<script lang="ts">
  /**
   * DateField — calendar-popover date picker (#126).
   *
   * A reusable date input that replaces the native
   * `<input type="date">`.  The native control varies wildly
   * across browsers and platforms (Chromium on Linux ships a
   * particularly minimal one), so we render a custom popover
   * matching Outlook's calendar-grid style.  Locale-formatted
   * display, prev/next month arrows, current-day highlight,
   * outside-click + Escape to close, arrow-key navigation
   * inside the grid.
   *
   * Value is `YYYY-MM-DD` so it round-trips through the
   * existing date helpers in EventEditor unchanged.
   */

  import { onMount } from 'svelte'

  let {
    value = $bindable(''),
    id,
    ariaLabel,
  }: {
    value?: string
    id?: string
    ariaLabel?: string
  } = $props()

  // Popover open / close.  Closing snaps the focused-month
  // view back to the selected date so reopening doesn't keep
  // the user on a month they were just browsing.
  let open = $state(false)
  let anchor: HTMLDivElement | undefined = $state()

  /** Parse `YYYY-MM-DD` to a `Date` (local-zone calendar
   *  date).  Returns today on bad / empty input. */
  function parseDate(s: string): Date {
    if (!s) return new Date()
    const [y, m, d] = s.split('-').map((p) => parseInt(p, 10))
    if (!y || !m || !d) return new Date()
    return new Date(y, m - 1, d)
  }
  /** Format `Date` → `YYYY-MM-DD` (local-zone). */
  function formatISO(d: Date): string {
    const pad = (n: number) => String(n).padStart(2, '0')
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
  }
  /** Localised display: "Apr 28, 2026". */
  function formatDisplay(s: string): string {
    if (!s) return ''
    const d = parseDate(s)
    return d.toLocaleDateString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
    })
  }
  function sameDay(a: Date, b: Date): boolean {
    return (
      a.getFullYear() === b.getFullYear() &&
      a.getMonth() === b.getMonth() &&
      a.getDate() === b.getDate()
    )
  }

  /** Currently-displayed month — driven by the prev/next
   *  arrows.  Initialised to the value's month so the popover
   *  opens centred on the current selection. */
  // svelte-ignore state_referenced_locally
  let view = $state(monthStart(parseDate(value)))
  function monthStart(d: Date): Date {
    return new Date(d.getFullYear(), d.getMonth(), 1)
  }
  // Keep the view in sync when the bound value changes
  // externally (e.g. reset by the parent on form open).
  $effect(() => {
    if (!open) view = monthStart(parseDate(value))
  })

  /** 6-week grid of `Date`s for the current view month.
   *  First row may include trailing days of the previous
   *  month; last row may spill into the next.  Always 42
   *  cells so the popover height never jumps as the user
   *  navigates between 28-, 30-, and 31-day months. */
  let grid = $derived.by(() => {
    const first = new Date(view.getFullYear(), view.getMonth(), 1)
    // RFC: week starts on Monday for our locale; getDay() 0=Sun → shift.
    const offset = (first.getDay() + 6) % 7
    const start = new Date(first)
    start.setDate(1 - offset)
    const cells: Date[] = []
    for (let i = 0; i < 42; i++) {
      const d = new Date(start)
      d.setDate(start.getDate() + i)
      cells.push(d)
    }
    return cells
  })

  let today = new Date()
  let selected = $derived(parseDate(value))

  function pick(d: Date) {
    value = formatISO(d)
    open = false
  }
  function prevMonth() {
    view = new Date(view.getFullYear(), view.getMonth() - 1, 1)
  }
  function nextMonth() {
    view = new Date(view.getFullYear(), view.getMonth() + 1, 1)
  }
  function goToday() {
    pick(new Date())
  }

  // Outside-click + Escape close.  Bound at mount so we don't
  // leak listeners on hot-reload.
  onMount(() => {
    function onKey(e: KeyboardEvent) {
      if (!open) return
      if (e.key === 'Escape') {
        open = false
      } else if (e.key === 'ArrowLeft') {
        e.preventDefault()
        const d = new Date(selected)
        d.setDate(d.getDate() - 1)
        value = formatISO(d)
      } else if (e.key === 'ArrowRight') {
        e.preventDefault()
        const d = new Date(selected)
        d.setDate(d.getDate() + 1)
        value = formatISO(d)
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        const d = new Date(selected)
        d.setDate(d.getDate() - 7)
        value = formatISO(d)
      } else if (e.key === 'ArrowDown') {
        e.preventDefault()
        const d = new Date(selected)
        d.setDate(d.getDate() + 7)
        value = formatISO(d)
      } else if (e.key === 'Enter') {
        open = false
      }
    }
    function onClick(e: MouseEvent) {
      if (!open || !anchor) return
      if (!anchor.contains(e.target as Node)) open = false
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onClick)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onClick)
    }
  })

  // Locale-aware month + weekday labels.  Computed once per
  // view change so we don't reformat on every grid cell.
  let monthLabel = $derived(
    view.toLocaleDateString(undefined, { month: 'long', year: 'numeric' }),
  )
  // Mon … Sun — generated from a fixed reference week so the
  // labels match whatever the browser's locale renders.
  let weekdayLabels = $derived.by(() => {
    // 2024-01-01 was a Monday.
    const ref = new Date(2024, 0, 1)
    const out: string[] = []
    for (let i = 0; i < 7; i++) {
      const d = new Date(ref)
      d.setDate(ref.getDate() + i)
      out.push(d.toLocaleDateString(undefined, { weekday: 'short' }))
    }
    return out
  })
</script>

<div class="relative" bind:this={anchor}>
  <button
    type="button"
    {id}
    aria-label={ariaLabel}
    aria-haspopup="dialog"
    aria-expanded={open}
    class="input w-full px-3 py-2 text-sm rounded-md text-left flex items-center justify-between gap-2"
    onclick={() => (open = !open)}
  >
    <span class={value ? '' : 'text-surface-400'}>
      {value ? formatDisplay(value) : 'Pick a date'}
    </span>
    <svg
      xmlns="http://www.w3.org/2000/svg"
      class="w-4 h-4 text-surface-500 shrink-0"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <rect x="3" y="4" width="18" height="18" rx="2" />
      <path d="M16 2v4M8 2v4M3 10h18" />
    </svg>
  </button>

  {#if open}
    <div
      class="absolute z-50 mt-1 w-[280px] rounded-md border border-surface-300 dark:border-surface-700 bg-surface-50 dark:bg-surface-900 shadow-lg p-3"
      role="dialog"
      aria-label="Pick a date"
    >
      <!-- Header: prev | month-year label | next -->
      <div class="flex items-center justify-between mb-2">
        <button
          type="button"
          class="btn btn-sm preset-tonal-surface w-8 h-8 p-0"
          aria-label="Previous month"
          onclick={prevMonth}
        >‹</button>
        <span class="text-sm font-medium">{monthLabel}</span>
        <button
          type="button"
          class="btn btn-sm preset-tonal-surface w-8 h-8 p-0"
          aria-label="Next month"
          onclick={nextMonth}
        >›</button>
      </div>

      <!-- Weekday header row (Mon-first). -->
      <div class="grid grid-cols-7 gap-0.5 mb-1">
        {#each weekdayLabels as wd (wd)}
          <div class="text-[10px] uppercase tracking-wide text-surface-500 text-center py-1">
            {wd}
          </div>
        {/each}
      </div>

      <!-- 6×7 grid.  Out-of-month days stay clickable but
           dimmed so the user can drag selection across month
           boundaries without breaking flow. -->
      <div class="grid grid-cols-7 gap-0.5">
        {#each grid as d (d.getTime())}
          {@const inMonth = d.getMonth() === view.getMonth()}
          {@const isToday = sameDay(d, today)}
          {@const isSelected = sameDay(d, selected)}
          <button
            type="button"
            class="text-sm h-8 rounded-md flex items-center justify-center {isSelected
              ? 'bg-primary-500 text-white font-semibold'
              : isToday
                ? 'border border-primary-500 text-primary-500'
                : inMonth
                  ? 'hover:bg-surface-200 dark:hover:bg-surface-800'
                  : 'text-surface-400 hover:bg-surface-200 dark:hover:bg-surface-800'}"
            onclick={() => pick(d)}
          >
            {d.getDate()}
          </button>
        {/each}
      </div>

      <!-- Footer: Today shortcut.  Clear button is
           deliberately omitted — the field is required for
           events, so giving the user a button to wipe it would
           just produce form-validation errors on save. -->
      <div class="flex justify-end mt-2">
        <button
          type="button"
          class="btn btn-sm preset-tonal-primary"
          onclick={goToday}
        >Today</button>
      </div>
    </div>
  {/if}
</div>
