<script lang="ts">
  /**
   * IconRail — the always-visible left-edge shell nav.
   *
   * Two-tier layout:
   *
   * 1. Account avatars at the top (initials bubbles). Clicking one
   *    selects that account. When the user has more than one account
   *    there's also an "All" bubble that turns on unified-inbox mode
   *    — same semantics as the `__all__` sentinel the old Sidebar
   *    dropdown used, but expressed as a chip you can stab at from
   *    any view.
   *
   * 2. View nav below a divider: Mail, Contacts, Calendar, Files,
   *    Talk — and Settings pinned at the bottom via a `mt-auto`
   *    spacer. Active state highlights the current `currentView`;
   *    clicks drop back to the parent via `onselectview`.
   *
   * The rail is mounted once at the App-shell level (outside any
   *  `currentView` branch) so switching between Mail and the
   *  integration views doesn't unmount / remount the rail — avatars,
   *  Talk unread-badge polling, and the active-view ring all stay
   *  smooth across navigation.
   *
   * Talk unread polling lives here (moved from the old Sidebar)
   * because the Talk badge belongs to the rail's Talk icon now; the
   * previous home inside Sidebar was only correct when the Sidebar
   * was the single surface showing the integration nav.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { listen, type UnlistenFn } from '@tauri-apps/api/event'
  import { onDestroy } from 'svelte'

  interface Account {
    id: string
    display_name: string
    email: string
    /** Optional emoji avatar (issue #115) — replaces the
     *  initials bubble when set. */
    emoji?: string | null
    /** Display order for the rail; lower values render first. */
    sort_order?: number
  }

  interface TalkRoomSummary {
    unread_messages: number
    unread_mention: boolean
  }

  /** The view-nav slots the rail surfaces. Mirrors App's `View`
   *  enum for the branches the rail can reach — `loading` / `setup`
   *  are shell-level states, not rail destinations. */
  export type RailView =
    | 'inbox'
    | 'contacts'
    | 'calendar'
    | 'files'
    | 'talk'
    | 'notes'
    | 'settings'

  interface Props {
    accounts: Account[]
    /** The currently-active mail account's id. Drives the avatar
     *  ring unless `unified` is on. */
    accountId: string | null
    /** Unified-inbox mode. When true the "All" bubble wears the
     *  active ring instead of any individual avatar. */
    unified: boolean
    /** Current view. Used to ring the matching nav icon. */
    currentView: RailView | 'loading' | 'setup'
    /** Real account id → switch to that account; sentinel
     *  `'__all__'` → turn on unified-inbox mode. Same contract the
     *  old Sidebar dropdown used. */
    onselectaccount: (id: string) => void
    onselectview: (view: RailView) => void
  }
  let {
    accounts,
    accountId,
    unified,
    currentView,
    onselectaccount,
    onselectview,
  }: Props = $props()

  /** First-letter / initial-pair fallback for the avatar bubble.
   *  Mirrors the pattern AddressAutocomplete uses for contact rows
   *  so single-word and multi-word display names both look right. */
  function initials(a: Account): string {
    const src = (a.display_name || a.email || '').trim()
    if (!src) return '?'
    const parts = src.split(/\s+/).filter(Boolean)
    if (parts.length === 1) return parts[0][0].toUpperCase()
    return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase()
  }

  /** Accounts sorted by `sort_order` (#115) so the rail honours
   *  the user's chosen ordering — `id` ties keep the order
   *  stable when two rows share the same sort_order. */
  const sortedAccounts = $derived(
    [...accounts].sort((a, b) => {
      const ao = a.sort_order ?? 0
      const bo = b.sort_order ?? 0
      if (ao !== bo) return ao - bo
      return a.id.localeCompare(b.id)
    }),
  )

  // ── Per-account unread badges (#115) ────────────────────────
  // The Rust side emits `unread-count-by-account-updated` after
  // every poll, carrying a HashMap<accountId, count>.  We seed
  // the state once on mount via `get_unread_counts_by_account`
  // so the badge paints immediately, then keep it live with the
  // event subscription.
  let unreadByAccount = $state<Record<string, number>>({})
  $effect(() => {
    void invoke<Record<string, number>>('get_unread_counts_by_account')
      .then((m) => (unreadByAccount = m))
      .catch((e) => console.warn('get_unread_counts_by_account failed', e))
    let unlisten: UnlistenFn | null = null
    void listen<Record<string, number>>(
      'unread-count-by-account-updated',
      (e) => {
        unreadByAccount = e.payload ?? {}
      },
    )
      .then((fn) => (unlisten = fn))
      .catch((e) =>
        console.warn('listen unread-count-by-account-updated failed', e),
      )
    return () => {
      if (unlisten) unlisten()
    }
  })
  function unreadFor(id: string): number {
    return unreadByAccount[id] ?? 0
  }
  /** Sum of every account's unread count — drives the "All
   *  inboxes" bubble's badge so the unified view also shows a
   *  single aggregate red dot when anything's pending. */
  const totalUnread = $derived(
    Object.values(unreadByAccount).reduce((a, b) => a + b, 0),
  )

  // ── Talk unread badge ───────────────────────────────────────
  // Polls the first configured Nextcloud account every 30s and
  // aggregates unread counts + mention state across rooms. Same
  // logic that used to live in Sidebar.svelte — follows the Talk
  // rail icon now.
  let talkUnreadTotal = $state(0)
  let talkUnreadHasMention = $state(false)
  const TALK_POLL_MS = 30_000
  let talkPollTimer: number | null = null

  async function refreshTalkBadge() {
    try {
      const ncAccounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
      if (ncAccounts.length === 0) {
        talkUnreadTotal = 0
        talkUnreadHasMention = false
        return
      }
      const rooms = await invoke<TalkRoomSummary[]>('list_talk_rooms', {
        ncId: ncAccounts[0].id,
      })
      let total = 0
      let mention = false
      for (const r of rooms) {
        total += r.unread_messages
        if (r.unread_mention && r.unread_messages > 0) mention = true
      }
      talkUnreadTotal = total
      talkUnreadHasMention = mention
    } catch (e) {
      console.warn('Talk unread poll failed:', e)
    }
  }

  $effect(() => {
    void refreshTalkBadge()
    talkPollTimer = window.setInterval(refreshTalkBadge, TALK_POLL_MS)
    return () => {
      if (talkPollTimer !== null) window.clearInterval(talkPollTimer)
      talkPollTimer = null
    }
  })

  onDestroy(() => {
    if (talkPollTimer !== null) window.clearInterval(talkPollTimer)
  })

  /** Nav entries rendered below the divider. `match` is the
   *  `currentView` value that should light the ring. Kept as a
   *  plain array so re-ordering / renaming is a one-line edit. */
  interface NavEntry {
    match: RailView
    label: string
    icon: string
  }
  const MAIN_NAV: NavEntry[] = [
    { match: 'inbox', label: 'Mail', icon: '\u{1F4E7}' },        // 📧
    { match: 'contacts', label: 'Contacts', icon: '\u{1F464}' }, // 👤
    { match: 'calendar', label: 'Calendar', icon: '\u{1F4C5}' }, // 📅
    { match: 'files', label: 'Files', icon: '\u{1F4C1}' },       // 📁
    { match: 'talk', label: 'Talk', icon: '\u{1F4AC}' },         // 💬
    { match: 'notes', label: 'Notes', icon: '\u{1F4DD}' },       // 📝
  ]
</script>

<aside
  class="w-14 shrink-0 border-r border-surface-200 dark:border-surface-700
         bg-surface-100 dark:bg-surface-800 flex flex-col items-center py-2 gap-3"
>
  <!-- Account avatars. The "All" bubble only appears when the user
       has more than one account — for a single-account setup it's
       chrome with no distinct behaviour. -->
  {#if accounts.length > 1}
    <button
      class="relative w-9 h-9 rounded-full flex items-center justify-center text-xs font-semibold
             transition-colors
             {unified
               ? 'bg-primary-500 text-white ring-2 ring-offset-2 ring-offset-surface-100 dark:ring-offset-surface-800 ring-primary-500'
               : 'bg-surface-300 dark:bg-surface-700 text-surface-700 dark:text-surface-300 hover:bg-surface-400 dark:hover:bg-surface-600'}"
      title="All inboxes (unified)"
      aria-label="All inboxes"
      onclick={() => onselectaccount('__all__')}
    >
      <!-- Inbox glyph matches the Mail rail icon + the "All
           Inboxes" folder entry, so the "this is the aggregate"
           meaning carries across every surface the user sees. The
           wrapper has a fixed 28×28 box so the composite (inbox +
           🌐 corner badge) sits as one centered unit inside the
           36×36 bubble — without the explicit size, the badge
           protrudes outside the wrapper and pulls the visual
           weight to the bottom-right. -->
      <span class="relative block w-7 h-7">
        <!-- The emoji's intrinsic font metrics put extra ascent above
             the visible glyph, so a flex-centered emoji renders low.
             A 2px upward translate lands the visible centre on the
             box's geometric centre across Linux/macOS/Windows emoji
             fonts. -->
        <span class="absolute inset-0 flex items-center justify-center text-lg leading-none -translate-y-[6px]">&#x1F4E5;</span>
        <span class="absolute bottom-0 right-0 text-[0.65rem] leading-none drop-shadow">&#x1F310;</span>
      </span>
      {#if totalUnread > 0}
        <span
          class="absolute -top-0.5 -right-0.5 min-w-4 h-4 px-1 text-[10px] rounded-full bg-red-500 text-white flex items-center justify-center font-semibold ring-2 ring-surface-100 dark:ring-surface-800"
          title={`${totalUnread} unread across all inboxes`}
        >{totalUnread > 99 ? '99+' : totalUnread}</span>
      {/if}
    </button>
  {/if}
  {#each sortedAccounts as a (a.id)}
    {@const active = !unified && accountId === a.id}
    {@const unread = unreadFor(a.id)}
    <button
      class="relative w-9 h-9 rounded-full flex items-center justify-center text-xs font-semibold
             transition-colors
             {active
               ? 'bg-primary-500 text-white ring-2 ring-offset-2 ring-offset-surface-100 dark:ring-offset-surface-800 ring-primary-500'
               : 'bg-surface-300 dark:bg-surface-700 text-surface-700 dark:text-surface-300 hover:bg-surface-400 dark:hover:bg-surface-600'}"
      title={`${a.display_name || a.email}${unread > 0 ? ` — ${unread} unread` : ''}`}
      aria-label="Switch to {a.display_name || a.email}"
      onclick={() => onselectaccount(a.id)}
    >
      {#if a.emoji && a.emoji.trim()}
        <span class="text-lg leading-none">{a.emoji}</span>
      {:else}
        {initials(a)}
      {/if}
      {#if unread > 0}
        <!-- Red unread badge (#115).  Pinned top-right and
             ringed in the rail's surface colour so the badge
             reads cleanly over both light and dark themes
             without doubling as part of the avatar bubble's
             outline. -->
        <span
          class="absolute -top-0.5 -right-0.5 min-w-4 h-4 px-1 text-[10px] rounded-full bg-red-500 text-white flex items-center justify-center font-semibold ring-2 ring-surface-100 dark:ring-surface-800"
        >{unread > 99 ? '99+' : unread}</span>
      {/if}
    </button>
  {/each}

  <!-- Divider between account bubbles and the view nav. Only
       renders when there's at least one account so the very-first-
       launch empty state doesn't show an orphan line. -->
  {#if accounts.length > 0}
    <div class="w-6 h-px my-1 bg-surface-300 dark:bg-surface-600" aria-hidden="true"></div>
  {/if}

  {#each MAIN_NAV as entry (entry.match)}
    {@const active = currentView === entry.match}
    <button
      class="w-9 h-9 rounded-md flex items-center justify-center text-lg transition-colors relative
             {active
               ? 'bg-primary-500/15 text-primary-500'
               : 'text-surface-600 dark:text-surface-300 hover:bg-surface-200 dark:hover:bg-surface-700'}"
      title={entry.label}
      aria-label={entry.label}
      onclick={() => onselectview(entry.match)}
    >
      <span>{entry.icon}</span>
      <!-- Talk-specific unread badge, pinned to the top-right
           corner of the icon. Red tint when there's a mention,
           primary otherwise. -->
      {#if entry.match === 'talk' && talkUnreadTotal > 0}
        <span
          class="absolute -top-0.5 -right-0.5 min-w-4 h-4 px-1 text-[10px] rounded-full
                 flex items-center justify-center font-semibold
                 {talkUnreadHasMention
                   ? 'bg-red-500 text-white'
                   : 'bg-primary-500 text-white'}"
          title={talkUnreadHasMention ? 'You were mentioned' : 'Unread Talk messages'}
        >{talkUnreadTotal}</span>
      {/if}
    </button>
  {/each}

  <!-- Settings pinned to the bottom. `mt-auto` on the wrapper
       pushes the remaining flex space between the main nav and
       this slot. -->
  <div class="mt-auto">
    <button
      class="w-9 h-9 rounded-md flex items-center justify-center text-lg transition-colors
             {currentView === 'settings'
               ? 'bg-primary-500/15 text-primary-500'
               : 'text-surface-600 dark:text-surface-300 hover:bg-surface-200 dark:hover:bg-surface-700'}"
      title="Settings"
      aria-label="Settings"
      onclick={() => onselectview('settings')}
    >
      <span>&#9881;</span>
    </button>
  </div>
</aside>
