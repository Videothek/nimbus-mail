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
    onselect: (uid: number) => void
  }
  let { accountId, folder = 'INBOX', selectedUid, onselect }: Props = $props()

  // ── Fetch state ─────────────────────────────────────────────
  let envelopes = $state<EmailEnvelope[]>([])
  let loading = $state(true)
  let error = $state('')

  // Re-fetch whenever the account or folder changes.
  $effect(() => {
    void load(accountId, folder)
  })

  async function load(id: string, f: string) {
    loading = true
    error = ''
    try {
      envelopes = await invoke<EmailEnvelope[]>('fetch_envelopes', {
        accountId: id,
        folder: f,
        limit: 50,
      })
    } catch (e: any) {
      error = formatError(e) || 'Failed to load mail'
      envelopes = []
    } finally {
      loading = false
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

<div class="w-80 shrink-0 border-r border-surface-200 dark:border-surface-700 flex flex-col">
  <!-- Search bar -->
  <div class="p-3 border-b border-surface-200 dark:border-surface-700">
    <input
      type="text"
      placeholder="Search mail..."
      class="input w-full px-3 py-2 text-sm rounded-md"
    />
  </div>

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
