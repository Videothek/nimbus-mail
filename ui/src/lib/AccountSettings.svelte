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
  import { THEMES, applyTheme, type ThemeMode } from './theme'

  // ── Types ───────────────────────────────────────────────────
  // Mirrors the Rust `Account` struct from nimbus-core
  interface FolderIconRule {
    keyword: string
    icon: string
  }
  interface Account {
    id: string
    display_name: string
    email: string
    imap_host: string
    imap_port: number
    smtp_host: string
    smtp_port: number
    use_jmap: boolean
    jmap_url?: string | null
    signature?: string | null
    folder_icons?: FolderIconRule[]
    /** Per-account TLS trust list. Each entry is a leaf cert the
     *  user has explicitly trusted via the AccountSetup wizard or
     *  the Re-trust button below. Round-tripped through
     *  `update_account` whenever the trust list changes (e.g.
     *  after a server cert renewal). */
    trusted_certs?: TrustedCert[]
    folder_icon_overrides?: Record<string, string>
  }

  interface TrustedCert {
    /** DER bytes as a JSON byte-array — matches the Rust
     *  `Vec<u8>` serialisation. */
    der: number[]
    /** SHA-256 fingerprint, lowercase hex with `:` separators. */
    sha256: string
    host: string
    /** Unix epoch seconds when the cert was trusted. */
    added_at: number
  }

  // ── Props ───────────────────────────────────────────────────
  interface Props {
    onclose: () => void         // Go back to the inbox view
    onaddaccount: () => void    // Switch to the setup wizard to add another account
    /** Notify the parent (App.svelte) whenever app-wide preferences
        change so it can keep its cached snapshot — and the theme
        `$effect` it drives — in sync. Optional so callers that don't
        care about live updates aren't forced to handle it. */
    onappprefschanged?: (prefs: AppSettings) => void
  }
  let { onclose, onaddaccount, onappprefschanged }: Props = $props()

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
    theme_name: string
    theme_mode: ThemeMode
    mail_html_white_background: boolean
    auto_advance_after_remove: boolean
    default_calendar_id: string | null
    talk_reminder_enabled: boolean
  }

  let appSettings = $state<AppSettings>({
    minimize_to_tray: true,
    background_sync_enabled: true,
    background_sync_interval_secs: 300,
    notifications_enabled: true,
    start_minimized: false,
    theme_name: 'cerberus',
    theme_mode: 'system',
    mail_html_white_background: true,
    auto_advance_after_remove: true,
    default_calendar_id: null,
    talk_reminder_enabled: true,
  })

  // Calendar list for the "default calendar" picker.  Loaded
  // lazily once on mount alongside the other app settings.
  // Empty list = no Nextcloud connected yet → setting is hidden.
  interface CalendarRow {
    id: string
    nextcloud_account_id: string
    display_name: string
    color: string | null
    last_synced_at: string | null
    hidden?: boolean
  }
  let calendarsForPicker = $state<CalendarRow[]>([])
  let prefsSaveStatus = $state<'' | 'saving' | 'saved' | 'error'>('')
  let checkNowBusy = $state(false)

  // ── Load accounts on mount ──────────────────────────────────
  // $effect runs when the component is first rendered (like onMount).
  // We call the Rust backend to get all saved accounts.
  $effect(() => {
    loadAccounts()
    loadAppSettings()
    loadCalendarsForPicker()
  })

  async function loadCalendarsForPicker() {
    try {
      const accounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
      const all: CalendarRow[] = []
      for (const acc of accounts) {
        try {
          const cs = await invoke<CalendarRow[]>('get_cached_calendars', {
            ncId: acc.id,
          })
          all.push(...cs)
        } catch (e) {
          console.warn('default-calendar picker: get_cached_calendars failed', e)
        }
      }
      calendarsForPicker = all.filter((c) => !c.hidden)
    } catch (e) {
      console.warn('default-calendar picker: get_nextcloud_accounts failed', e)
      calendarsForPicker = []
    }
  }

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
    // Tell the parent immediately so its derived state (notification
    // toggle, theme `$effect`) reacts without waiting for the debounce.
    onappprefschanged?.({ ...appSettings })
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

  /** Theme picker handler — apply the change to the DOM immediately
      so the user sees it before the debounced save fires. We still go
      through `scheduleSave` so the persistence + parent-notify path
      is unchanged. */
  function onThemeChange(name: string, mode: ThemeMode) {
    appSettings.theme_name = name
    appSettings.theme_mode = mode
    applyTheme(name, mode)
    scheduleSave()
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

  // ── TLS re-trust flow ───────────────────────────────────────
  //
  // The AccountSetup wizard already trusts a self-signed leaf at
  // the moment the account is added, but the trust list is frozen
  // after that. When the user's mail server rotates its cert
  // (Let's-Encrypt-style renewals on a self-signed CA, manual
  // re-issuance, etc.) every IMAP/SMTP connect bombs with
  // "invalid peer certificate: UnknownIssuer" and there's no in-
  // app way to recover — the user's stuck reading nothing.
  //
  // This pair of helpers re-runs the same probe-and-trust dance
  // the wizard uses, against an account that already exists. The
  // probe captures the *full chain* (leaf + intermediates) the
  // server is currently presenting, the user reviews each
  // fingerprint, and on confirm every cert in the chain is
  // appended to `account.trusted_certs`. Trusting the whole chain
  // (not just the leaf) means a future leaf reissue under the
  // same intermediate, or a server that reorders certs on the
  // wire, still resolves through the verifier's chain-walk
  // matcher without dropping the user back into this prompt.

  interface ProbedCertEntry {
    der: number[]
    sha256: string
  }
  interface ProbedCert {
    chain: ProbedCertEntry[]
    host: string
  }

  /** Probed chain awaiting confirmation. `null` = no flow open. */
  let trustPrompt = $state<{
    account: Account
    chain: ProbedCertEntry[]
  } | null>(null)
  let trustBusy = $state(false)
  let trustError = $state('')

  async function startRetrust(account: Account) {
    trustError = ''
    trustBusy = true
    try {
      const probed = await invoke<ProbedCert>('probe_server_certificate', {
        host: account.imap_host,
        port: account.imap_port,
      })
      trustPrompt = {
        account,
        chain: probed.chain,
      }
    } catch (e: any) {
      trustError = typeof e === 'string' ? e : e?.message ?? 'Failed to probe server certificate'
    } finally {
      trustBusy = false
    }
  }

  async function commitRetrust() {
    if (!trustPrompt || trustBusy) return
    trustBusy = true
    trustError = ''
    try {
      // Append every cert in the probed chain to the account's
      // trust list. We don't dedupe — `nimbus_core::tls::
      // build_client_config` happily accepts duplicates, and an
      // exact-match dupe is harmless. Anything else (cert renewed
      // under same CN, server moved hosts, …) is a *new* entry
      // the user explicitly wants.
      const addedAt = Math.floor(Date.now() / 1000)
      const additions: TrustedCert[] = trustPrompt.chain.map((entry) => ({
        der: entry.der,
        sha256: entry.sha256,
        host: trustPrompt!.account.imap_host,
        added_at: addedAt,
      }))
      const updated: Account = {
        ...trustPrompt.account,
        trusted_certs: [...(trustPrompt.account.trusted_certs ?? []), ...additions],
      }
      await invoke('update_account', { account: updated })
      trustPrompt = null
      await loadAccounts()
    } catch (e: any) {
      trustError = typeof e === 'string' ? e : e?.message ?? 'Failed to update account'
    } finally {
      trustBusy = false
    }
  }

  function cancelRetrust() {
    trustPrompt = null
    trustError = ''
  }

  // ── Signature editing ───────────────────────────────────────
  // The signature lives directly on the `Account` row. Edits are
  // debounced like the app preferences so dragging through a long
  // textarea doesn't write to disk on every keystroke.
  const sigSaveTimers = new Map<string, ReturnType<typeof setTimeout>>()
  const sigSaveStatus = $state<Record<string, '' | 'saving' | 'saved' | 'error'>>({})

  function onSignatureChange(account: Account, next: string) {
    account.signature = next
    sigSaveStatus[account.id] = 'saving'
    const existing = sigSaveTimers.get(account.id)
    if (existing) clearTimeout(existing)
    sigSaveTimers.set(
      account.id,
      setTimeout(async () => {
        try {
          // The Rust `update_account` takes the full Account record;
          // sending the in-place edited copy is fine because we never
          // mutate fields the user can't edit here (host/port/etc).
          await invoke('update_account', {
            account: { ...account, signature: next.trim() || null },
          })
          sigSaveStatus[account.id] = 'saved'
          setTimeout(() => {
            if (sigSaveStatus[account.id] === 'saved') sigSaveStatus[account.id] = ''
          }, 1500)
        } catch (e) {
          console.warn('failed to save signature', e)
          sigSaveStatus[account.id] = 'error'
        }
      }, 400),
    )
  }

  // ── Custom folder icons (Issue #63) ─────────────────────────
  // Each account carries a list of `{keyword, icon}` rules. The
  // sidebar applies them before its built-in icon heuristics. Edits
  // save immediately (no debounce) — the dataset is tiny and each
  // change is the result of an explicit click, not keystroke spam.
  const iconSaveStatus = $state<Record<string, '' | 'saving' | 'saved' | 'error'>>({})
  // svelte-ignore state_referenced_locally
  const iconDrafts = $state<Record<string, { keyword: string; icon: string }>>({})

  /** Make sure every account has a stable draft slot before the
      template tries to `bind:` to it — `bind:` requires a plain
      MemberExpression, so the slot has to exist up-front. Runs as
      an `$effect` so it covers both initial load and any later
      account additions. */
  $effect(() => {
    for (const a of accounts) {
      if (!iconDrafts[a.id]) iconDrafts[a.id] = { keyword: '', icon: '' }
    }
  })

  async function persistIcons(account: Account, rules: FolderIconRule[]) {
    iconSaveStatus[account.id] = 'saving'
    account.folder_icons = rules
    try {
      await invoke('update_account', {
        account: { ...account, folder_icons: rules },
      })
      iconSaveStatus[account.id] = 'saved'
      setTimeout(() => {
        if (iconSaveStatus[account.id] === 'saved') iconSaveStatus[account.id] = ''
      }, 1500)
    } catch (e) {
      console.warn('failed to save folder icons', e)
      iconSaveStatus[account.id] = 'error'
    }
  }

  function addIconRule(account: Account) {
    const draft = iconDrafts[account.id]
    if (!draft) return
    const keyword = draft.keyword.trim()
    const icon = draft.icon.trim()
    if (!keyword || !icon) return
    const rules = [...(account.folder_icons ?? []), { keyword, icon }]
    iconDrafts[account.id] = { keyword: '', icon: '' }
    void persistIcons(account, rules)
  }

  function removeIconRule(account: Account, idx: number) {
    const rules = (account.folder_icons ?? []).filter((_, i) => i !== idx)
    void persistIcons(account, rules)
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
            bind:checked={appSettings.talk_reminder_enabled}
            onchange={scheduleSave}
          />
          <span>
            Notify me before meetings with a Talk room
            <span class="block text-xs text-surface-500">
              Lead time follows the event's own reminder.
            </span>
          </span>
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

        <label class="flex items-center gap-2">
          <input
            type="checkbox"
            class="checkbox"
            bind:checked={appSettings.auto_advance_after_remove}
            onchange={scheduleSave}
          />
          <span>After delete / archive, open the next message automatically</span>
        </label>

        <!-- Default calendar.  Used by the EventEditor as the
             pre-selected calendar in create-mode, and by the
             RSVP card as the default destination for accepted
             invites.  Hidden when no Nextcloud calendars are
             cached yet (the user hasn't connected NC, or the
             initial sync hasn't run). -->
        {#if calendarsForPicker.length > 0}
          <label class="flex items-center gap-2 pt-2">
            <span class="shrink-0">Default calendar</span>
            <select
              class="select px-2 py-1 text-sm rounded-md flex-1 max-w-[320px]"
              bind:value={appSettings.default_calendar_id}
              onchange={scheduleSave}
            >
              <option value={null}>(use first available)</option>
              {#each calendarsForPicker as c (c.id)}
                <option value={c.id}>{c.display_name}</option>
              {/each}
            </select>
          </label>
        {/if}
      </div>
    </div>

    <!-- Appearance (Issue #17) — theme + light/dark mode picker.
         Changes apply live via `onThemeChange` so the user sees the
         result before navigating away from settings. -->
    <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg mb-6">
      <div class="flex items-center justify-between mb-3">
        <h2 class="text-base font-semibold">Appearance</h2>
      </div>

      <div class="space-y-4 text-sm">
        <div>
          <p class="font-medium mb-2">Mode</p>
          <div class="flex gap-2">
            {#each ['system', 'light', 'dark'] as const as mode}
              <button
                type="button"
                class="btn btn-sm {appSettings.theme_mode === mode
                  ? 'preset-filled-primary-500'
                  : 'preset-outlined-surface-500'}"
                onclick={() => onThemeChange(appSettings.theme_name, mode)}
              >
                {mode === 'system' ? 'Follow OS' : mode === 'light' ? 'Light' : 'Dark'}
              </button>
            {/each}
          </div>
          <p class="text-xs text-surface-400 mt-1">
            "Follow OS" tracks your system light/dark preference live.
          </p>
        </div>

        <div>
          <p class="font-medium mb-2">Theme</p>
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
            {#each THEMES as theme (theme.id)}
              <button
                type="button"
                class="text-left p-3 rounded-md border transition-colors {appSettings.theme_name ===
                theme.id
                  ? 'border-primary-500 bg-primary-500/10'
                  : 'border-surface-300 dark:border-surface-700 hover:bg-surface-200 dark:hover:bg-surface-700'}"
                onclick={() => onThemeChange(theme.id, appSettings.theme_mode)}
              >
                <div class="font-medium">{theme.label}</div>
                <div class="text-xs text-surface-500 mt-0.5">{theme.description}</div>
              </button>
            {/each}
          </div>
        </div>

        <div>
          <p class="font-medium mb-2">HTML mail background</p>
          <div class="flex flex-wrap gap-2">
            <button
              type="button"
              class="btn btn-sm {appSettings.mail_html_white_background
                ? 'preset-filled-primary-500'
                : 'preset-outlined-surface-500'}"
              onclick={() => {
                appSettings.mail_html_white_background = true
                scheduleSave()
              }}
            >Always white</button>
            <button
              type="button"
              class="btn btn-sm {!appSettings.mail_html_white_background
                ? 'preset-filled-primary-500'
                : 'preset-outlined-surface-500'}"
              onclick={() => {
                appSettings.mail_html_white_background = false
                scheduleSave()
              }}
            >Use mail's theme</button>
          </div>
          <p class="text-xs text-surface-400 mt-1">
            HTML emails usually assume a white background — "Always white" keeps
            them readable in dark mode. "Use mail's theme" lets the email render
            against the app's background, which respects dark-mode-aware emails
            but can wash out the rest. Each open mail also has its own toggle
            to override this default.
          </p>
        </div>
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
        {#each accounts as account (account.id)}
          <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg">
            <div class="flex items-start justify-between">
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
              <div class="flex flex-col items-end gap-1">
                <button
                  class="btn btn-sm preset-outlined-surface-500 text-xs"
                  disabled={trustBusy}
                  title="Probe the IMAP server's current TLS certificate and add it to this account's trust list. Use after a server cert renewal if connections start failing with 'invalid peer certificate / UnknownIssuer'."
                  onclick={() => void startRetrust(account)}
                >
                  {trustBusy ? '…' : '🔒 Trust server cert'}
                </button>
                <button
                  class="btn btn-sm preset-outlined-error-500"
                  onclick={() => removeAccount(account.id, account.email)}
                >
                  Remove
                </button>
              </div>
            </div>

            <div class="mt-4 pt-4 border-t border-surface-200 dark:border-surface-700">
              <div class="flex items-center justify-between mb-1">
                <label class="text-sm font-medium" for="sig-{account.id}">Signature</label>
                {#if sigSaveStatus[account.id] === 'saving'}
                  <span class="text-xs text-surface-400">Saving…</span>
                {:else if sigSaveStatus[account.id] === 'saved'}
                  <span class="text-xs text-success-500">Saved</span>
                {:else if sigSaveStatus[account.id] === 'error'}
                  <span class="text-xs text-error-500">Save failed</span>
                {/if}
              </div>
              <textarea
                id="sig-{account.id}"
                rows="4"
                value={account.signature ?? ''}
                oninput={(e) => onSignatureChange(account, (e.currentTarget as HTMLTextAreaElement).value)}
                placeholder="Appended to new messages sent from this account."
                class="input w-full px-3 py-2 rounded-md font-mono text-sm"
              ></textarea>
            </div>

            <!-- Folder icon rules (Issue #63). Match a folder name
                 against a keyword and show the chosen icon next to
                 it in the sidebar. Useful for personal categories
                 ("Bank", "Amazon", a project name) where the IMAP
                 special-use attributes don't help. -->
            <div class="mt-4 pt-4 border-t border-surface-200 dark:border-surface-700">
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm font-medium">Folder icons</span>
                {#if iconSaveStatus[account.id] === 'saving'}
                  <span class="text-xs text-surface-400">Saving…</span>
                {:else if iconSaveStatus[account.id] === 'saved'}
                  <span class="text-xs text-success-500">Saved</span>
                {:else if iconSaveStatus[account.id] === 'error'}
                  <span class="text-xs text-error-500">Save failed</span>
                {/if}
              </div>

              {#if (account.folder_icons ?? []).length > 0}
                <ul class="space-y-1 mb-2">
                  {#each account.folder_icons ?? [] as rule, i (`${rule.keyword}:${i}`)}
                    <li class="flex items-center gap-2 text-sm">
                      <span class="text-lg w-6 text-center">{rule.icon}</span>
                      <span class="text-surface-500">contains</span>
                      <span class="font-mono">{rule.keyword}</span>
                      <button
                        type="button"
                        class="ml-auto text-xs text-error-500 hover:underline"
                        onclick={() => removeIconRule(account, i)}
                      >Remove</button>
                    </li>
                  {/each}
                </ul>
              {/if}

              {#if iconDrafts[account.id]}
                <div class="flex gap-2 items-center">
                  <input
                    type="text"
                    maxlength="4"
                    placeholder="🏦"
                    bind:value={iconDrafts[account.id].icon}
                    class="input text-lg text-center w-14 px-2 py-1 rounded-md"
                    aria-label="Icon"
                  />
                  <input
                    type="text"
                    placeholder="bank"
                    bind:value={iconDrafts[account.id].keyword}
                    onkeydown={(e) => e.key === 'Enter' && addIconRule(account)}
                    class="input flex-1 text-sm px-3 py-1 rounded-md"
                    aria-label="Folder name keyword"
                  />
                  <button
                    type="button"
                    class="btn btn-sm preset-outlined-primary-500"
                    disabled={!iconDrafts[account.id].icon.trim() || !iconDrafts[account.id].keyword.trim()}
                    onclick={() => addIconRule(account)}
                  >Add</button>
                </div>
              {/if}
              <p class="text-xs text-surface-400 mt-1">
                Match is case-insensitive against any folder whose
                name contains the keyword.
              </p>
            </div>
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

<!-- TLS re-trust confirm. Shown after `startRetrust` probes the
     IMAP host and captures a leaf cert. The user sees the SHA-256
     so they can compare against what they expected (matches the
     fingerprint Nextcloud / Let's Encrypt / their CA prints) before
     trusting it. -->
{#if trustPrompt}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onmousedown={(e) => { if (e.target === e.currentTarget && !trustBusy) cancelRetrust() }}
  >
    <div class="bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl w-md max-w-full p-5">
      <h3 class="text-base font-semibold mb-1">Trust this server certificate?</h3>
      <p class="text-xs text-surface-500 mb-3">
        For <span class="font-medium text-surface-700 dark:text-surface-300">{trustPrompt.account.email}</span>
        on <span class="font-mono">{trustPrompt.account.imap_host}:{trustPrompt.account.imap_port}</span>.
        Compare the SHA-256 against what your server admin (or Nextcloud's
        <em>Personal → Security</em> page) shows before clicking Trust.
      </p>

      <div class="text-xs text-surface-500 mb-1">
        SHA-256 fingerprint{trustPrompt.chain.length === 1 ? '' : 's'}
        ({trustPrompt.chain.length === 1
          ? 'leaf'
          : `leaf + ${trustPrompt.chain.length - 1} intermediate${trustPrompt.chain.length === 2 ? '' : 's'}`})
      </div>
      <ul class="font-mono text-xs wrap-break-word p-2 rounded bg-surface-100 dark:bg-surface-800 mb-3 space-y-1">
        {#each trustPrompt.chain as entry, i (entry.sha256)}
          <li>
            <span class="text-surface-500">{i === 0 ? 'leaf:' : `int${i}:`}</span>
            {entry.sha256}
          </li>
        {/each}
      </ul>

      {#if trustError}
        <p class="text-xs text-red-500 mb-3 wrap-break-word">{trustError}</p>
      {/if}

      <div class="flex justify-end gap-2">
        <button
          class="btn preset-outlined-surface-500"
          disabled={trustBusy}
          onclick={cancelRetrust}
        >Cancel</button>
        <button
          class="btn preset-filled-primary-500"
          disabled={trustBusy}
          onclick={() => void commitRetrust()}
        >{trustBusy ? 'Trusting…' : 'Trust'}</button>
      </div>
    </div>
  </div>
{/if}

<!-- Standalone error surface for the probe path — fires when
     `startRetrust` itself errored (no modal opens) so the user
     still sees what went wrong. -->
{#if trustError && !trustPrompt}
  <div class="fixed bottom-4 right-4 z-50 max-w-sm bg-red-500/95 text-white text-sm rounded-md shadow-lg px-3 py-2">
    {trustError}
    <button
      class="ml-2 underline"
      onclick={() => (trustError = '')}
    >Dismiss</button>
  </div>
{/if}
