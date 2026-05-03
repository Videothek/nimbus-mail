<script lang="ts">
  /**
   * StandaloneReminder — the *whole* contents of the popped-out
   * reminder window (#203 follow-up).  Mounted by `main.ts` when
   * the URL carries `?view=reminder&key=…`.
   *
   * Reads the payload synchronously from `localStorage` (the
   * spawning window stashed it there before opening this window —
   * same origin, so localStorage is shared).  Closes the window
   * if the payload is missing or malformed; otherwise shows
   * title / time / location / attendees and three action paths:
   *
   *   * Show event — emit a `reminder-show-event` Tauri event so
   *     the main window flips to the calendar view and opens
   *     this event's editor, then close ourselves.
   *   * Join meeting — open the meeting URL via `open_url`,
   *     dismiss + close.
   *   * Snooze — pick a "Remind me…" option from the
   *     dropdown; calls `snooze_event_reminder` with a UTC
   *     deadline computed from the choice and the event's
   *     start, then closes the window.  The next scan tick
   *     re-fires a synthetic reminder once that moment passes.
   *   * Dismiss — call `dismiss_event_reminder`, close.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { emit } from '@tauri-apps/api/event'
  import { onMount, onDestroy } from 'svelte'
  import Icon from './Icon.svelte'
  import { applyTheme, installSystemModeListener, type ThemeMode } from './theme'
  import {
    takeReminderPopoutPayload,
    type EventReminderPayload,
  } from './reminderPopupWindow'

  interface Props {
    popoutKey: string
  }
  let { popoutKey }: Props = $props()

  let reminder = $state<EventReminderPayload | null>(null)
  let snoozeChoice = $state<SnoozeChoice>('')
  let unlistenSystemMode: (() => void) | null = null

  // Snooze options.  Each label maps to a callback that returns
  // the `snooze_until` UTC date (computed when the user clicks
  // Snooze, not at render time, so a long-open popup doesn't
  // schedule a stale moment).  An option is offered only when
  // its target moment is in the future for *this* event — e.g.
  // "15 min before" is hidden once the event starts in less
  // than 15 minutes.
  type SnoozeChoice =
    | ''
    | 'before-15'
    | 'before-10'
    | 'before-5'
    | 'at-start'

  function snoozeTargetUtc(choice: SnoozeChoice, eventStartIso: string): Date | null {
    if (!choice) return null
    const start = new Date(eventStartIso)
    switch (choice) {
      case 'before-15': return new Date(start.getTime() - 15 * 60_000)
      case 'before-10': return new Date(start.getTime() - 10 * 60_000)
      case 'before-5':  return new Date(start.getTime() - 5 * 60_000)
      case 'at-start':  return start
    }
  }

  function isSnoozeOptionAvailable(choice: SnoozeChoice, eventStartIso: string): boolean {
    if (!choice) return true
    const target = snoozeTargetUtc(choice, eventStartIso)
    if (!target) return false
    // Need a meaningful gap (>30 s) between now and the target,
    // otherwise the snooze would fire on the very next tick and
    // the option is just noise.
    return target.getTime() - Date.now() > 30_000
  }

  /** "now" / "in 5 min" / "in 1h 30m" — same wording the OS
   *  toast body uses, repeated on the popup so the user
   *  doesn't have to re-read the toast. */
  function formatLeadTime(min: number): string {
    if (min <= 0) return 'now'
    if (min < 60) return `in ${min} min`
    const hours = Math.floor(min / 60)
    const remainder = min % 60
    if (remainder === 0) return `in ${hours} hour${hours === 1 ? '' : 's'}`
    return `in ${hours}h ${remainder}m`
  }

  /** "14:00" in the user's local zone. */
  function formatLocalTime(iso: string): string {
    return new Date(iso).toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
    })
  }

  /** First three attendees + "+N more" tail. */
  function formatAttendees(list: string[]): string {
    if (list.length === 0) return ''
    const first = list.slice(0, 3).join(', ')
    return list.length > 3 ? `${first} +${list.length - 3} more` : first
  }

  async function closeSelf() {
    const { getCurrentWebviewWindow } = await import(
      '@tauri-apps/api/webviewWindow'
    )
    void getCurrentWebviewWindow().close()
  }

  async function dismiss() {
    if (reminder) {
      void invoke('dismiss_event_reminder', { uid: reminder.uid }).catch(
        () => {},
      )
    }
    await closeSelf()
  }

  async function showEvent() {
    if (!reminder) return await closeSelf()
    // Two steps:
    //   1. Bring the main window forward via the existing
    //      `show_main_window_cmd` Rust IPC.  Calling from
    //      Rust avoids the Win32 SetForegroundWindow lock
    //      that bites JS-side `setFocus()` from a non-
    //      foreground window — important for the "main
    //      window is hidden in the system tray" case where
    //      a JS focus call from the popup would silently
    //      no-op.
    //   2. Emit the cross-window `reminder-show-event`
    //      event so App.svelte's listener flips the view to
    //      calendar and threads the event id through to
    //      CalendarView.
    void invoke('show_main_window_cmd').catch((err) =>
      console.warn('show_main_window_cmd failed', err),
    )
    await emit('reminder-show-event', { eventId: reminder.eventId })
    void invoke('dismiss_event_reminder', { uid: reminder.uid }).catch(
      () => {},
    )
    await closeSelf()
  }

  async function joinMeeting() {
    if (!reminder?.meetingUrl) return await closeSelf()
    void invoke('open_url', { url: reminder.meetingUrl }).catch((err) =>
      console.warn('open_url for reminder popup failed', err),
    )
    void invoke('dismiss_event_reminder', { uid: reminder.uid }).catch(
      () => {},
    )
    await closeSelf()
  }

  async function snooze() {
    if (!reminder || !snoozeChoice) return
    const target = snoozeTargetUtc(snoozeChoice, reminder.start)
    if (!target) return
    void invoke('snooze_event_reminder', {
      uid: reminder.uid,
      snoozeUntilIso: target.toISOString(),
    }).catch((err) => console.warn('snooze_event_reminder failed', err))
    await closeSelf()
  }

  onMount(() => {
    // Theme bootstrap so the popout matches the user's chosen
    // Skeleton theme + light/dark mode.  Same shape as
    // StandaloneCompose / StandaloneMail — pull the prefs from
    // the backend, set the `data-theme` + `data-mode`
    // attributes on `<html>`, then install the system-mode
    // listener so a runtime theme switch in the main app also
    // ripples to this window.
    void (async () => {
      try {
        const prefs = await invoke<{
          theme_name: string
          theme_mode: ThemeMode
        }>('get_app_settings')
        applyTheme(prefs.theme_name, prefs.theme_mode)
        unlistenSystemMode = installSystemModeListener(
          prefs.theme_mode,
          prefs.theme_name,
        )
      } catch (e) {
        console.warn('get_app_settings failed in standalone reminder', e)
      }
    })()

    const payload = takeReminderPopoutPayload(popoutKey)
    if (!payload) {
      // Stale / missing payload — close immediately so we don't
      // leave a useless empty popup on screen.
      void closeSelf()
      return
    }
    reminder = payload
  })

  onDestroy(() => {
    unlistenSystemMode?.()
  })
</script>

<svelte:head>
  <title>{reminder?.summary || 'Nimbus Reminder'}</title>
</svelte:head>

<!--
  Whole-window layout: the popup IS the card.  Drag-region in the
  top bar so the user can reposition the window if our auto-bottom-
  right placement collides with their workflow.
-->
<div class="h-screen w-screen flex flex-col bg-surface-50 dark:bg-surface-900 text-surface-900 dark:text-surface-100 overflow-hidden">
  {#if !reminder}
    <div class="h-full w-full flex items-center justify-center text-sm text-surface-500">
      Loading…
    </div>
  {:else}
    <!-- Drag region + close X.  Without window decorations we
         have to provide our own way to move the window. -->
    <div
      data-tauri-drag-region
      class="flex items-center justify-between px-3 py-2 border-b border-surface-300/60 dark:border-surface-700/60 bg-surface-100 dark:bg-surface-800 cursor-move select-none"
    >
      <div class="flex items-center gap-2 min-w-0" data-tauri-drag-region>
        <span class="text-primary-500 shrink-0">
          <Icon name={reminder.meetingUrl ? 'meetings' : 'calendar'} size={16} />
        </span>
        <span class="text-xs uppercase tracking-wide text-surface-500 truncate">
          Reminder · {formatLeadTime(reminder.minutesBefore)}
        </span>
      </div>
      <button
        type="button"
        class="p-1 rounded-md text-surface-500 hover:text-surface-900 hover:bg-surface-200 dark:hover:text-surface-100 dark:hover:bg-surface-700 transition-colors"
        onclick={dismiss}
        aria-label="Dismiss reminder"
        title="Dismiss"
      >
        <Icon name="close" size={14} />
      </button>
    </div>

    <div class="flex-1 overflow-auto p-4 flex flex-col gap-3">
      <!-- Title row. -->
      <h1 class="text-base font-semibold leading-snug wrap-break-word">
        {reminder.summary || 'Event'}
      </h1>

      <!-- Time-slot row.  Always present. -->
      <div class="flex items-center gap-2 text-sm text-surface-700 dark:text-surface-300">
        <span class="text-surface-500"><Icon name="time" size={14} /></span>
        <span class="font-mono">
          {formatLocalTime(reminder.start)}–{formatLocalTime(reminder.end)}
        </span>
      </div>

      <!-- Location.  Hidden when the event has none. -->
      {#if reminder.location}
        <div class="flex items-start gap-2 text-sm text-surface-700 dark:text-surface-300">
          <span class="text-surface-500 mt-0.5 shrink-0"><Icon name="location" size={14} /></span>
          <span class="wrap-break-word min-w-0">{reminder.location}</span>
        </div>
      {/if}

      <!-- Attendees.  First three + "+N more". -->
      {#if reminder.attendees.length > 0}
        <div class="flex items-start gap-2 text-sm text-surface-700 dark:text-surface-300">
          <span class="text-surface-500 mt-0.5 shrink-0"><Icon name="contacts" size={14} /></span>
          <span class="wrap-break-word min-w-0">{formatAttendees(reminder.attendees)}</span>
        </div>
      {/if}

      <!-- Snooze dropdown + button.  Each preset is hidden when
           its target moment isn't usefully far in the future. -->
      <div class="flex items-center gap-2 mt-1">
        <span class="text-surface-500 shrink-0"><Icon name="snooze" size={14} /></span>
        <select
          class="select px-2 py-1 text-sm rounded-md flex-1 min-w-0"
          bind:value={snoozeChoice}
          aria-label="Remind me"
        >
          <!-- `hidden` + `disabled` keep "Remind me…" as the
               closed-state placeholder text without surfacing
               it as a clickable option in the open dropdown.
               All real options are left-aligned at the same
               column as the placeholder — no numeric padding
               so the labels start flush. -->
          <option value="" hidden disabled selected>Remind me…</option>
          {#if isSnoozeOptionAvailable('before-15', reminder.start)}
            <option value="before-15">15 min before</option>
          {/if}
          {#if isSnoozeOptionAvailable('before-10', reminder.start)}
            <option value="before-10">10 min before</option>
          {/if}
          {#if isSnoozeOptionAvailable('before-5', reminder.start)}
            <option value="before-5">5 min before</option>
          {/if}
          {#if isSnoozeOptionAvailable('at-start', reminder.start)}
            <option value="at-start">At event start</option>
          {/if}
        </select>
        <button
          type="button"
          class="btn btn-sm preset-outlined-surface-500"
          disabled={!snoozeChoice}
          onclick={snooze}
        >
          Snooze
        </button>
      </div>
    </div>

    <!-- Footer action row. -->
    <div class="flex flex-wrap items-center justify-end gap-2 px-4 py-3 border-t border-surface-300/60 dark:border-surface-700/60 bg-surface-100 dark:bg-surface-800">
      <button
        type="button"
        class="btn btn-sm preset-outlined-surface-500 flex items-center gap-1.5"
        onclick={showEvent}
      >
        <Icon name="open-on-desktop" size={14} />
        Show event
      </button>
      {#if reminder.meetingUrl}
        <button
          type="button"
          class="btn btn-sm preset-filled-primary-500 flex items-center gap-1.5"
          onclick={joinMeeting}
        >
          <Icon name="open-link" size={14} />
          Join meeting
        </button>
      {/if}
    </div>
  {/if}
</div>
