<script lang="ts">
  /**
   * LockScreen — shown at app startup when the SQLCipher cache
   * is in FIDO-only mode (#164 Phase 1B).  Lists every registered
   * unlock method (hardware keys + the recovery passphrase),
   * runs WebAuthn or a passphrase prompt, and asks Rust to open
   * the cache pool.
   *
   * Once the unlock IPC succeeds the parent (`App.svelte`)
   * receives `onunlock()` and routes the user into the inbox.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import { evaluateFidoPrf } from './webauthnPrf'

  interface Method {
    kind: 'fido_prf' | 'passphrase'
    credentialId: string
    label: string
    salt: string
    createdAt: number
  }

  interface Props {
    methods: Method[]
    onunlock: () => void
  }
  let { methods, onunlock }: Props = $props()

  let busy = $state(false)
  let error = $state('')
  let passphraseValue = $state('')
  let activeMethod = $state<Method | null>(null)

  // Default focus: prefer a hardware key (one tap, no typing) if
  // any are registered; otherwise show the passphrase prompt.
  $effect(() => {
    if (activeMethod) return
    activeMethod = methods.find((m) => m.kind === 'fido_prf') ?? methods[0] ?? null
  })

  async function unlockWithPassphrase() {
    if (busy || !passphraseValue) return
    busy = true
    error = ''
    try {
      await invoke('unlock_with_passphrase', { passphrase: passphraseValue })
      passphraseValue = ''
      onunlock()
    } catch (e) {
      error = formatError(e) || 'Unlock failed'
    } finally {
      busy = false
    }
  }

  async function unlockWithFido(method: Method) {
    if (busy) return
    busy = true
    error = ''
    try {
      // OS sheet pops here — Touch ID / Windows Hello / "tap
      // your security key", same UX surface the enrollment
      // already uses.
      const prfOutput = await evaluateFidoPrf(method.credentialId, method.salt)
      await invoke('unlock_with_prf', {
        credentialIdB64: method.credentialId,
        prfOutputB64: prfOutput,
      })
      onunlock()
    } catch (e) {
      error = formatError(e) || 'Unlock failed'
    } finally {
      busy = false
    }
  }

  function handleEnter(e: KeyboardEvent) {
    if (e.key === 'Enter' && activeMethod?.kind === 'passphrase') {
      e.preventDefault()
      void unlockWithPassphrase()
    }
  }
</script>

<div class="fixed inset-0 z-[1000] flex items-center justify-center bg-surface-50 dark:bg-surface-900">
  <div class="w-full max-w-md p-8 space-y-6">
    <div class="text-center space-y-1">
      <div class="text-5xl mb-2" aria-hidden="true">🔒</div>
      <h1 class="text-2xl font-semibold">Nimbus is locked</h1>
      <p class="text-sm text-surface-500">
        Authenticate to open your encrypted mail cache.
      </p>
    </div>

    {#if methods.length === 0}
      <div class="rounded-md border border-error-500/40 bg-error-500/10 p-4 text-sm">
        No unlock methods are registered.  Use the Settings → Security
        panel to enroll a hardware key or recovery passphrase before
        switching to FIDO-only mode.
      </div>
    {:else}
      <!-- Method picker — shown when more than one method is
           registered.  Single-method case skips straight to the
           form for that method. -->
      {#if methods.length > 1}
        <div class="space-y-2">
          {#each methods as m (m.credentialId)}
            <button
              type="button"
              class="w-full flex items-center gap-3 px-4 py-3 rounded-md text-left transition-colors
                     border {activeMethod?.credentialId === m.credentialId
                       ? 'border-primary-500 bg-primary-500/10'
                       : 'border-surface-300 dark:border-surface-700 hover:bg-surface-200/60 dark:hover:bg-surface-800/40'}"
              onclick={() => (activeMethod = m)}
              disabled={busy}
            >
              <span class="text-lg" aria-hidden="true">
                {m.kind === 'passphrase' ? '🔐' : '🔑'}
              </span>
              <span class="flex-1 min-w-0">
                <span class="font-medium block truncate">
                  {m.kind === 'passphrase' ? 'Passphrase' : m.label}
                </span>
                <span class="text-xs text-surface-500 block">
                  {m.kind === 'passphrase' ? 'Recovery method' : 'Hardware key / biometric'}
                </span>
              </span>
            </button>
          {/each}
        </div>
      {/if}

      {#if activeMethod?.kind === 'passphrase'}
        <div class="space-y-2">
          <input
            type="password"
            class="input w-full text-sm px-3 py-2 rounded-md"
            placeholder="Passphrase"
            bind:value={passphraseValue}
            onkeydown={handleEnter}
            disabled={busy}
            autofocus
            autocomplete="current-password"
          />
          <button
            class="btn preset-filled-primary-500 w-full"
            disabled={busy || !passphraseValue}
            onclick={() => void unlockWithPassphrase()}
          >{busy ? 'Unlocking…' : 'Unlock'}</button>
        </div>
      {:else if activeMethod?.kind === 'fido_prf'}
        <div class="space-y-2">
          <p class="text-sm text-surface-500 text-center">
            Click below — your operating system will ask you to
            authenticate with the registered key.
          </p>
          <button
            class="btn preset-filled-primary-500 w-full"
            disabled={busy}
            onclick={() => activeMethod && void unlockWithFido(activeMethod)}
          >{busy ? 'Awaiting authenticator…' : `Unlock with ${activeMethod.label}`}</button>
        </div>
      {/if}

      {#if error}
        <p class="text-sm text-red-500 wrap-break-word text-center">{error}</p>
      {/if}
    {/if}
  </div>
</div>
