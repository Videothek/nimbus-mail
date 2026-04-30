<script lang="ts">
  /**
   * Security settings — FIDO unlock enrollment (#164, Phase 1A).
   *
   * Lists registered FIDO credentials for the SQLCipher database
   * and lets the user add or remove them.  Enrollment uses
   * WebAuthn's PRF extension via the OS — Touch ID, Windows
   * Hello, USB hardware key, whichever the user picks at the
   * authenticator sheet.  The PRF output gets shipped to Rust,
   * which uses it as an AES-256-GCM key to wrap the DB master
   * key inside the keychain envelope.
   *
   * Phase 1A: enrollment scaffolding.  The plain master key
   * stays in the keychain alongside the wraps so cold launch
   * still works without a hardware-key tap.  Phase 1B will
   * delete the plain key, gate startup on unlock, and surface
   * a real lock screen.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import {
    enrollFidoCredential,
    evaluateFidoPrf,
    isWebAuthnAvailable,
  } from './webauthnPrf'
  import Toggle from './Toggle.svelte'

  interface FidoCredential {
    kind: 'fido_prf' | 'passphrase'
    credentialId: string
    label: string
    salt: string
    createdAt: number
  }
  interface FidoStatus {
    hasPlainKey: boolean
    credentials: FidoCredential[]
  }

  /** Whether the registered methods meet the safety bar for
   *  enabling FIDO-only mode: at least one passphrase OR ≥ 2
   *  hardware keys.  Without one of those, losing a single key
   *  would lock the cache permanently. */
  function safeForFidoOnlyMode(s: FidoStatus | null): boolean {
    if (!s) return false
    const passphraseCount = s.credentials.filter((c) => c.kind === 'passphrase').length
    const fidoCount = s.credentials.filter((c) => c.kind === 'fido_prf').length
    return passphraseCount >= 1 || fidoCount >= 2
  }

  let status = $state<FidoStatus | null>(null)
  let loading = $state(true)
  let busy = $state(false)
  let error = $state('')
  // Just a coarse "WebAuthn API exists" check.  Don't gate on
  // PRF capability up front — older engines mis-report it; let
  // the actual `credentials.create` reveal the truth.
  const webauthnAvailable = $state(isWebAuthnAvailable())

  let newLabel = $state('')
  /** Fixed label for the passphrase entry — there's only ever
   *  one passphrase wrap (it's the recovery slot, not a per-
   *  device thing), so the user doesn't need to name it. */
  const PASSPHRASE_LABEL = 'Recovery passphrase'
  let passphraseValue = $state('')
  let passphraseConfirm = $state('')
  /** Master "Key Encryption" toggle.  Drives both the
   *  registration UI gate AND the backend FIDO-only-mode
   *  switch:
   *
   *  - User flips ON: registration UI becomes interactive.
   *    As soon as the registered methods cover a recovery
   *    path (≥ 1 passphrase or ≥ 2 hardware keys), the
   *    backend automatically drops the plain master key from
   *    the keychain envelope — every subsequent cold launch
   *    will require authentication.
   *  - User flips OFF: registration UI greys out.  If the
   *    backend is already in FIDO-only mode, we can't
   *    transparently restore the plain key (we'd need to
   *    re-derive the master via the unlock flow), so we
   *    surface that as a clear error and leave the toggle
   *    on until the user does the disable dance manually.
   *
   *  Persisted in localStorage so the user's intent survives
   *  launches even when no methods are registered yet
   *  (toggle stays "on" but the backend stays plain-mode
   *  until they enroll a method). */
  let keyEncryptionEnabled = $state(false)
  $effect(() => {
    try {
      keyEncryptionEnabled = localStorage.getItem('nimbus.keyEncryption') === '1'
    } catch {
      /* localStorage may be unavailable in some webview modes */
    }
  })
  function persistToggle(v: boolean) {
    try {
      localStorage.setItem('nimbus.keyEncryption', v ? '1' : '0')
    } catch {
      /* swallow — webview may not expose localStorage */
    }
  }
  async function setKeyEncryption(v: boolean) {
    if (busy) return
    if (v) {
      keyEncryptionEnabled = true
      persistToggle(true)
      // The backend flip happens in the $effect below once the
      // user has actually registered a recovery method.
    } else {
      // Turning off: if we're still in plain mode (no FIDO-only
      // activation has happened yet) the toggle is purely a UI
      // gate — flip it freely.  If FIDO-only mode is active we
      // need a real disable path, which isn't wired yet.
      if (status && !status.hasPlainKey) {
        error =
          'Disabling key encryption while the cache is in FIDO-only mode is not yet supported. Re-add your accounts to revert.'
        return
      }
      keyEncryptionEnabled = false
      persistToggle(false)
    }
  }
  /** Auto-activate FIDO-only mode when the toggle is on AND
   *  the registered methods cover a recovery path AND the
   *  backend still has the plain master key.  Saves the user
   *  from having to find a separate "activate" button. */
  $effect(() => {
    if (busy) return
    if (!keyEncryptionEnabled) return
    if (!status) return
    if (!status.hasPlainKey) return // already FIDO-only
    if (!safeForFidoOnlyMode(status)) return
    void (async () => {
      try {
        await invoke('enable_fido_only_mode')
        await loadStatus()
      } catch (e) {
        error = formatError(e) || 'Failed to activate key encryption'
      }
    })()
  })

  async function loadStatus() {
    loading = true
    try {
      status = await invoke<FidoStatus>('fido_status')
    } catch (e) {
      error = formatError(e) || 'Failed to load FIDO status'
    } finally {
      loading = false
    }
  }

  $effect(() => {
    void loadStatus()
  })

  async function addKey() {
    if (busy) return
    const label = newLabel.trim() || 'Untitled hardware key'
    busy = true
    error = ''
    try {
      // Generate a fresh PRF salt server-side so it shares the
      // app's RNG and can't be influenced from the renderer.
      const saltB64 = await invoke<string>('fido_generate_salt')
      // The OS shows its own auth sheet here; we receive the
      // PRF output once the user authenticates.
      const enrolled = await enrollFidoCredential(saltB64, 'nimbus-user', label)
      await invoke('fido_enroll', {
        credentialIdB64: enrolled.credentialIdB64,
        saltB64: enrolled.saltB64,
        prfOutputB64: enrolled.prfOutputB64,
        label,
      })
      newLabel = ''
      await loadStatus()
    } catch (e) {
      error = formatError(e) || 'Failed to enroll hardware key'
    } finally {
      busy = false
    }
  }

  async function addPassphrase() {
    if (busy) return
    if (passphraseValue.length < 8) {
      error = 'Passphrase must be at least 8 characters.'
      return
    }
    if (passphraseValue !== passphraseConfirm) {
      error = "Passphrase and confirmation don't match."
      return
    }
    busy = true
    error = ''
    try {
      await invoke('fido_enroll_passphrase', {
        passphrase: passphraseValue,
        label: PASSPHRASE_LABEL,
      })
      passphraseValue = ''
      passphraseConfirm = ''
      await loadStatus()
    } catch (e) {
      error = formatError(e) || 'Failed to enroll passphrase'
    } finally {
      busy = false
    }
  }

  async function removeKey(credentialId: string, salt: string, label: string) {
    if (busy) return
    if (!confirm(`Remove "${label}"? You'll need to re-enroll to use it again.`))
      return
    busy = true
    error = ''
    try {
      // Require the user to actually still possess the key
      // before we let them drop the wrap.  Skipped in plain-
      // key mode for the trivial case of an enrolled key the
      // user already lost — they can always reset by removing
      // the plain key entry from the keychain manually.
      if (status && !status.hasPlainKey) {
        await evaluateFidoPrf(credentialId, salt)
      }
      await invoke('fido_remove', { credentialIdB64: credentialId })
      await loadStatus()
    } catch (e) {
      error = formatError(e) || 'Failed to remove hardware key'
    } finally {
      busy = false
    }
  }

  function fmtDate(epoch: number): string {
    if (!epoch) return ''
    return new Date(epoch * 1000).toLocaleDateString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
    })
  }
</script>

<section class="space-y-6">
  <header>
    <h2 class="text-xl font-semibold">Security</h2>
    <p class="text-sm text-surface-500 mt-1 max-w-xl">
      Hardware-backed authentication for the local mail cache. Register a
      hardware key (YubiKey, Apple Touch ID, Windows Hello, …) to seal the
      cache's encryption key behind a tap or biometric.
    </p>
  </header>

  {#if loading}
    <p class="text-sm text-surface-500">Loading…</p>
  {:else}
    <!-- Master toggle.  Sits above the registration sections,
         left-aligned, rendered as a true iOS-style switch
         (track + sliding thumb) rather than a native checkbox.
         When off, every section below greys out and stops
         accepting input — clear visual signal that the feature
         is opt-in. -->
    <div class="flex items-center gap-3">
      <Toggle
        checked={keyEncryptionEnabled}
        label="Enable key encryption"
        onchange={(v) => void setKeyEncryption(v)}
      />
      <div class="max-w-xl">
        <p class="font-medium leading-tight">Key Encryption</p>
        <p class="text-xs text-surface-500 leading-snug mt-1">
          {#if !keyEncryptionEnabled}
            Off — the cache opens automatically using the plain master key
            stored in your OS keychain.  Turn on to seal the key behind a
            registered hardware key, biometric, or passphrase.
          {:else if status && !status.hasPlainKey}
            Active — your cache is sealed.  Every cold launch will ask you
            to authenticate with one of the registered methods below before
            the inbox loads.
          {:else if status && safeForFidoOnlyMode(status)}
            Activating… your registered methods will protect the cache on
            the next start.  (Activation usually completes within a moment
            of enrollment.)
          {:else}
            On — register at least one recovery method below.  As soon as
            you have one passphrase or two hardware keys registered, key
            encryption activates automatically and future launches will
            require authentication.
          {/if}
        </p>
      </div>
    </div>

    <div
      class="space-y-4 transition-opacity {keyEncryptionEnabled
        ? ''
        : 'opacity-50 pointer-events-none select-none'}"
      aria-disabled={!keyEncryptionEnabled}
    >
      <!-- Hardware-key enrollment is gated on a working WebAuthn
           API.  Linux builds whose libwebkit2gtk lacks WebAuthn
           (the default on most stable distros today) get a
           warning box instead of the form, but passphrase
           enrollment below still works and is the recommended
           path on those systems. -->
      {#if webauthnAvailable}
        <div class="rounded-md border border-surface-200 dark:border-surface-700 p-4">
          <h3 class="font-medium mb-2">Add a hardware key</h3>
          <p class="text-xs text-surface-500 mb-3">
            You'll be asked to authenticate (touch your security key, scan
            a fingerprint, …). The OS handles the prompt; Nimbus only sees
            the resulting key material.
          </p>
          <div class="flex items-center gap-2">
            <input
              type="text"
              class="input flex-1 text-sm px-3 py-1.5 rounded-md"
              placeholder="Label — e.g. “YubiKey 5C”, “MacBook Touch ID”"
              bind:value={newLabel}
              disabled={busy}
            />
            <button
              class="btn preset-filled-primary-500"
              disabled={busy}
              onclick={() => void addKey()}
            >{busy ? 'Working…' : 'Add'}</button>
          </div>
        </div>
      {:else}
        <div class="rounded-md border border-warning-500/40 bg-warning-500/10 p-4 text-sm text-warning-700 dark:text-warning-300">
          <p class="font-medium mb-1">Hardware-key registration is unavailable on this build.</p>
          <p class="text-xs">
            The webview Tauri uses on Linux (<code>libwebkit2gtk</code>) doesn't
            expose <code>navigator.credentials</code> on most stable distros
            yet.  You can still register a recovery passphrase below — it
            uses the same envelope and unlocks the same way at startup.
            Hardware keys will become available once your distro ships
            WebKitGTK ≥ 2.46 with WebAuthn enabled.
          </p>
        </div>
      {/if}

      <div class="rounded-md border border-surface-200 dark:border-surface-700 p-4">
        <h3 class="font-medium mb-2">Add a recovery passphrase</h3>
        <p class="text-xs text-surface-500 mb-3">
          A passphrase derives the same kind of 32-byte key (via
          PBKDF2-HMAC-SHA-256, 720 000 iterations) that a FIDO
          authenticator's PRF output would. Useful as a fallback when a
          hardware key is lost — and as the primary unlock method on
          Linux until WebKitGTK ships the WebAuthn PRF extension.
        </p>
        <div class="space-y-2">
          <input
            type="password"
            class="input w-full text-sm px-3 py-1.5 rounded-md"
            placeholder="Passphrase (8+ characters)"
            bind:value={passphraseValue}
            disabled={busy}
            autocomplete="new-password"
          />
          <input
            type="password"
            class="input w-full text-sm px-3 py-1.5 rounded-md"
            placeholder="Confirm passphrase"
            bind:value={passphraseConfirm}
            disabled={busy}
            autocomplete="new-password"
          />
          <button
            class="btn preset-filled-primary-500 w-full"
            disabled={busy || passphraseValue.length < 8}
            onclick={() => void addPassphrase()}
          >{busy ? 'Working…' : 'Save passphrase'}</button>
        </div>
      </div>

      <div>
        <h3 class="font-medium mb-2">Registered methods</h3>
        {#if status && status.credentials.length === 0}
          <p class="text-sm text-surface-500 italic">
            No unlock methods registered yet.
          </p>
        {:else if status}
          <ul class="divide-y divide-surface-200 dark:divide-surface-700 rounded-md border border-surface-200 dark:border-surface-700">
            {#each status.credentials as c (c.credentialId)}
              <li class="flex items-center gap-3 p-3">
                <span class="text-lg" aria-hidden="true">
                  {c.kind === 'passphrase' ? '🔐' : '🔑'}
                </span>
                <div class="flex-1 min-w-0">
                  <!-- For passphrase entries we surface the kind
                       directly; the internal label is an
                       implementation detail the user never set
                       and shouldn't have to see.  Hardware keys
                       keep the user-supplied label since that's
                       what distinguishes one device from another. -->
                  <p class="font-medium truncate">
                    {c.kind === 'passphrase' ? 'Passphrase' : c.label}
                  </p>
                  <p class="text-xs text-surface-500 truncate">
                    {c.kind === 'passphrase' ? 'Recovery method' : 'Hardware key'}
                    · Added {fmtDate(c.createdAt)}
                  </p>
                </div>
                <button
                  class="btn btn-sm preset-outlined-error-500"
                  disabled={busy}
                  onclick={() => void removeKey(c.credentialId, c.salt, c.label)}
                >Remove</button>
              </li>
            {/each}
          </ul>
        {/if}
      </div>


      {#if error}
        <p class="text-sm text-red-500 wrap-break-word">{error}</p>
      {/if}
    </div>
  {/if}
</section>
