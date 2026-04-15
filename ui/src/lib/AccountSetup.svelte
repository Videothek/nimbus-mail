<script lang="ts">
  /**
   * AccountSetup — a step-by-step wizard for adding a new email account.
   *
   * This is the first thing users see when they launch Nimbus for the
   * first time (no accounts configured yet). It collects:
   *   1. Display name + email address
   *   2. IMAP server settings (incoming mail)
   *   3. SMTP server settings (outgoing mail)
   *
   * On submit it calls the `add_account` Tauri command, which persists
   * the account to disk via nimbus-store.
   *
   * The component fires an `oncomplete` event when setup succeeds,
   * so the parent (App.svelte) can switch to the inbox view.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  // ── Props ───────────────────────────────────────────────────
  // Called when account setup completes successfully
  interface Props {
    oncomplete: () => void
  }
  let { oncomplete }: Props = $props()

  // ── Wizard state ────────────────────────────────────────────
  // Which step of the wizard we're on (0-indexed)
  let step = $state(0)
  let error = $state('')
  let saving = $state(false)

  // ── Form fields ─────────────────────────────────────────────
  let displayName = $state('')
  let email = $state('')
  let password = $state('')     // stored in the OS keychain, never on disk
  let imapHost = $state('')
  let imapPort = $state(993)    // 993 = standard IMAP-over-TLS port
  let smtpHost = $state('')
  let smtpPort = $state(587)    // 587 = standard SMTP submission port
  let useJmap = $state(false)

  // ── Step navigation ─────────────────────────────────────────
  const totalSteps = 3

  function nextStep() {
    error = ''
    if (step === 0 && (!displayName.trim() || !email.trim())) {
      error = 'Please fill in your name and email address.'
      return
    }
    if (step === 1 && (!imapHost.trim() || !password)) {
      error = 'Please enter your IMAP server hostname and password.'
      return
    }
    if (step === 2 && !smtpHost.trim()) {
      error = 'Please enter your SMTP server hostname.'
      return
    }
    step++
  }

  function prevStep() {
    error = ''
    step--
  }

  // ── Auto-fill server settings from email domain ─────────────
  // When the user types their email, we pre-fill server hostnames
  // with common patterns. This is just a convenience — they can
  // always change it. Real auto-discovery (SRV/MX records) will
  // come later.
  function autoFillServers() {
    if (!email.includes('@')) return
    const domain = email.split('@')[1]
    if (!imapHost) imapHost = `imap.${domain}`
    if (!smtpHost) smtpHost = `smtp.${domain}`
  }

  // ── Submit ──────────────────────────────────────────────────
  async function submit() {
    error = ''
    saving = true

    try {
      // Probe the IMAP server with the entered credentials *before*
      // persisting anything. This turns "saved a bad account and
      // everything breaks silently on first fetch" into a clear,
      // immediate error the user can act on (wrong host, wrong port,
      // TLS failure, bad password — all surface here).
      await invoke('test_connection', {
        host: imapHost.trim(),
        port: imapPort,
        username: email.trim(),
        password,
      })

      // Generate a simple unique ID for this account.
      // crypto.randomUUID() is available in all modern browsers
      // (and Tauri's webview).
      const id = crypto.randomUUID()

      // Call the Rust backend to save this account. The password is
      // handed over as a separate argument so the Rust side can stash
      // it in the OS keychain — it never gets written to accounts.json.
      await invoke('add_account', {
        account: {
          id,
          display_name: displayName.trim(),
          email: email.trim(),
          imap_host: imapHost.trim(),
          imap_port: imapPort,
          smtp_host: smtpHost.trim(),
          smtp_port: smtpPort,
          use_jmap: useJmap,
        },
        password,
      })

      // Success! Tell the parent component to switch to inbox
      oncomplete()
    } catch (e: any) {
      error = formatError(e) || 'Failed to save account'
    } finally {
      saving = false
    }
  }
</script>

<!--
  The wizard is a centered card with a step indicator at the top.
  Each step shows different form fields. Navigation buttons at
  the bottom move between steps.
-->
<div class="h-full flex items-center justify-center bg-surface-50 dark:bg-surface-900">
  <div class="w-full max-w-lg mx-4">
    <!-- Header -->
    <div class="text-center mb-8">
      <h1 class="text-3xl font-bold text-primary-500 mb-2">Welcome to Nimbus Mail</h1>
      <p class="text-surface-600 dark:text-surface-400">Let's set up your email account</p>
    </div>

    <!-- Card -->
    <div class="card p-6 bg-surface-100 dark:bg-surface-800 rounded-xl shadow-lg">
      <!-- Step indicator -->
      <div class="flex items-center justify-center gap-2 mb-6">
        {#each Array(totalSteps) as _, i}
          <div
            class="w-3 h-3 rounded-full transition-colors {i === step
              ? 'bg-primary-500'
              : i < step
                ? 'bg-primary-300'
                : 'bg-surface-300 dark:bg-surface-600'}"
          ></div>
        {/each}
      </div>

      <!-- Step 0: Basic info -->
      {#if step === 0}
        <div>
          <h2 class="text-lg font-semibold mb-4">Your Information</h2>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Display Name</span>
            <input
              type="text"
              bind:value={displayName}
              placeholder="e.g. Nick"
              class="input w-full mt-1 px-3 py-2 rounded-md"
            />
          </label>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Email Address</span>
            <input
              type="email"
              bind:value={email}
              placeholder="e.g. nick@example.com"
              class="input w-full mt-1 px-3 py-2 rounded-md"
              onblur={autoFillServers}
            />
          </label>
        </div>

      <!-- Step 1: IMAP settings -->
      {:else if step === 1}
        <div>
          <h2 class="text-lg font-semibold mb-1">Incoming Mail (IMAP)</h2>
          <p class="text-sm text-surface-500 mb-4">
            IMAP is the protocol used to <strong>receive</strong> your emails.
            Port 993 uses TLS encryption (recommended).
          </p>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">IMAP Server</span>
            <input
              type="text"
              bind:value={imapHost}
              placeholder="e.g. imap.example.com"
              class="input w-full mt-1 px-3 py-2 rounded-md"
            />
          </label>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Port</span>
            <input
              type="number"
              bind:value={imapPort}
              class="input w-full mt-1 px-3 py-2 rounded-md"
            />
          </label>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Password</span>
            <input
              type="password"
              bind:value={password}
              placeholder="Your IMAP/SMTP password"
              class="input w-full mt-1 px-3 py-2 rounded-md"
              autocomplete="current-password"
            />
            <span class="block text-xs text-surface-500 mt-1">
              Stored securely in your OS keychain — never written to disk in plain text.
            </span>
          </label>
        </div>

      <!-- Step 2: SMTP settings -->
      {:else if step === 2}
        <div>
          <h2 class="text-lg font-semibold mb-1">Outgoing Mail (SMTP)</h2>
          <p class="text-sm text-surface-500 mb-4">
            SMTP is the protocol used to <strong>send</strong> your emails.
            Port 587 uses STARTTLS encryption (recommended).
          </p>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">SMTP Server</span>
            <input
              type="text"
              bind:value={smtpHost}
              placeholder="e.g. smtp.example.com"
              class="input w-full mt-1 px-3 py-2 rounded-md"
            />
          </label>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Port</span>
            <input
              type="number"
              bind:value={smtpPort}
              class="input w-full mt-1 px-3 py-2 rounded-md"
            />
          </label>
          <label class="flex items-center gap-2 mb-4">
            <input type="checkbox" bind:checked={useJmap} class="checkbox" />
            <span class="text-sm text-surface-700 dark:text-surface-300">
              Use JMAP instead of IMAP (if supported by your provider)
            </span>
          </label>
        </div>
      {/if}

      <!-- Error message -->
      {#if error}
        <div class="text-sm text-red-500 mb-4 p-3 bg-red-500/10 rounded-md">
          {error}
        </div>
      {/if}

      <!-- Navigation buttons -->
      <div class="flex justify-between mt-6">
        {#if step > 0}
          <button
            class="btn preset-outlined-surface-500"
            onclick={prevStep}
          >
            Back
          </button>
        {:else}
          <div></div>
        {/if}

        {#if step < totalSteps - 1}
          <button
            class="btn preset-filled-primary-500"
            onclick={nextStep}
          >
            Next
          </button>
        {:else}
          <button
            class="btn preset-filled-primary-500"
            onclick={submit}
            disabled={saving}
          >
            {saving ? 'Saving...' : 'Add Account'}
          </button>
        {/if}
      </div>
    </div>
  </div>
</div>
