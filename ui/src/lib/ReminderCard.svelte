<script lang="ts">
  /**
   * ReminderCard — small in-app overlay that pops up when the
   * background scanner fires an `event-reminder` (#203).
   *
   * Why this lives next to the OS notification rather than
   * inside it:
   *   * OS notification action buttons are per-platform: Linux
   *     libnotify supports them but the click-back IPC is
   *     fragile, macOS supports them through the Tauri plugin
   *     but only in `NSUserNotification` mode, Windows toast
   *     actions need WinRT.
   *   * Action buttons that work everywhere is too much fiddly
   *     plumbing for the value.  An in-app card with the same
   *     buttons shows when the user comes to the app — that's
   *     when they'd act on the reminder anyway.
   *
   * Auto-dismiss after 30 s so a long string of reminders
   * doesn't accumulate; the OS notification persists in the
   * notification center independently for any reminder the
   * user missed while away from the keyboard.
   */

  import { invoke } from '@tauri-apps/api/core'
  import Icon from './Icon.svelte'

  /** Same shape as the backend `EventReminderPayload`. */
  export type EventReminder = {
    eventId: string
    uid: string
    summary: string
    start: string
    end: string
    location: string | null
    attendees: string[]
    meetingUrl: string | null
    minutesBefore: number
  }

  interface Props {
    reminder: EventReminder
    /** Switch app to the calendar view and open this event in the
     *  editor.  Wired by App.svelte to a function that flips
     *  `currentView = 'calendar'` and threads the event id into
     *  CalendarView's `focusEventId` prop. */
    onShowEvent: (eventId: string) => void
    /** Hide the card.  Also calls `dismiss_event_reminder` on
     *  the backend so the same reminder doesn't fire again on
     *  the next sync tick. */
    ondismiss: () => void
  }
  let { reminder, onShowEvent, ondismiss }: Props = $props()

  /** "now" / "in 5 min" / "in 1h 30m" — same shape the OS
   *  notification body uses, repeated on the card so the user
   *  doesn't have to re-read the toast. */
  function formatLeadTime(min: number): string {
    if (min <= 0) return 'now'
    if (min < 60) return `in ${min} min`
    const hours = Math.floor(min / 60)
    const remainder = min % 60
    if (remainder === 0) return `in ${hours} hour${hours === 1 ? '' : 's'}`
    return `in ${hours}h ${remainder}m`
  }

  /** "14:00" in the user's local zone — keeps the card concise
   *  versus the full RFC 3339 the payload carries. */
  function formatLocalTime(iso: string): string {
    return new Date(iso).toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
    })
  }

  /** First three attendees + "+N more" tail.  OS notification
   *  bodies wrap badly on Linux past three lines, but the
   *  in-app card has more room — we still cap at three so the
   *  layout stays tight. */
  function formatAttendees(list: string[]): string {
    if (list.length === 0) return ''
    const first = list.slice(0, 3).join(', ')
    return list.length > 3 ? `${first} +${list.length - 3} more` : first
  }

  function dismiss() {
    void invoke('dismiss_event_reminder', { uid: reminder.uid }).catch(
      () => {},
    )
    ondismiss()
  }

  function showEvent() {
    onShowEvent(reminder.eventId)
    // Keep the dismiss-tracking in sync — the user is now seeing
    // the full event view, no need to nag them again.
    void invoke('dismiss_event_reminder', { uid: reminder.uid }).catch(
      () => {},
    )
    ondismiss()
  }

  function joinMeeting() {
    if (!reminder.meetingUrl) return
    void invoke('open_url', { url: reminder.meetingUrl }).catch((err) =>
      console.warn('open_url for event reminder failed', err),
    )
    void invoke('dismiss_event_reminder', { uid: reminder.uid }).catch(
      () => {},
    )
    ondismiss()
  }

  // Auto-dismiss after 30 s.  $effect with a returned cleanup so
  // the timer is cancelled if the parent re-renders us with a
  // new reminder before the old one expires.
  $effect(() => {
    const id = setTimeout(() => ondismiss(), 30_000)
    return () => clearTimeout(id)
  })
</script>

<!--
  Bottom-right overlay, fixed position, doesn't block the rest
  of the UI.  Card style mirrors the toast / settings cards so
  it reads as part of the app, not a foreign popup.
-->
<div
  class="fixed bottom-4 right-4 z-50 max-w-sm w-[22rem] rounded-xl shadow-xl
         bg-surface-50 dark:bg-surface-800 border border-surface-300/60 dark:border-surface-600/60"
  role="alert"
  aria-live="polite"
>
  <div class="p-4">
    <!-- Title row + close X -->
    <div class="flex items-start justify-between gap-2 mb-1">
      <div class="flex items-center gap-2 min-w-0">
        <span class="text-primary-500 shrink-0">
          <Icon name={reminder.meetingUrl ? 'meetings' : 'calendar'} size={18} />
        </span>
        <h3 class="font-semibold truncate">{reminder.summary || 'Event'}</h3>
      </div>
      <button
        type="button"
        class="p-1 rounded-md text-surface-500 hover:text-surface-900 hover:bg-surface-200 dark:hover:text-surface-100 dark:hover:bg-surface-700 transition-colors shrink-0"
        onclick={dismiss}
        aria-label="Dismiss reminder"
        title="Dismiss"
      >
        <Icon name="close" size={16} />
      </button>
    </div>

    <!-- Lead-time + start-time line -->
    <p class="text-sm text-surface-600 dark:text-surface-300 mb-2">
      Starts {formatLeadTime(reminder.minutesBefore)} ·
      <span class="font-mono">{formatLocalTime(reminder.start)}–{formatLocalTime(reminder.end)}</span>
    </p>

    <!-- Detail rows: location, attendees.  Each one is shown
         only when the event has it, so a sparse event doesn't
         waste card space. -->
    {#if reminder.location}
      <div class="flex items-start gap-2 text-xs text-surface-600 dark:text-surface-300 mb-1">
        <span class="text-surface-500 mt-0.5 shrink-0"><Icon name="location" size={12} /></span>
        <span class="break-words">{reminder.location}</span>
      </div>
    {/if}
    {#if reminder.attendees.length > 0}
      <div class="flex items-start gap-2 text-xs text-surface-600 dark:text-surface-300 mb-3">
        <span class="text-surface-500 mt-0.5 shrink-0"><Icon name="contacts" size={12} /></span>
        <span class="break-words">{formatAttendees(reminder.attendees)}</span>
      </div>
    {:else}
      <div class="mb-3"></div>
    {/if}

    <!-- Action buttons. -->
    <div class="flex flex-wrap items-center gap-2">
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
  </div>
</div>
