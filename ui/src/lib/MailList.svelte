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
  import MoveFolderPicker from './MoveFolderPicker.svelte'

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
    /** Fires after the right-click "Move to folder…" picker (#89)
        successfully moves a message.  Same shape as `onmessagemoved`
        in `Sidebar`: parent uses it to drop the source-folder
        envelope and run the auto-advance flow when the moved row
        was the currently-open one. */
    onmessagemoved?: (removedUid: number) => void
    /** Bindable hint to the parent that the network refresh after
     *  the cache-paint is still in flight.  `App.svelte` ORs this
     *  with MailView's flag and surfaces it as a calm spinner on
     *  the active-account avatar in the IconRail (#161) — the
     *  inline "Refreshing…" strip used to live here. */
    refreshing?: boolean
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
    onmessagemoved,
    refreshing = $bindable(false),
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
  let error = $state('')

  // ── Multi-select (#89, follow-up) ─────────────────────────────
  // Ctrl/Cmd+clicking rows toggles them in this set; plain-clicking
  // any row clears the set and falls through to the existing
  // single-row select (`onselect`).  The set persists across the
  // session as long as the folder + account stay the same — we
  // clear it whenever the inputs change so a leftover selection
  // never leaks across folders.
  let multiSelectedUids = $state<Set<number>>(new Set())

  $effect(() => {
    // Tracked so the effect re-runs on context change.
    void accountId
    void folder
    void unified
    multiSelectedUids = new Set()
  })

  function isMulti(uid: number): boolean {
    return multiSelectedUids.has(uid)
  }

  function onRowClick(e: MouseEvent, env: EmailEnvelope) {
    if (e.ctrlKey || e.metaKey) {
      // Ctrl/Cmd+click → toggle in multi-select; never opens the row.
      // First ctrl-click on a fresh state promotes the currently-
      // open row into the set so the user's "selection" mental
      // model matches Outlook / Apple Mail: plain-click A, ctrl-
      // click B, drag → both A and B move.  Without this promotion
      // A wasn't in the multi-set and got left behind on every
      // batch operation, which is what looked like "the last
      // selected mail won't move".
      const next = new Set(multiSelectedUids)
      if (next.size === 0 && selectedUid != null && selectedUid !== env.uid) {
        next.add(selectedUid)
      }
      if (next.has(env.uid)) next.delete(env.uid)
      else next.add(env.uid)
      multiSelectedUids = next
      return
    }
    // Plain click — clear multi-select and open as before.
    if (multiSelectedUids.size > 0) multiSelectedUids = new Set()
    onselect(env.uid, unified ? env.account_id : undefined)
  }

  /** Resolve the right (accountId, folder) tuple for a given
   *  envelope.  In unified mode the row carries its owning account
   *  on `env.account_id`; outside unified mode the active account +
   *  folder props are the truth.  Used by drag, right-click move,
   *  and the picker callback. */
  function srcCoordinates(env: EmailEnvelope) {
    return {
      accountId: unified && env.account_id ? env.account_id : accountId,
      folder: env.folder || folder,
    }
  }

  /** Envelopes that should be acted on for an operation triggered
   *  on `env`.  When `env` is part of a multi-select group with
   *  more than one row, we operate on the whole group; otherwise
   *  it's just `env` (this matches Outlook / Apple Mail's
   *  right-click + drag behaviour). */
  function affectedEnvelopes(env: EmailEnvelope): EmailEnvelope[] {
    if (multiSelectedUids.size > 1 && multiSelectedUids.has(env.uid)) {
      return envelopes.filter((e) => multiSelectedUids.has(e.uid))
    }
    return [env]
  }

  // ── Move-to-folder picker (#89) — opened via the right-click
  // "Move to folder…" entry.  We hold the envelope group being
  // moved here so the picker can target the right account even in
  // unified mode, and so `move_message` gets the correct source
  // `folder` field for each envelope.
  let movingGroup = $state<EmailEnvelope[] | null>(null)

  async function moveGroupToFolder(group: EmailEnvelope[], dest: string) {
    movingGroup = null
    // Group by (accountId, sourceFolder) so each subgroup goes
    // through a single batched IMAP MOVE on the backend
    // (`move_messages`).  Looping per-UID with `move_message` opened
    // a fresh IMAP connection every time and some servers were
    // dropping the last move in the burst due to rapid connection
    // recycling — the batched command does the whole subgroup in
    // one COPY + STORE + EXPUNGE round-trip.
    const groups = new Map<
      string,
      { accountId: string; folder: string; uids: number[] }
    >()
    for (const env of group) {
      const { accountId: src, folder: srcFolder } = srcCoordinates(env)
      if (dest === srcFolder) continue
      const key = `${src} ${srcFolder}`
      const existing = groups.get(key)
      if (existing) existing.uids.push(env.uid)
      else groups.set(key, { accountId: src, folder: srcFolder, uids: [env.uid] })
    }

    const succeeded: number[] = []
    const failures: unknown[] = []
    for (const g of groups.values()) {
      try {
        const moved = await invoke<number[]>('move_messages', {
          accountId: g.accountId,
          folder: g.folder,
          uids: g.uids,
          destFolder: dest,
        })
        succeeded.push(...moved)
      } catch (err) {
        console.warn('move_messages failed', err)
        failures.push(err)
      }
    }
    for (const uid of succeeded) {
      onmessagemoved?.(uid)
    }
    if (failures.length > 0) {
      error =
        succeeded.length === 0
          ? formatError(failures[0]) || 'Failed to move message'
          : `Moved ${succeeded.length} of ${group.length} messages — ${failures.length} group(s) failed.`
    }
    multiSelectedUids = new Set()
  }

  // ── Drag source: serialize a list of {accountId, folder, uid}
  // into the dataTransfer payload so Sidebar's folder rows (#89)
  // can iterate moves on drop.  The payload is always an array —
  // single-row drags become a 1-element list.  When the dragged
  // row is part of a multi-select group, the whole group rides
  // along.  The custom `application/x-nimbus-mail` MIME type means
  // the browser ignores the drag for non-Sidebar drop targets.
  function onMailDragStart(e: DragEvent, env: EmailEnvelope) {
    if (!e.dataTransfer) return
    // Dragging a row that *isn't* part of an existing multi-select
    // shouldn't drag the multi-select set — that would surprise the
    // user.  The affectedEnvelopes() rule already does the right
    // thing: it only expands to the group when the dragged row is
    // a member.
    const group = affectedEnvelopes(env)
    const payload = group.map((g) => {
      const { accountId: src, folder: srcFolder } = srcCoordinates(g)
      return { accountId: src, folder: srcFolder, uid: g.uid }
    })
    e.dataTransfer.setData(
      'application/x-nimbus-mail',
      JSON.stringify(payload),
    )
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
  // ── Quick-action handlers (#98) ───────────────────────────────
  // Inline icon buttons on each mail-list row — Delete + Mark
  // read/unread + Move to folder — so the user can triage a stack
  // of mail without ever opening it.  All three follow the same
  // optimistic shape: the row visibly disappears / changes state
  // instantly, the backend call follows, errors get surfaced into
  // the existing error banner.
  //
  // Click handlers MUST `stopPropagation` so the row-level click /
  // dblclick / dragstart never fires alongside the action.

  async function quickDelete(env: EmailEnvelope) {
    const srcAccountId = env.account_id || accountId
    const srcFolder = env.folder || folder
    try {
      await invoke('delete_message', {
        accountId: srcAccountId,
        folder: srcFolder,
        uid: env.uid,
      })
      onmessagemoved?.(env.uid)
    } catch (err) {
      console.warn('quickDelete failed', err)
      error = formatError(err) || 'Failed to delete'
    }
  }

  function quickMove(env: EmailEnvelope) {
    // Re-uses the multi-select "group" picker plumbing — a single-row
    // quick-action move is just a 1-element group from its
    // perspective.  affectedEnvelopes does the right thing here:
    // when this row is part of a multi-select group, the picker
    // moves the whole group; otherwise just the one row.
    movingGroup = affectedEnvelopes(env)
  }

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
        {@const multi = isMulti(env.uid)}
        <!-- Unread visual treatment: a 3px themed accent strip on the
             leading edge plus a subtle primary tint on the row.  The
             border is always present (transparent when read) so rows
             never reflow between states.  Selection > multi-select >
             unread tint for the background colour; the accent strip
             stays orthogonal so an unread+selected row keeps both.
             The row is wrapped in a `group` so the inline quick-
             action icons (#98) reveal on row hover. -->
        <div class="group relative">
          <button
            class="w-full text-left pl-3 pr-4 py-3 border-b border-l-[3px] border-surface-100 dark:border-surface-800 transition-colors
              {!env.is_read ? 'border-l-primary-500' : 'border-l-transparent'}
              {selected
                ? 'bg-primary-500/10'
                : multi
                  ? 'bg-primary-500/15 hover:bg-primary-500/20'
                  : !env.is_read
                    ? 'bg-primary-500/[0.04] dark:bg-primary-500/[0.07] hover:bg-primary-500/10'
                    : 'hover:bg-surface-100 dark:hover:bg-surface-800'}"
            draggable="true"
            ondragstart={(e) => onMailDragStart(e, env)}
            onclick={(e) => onRowClick(e, env)}
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
          <!-- Hover-revealed quick actions (#98).  Anchored to the
               BOTTOM-right corner of the row so the cluster never
               overlaps the date in the top-right.  Sibling of the
               row button (HTML forbids nested buttons).
               `pointer-events-none` on the wrapper while hidden
               keeps the layer click-through so the row's drag /
               click still work in the gap. -->
          <div
            class="absolute right-1 bottom-3 flex items-center gap-0.5 opacity-0 pointer-events-none transition-opacity
                   group-hover:opacity-100 group-hover:pointer-events-auto
                   focus-within:opacity-100 focus-within:pointer-events-auto"
          >
            <button
              type="button"
              class="w-7 h-7 rounded-md flex items-center justify-center text-sm bg-surface-50/90 dark:bg-surface-800/90 hover:bg-surface-200 dark:hover:bg-surface-700 shadow-sm"
              title={env.is_read ? 'Mark as unread' : 'Mark as read'}
              aria-label={env.is_read ? 'Mark as unread' : 'Mark as read'}
              onclick={(e) => {
                e.stopPropagation()
                void toggleEnvelopeRead(env)
              }}
            >{env.is_read ? '📭' : '📥'}</button>
            <button
              type="button"
              class="w-7 h-7 rounded-md flex items-center justify-center text-sm bg-surface-50/90 dark:bg-surface-800/90 hover:bg-surface-200 dark:hover:bg-surface-700 shadow-sm"
              title="Move to folder"
              aria-label="Move to folder"
              onclick={(e) => {
                e.stopPropagation()
                quickMove(env)
              }}
            >📁</button>
            <button
              type="button"
              class="w-7 h-7 rounded-md flex items-center justify-center text-sm bg-surface-50/90 dark:bg-surface-800/90 hover:bg-red-500/20 hover:text-red-500 shadow-sm"
              title="Delete"
              aria-label="Delete"
              onclick={(e) => {
                e.stopPropagation()
                void quickDelete(env)
              }}
            >🗑</button>
          </div>
        </div>
      {/each}
    {/if}
  </div>
</div>

{#if contextMenu}
  {@const ctxGroup = affectedEnvelopes(contextMenu.env)}
  {@const groupSize = ctxGroup.length}
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
      onclick={() => {
        if (!contextMenu) return
        // For a single-row context menu just flip the row's read
        // flag.  For a multi-row group flip every row to the
        // *opposite* of the right-clicked row's current state, so
        // a mixed group converges to one consistent state in one
        // click (matches Outlook / Apple Mail).
        if (groupSize > 1) {
          const target = !contextMenu.env.is_read
          for (const env of ctxGroup) {
            if (env.is_read !== target) void toggleEnvelopeRead(env)
          }
          multiSelectedUids = new Set()
          closeContextMenu()
        } else {
          void toggleEnvelopeRead(contextMenu.env)
        }
      }}
    >
      {#if groupSize > 1}
        Mark {groupSize} as {contextMenu.env.is_read ? 'unread' : 'read'}
      {:else}
        {contextMenu.env.is_read ? 'Mark as unread' : 'Mark as read'}
      {/if}
    </button>
    <button
      type="button"
      class="w-full text-left px-3 py-1.5 hover:bg-surface-200 dark:hover:bg-surface-800"
      onclick={() => {
        if (!contextMenu) return
        movingGroup = ctxGroup
        closeContextMenu()
      }}
    >
      {#if groupSize > 1}
        Move {groupSize} messages to folder…
      {:else}
        Move to folder…
      {/if}
    </button>
    <div class="my-1 border-t border-surface-200 dark:border-surface-700"></div>
    <button
      type="button"
      class="w-full text-left px-3 py-1.5 hover:bg-red-500/10 hover:text-red-500"
      onclick={() => {
        if (!contextMenu) return
        // Single-row delete reuses the row-level `quickDelete` (which
        // already feeds through `onmessagemoved` for auto-advance).
        // Multi-row batches iterate the group sequentially —
        // `delete_message` opens its own short-lived IMAP session per
        // call, but unlike MOVE we don't have a batched server-side
        // command yet.  N is small in practice (the user just
        // hand-picked the rows) so the overhead is acceptable.
        if (groupSize > 1) {
          for (const env of ctxGroup) void quickDelete(env)
          multiSelectedUids = new Set()
        } else {
          void quickDelete(contextMenu.env)
        }
        closeContextMenu()
      }}
    >
      {#if groupSize > 1}
        Delete {groupSize} messages
      {:else}
        Delete
      {/if}
    </button>
  </div>
{/if}

{#if movingGroup && movingGroup.length > 0}
  {@const head = movingGroup[0]!}
  <MoveFolderPicker
    accountId={unified && head.account_id ? head.account_id : accountId}
    currentFolder={head.folder || folder}
    accounts={accounts}
    onpicked={(name) => {
      const group = movingGroup
      if (group) void moveGroupToFolder(group, name)
    }}
    onclose={() => (movingGroup = null)}
  />
{/if}
