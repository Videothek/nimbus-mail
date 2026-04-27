<script lang="ts">
  /**
   * MailList — the middle panel listing message envelopes for a folder.
   *
   * On mount (and whenever the account/folder changes) it calls the
   * `fetch_envelopes` Tauri command, which opens an IMAP connection,
   * selects the folder, and fetches the newest N envelopes.
   *
   * Envelopes are lightweight — just sender, subject, date, flags —
   * which is why they're fast enough to list. Clicking a row fires
   * `onselect(uid)` so the parent can swap MailView to that message.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import { openMailInStandaloneWindow } from './standaloneMailWindow'

  // ── Props ───────────────────────────────────────────────────
  interface EmailEnvelope {
    uid: number
    folder: string
    from: string
    subject: string
    date: string      // RFC 3339 string (serde serialises DateTime<Utc> this way)
    is_read: boolean
    is_starred: boolean
    /** Owning account id. Always populated for envelopes read out of
        the cache; left empty for envelopes coming straight from the
        IMAP/JMAP clients (those paths don't surface to the UI). */
    account_id: string
  }

  /** Slim account row used to render the account label on each row in
      unified mode. We only need the id + display info. */
  interface Account {
    id: string
    display_name: string
    email: string
  }

  interface Props {
    /** Required when `unified` is true; otherwise unused. The list
        looks up each row's `account_id` here to render a short label. */
    accounts?: Account[]
    accountId: string
    folder?: string
    /** Aggregate INBOX across every account instead of fetching for a
        single account. The list shows an extra account label per row
        and reports the row's `account_id` back through `onselect`. */
    unified?: boolean
    selectedUid: number | null
    /** Bumped by the parent to force a network re-fetch (manual refresh). */
    refreshToken?: number
    /** `accountId` is passed back when in unified mode so the parent
        can route the open-message action to the right account. In
        single-account mode it's omitted (the active account is implicit). */
    onselect: (uid: number, accountId?: string) => void
    /** Bindable mirror of the rendered envelope list.  Lets the
        parent peek at "what's currently shown" without re-fetching —
        used by the auto-advance-after-delete logic to pick the next
        UID below the removed row. */
    envelopes?: EmailEnvelope[]
  }
  let {
    accounts = [],
    accountId,
    folder = 'INBOX',
    unified = false,
    selectedUid,
    refreshToken = 0,
    onselect,
    envelopes = $bindable([]),
  }: Props = $props()

  /** Short label for the per-row account chip in unified mode. We
      prefer the display name and fall back to the email's local part
      so the chip stays compact even with long names. */
  function accountLabel(id: string): string {
    const a = accounts.find((x) => x.id === id)
    if (!a) return ''
    if (a.display_name) return a.display_name
    return a.email.split('@')[0] ?? a.email
  }

  // ── Fetch state ─────────────────────────────────────────────
  //
  // Two-phase load: first ask the cache (instant, offline-safe), then
  // kick off the network refresh in parallel. `loading` covers the
  // *initial* paint and is dropped as soon as either source returns.
  // `refreshing` stays true while the network call is still in flight
  // after the cache has rendered, so the UI can show a subtle hint
  // without blanking the list.
  let loading = $state(true)
  let refreshing = $state(false)
  let error = $state('')

  // ── Drag source: serialize {accountId, folder, uid} into the
  // dataTransfer payload so Sidebar's folder rows (#89) can call
  // `move_message` with the right source coordinates on drop.  The
  // custom `application/x-nimbus-mail` MIME type means the browser
  // ignores the drag for any non-Sidebar drop target (system files,
  // text fields, etc.) — no accidental no-op drops onto random UI.
  function onMailDragStart(e: DragEvent, env: EmailEnvelope) {
    if (!e.dataTransfer) return
    const srcAccountId = unified && env.account_id ? env.account_id : accountId
    const payload = JSON.stringify({
      accountId: srcAccountId,
      folder,
      uid: env.uid,
    })
    e.dataTransfer.setData('application/x-nimbus-mail', payload)
    e.dataTransfer.effectAllowed = 'move'
  }

  // Re-fetch whenever the account, folder, unified flag, or
  // refreshToken changes.
  $effect(() => {
    refreshToken
    void load(accountId, folder, unified)
  })

  async function load(id: string, f: string, isUnified: boolean) {
    loading = true
    refreshing = false
    error = ''

    // Stale-response guard helper — `id`, `f`, and `isUnified` close
    // over the call's arguments while `accountId`/`folder`/`unified`
    // refer to whatever the parent currently has.
    const stillCurrent = () =>
      isUnified === unified && (isUnified || (id === accountId && f === folder))

    // Cache first — usually instant, may return [] on cold start.
    try {
      const cached = await invoke<EmailEnvelope[]>(
        isUnified ? 'get_unified_cached_envelopes' : 'get_cached_envelopes',
        isUnified
          ? { folder: f, limit: 50 }
          : { accountId: id, folder: f, limit: 50 },
      )
      if (stillCurrent()) {
        envelopes = cached
        if (cached.length > 0) loading = false
      }
    } catch (e: any) {
      // Cache miss is not an error — just ignore and wait for network.
      console.warn('get_cached_envelopes failed:', e)
    }

    // Network refresh. Always runs, even when the cache hit, so users
    // see new mail as soon as the server responds.
    refreshing = envelopes.length > 0
    try {
      const fresh = await invoke<EmailEnvelope[]>(
        isUnified ? 'fetch_unified_envelopes' : 'fetch_envelopes',
        isUnified
          ? { folder: f, limit: 50 }
          : { accountId: id, folder: f, limit: 50 },
      )
      if (stillCurrent()) {
        envelopes = fresh
      }
    } catch (e: any) {
      if (envelopes.length === 0) {
        error = formatError(e) || 'Failed to load mail'
      } else {
        console.warn('fetch_envelopes failed (showing cached):', e)
      }
    } finally {
      loading = false
      refreshing = false
    }
  }

  // Render dates compactly: today → time, otherwise short date.
  function formatDate(iso: string): string {
    const d = new Date(iso)
    const now = new Date()
    const sameDay = d.toDateString() === now.toDateString()
    if (sameDay) {
      return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    }
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
  }

  // ── Right-click context menu ──────────────────────────────────
  // Positioned absolutely at the click coordinates. Closing it is
  // delegated to a window-level click / keydown listener so any
  // interaction outside the menu dismisses it (no overlay element
  // needed). Menu actions act on the captured `env`, not on whatever
  // is currently selected — right-clicking row B while row A is open
  // should affect row B.
  let contextMenu = $state<{
    x: number
    y: number
    env: EmailEnvelope
  } | null>(null)

  function openContextMenu(e: MouseEvent, env: EmailEnvelope) {
    e.preventDefault()
    contextMenu = { x: e.clientX, y: e.clientY, env }
  }

  function closeContextMenu() {
    contextMenu = null
  }

  $effect(() => {
    if (!contextMenu) return
    const onDocClick = () => closeContextMenu()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeContextMenu()
    }
    // `setTimeout` so the click that *opened* the menu doesn't also
    // close it on the same frame.
    const t = setTimeout(() => {
      window.addEventListener('click', onDocClick)
      window.addEventListener('keydown', onKey)
    }, 0)
    return () => {
      clearTimeout(t)
      window.removeEventListener('click', onDocClick)
      window.removeEventListener('keydown', onKey)
    }
  })

  /** Toggle a single envelope's read state from the context menu.
      Optimistic: flip the local row immediately so the bold styling
      updates without a round-trip, then call the backend. The
      backend in turn fires `unread-count-updated`, which the Sidebar
      listener uses to refresh the per-folder badge. */
  async function toggleEnvelopeRead(env: EmailEnvelope) {
    const next = !env.is_read
    env.is_read = next
    closeContextMenu()
    try {
      await invoke('set_message_read', {
        accountId: env.account_id || accountId,
        folder: env.folder,
        uid: env.uid,
        read: next,
      })
    } catch (e) {
      console.warn('set_message_read failed:', e)
      env.is_read = !next
    }
  }
</script>

<div class="flex-1 flex flex-col min-w-0">
  {#if refreshing}
    <div class="px-3 py-1 text-[11px] text-surface-500 border-b border-surface-100 dark:border-surface-800">
      Refreshing…
    </div>
  {/if}

  <!-- Email list -->
  <div class="flex-1 overflow-y-auto">
    {#if loading}
      <div class="p-6 text-center text-sm text-surface-500">Loading…</div>
    {:else if error}
      <div class="p-4 text-sm text-red-500">{error}</div>
    {:else if envelopes.length === 0}
      <div class="p-6 text-center text-sm text-surface-500">No messages in {folder}.</div>
    {:else}
      {#each envelopes as env (`${env.account_id}:${env.uid}`)}
        {@const selected = selectedUid === env.uid && (!unified || selectedUid === env.uid)}
        <!-- Unread visual treatment: a 3px themed accent strip on the
             leading edge plus a subtle primary tint on the row.  The
             border is always present (transparent when read) so rows
             never reflow between states.  Selection wins over the
             unread tint for the background colour, but both states
             can co-exist on the accent strip. -->
        <button
          class="w-full text-left pl-3 pr-4 py-3 border-b border-l-[3px] border-surface-100 dark:border-surface-800 transition-colors
            {!env.is_read ? 'border-l-primary-500' : 'border-l-transparent'}
            {selected
              ? 'bg-primary-500/10'
              : !env.is_read
                ? 'bg-primary-500/[0.04] dark:bg-primary-500/[0.07] hover:bg-primary-500/10'
                : 'hover:bg-surface-100 dark:hover:bg-surface-800'}"
          draggable="true"
          ondragstart={(e) => onMailDragStart(e, env)}
          onclick={() => onselect(env.uid, unified ? env.account_id : undefined)}
          ondblclick={() =>
            openMailInStandaloneWindow(
              unified && env.account_id ? env.account_id : accountId,
              folder,
              env.uid,
            )}
          oncontextmenu={(e) => openContextMenu(e, env)}
        >
          <div class="flex items-center justify-between mb-1">
            <span class="text-sm {!env.is_read ? 'font-semibold' : 'font-normal'} truncate pr-2">
              {env.from || '(unknown sender)'}
            </span>
            <span class="text-xs {!env.is_read ? 'text-primary-500 font-medium' : 'text-surface-500'} shrink-0">{formatDate(env.date)}</span>
          </div>
          <p class="text-sm {!env.is_read ? 'font-medium' : ''} truncate">
            {env.subject || '(no subject)'}
          </p>
          {#if unified && env.account_id}
            <p class="text-[11px] text-surface-500 mt-1 truncate">
              {accountLabel(env.account_id)}
            </p>
          {/if}
        </button>
      {/each}
    {/if}
  </div>
</div>

{#if contextMenu}
  <!-- Right-click menu. Stop propagation so a click *inside* the menu
       doesn't reach the window-level dismiss listener and close it
       before the action handler runs. `role="menu"` keeps screen
       readers oriented. -->
  <div
    class="fixed z-50 min-w-45 py-1 rounded-md shadow-lg border border-surface-200 dark:border-surface-700 bg-surface-50 dark:bg-surface-900 text-sm"
    style="top: {contextMenu.y}px; left: {contextMenu.x}px;"
    role="menu"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.key === 'Escape' && closeContextMenu()}
    oncontextmenu={(e) => e.preventDefault()}
  >
    <button
      type="button"
      class="w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
      onclick={() => contextMenu && toggleEnvelopeRead(contextMenu.env)}
    >
      {contextMenu.env.is_read ? 'Mark as unread' : 'Mark as read'}
    </button>
  </div>
{/if}
