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

  // ── Props ───────────────────────────────────────────────────
  interface EmailEnvelope {
    uid: number
    folder: string
    from: string
    subject: string
    date: string      // RFC 3339 string (serde serialises DateTime<Utc> this way)
    is_read: boolean
    is_starred: boolean
  }

  interface Props {
    accountId: string
    folder?: string
    selectedUid: number | null
    /** Bumped by the parent to force a network re-fetch (manual refresh). */
    refreshToken?: number
    onselect: (uid: number) => void
  }
  let {
    accountId,
    folder = 'INBOX',
    selectedUid,
    refreshToken = 0,
    onselect,
  }: Props = $props()

  // ── Fetch state ─────────────────────────────────────────────
  //
  // Two-phase load: first ask the cache (instant, offline-safe), then
  // kick off the network refresh in parallel. `loading` covers the
  // *initial* paint and is dropped as soon as either source returns.
  // `refreshing` stays true while the network call is still in flight
  // after the cache has rendered, so the UI can show a subtle hint
  // without blanking the list.
  let envelopes = $state<EmailEnvelope[]>([])
  let loading = $state(true)
  let refreshing = $state(false)
  let error = $state('')

  // Re-fetch whenever the account, folder, or refreshToken changes.
  $effect(() => {
    // Touch refreshToken so Svelte re-runs this effect when it's bumped.
    refreshToken
    void load(accountId, folder)
  })

  async function load(id: string, f: string) {
    loading = true
    refreshing = false
    error = ''

    // Cache first — usually instant, may return [] on cold start.
    try {
      const cached = await invoke<EmailEnvelope[]>('get_cached_envelopes', {
        accountId: id,
        folder: f,
        limit: 50,
      })
      // Guard against a stale response landing after the user already
      // navigated to a different folder/account.
      if (id === accountId && f === folder) {
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
      const fresh = await invoke<EmailEnvelope[]>('fetch_envelopes', {
        accountId: id,
        folder: f,
        limit: 50,
      })
      if (id === accountId && f === folder) {
        envelopes = fresh
      }
    } catch (e: any) {
      // Only surface the network error when we have nothing to show.
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
      {#each envelopes as env (env.uid)}
        <button
          class="w-full text-left px-4 py-3 border-b border-surface-100 dark:border-surface-800 transition-colors
            {selectedUid === env.uid
              ? 'bg-primary-500/10'
              : 'hover:bg-surface-100 dark:hover:bg-surface-800'}"
          onclick={() => onselect(env.uid)}
        >
          <div class="flex items-center justify-between mb-1">
            <span class="text-sm {!env.is_read ? 'font-semibold' : 'font-normal'} truncate pr-2">
              {env.from || '(unknown sender)'}
            </span>
            <span class="text-xs text-surface-500 shrink-0">{formatDate(env.date)}</span>
          </div>
          <p class="text-sm {!env.is_read ? 'font-medium' : ''} truncate">
            {env.subject || '(no subject)'}
          </p>
        </button>
      {/each}
    {/if}
  </div>
</div>
