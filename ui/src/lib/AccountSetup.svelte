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
  // Optional plain-text signature appended below new messages from
  // this account. Empty string = no signature; the backend stores it
  // as Option<String>.
  let signature = $state('')

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
  // When the user blurs the email field we ask the backend to
  // probe Mozilla autoconfig and DNS SRV for that domain. If
  // anything comes back we prefill the IMAP/SMTP fields with the
  // discovered hosts/ports — the user can still edit them on the
  // next step. If nothing comes back we fall back to the naive
  // `imap.<domain>` / `smtp.<domain>` heuristic so the form
  // doesn't look completely empty.
  let discovering = $state(false)
  let discoveryHint = $state<string | null>(null)

  interface DiscoveredAccount {
    imap_host: string
    imap_port: number
    imap_tls: boolean
    smtp_host: string
    smtp_port: number
    smtp_tls: boolean
    source: 'autoconfig-domain' | 'autoconfig-ispdb' | 'srv'
  }

  async function autoFillServers() {
    if (!email.includes('@')) return
    const domain = email.split('@')[1]
    discoveryHint = null
    discovering = true
    try {
      const found = await invoke<DiscoveredAccount | null>(
        'discover_account_settings',
        { email: email.trim() },
      )
      if (found) {
        // Only overwrite blank fields so a user mid-edit doesn't
        // lose what they typed. Same posture as the old heuristic.
        if (!imapHost) imapHost = found.imap_host
        imapPort = found.imap_port
        if (!smtpHost) smtpHost = found.smtp_host
        smtpPort = found.smtp_port
        const label =
          found.source === 'autoconfig-domain'
            ? 'your provider'
            : found.source === 'autoconfig-ispdb'
              ? "Mozilla's database"
              : 'DNS records'
        discoveryHint = `Server settings auto-discovered from ${label}.`
        return
      }
    } catch (e) {
      console.warn('discover_account_settings failed:', e)
    } finally {
      discovering = false
    }

    // Fallback heuristic when discovery returns nothing.
    if (!imapHost) imapHost = `imap.${domain}`
    if (!smtpHost) smtpHost = `smtp.${domain}`
    discoveryHint = `Couldn't auto-discover ${domain} — best-guess hostnames filled in. Edit if needed.`
  }

  // ── TLS-trust prompt state ─────────────────────────────────
  // When test_connection fails because the IMAP server's cert
  // can't be validated, we show a prompt that lets the user trust
  // the cert and retry. The flow:
  //   1. submit() catches the cert error from test_connection
  //   2. invoke probe_server_certificate to capture the leaf cert
  //   3. show ProbedCert details + "Trust this server" button
  //   4. user confirms → trustedCerts gets the cert → retry submit
  // The list rides through to add_account so the saved account
  // remembers the trust decision and uses it on future connects.
  interface ProbedCert {
    der: number[]
    sha256: string
    host: string
  }
  /** Full Rust `TrustedCert` shape — what `test_connection` and
      `add_account` both deserialize. The `added_at` epoch is
      stamped at trust time so we don't depend on the user
      finishing the wizard before the timestamp is set. */
  interface TrustedCert {
    der: number[]
    sha256: string
    host: string
    added_at: number
  }
  let pendingCert = $state<ProbedCert | null>(null)
  let trustedCerts = $state<TrustedCert[]>([])

  /** Heuristic: does this error message look like it came from a
      TLS cert validation failure? rustls's wording is fairly stable
      ("invalid peer certificate", "UnknownIssuer", etc.) but we
      cast a wide net to be tolerant of OS-level wrappers. */
  function looksLikeCertError(message: string): boolean {
    const m = message.toLowerCase()
    return (
      m.includes('certificate') ||
      m.includes('cert ') ||
      m.includes('unknownissuer') ||
      m.includes('untrustedissuer') ||
      m.includes('badcertificate') ||
      m.includes('tls handshake')
    )
  }

  async function handleCertError() {
    pendingCert = null
    try {
      const probed = await invoke<ProbedCert>('probe_server_certificate', {
        host: imapHost.trim(),
        port: imapPort,
      })
      pendingCert = probed
    } catch (e: any) {
      error =
        'Could not retrieve the server certificate to display: ' +
        (formatError(e) || 'unknown error')
    }
  }

  function trustPendingCert() {
    if (!pendingCert) return
    // Promote `ProbedCert` → `TrustedCert` by stamping the trust
    // timestamp here. The retried `test_connection` and the
    // eventual `add_account` both deserialize this as the full
    // Rust `TrustedCert` struct, so the shape has to match.
    trustedCerts = [
      ...trustedCerts,
      { ...pendingCert, added_at: Math.floor(Date.now() / 1000) },
    ]
    pendingCert = null
    void submit()
  }

  function dismissCertPrompt() {
    pendingCert = null
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
      // TLS failure, bad password — all surface here). `trustedCerts`
      // grows when the user accepts a self-signed cert via the
      // prompt below, so the same probe will pass on the retry.
      await invoke('test_connection', {
        host: imapHost.trim(),
        port: imapPort,
        username: email.trim(),
        password,
        trustedCerts: trustedCerts.length > 0 ? trustedCerts : null,
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
          signature: signature.trim() || null,
          folder_icons: [],
          // `trustedCerts` already carries `added_at` from when the
          // user accepted each cert, so it ships through unchanged.
          trusted_certs: trustedCerts,
        },
        password,
      })

      // Success! Tell the parent component to switch to inbox
      oncomplete()
    } catch (e: any) {
      const msg = formatError(e) || 'Failed to save account'
      if (looksLikeCertError(msg)) {
        // Don't surface the raw error — the prompt explains the
        // situation more clearly. Kick off the cert probe in the
        // background; UI shows a spinner until it returns.
        error = ''
        void handleCertError()
      } else {
        error = msg
      }
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
              disabled={discovering}
            />
            {#if discovering}
              <span class="block text-xs text-surface-500 mt-1">Looking up server settings…</span>
            {:else if discoveryHint}
              <span class="block text-xs text-surface-500 mt-1">{discoveryHint}</span>
            {/if}
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

          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Signature (optional)</span>
            <textarea
              bind:value={signature}
              rows="4"
              placeholder={`Jane Doe\nProduct Manager · Example Corp\n+1 555 0100`}
              class="input w-full mt-1 px-3 py-2 rounded-md font-mono text-sm"
            ></textarea>
            <span class="block text-xs text-surface-500 mt-1">
              Appended to new messages sent from this account. You can change it later in Settings.
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

      <!-- TLS-trust prompt. Shown when test_connection failed with a
           cert error and probe_server_certificate succeeded in
           capturing the leaf cert. The user gets the SHA-256 to
           compare against their server, then chooses whether to
           trust it for this account. -->
      {#if pendingCert}
        <div class="mb-4 p-4 rounded-md border border-warning-500/40 bg-warning-500/5">
          <p class="text-sm font-medium mb-1">
            The server's TLS certificate isn't trusted by default.
          </p>
          <p class="text-xs text-surface-500 mb-3">
            This is normal for self-hosted mail servers using a
            self-signed certificate. Compare the fingerprint below
            with your server's actual certificate before trusting.
          </p>
          <p class="text-xs mb-1"><span class="text-surface-500">Host:</span> <span class="font-mono">{pendingCert.host}</span></p>
          <p class="text-xs mb-3 break-all">
            <span class="text-surface-500">SHA-256:</span>
            <span class="font-mono">{pendingCert.sha256}</span>
          </p>
          <div class="flex gap-2">
            <button
              type="button"
              class="btn btn-sm preset-filled-primary-500"
              onclick={trustPendingCert}
            >Trust this server and continue</button>
            <button
              type="button"
              class="btn btn-sm preset-outlined-surface-500"
              onclick={dismissCertPrompt}
            >Cancel</button>
          </div>
        </div>
      {/if}

      {#if trustedCerts.length > 0 && !pendingCert}
        <div class="mb-4 p-3 rounded-md border border-success-500/30 bg-success-500/5 text-xs text-surface-600 dark:text-surface-400">
          Trusting {trustedCerts.length}
          self-signed certificate{trustedCerts.length === 1 ? '' : 's'}
          for this account.
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
