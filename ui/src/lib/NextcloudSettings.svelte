<script lang="ts">
  /**
   * NextcloudSettings — manage Nextcloud server connection(s).
   *
   * Flow for connecting:
   * 1. User types their NC server URL and clicks "Connect".
   * 2. We call `start_nextcloud_login` to get a browser URL + poll handle.
   * 3. We open the URL via `open_url` (system default browser).
   * 4. We poll `poll_nextcloud_login` every 2s until the server returns
   *    the app password (user approved in the browser) or the user cancels.
   *
   * The app password is stored in the OS keychain by the backend; this
   * component only ever sees the account metadata.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  // ── Types (mirror the Rust models) ──────────────────────────
  interface NextcloudCapabilities {
    version?: string | null
    talk: boolean
    files: boolean
    caldav: boolean
    carddav: boolean
  }
  interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
    capabilities?: NextcloudCapabilities | null
  }
  interface LoginFlowInit {
    login_url: string
    poll_token: string
    poll_endpoint: string
  }

  // Returned by sync_nextcloud_contacts so the UI can show
  // "12 new, 1 removed" instead of a bare "done".
  interface SyncContactsReport {
    nc_account_id: string
    books_synced: number
    upserted: number
    deleted: number
    errors: string[]
  }
  // Per-account sync view-model. Keyed by account id so rows
  // render independently and one account's spinner doesn't block
  // another's. `lastSyncedAt` is only set after a successful run.
  interface ContactsState {
    syncing: boolean
    lastSyncedAt: number | null  // ms epoch
    lastReport: SyncContactsReport | null
    count: number                 // contacts cached locally
    error: string
  }

  // ── State ───────────────────────────────────────────────────
  let accounts = $state<NextcloudAccount[]>([])
  let loading = $state(true)
  let error = $state('')

  // Per-account contacts state, keyed by NC account id. Lives
  // outside `accounts` so resorting/refreshing the list doesn't
  // wipe in-flight sync status.
  let contactsState = $state<Record<string, ContactsState>>({})

  // Connect flow
  let serverInput = $state('')
  let connecting = $state(false)      // true while a login is in flight
  let pollTimer: number | null = null // setInterval handle, so we can cancel

  $effect(() => {
    loadAccounts()
    // Cleanup: cancel any in-flight polling if the component unmounts.
    return () => stopPolling()
  })

  async function loadAccounts() {
    loading = true
    error = ''
    try {
      accounts = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      // Seed contacts state for any new accounts, and refresh the
      // cached count for existing ones. Count failure is non-fatal —
      // we just leave the old value.
      for (const a of accounts) {
        if (!contactsState[a.id]) {
          contactsState[a.id] = {
            syncing: false,
            lastSyncedAt: null,
            lastReport: null,
            count: 0,
            error: '',
          }
        }
        try {
          const rows = await invoke<unknown[]>('get_contacts', { ncId: a.id })
          contactsState[a.id].count = rows.length
        } catch (e) {
          console.warn('get_contacts failed for', a.id, e)
        }
      }
    } catch (e) {
      error = formatError(e) || 'Failed to load Nextcloud connections'
    } finally {
      loading = false
    }
  }

  /**
   * Trigger a fresh contacts sync for one NC account.
   *
   * The backend returns a `SyncContactsReport` with upsert/delete
   * counts so we can show something concrete in the UI. Errors
   * encountered on individual addressbooks are surfaced per-account
   * but don't block other accounts.
   */
  async function syncContacts(acct: NextcloudAccount) {
    const state = contactsState[acct.id]
    if (!state || state.syncing) return
    state.syncing = true
    state.error = ''
    try {
      const report = await invoke<SyncContactsReport>('sync_nextcloud_contacts', {
        ncId: acct.id,
      })
      state.lastReport = report
      state.lastSyncedAt = Date.now()
      if (report.errors.length > 0) {
        state.error = report.errors.join('; ')
      }
      // Refresh the cached count — the sync report gives deltas, not
      // an absolute total.
      try {
        const rows = await invoke<unknown[]>('get_contacts', { ncId: acct.id })
        state.count = rows.length
      } catch (e) {
        console.warn('get_contacts refresh failed', e)
      }
    } catch (e) {
      state.error = formatError(e) || 'Sync failed'
    } finally {
      state.syncing = false
    }
  }

  function formatRelative(ts: number | null): string {
    if (ts === null) return 'never'
    const diffMs = Date.now() - ts
    if (diffMs < 60_000) return 'just now'
    const mins = Math.floor(diffMs / 60_000)
    if (mins < 60) return `${mins}m ago`
    const hours = Math.floor(mins / 60)
    if (hours < 24) return `${hours}h ago`
    return `${Math.floor(hours / 24)}d ago`
  }

  async function startConnect() {
    error = ''
    const url = serverInput.trim()
    if (!url) {
      error = 'Please enter your Nextcloud server URL.'
      return
    }
    // Normalise: tolerate "cloud.example.com" by assuming https. NC
    // never supports plain http in practice, so we don't add that path.
    const normalised = /^https?:\/\//.test(url) ? url : `https://${url}`

    connecting = true
    try {
      const init = await invoke<LoginFlowInit>('start_nextcloud_login', {
        serverUrl: normalised,
      })
      // Fire-and-forget the browser open — if it fails the user can copy
      // the URL manually from a fallback we'll show below.
      try {
        await invoke('open_url', { url: init.login_url })
      } catch (e) {
        console.warn('open_url failed, user must open manually', e)
      }
      pendingLoginUrl = init.login_url
      beginPolling(init)
    } catch (e) {
      error = formatError(e) || 'Failed to start Nextcloud login'
      connecting = false
    }
  }

  // Shown so the user can click/copy the URL if auto-open didn't work.
  let pendingLoginUrl = $state('')

  function beginPolling(init: LoginFlowInit) {
    // 2-second cadence is a compromise between UI responsiveness and
    // not hammering the NC server. Login Flow v2 tokens live for ~20
    // minutes; we stop on success, cancel, or any unexpected error.
    pollTimer = window.setInterval(async () => {
      try {
        const result = await invoke<NextcloudAccount | null>('poll_nextcloud_login', {
          pollEndpoint: init.poll_endpoint,
          pollToken: init.poll_token,
        })
        if (result) {
          stopPolling()
          connecting = false
          pendingLoginUrl = ''
          serverInput = ''
          await loadAccounts()
        }
      } catch (e) {
        stopPolling()
        connecting = false
        pendingLoginUrl = ''
        error = formatError(e) || 'Login failed'
      }
    }, 2000)
  }

  function stopPolling() {
    if (pollTimer !== null) {
      window.clearInterval(pollTimer)
      pollTimer = null
    }
  }

  function cancelConnect() {
    // The server-side token just expires on its own — nothing to tell
    // Nextcloud. Local teardown is enough.
    stopPolling()
    connecting = false
    pendingLoginUrl = ''
  }

  async function removeAccount(acct: NextcloudAccount) {
    if (!confirm(`Disconnect Nextcloud ${acct.username}@${acct.server_url}?`)) return
    try {
      await invoke('remove_nextcloud_account', { id: acct.id })
      await loadAccounts()
    } catch (e) {
      error = formatError(e) || 'Failed to remove'
    }
  }
</script>

<div class="space-y-4">
  <div class="flex items-center justify-between">
    <div>
      <h2 class="text-lg font-semibold">Nextcloud</h2>
      <p class="text-xs text-surface-500">
        Connect a Nextcloud server to enable Talk, Files attachments, and calendar/contact sync.
      </p>
    </div>
  </div>

  {#if error}
    <div class="text-sm text-red-500 p-3 bg-red-500/10 rounded-md">{error}</div>
  {/if}

  {#if loading}
    <p class="text-surface-500 text-sm">Loading…</p>
  {:else}
    <!-- Connected accounts -->
    {#if accounts.length > 0}
      <div class="space-y-2">
        {#each accounts as acct (acct.id)}
          {@const cs = contactsState[acct.id]}
          <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg space-y-3">
            <div class="flex items-start justify-between">
              <div class="flex-1">
                <p class="font-semibold">{acct.display_name ?? acct.username}</p>
                <p class="text-sm text-surface-500 break-all">{acct.server_url}</p>
                {#if acct.capabilities}
                  <div class="flex flex-wrap gap-1.5 mt-2">
                    {#if acct.capabilities.version}
                      <span class="text-xs px-2 py-0.5 rounded-full bg-surface-200 dark:bg-surface-700">
                        v{acct.capabilities.version}
                      </span>
                    {/if}
                    {#if acct.capabilities.talk}
                      <span class="text-xs px-2 py-0.5 rounded-full bg-blue-500/20 text-blue-600 dark:text-blue-300">Talk</span>
                    {/if}
                    {#if acct.capabilities.files}
                      <span class="text-xs px-2 py-0.5 rounded-full bg-green-500/20 text-green-600 dark:text-green-300">Files</span>
                    {/if}
                    {#if acct.capabilities.caldav}
                      <span class="text-xs px-2 py-0.5 rounded-full bg-purple-500/20 text-purple-600 dark:text-purple-300">Calendar</span>
                    {/if}
                    {#if acct.capabilities.carddav}
                      <span class="text-xs px-2 py-0.5 rounded-full bg-orange-500/20 text-orange-600 dark:text-orange-300">Contacts</span>
                    {/if}
                  </div>
                {/if}
              </div>
              <button
                class="btn btn-sm preset-outlined-error-500"
                onclick={() => removeAccount(acct)}
              >
                Disconnect
              </button>
            </div>

            <!-- Contacts sync row -->
            {#if acct.capabilities?.carddav !== false}
              <div class="flex items-center justify-between pt-2 border-t border-surface-300/40 dark:border-surface-700/60">
                <div class="text-sm">
                  <p>
                    <span class="font-medium">Contacts:</span>
                    {cs?.count ?? 0} cached
                  </p>
                  <p class="text-xs text-surface-500">
                    Last sync: {formatRelative(cs?.lastSyncedAt ?? null)}
                    {#if cs?.lastReport && (cs.lastReport.upserted > 0 || cs.lastReport.deleted > 0)}
                      · {cs.lastReport.upserted} updated, {cs.lastReport.deleted} removed
                    {/if}
                  </p>
                  {#if cs?.error}
                    <p class="text-xs text-red-500 mt-1">{cs.error}</p>
                  {/if}
                </div>
                <button
                  class="btn btn-sm preset-outlined-primary-500"
                  onclick={() => syncContacts(acct)}
                  disabled={cs?.syncing}
                >
                  {cs?.syncing ? 'Syncing…' : 'Sync now'}
                </button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    <!-- Connect form -->
    {#if !connecting}
      <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg">
        <label class="text-xs text-surface-500 block mb-1" for="nc-server">
          Nextcloud server URL
        </label>
        <div class="flex gap-2">
          <input
            id="nc-server"
            class="input flex-1 px-3 py-2 text-sm rounded-md"
            placeholder="https://cloud.example.com"
            bind:value={serverInput}
            onkeydown={(e) => e.key === 'Enter' && startConnect()}
          />
          <button class="btn preset-filled-primary-500" onclick={startConnect}>
            Connect
          </button>
        </div>
      </div>
    {:else}
      <!-- Waiting for browser auth -->
      <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg space-y-2">
        <p class="text-sm">
          Waiting for authorisation in your browser…
        </p>
        {#if pendingLoginUrl}
          <p class="text-xs text-surface-500">
            If nothing opened, click here:
            <a class="underline text-primary-500 break-all" href={pendingLoginUrl} target="_blank" rel="noopener">
              {pendingLoginUrl}
            </a>
          </p>
        {/if}
        <button class="btn btn-sm preset-outlined-surface-500" onclick={cancelConnect}>
          Cancel
        </button>
      </div>
    {/if}
  {/if}
</div>
