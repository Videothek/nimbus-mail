<script lang="ts">
  /**
   * AccountSettings — displays and manages configured email accounts.
   *
   * Shows a list of all accounts with options to remove them or
   * add new ones. This is accessible from the sidebar's settings
   * area and lets users manage their accounts after initial setup.
   */

  import { invoke } from '@tauri-apps/api/core'
  import NextcloudSettings from './NextcloudSettings.svelte'

  // ── Types ───────────────────────────────────────────────────
  // Mirrors the Rust `Account` struct from nimbus-core
  interface Account {
    id: string
    display_name: string
    email: string
    imap_host: string
    imap_port: number
    smtp_host: string
    smtp_port: number
    use_jmap: boolean
  }

  // ── Props ───────────────────────────────────────────────────
  interface Props {
    onclose: () => void         // Go back to the inbox view
    onaddaccount: () => void    // Switch to the setup wizard to add another account
  }
  let { onclose, onaddaccount }: Props = $props()

  // ── State ───────────────────────────────────────────────────
  let accounts = $state<Account[]>([])
  let loading = $state(true)
  let error = $state('')

  // ── App-wide preferences (Issue #16) ────────────────────────
  // Mirrors the Rust `AppSettings` struct. A missing/failing load
  // falls back to the Rust-side defaults — we never render with a
  // null form state.
  interface AppSettings {
    minimize_to_tray: boolean
    background_sync_enabled: boolean
    background_sync_interval_secs: number
    notifications_enabled: boolean
    start_minimized: boolean
  }

  let appSettings = $state<AppSettings>({
    minimize_to_tray: true,
    background_sync_enabled: true,
    background_sync_interval_secs: 300,
    notifications_enabled: true,
    start_minimized: false,
  })
  let prefsSaveStatus = $state<'' | 'saving' | 'saved' | 'error'>('')
  let checkNowBusy = $state(false)

  // ── Load accounts on mount ──────────────────────────────────
  // $effect runs when the component is first rendered (like onMount).
  // We call the Rust backend to get all saved accounts.
  $effect(() => {
    loadAccounts()
    loadAppSettings()
  })

  async function loadAppSettings() {
    try {
      appSettings = await invoke<AppSettings>('get_app_settings')
    } catch (e: any) {
      console.warn('failed to load app settings', e)
    }
  }

  // Debounced save so dragging the interval field doesn't hammer the
  // disk on every keystroke. 400 ms is imperceptible to the user but
  // easily coalesces a burst of edits.
  let saveTimer: ReturnType<typeof setTimeout> | null = null
  function scheduleSave() {
    prefsSaveStatus = 'saving'
    if (saveTimer) clearTimeout(saveTimer)
    saveTimer = setTimeout(async () => {
      try {
        await invoke('update_app_settings', { newSettings: appSettings })
        prefsSaveStatus = 'saved'
        setTimeout(() => {
          if (prefsSaveStatus === 'saved') prefsSaveStatus = ''
        }, 1500)
      } catch (e: any) {
        console.warn('failed to save app settings', e)
        prefsSaveStatus = 'error'
      }
    }, 400)
  }

  async function runCheckMailNow() {
    checkNowBusy = true
    try {
      await invoke('check_mail_now')
    } catch (e: any) {
      console.warn('check_mail_now failed', e)
    } finally {
      checkNowBusy = false
    }
  }

  async function loadAccounts() {
    loading = true
    error = ''
    try {
      accounts = await invoke<Account[]>('get_accounts')
    } catch (e: any) {
      error = typeof e === 'string' ? e : e?.message ?? 'Failed to load accounts'
    } finally {
      loading = false
    }
  }

  async function removeAccount(id: string, email: string) {
    // Simple confirmation — in a real app you might use a modal
    if (!confirm(`Remove account ${email}? This cannot be undone.`)) return

    try {
      await invoke('remove_account', { id })
      // Refresh the list after removal
      await loadAccounts()
    } catch (e: any) {
      error = typeof e === 'string' ? e : e?.message ?? 'Failed to remove account'
    }
  }
</script>

<div class="h-full flex flex-col bg-surface-50 dark:bg-surface-900">
  <!-- Header bar -->
  <div class="flex items-center justify-between p-4 border-b border-surface-200 dark:border-surface-700">
    <h1 class="text-xl font-bold">Account Settings</h1>
    <button class="btn btn-sm preset-outlined-surface-500" onclick={onclose}>
      Back to Inbox
    </button>
  </div>

  <!-- Content -->
  <div class="flex-1 overflow-y-auto p-6 max-w-2xl mx-auto w-full">
    <!-- App Preferences (Issue #16) — always visible, independent
         of per-account loading state so the user can tweak tray /
         sync / notification behaviour even before adding an account. -->
    <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg mb-6">
      <div class="flex items-center justify-between mb-3">
        <h2 class="text-base font-semibold">App Preferences</h2>
        <div class="flex items-center gap-2">
          {#if prefsSaveStatus === 'saving'}
            <span class="text-xs text-surface-400">Saving…</span>
          {:else if prefsSaveStatus === 'saved'}
            <span class="text-xs text-success-500">Saved</span>
          {:else if prefsSaveStatus === 'error'}
            <span class="text-xs text-error-500">Save failed</span>
          {/if}
          <button
            class="btn btn-sm preset-outlined-primary-500"
            disabled={checkNowBusy}
            onclick={runCheckMailNow}
          >
            {checkNowBusy ? 'Checking…' : 'Check Mail Now'}
          </button>
        </div>
      </div>

      <div class="space-y-2 text-sm">
        <label class="flex items-center gap-2">
          <input
            type="checkbox"
            class="checkbox"
            bind:checked={appSettings.minimize_to_tray}
            onchange={scheduleSave}
          />
          <span>Minimize to tray when closing the window</span>
        </label>

        <label class="flex items-center gap-2">
          <input
            type="checkbox"
            class="checkbox"
            bind:checked={appSettings.background_sync_enabled}
            onchange={scheduleSave}
          />
          <span>Run background mail sync</span>
        </label>

        <label class="flex items-center gap-2 pl-6">
          <span class="text-surface-500">Interval (seconds):</span>
          <input
            type="number"
            min="30"
            step="30"
            class="input w-24 text-sm py-1 px-2"
            disabled={!appSettings.background_sync_enabled}
            bind:value={appSettings.background_sync_interval_secs}
            onchange={scheduleSave}
          />
          <span class="text-xs text-surface-400">min. 30</span>
        </label>

        <label class="flex items-center gap-2">
          <input
            type="checkbox"
            class="checkbox"
            bind:checked={appSettings.notifications_enabled}
            onchange={scheduleSave}
          />
          <span>Show desktop notifications for new mail</span>
        </label>

        <label class="flex items-center gap-2">
          <input
            type="checkbox"
            class="checkbox"
            bind:checked={appSettings.start_minimized}
            onchange={scheduleSave}
          />
          <span>Start minimized to tray</span>
        </label>
      </div>
    </div>

    {#if loading}
      <p class="text-surface-500 text-center py-8">Loading accounts...</p>

    {:else if error}
      <div class="text-sm text-red-500 p-4 bg-red-500/10 rounded-md mb-4">
        {error}
      </div>

    {:else if accounts.length === 0}
      <div class="text-center py-12">
        <p class="text-surface-500 mb-4">No accounts configured yet.</p>
        <button class="btn preset-filled-primary-500" onclick={onaddaccount}>
          Add Account
        </button>
      </div>

    {:else}
      <!-- Account list -->
      <div class="space-y-4">
        {#each accounts as account}
          <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg flex items-start justify-between">
            <div>
              <p class="font-semibold">{account.display_name}</p>
              <p class="text-sm text-surface-500">{account.email}</p>
              <div class="text-xs text-surface-400 mt-2 space-y-0.5">
                <p>IMAP: {account.imap_host}:{account.imap_port}</p>
                <p>SMTP: {account.smtp_host}:{account.smtp_port}</p>
                {#if account.use_jmap}
                  <p class="text-primary-500">JMAP enabled</p>
                {/if}
              </div>
            </div>
            <button
              class="btn btn-sm preset-outlined-error-500"
              onclick={() => removeAccount(account.id, account.email)}
            >
              Remove
            </button>
          </div>
        {/each}
      </div>

      <!-- Add another account -->
      <div class="mt-6">
        <button class="btn preset-outlined-primary-500" onclick={onaddaccount}>
          + Add Another Account
        </button>
      </div>
    {/if}

    <!-- Nextcloud section (always shown, independent of mail accounts) -->
    <div class="mt-10 pt-6 border-t border-surface-200 dark:border-surface-700">
      <NextcloudSettings />
    </div>
  </div>
</div>
