<script lang="ts">
  /**
   * TalkView — sidebar-routed full-pane Nextcloud Talk room list.
   *
   * Mirrors `FilesView`'s shape (header + scrollable body + footer)
   * so the integration views feel like one app. Three actions per
   * room: open in browser, share link in email, and (at the top)
   * "+ New room" to create a fresh one.
   *
   * # Why we don't cache rooms
   *
   * Talk's `/room` endpoint is cheap (few KB) and unread counts go
   * stale the second a colleague sends a message. We refetch on a
   * 30s timer plus on focus, and on demand via the refresh button.
   * No SQLite layer — the room list is purely a UI cache.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { onDestroy, onMount } from 'svelte'
  import { formatError } from './errors'
  import CreateTalkRoomModal, { type TalkRoom } from './CreateTalkRoomModal.svelte'
  import type { ComposeInitial } from './Compose.svelte'

  interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
  }

  interface Props {
    onclose: () => void
    /** Open Compose with the given prefill (used for "Share link"). */
    oncompose: (initial: ComposeInitial) => void
  }
  const { onclose, oncompose }: Props = $props()

  let accounts = $state<NextcloudAccount[]>([])
  let accountId = $state('')
  let rooms = $state<TalkRoom[]>([])
  let loading = $state(false)
  let error = $state('')
  let showCreate = $state(false)

  // Periodic refresh — 30s is the same cadence the Nextcloud web UI
  // polls at. Long enough to be cheap, short enough that the unread
  // counts don't lie for more than half a minute.
  const REFRESH_INTERVAL_MS = 30_000
  let pollTimer: number | null = null

  onMount(async () => {
    await loadAccounts()
  })

  onDestroy(() => {
    if (pollTimer !== null) window.clearInterval(pollTimer)
  })

  async function loadAccounts() {
    try {
      const list = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      accounts = list
      if (list.length === 1 && !accountId) {
        accountId = list[0].id
        await refresh()
        startPolling()
      }
    } catch (e) {
      error = formatError(e) || 'Failed to load Nextcloud accounts'
    }
  }

  async function selectAccount(id: string) {
    accountId = id
    rooms = []
    await refresh()
    startPolling()
  }

  function startPolling() {
    if (pollTimer !== null) window.clearInterval(pollTimer)
    pollTimer = window.setInterval(() => {
      // Silent refresh — don't flash the loading indicator on the
      // periodic ticks. Errors stay quiet too; the next tick retries.
      void refresh({ silent: true })
    }, REFRESH_INTERVAL_MS)
  }

  async function refresh(opts: { silent?: boolean } = {}) {
    if (!accountId) return
    if (!opts.silent) loading = true
    if (!opts.silent) error = ''
    try {
      const list = await invoke<TalkRoom[]>('list_talk_rooms', { ncId: accountId })
      // Sort by last activity desc so the freshest conversation is on
      // top. Rooms with zero last_activity (newly created, no messages
      // yet) sink to the bottom.
      list.sort((a, b) => b.last_activity - a.last_activity)
      rooms = list
    } catch (e) {
      if (!opts.silent) error = formatError(e) || 'Failed to load Talk rooms'
    } finally {
      if (!opts.silent) loading = false
    }
  }

  function openRoom(room: TalkRoom) {
    void invoke('open_url', { url: room.web_url })
  }

  /**
   * Open Compose with a "Join the Talk room: <link>" block in the
   * body. Reuses the same `ComposeInitial.talkLink` field that the
   * MailView "Talk" action populates — the rendered HTML lives in
   * Compose's `initialBodyHtml` so the format stays consistent.
   */
  function shareRoom(room: TalkRoom) {
    oncompose({
      subject: `Join Talk: ${room.display_name}`,
      talkLink: { name: room.display_name, url: room.web_url },
    })
  }

  function onRoomCreated(room: TalkRoom) {
    // Optimistically prepend the new room — the next periodic refresh
    // (or the user's manual refresh) will reconcile any drift.
    rooms = [room, ...rooms.filter((r) => r.token !== room.token)]
  }

  function formatRelative(unix: number): string {
    if (!unix) return ''
    const now = Date.now() / 1000
    const delta = now - unix
    if (delta < 60) return 'just now'
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`
    if (delta < 7 * 86400) return `${Math.floor(delta / 86400)}d ago`
    return new Date(unix * 1000).toLocaleDateString()
  }

  function roomTypeIcon(t: TalkRoom['room_type']): string {
    if (t === 'one_to_one') return '\u{1F464}' // 👤
    if (t === 'public') return '\u{1F310}' // 🌐
    if (t === 'changelog') return '\u{1F4DC}' // 📜
    return '\u{1F465}' // 👥
  }
</script>

<div class="h-full flex flex-col bg-surface-50 dark:bg-surface-900">
  <div
    class="flex items-center justify-between px-6 py-3 border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800"
  >
    <h2 class="text-xl font-semibold">Talk Rooms</h2>
    <div class="flex items-center gap-2">
      <button
        class="btn preset-filled-primary-500 text-sm"
        disabled={!accountId}
        onclick={() => (showCreate = true)}
      >+ New room</button>
      <button
        class="btn preset-tonal-surface text-sm"
        disabled={!accountId || loading}
        onclick={() => refresh()}
        title="Refresh room list"
      >{loading ? 'Refreshing…' : '↻ Refresh'}</button>
      <button class="btn preset-tonal-surface text-sm" onclick={onclose}>Close</button>
    </div>
  </div>

  {#if accounts.length === 0}
    <div class="p-6 text-sm text-surface-500">
      No Nextcloud account connected. Add one under
      <strong>Settings → Nextcloud</strong> first.
    </div>
  {:else}
    {#if accounts.length > 1}
      <div class="px-5 py-2 border-b border-surface-200 dark:border-surface-700 flex items-center gap-2">
        <label for="talk-account" class="text-xs text-surface-500">Account</label>
        <select
          id="talk-account"
          class="select text-sm"
          value={accountId}
          onchange={(e) => selectAccount((e.target as HTMLSelectElement).value)}
        >
          <option value="" disabled>Choose an account</option>
          {#each accounts as acc (acc.id)}
            <option value={acc.id}>{acc.display_name ?? acc.username} ({acc.server_url})</option>
          {/each}
        </select>
      </div>
    {/if}

    {#if error}
      <p class="px-5 py-2 text-sm text-red-500">{error}</p>
    {/if}

    {#if !accountId}
      <p class="p-6 text-sm text-surface-500">Pick an account to view its Talk rooms.</p>
    {:else if loading && rooms.length === 0}
      <p class="p-6 text-sm text-surface-500">Loading rooms…</p>
    {:else if rooms.length === 0}
      <div class="p-6 text-sm text-surface-500">
        No Talk rooms yet. Click <strong>+ New room</strong> to start your first conversation.
      </div>
    {:else}
      <ul class="flex-1 overflow-y-auto divide-y divide-surface-200 dark:divide-surface-800">
        {#each rooms as room (room.token)}
          <li class="px-5 py-3 flex items-center gap-3 hover:bg-surface-100 dark:hover:bg-surface-800">
            <span class="text-xl flex-shrink-0">{roomTypeIcon(room.room_type)}</span>

            <div class="flex-1 min-w-0">
              <div class="flex items-center gap-2">
                <span class="font-medium truncate">{room.display_name}</span>
                {#if room.unread_messages > 0}
                  <span
                    class="badge text-xs flex-shrink-0
                           {room.unread_mention ? 'preset-filled-error-500' : 'preset-filled-primary-500'}"
                    title={room.unread_mention ? 'You were mentioned' : 'Unread messages'}
                  >{room.unread_messages}</span>
                {/if}
              </div>
              <p class="text-xs text-surface-500 truncate">
                {formatRelative(room.last_activity)}
              </p>
            </div>

            <button
              class="btn preset-outlined-surface-500 text-xs"
              onclick={() => shareRoom(room)}
              title="Open a new mail with this room's join link"
            >🔗 Share link</button>
            <button
              class="btn preset-filled-primary-500 text-xs"
              onclick={() => openRoom(room)}
              title="Open this Talk room in your browser"
            >Open ↗</button>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</div>

{#if showCreate && accountId}
  <CreateTalkRoomModal
    ncId={accountId}
    onclose={() => (showCreate = false)}
    oncreated={onRoomCreated}
  />
{/if}
