<script lang="ts">
  /**
   * App.svelte — root component and simple view router.
   *
   * On startup, it asks the Rust backend how many accounts exist:
   *   - 0 accounts → show the AccountSetup wizard
   *   - 1+ accounts → show the main inbox (3-panel layout)
   *
   * The user can also navigate to AccountSettings from the sidebar.
   * This is a simple state-based "router" — no URL routing needed
   * since this is a desktop app, not a website.
   */

  import { invoke } from '@tauri-apps/api/core'
  import Sidebar from './lib/Sidebar.svelte'
  import MailList from './lib/MailList.svelte'
  import MailView from './lib/MailView.svelte'
  import AccountSetup from './lib/AccountSetup.svelte'
  import AccountSettings from './lib/AccountSettings.svelte'

  // ── View state ──────────────────────────────────────────────
  // Which view is currently shown. Starts as 'loading' until we
  // check whether any accounts exist.
  type View = 'loading' | 'setup' | 'inbox' | 'settings'
  let currentView = $state<View>('loading')

  // ── Check for existing accounts on startup ──────────────────
  // This runs once when the component mounts. It calls get_accounts
  // to see if the user has already configured an email account.
  $effect(() => {
    checkAccounts()
  })

  async function checkAccounts() {
    try {
      const accounts = await invoke<any[]>('get_accounts')
      currentView = accounts.length > 0 ? 'inbox' : 'setup'
    } catch {
      // If we can't load accounts (e.g. first launch, file doesn't exist),
      // show the setup wizard
      currentView = 'setup'
    }
  }

  // ── Navigation handlers ─────────────────────────────────────
  function goToInbox() {
    currentView = 'inbox'
  }

  function goToSetup() {
    currentView = 'setup'
  }

  function goToSettings() {
    currentView = 'settings'
  }

  function onSetupComplete() {
    // After adding an account, go to the inbox
    currentView = 'inbox'
  }
</script>

<!-- Loading state: shown briefly while we check for accounts -->
{#if currentView === 'loading'}
  <div class="h-full flex items-center justify-center bg-surface-50 dark:bg-surface-900">
    <p class="text-surface-500">Loading...</p>
  </div>

<!-- Setup wizard: first-time experience -->
{:else if currentView === 'setup'}
  <AccountSetup oncomplete={onSetupComplete} />

<!-- Account settings -->
{:else if currentView === 'settings'}
  <AccountSettings onclose={goToInbox} onaddaccount={goToSetup} />

<!-- Main inbox: the 3-panel mail client layout -->
{:else}
  <div class="h-full flex">
    <Sidebar onsettings={goToSettings} />
    <MailList />
    <MailView />
  </div>
{/if}
