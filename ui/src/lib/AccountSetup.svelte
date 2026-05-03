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
   * The component fires an `oncomplete` callback when setup succeeds
   * so the parent (App.svelte) can switch to the inbox view.  When
   * the wizard is invoked from somewhere the user can back out of
   * (e.g. the "Add account" button in Settings, or the IconRail's
   * add-account affordance — both cases where they already have at
   * least one account configured) the parent passes `canCancel=true`
   * and an `oncancel` callback so the wizard can render a close (×)
   * button in the top-right.  On true first launch (zero accounts),
   * `canCancel` defaults to `false` and the button is hidden.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import Toggle from './Toggle.svelte'
  import Icon, { type IconName } from './Icon.svelte'

  // ── Props ───────────────────────────────────────────────────
  interface Props {
    /** Called when account setup completes successfully. */
    oncomplete: () => void
    /** When true, the wizard renders an "X" close button.  Set by
     *  the parent only when the user has at least one account
     *  configured already — first-launch must finish the wizard. */
    canCancel?: boolean
    /** Called when the user clicks the close button.  Required when
     *  `canCancel` is true. */
    oncancel?: () => void
  }
  let { oncomplete, canCancel = false, oncancel }: Props = $props()

  // Close on Escape (#192).  Only when `canCancel` is true — on
  // true first launch the user has to finish setup; we don't
  // want Escape to silently drop them back into a half-bootstrapped
  // app with no accounts.  We skip if an autocomplete listbox is
  // open so it owns the keystroke; the wizard has no nested
  // role="dialog" surfaces so we don't need to special-case those.
  $effect(() => {
    if (!canCancel) return
    function onKey(e: KeyboardEvent) {
      if (e.key !== 'Escape') return
      if (document.querySelector('[role="listbox"]')) return
      e.preventDefault()
      handleCancel()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  })

  // ── Wizard state ────────────────────────────────────────────
  // Which step of the wizard we're on (0-indexed)
  let step = $state(0)
  let error = $state('')
  let saving = $state(false)

  // ── Form fields ─────────────────────────────────────────────
  let displayName = $state('')
  // Sender name (#115) — what appears as the human name in the
  // From: header on outgoing mail.  `displayName` is the local
  // label for the account in the UI; `personName` is the
  // outward-facing identity.  Defaults to `displayName` on the
  // backend when left blank.
  let personName = $state('')
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
  // Step metadata drives the numbered progress indicator + the
  // section headers.  Keep `icon` keys in sync with the Icon
  // component's name union.
  const steps: ReadonlyArray<{ title: string; icon: IconName }> = [
    { title: 'Your information', icon: 'address-book' },
    { title: 'Incoming mail (IMAP)', icon: 'email-envelope' },
    { title: 'Outgoing mail (SMTP)', icon: 'sent' },
  ]
  const totalSteps = steps.length

  function nextStep() {
    error = ''
    if (step === 0 && (!displayName.trim() || !email.trim())) {
      error = 'Please fill in the account name and email address.'
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

  function handleCancel() {
    if (!canCancel) return
    oncancel?.()
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
  // the chain and retry. The flow:
  //   1. submit() catches the cert error from test_connection
  //   2. invoke probe_server_certificate to capture the full chain
  //   3. show fingerprints + "Trust this server" button
  //   4. user confirms → every cert in the chain gets added to
  //      trustedCerts → retry submit
  // The list rides through to add_account so the saved account
  // remembers the trust decision and uses it on future connects.
  // Trusting the whole chain (not just the leaf) means a leaf
  // reissue under the same intermediate, or a server that
  // reorders its chain, doesn't drop the user back into this
  // prompt.
  interface ProbedCertEntry {
    der: number[]
    sha256: string
  }
  interface ProbedCert {
    chain: ProbedCertEntry[]
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
    // Promote every cert in the probed chain → `TrustedCert`. We
    // trust the whole chain, not just the leaf, so a server that
    // reorders the chain on a future connect (or reissues the leaf
    // under the same intermediate) still validates without
    // re-prompting the user.
    const addedAt = Math.floor(Date.now() / 1000)
    const host = pendingCert.host
    const additions: TrustedCert[] = pendingCert.chain.map((entry) => ({
      der: entry.der,
      sha256: entry.sha256,
      host,
      added_at: addedAt,
    }))
    trustedCerts = [...trustedCerts, ...additions]
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
          person_name: personName.trim() || null,
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
  The wizard is a centered card.  Layout from top to bottom:
    * brand title + tagline
    * the card, with:
        - top-right "×" button (only when `canCancel`)
        - numbered step indicator (1 ── 2 ── 3, completed steps
          show a check mark, the active one is filled-primary)
        - icon-prefixed section header for the current step
        - the form fields for the current step
        - error / cert-trust prompts
        - Back / Next | Add Account buttons
-->
<div class="h-full flex items-center justify-center bg-surface-50 dark:bg-surface-900">
  <div class="w-full max-w-xl mx-4">
    <!-- Header -->
    <div class="text-center mb-8">
      <h1 class="text-3xl font-bold text-primary-500 mb-2">Welcome to Nimbus Mail</h1>
      <p class="text-surface-600 dark:text-surface-400">Let's set up your email account</p>
    </div>

    <!-- Card.  When the wizard is closeable, we add extra top
         padding so the corner-anchored "×" button doesn't crowd
         the step indicator below it. -->
    <div
      class="card relative p-6 {canCancel ? 'pt-10' : ''} bg-surface-100 dark:bg-surface-800 rounded-xl shadow-lg"
    >
      {#if canCancel}
        <button
          type="button"
          class="absolute top-1 right-1 p-1.5 rounded-md text-surface-500 hover:text-surface-900 hover:bg-surface-200 dark:hover:text-surface-100 dark:hover:bg-surface-700 transition-colors"
          onclick={handleCancel}
          aria-label="Close setup wizard"
          title="Close"
        >
          <Icon name="close" size={18} />
        </button>
      {/if}

      <!-- Numbered step indicator.  Each step is a circle (active /
           completed / pending) connected by a thin line.  The active
           step's circle is filled-primary, completed steps show the
           check icon, pending steps show the step number. -->
      <div class="flex items-center justify-center mb-6 px-2">
        {#each steps as s, i (s.title)}
          {#if i > 0}
            <div
              class="flex-1 h-px mx-2 transition-colors {i <= step
                ? 'bg-primary-500'
                : 'bg-surface-300 dark:bg-surface-600'}"
            ></div>
          {/if}
          <div class="flex flex-col items-center gap-1">
            <div
              class="w-8 h-8 rounded-full flex items-center justify-center text-xs font-semibold transition-colors {i ===
              step
                ? 'bg-primary-500 text-white'
                : i < step
                  ? 'bg-primary-500/20 text-primary-700 dark:text-primary-300'
                  : 'bg-surface-200 dark:bg-surface-700 text-surface-500'}"
            >
              {#if i < step}
                <Icon name="success" size={14} />
              {:else}
                {i + 1}
              {/if}
            </div>
            <span
              class="text-[10px] uppercase tracking-wide font-medium {i === step
                ? 'text-primary-600 dark:text-primary-400'
                : 'text-surface-500'}"
            >
              Step {i + 1}
            </span>
          </div>
        {/each}
      </div>

      <!-- Section header for the current step (icon + title). -->
      <div class="flex items-center gap-2 mb-4">
        <span class="text-primary-500"><Icon name={steps[step].icon} size={20} /></span>
        <h2 class="text-lg font-semibold">{steps[step].title}</h2>
      </div>

      <!-- Step 0: Basic info -->
      {#if step === 0}
        <div>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Account Name</span>
            <div class="relative mt-1">
              <span class="absolute left-2.5 top-1/2 -translate-y-1/2 text-surface-400 pointer-events-none flex items-center" aria-hidden="true">
                <Icon name="design-palette" size={14} />
              </span>
              <input
                type="text"
                bind:value={displayName}
                placeholder="e.g. Work, Personal"
                class="input w-full pl-8 pr-3 py-2 rounded-md"
              />
            </div>
            <span class="block text-xs text-surface-500 mt-1">
              How this account is labelled inside Nimbus.
            </span>
          </label>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Your Name</span>
            <div class="relative mt-1">
              <span class="absolute left-2.5 top-1/2 -translate-y-1/2 text-surface-400 pointer-events-none flex items-center" aria-hidden="true">
                <Icon name="contacts" size={14} />
              </span>
              <input
                type="text"
                bind:value={personName}
                placeholder="e.g. Alex Morgan"
                class="input w-full pl-8 pr-3 py-2 rounded-md"
              />
            </div>
            <span class="block text-xs text-surface-500 mt-1">
              Shown as the sender on outgoing mail. Defaults to the account name when empty.
            </span>
          </label>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Email Address</span>
            <div class="relative mt-1">
              <span class="absolute left-2.5 top-1/2 -translate-y-1/2 text-surface-400 pointer-events-none flex items-center" aria-hidden="true">
                <Icon name="email-envelope" size={14} />
              </span>
              <input
                type="email"
                bind:value={email}
                placeholder="e.g. you@example.com"
                class="input w-full pl-8 pr-3 py-2 rounded-md"
                onblur={autoFillServers}
                disabled={discovering}
              />
            </div>
            {#if discovering}
              <span class="text-xs text-surface-500 mt-1 flex items-center gap-1">
                <Icon name="loading" size={12} />
                Looking up server settings…
              </span>
            {:else if discoveryHint}
              <span class="text-xs text-surface-500 mt-1 flex items-center gap-1">
                <Icon name="info" size={12} />
                {discoveryHint}
              </span>
            {/if}
          </label>
        </div>

      <!-- Step 1: IMAP settings -->
      {:else if step === 1}
        <div>
          <p class="text-sm text-surface-500 mb-4">
            IMAP is the protocol used to <strong>receive</strong> your emails.
            Port 993 uses TLS encryption (recommended).
          </p>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">IMAP Server</span>
            <div class="relative mt-1">
              <span class="absolute left-2.5 top-1/2 -translate-y-1/2 text-surface-400 pointer-events-none flex items-center" aria-hidden="true">
                <Icon name="cloud" size={14} />
              </span>
              <input
                type="text"
                bind:value={imapHost}
                placeholder="e.g. imap.example.com"
                class="input w-full pl-8 pr-3 py-2 rounded-md"
              />
            </div>
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
            <div class="relative mt-1">
              <span class="absolute left-2.5 top-1/2 -translate-y-1/2 text-surface-400 pointer-events-none flex items-center" aria-hidden="true">
                <Icon name="lock" size={14} />
              </span>
              <input
                type="password"
                bind:value={password}
                placeholder="Your IMAP/SMTP password"
                class="input w-full pl-8 pr-3 py-2 rounded-md"
                autocomplete="current-password"
              />
            </div>
            <span class="block text-xs text-surface-500 mt-1">
              Stored securely in your OS keychain — never written to disk in plain text.
            </span>
          </label>
        </div>

      <!-- Step 2: SMTP settings -->
      {:else if step === 2}
        <div>
          <p class="text-sm text-surface-500 mb-4">
            SMTP is the protocol used to <strong>send</strong> your emails.
            Port 587 uses STARTTLS encryption (recommended).
          </p>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">SMTP Server</span>
            <div class="relative mt-1">
              <span class="absolute left-2.5 top-1/2 -translate-y-1/2 text-surface-400 pointer-events-none flex items-center" aria-hidden="true">
                <Icon name="cloud" size={14} />
              </span>
              <input
                type="text"
                bind:value={smtpHost}
                placeholder="e.g. smtp.example.com"
                class="input w-full pl-8 pr-3 py-2 rounded-md"
              />
            </div>
          </label>
          <label class="block mb-4">
            <span class="text-sm font-medium text-surface-700 dark:text-surface-300">Port</span>
            <input
              type="number"
              bind:value={smtpPort}
              class="input w-full mt-1 px-3 py-2 rounded-md"
            />
          </label>

          <!-- JMAP toggle.  A modern protocol some servers offer
               in addition to (or instead of) IMAP/SMTP. -->
          <div class="flex items-center justify-between gap-3 mb-4 p-3 rounded-md bg-surface-200/50 dark:bg-surface-700/40">
            <div class="flex items-start gap-2 min-w-0">
              <span class="text-primary-500 mt-0.5"><Icon name="sync" size={16} /></span>
              <div class="min-w-0">
                <span class="block text-sm font-medium text-surface-700 dark:text-surface-200">Use JMAP instead of IMAP</span>
                <span class="block text-xs text-surface-500">
                  Modern push-based mail protocol. Only enable if your provider supports it.
                </span>
              </div>
            </div>
            <Toggle bind:checked={useJmap} label="Use JMAP instead of IMAP" />
          </div>

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
        <div class="text-sm text-red-500 mb-4 p-3 bg-red-500/10 rounded-md flex items-start gap-2">
          <span class="mt-0.5"><Icon name="error" size={16} /></span>
          <span>{error}</span>
        </div>
      {/if}

      <!-- TLS-trust prompt. Shown when test_connection failed with a
           cert error and probe_server_certificate succeeded in
           capturing the leaf cert. The user gets the SHA-256 to
           compare against their server, then chooses whether to
           trust it for this account. -->
      {#if pendingCert}
        <div class="mb-4 p-4 rounded-md border border-warning-500/40 bg-warning-500/5">
          <p class="text-sm font-medium mb-1 flex items-center gap-2">
            <Icon name="lock" size={16} />
            The server's TLS certificate isn't trusted by default.
          </p>
          <p class="text-xs text-surface-500 mb-3">
            This is normal for self-hosted mail servers using a
            self-signed certificate. Compare the fingerprint below
            with your server's actual certificate before trusting.
          </p>
          <p class="text-xs mb-1"><span class="text-surface-500">Host:</span> <span class="font-mono">{pendingCert.host}</span></p>
          <div class="text-xs mb-3">
            <p class="text-surface-500 mb-1">
              SHA-256 fingerprint{pendingCert.chain.length === 1 ? '' : 's'}
              ({pendingCert.chain.length === 1 ? 'leaf' : `leaf + ${pendingCert.chain.length - 1} intermediate${pendingCert.chain.length === 2 ? '' : 's'}`}):
            </p>
            <ul class="space-y-1">
              {#each pendingCert.chain as entry, i (entry.sha256)}
                <li class="font-mono break-all">
                  <span class="text-surface-500">{i === 0 ? 'leaf:' : `int${i}:`}</span>
                  {entry.sha256}
                </li>
              {/each}
            </ul>
          </div>
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
        <div class="mb-4 p-3 rounded-md border border-success-500/30 bg-success-500/5 text-xs text-surface-600 dark:text-surface-400 flex items-center gap-2">
          <Icon name="verified" size={14} />
          Trusting {trustedCerts.length}
          self-signed certificate{trustedCerts.length === 1 ? '' : 's'}
          for this account.
        </div>
      {/if}

      <!-- Navigation buttons -->
      <div class="flex justify-between mt-6">
        {#if step > 0}
          <button class="btn preset-outlined-surface-500 flex items-center gap-1" onclick={prevStep}>
            <Icon name="arrow-left" size={14} />
            Back
          </button>
        {:else}
          <div></div>
        {/if}

        {#if step < totalSteps - 1}
          <button class="btn preset-filled-primary-500 flex items-center gap-1" onclick={nextStep}>
            Next
            <Icon name="arrow-right" size={14} />
          </button>
        {:else}
          <button
            class="btn preset-filled-primary-500 flex items-center gap-1"
            onclick={submit}
            disabled={saving}
          >
            {#if saving}
              <Icon name="loading" size={14} />
              Saving…
            {:else}
              <Icon name="add-account" size={14} />
              Add Account
            {/if}
          </button>
        {/if}
      </div>
    </div>
  </div>
</div>
