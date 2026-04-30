<script lang="ts">
  /**
   * AccountSettings — displays and manages configured email accounts.
   *
   * Shows a list of all accounts with options to remove them or
   * add new ones. This is accessible from the sidebar's settings
   * area and lets users manage their accounts after initial setup.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { open as openFileDialog } from '@tauri-apps/plugin-dialog'
  import { enable as autostartEnable, disable as autostartDisable, isEnabled as autostartIsEnabled } from '@tauri-apps/plugin-autostart'
  import NextcloudSettings from './NextcloudSettings.svelte'
  import SecuritySettings from './SecuritySettings.svelte'
  import EmojiPicker from './EmojiPicker.svelte'
  import Toggle from './Toggle.svelte'
  import {
    STOCK_THEMES,
    applyTheme,
    type ThemeMode,
    type ThemeOption,
  } from './theme'

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
    /** Optional emoji avatar for the IconRail (#115). */
    emoji?: string | null
    /** Display order in the IconRail; lower = top. */
    sort_order?: number
    /** Human's full name for outbound From: header (#115). */
    person_name?: string | null
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

  // ── Category navigation (#131) ──────────────────────────────
  // Settings used to be one long scroll; #131 split it into the
  // categories users actually look for.  The nav lives in a
  // left column and `activeCategory` gates which section block
  // renders so each panel stays focused.
  type SettingsCategory = 'general' | 'design' | 'mail' | 'calendar' | 'nextcloud' | 'security'
  let activeCategory = $state<SettingsCategory>('general')
  interface CategoryEntry {
    id: SettingsCategory
    label: string
    icon: string
  }
  const CATEGORIES: CategoryEntry[] = [
    { id: 'general', label: 'General', icon: '⚙️' },
    { id: 'design', label: 'Design', icon: '🎨' },
    { id: 'mail', label: 'E-Mail', icon: '📧' },
    { id: 'calendar', label: 'Calendar', icon: '📅' },
    { id: 'nextcloud', label: 'Nextcloud', icon: '☁️' },
    { id: 'security', label: 'Security', icon: '🔒' },
  ]

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
    autostart_enabled: boolean
    /** User-imported Skeleton themes (#132 tier 2). */
    custom_themes?: CustomThemeRow[]
  }
  interface CustomThemeRow {
    id: string
    label: string
    description?: string
    path: string
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
    autostart_enabled: false,
    custom_themes: [],
  })

  /** Picker rows — stock themes plus the user's imports.  Driven
   *  by `appSettings.custom_themes` so a fresh import / remove
   *  triggers Svelte's reactivity automatically (the previous
   *  Proxy-based `THEMES` export couldn't, because it was a
   *  plain module-level mutable). */
  const pickerThemes = $derived<ThemeOption[]>([
    ...STOCK_THEMES,
    ...((appSettings.custom_themes ?? []).map((t) => ({
      id: t.id,
      label: t.label,
      description: t.description ?? 'Imported theme',
      custom: true,
    }))),
  ])

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

  /** Toggle "launch on login" via the autostart plugin and
   *  persist the user's choice.  We talk to the OS first
   *  because the plugin call is the cross-platform side-effect
   *  (XDG entry / LaunchAgent / registry key); only on its
   *  success do we commit the new bit to AppSettings.  That
   *  way a misconfigured environment (e.g. read-only
   *  ~/.config) can't leave us with a checked box that doesn't
   *  actually autostart. */
  async function onAutostartToggle(next: boolean) {
    try {
      if (next) await autostartEnable()
      else await autostartDisable()
      appSettings.autostart_enabled = next
      scheduleSave()
    } catch (e) {
      console.warn('autostart toggle failed', e)
      // Roll the checkbox state back so the UI matches reality.
      appSettings.autostart_enabled = !next
      prefsSaveStatus = 'error'
    }
  }

  /** Reconcile the stored bit against the OS on mount — picks
   *  up the case where the user removed the autostart entry
   *  manually (e.g. via system settings) since the last
   *  launch. */
  $effect(() => {
    void autostartIsEnabled()
      .then((enabled) => {
        if (enabled !== appSettings.autostart_enabled) {
          appSettings.autostart_enabled = enabled
        }
      })
      .catch((e) => console.warn('autostart isEnabled failed', e))
  })

  // ── Custom theme import (#132 tier 2) ──────────────────────
  // The picker shows an "Import theme…" button next to the
  // Theme heading.  Clicking it opens a native file dialog
  // restricted to `.css` files; the picked path is handed to
  // the backend's `import_custom_theme` IPC, which copies the
  // bytes into the app's themes dir, parses out the
  // `[data-theme="…"]` slug, and persists a `CustomTheme` row.
  // App.svelte's `custom-themes-changed` listener picks the
  // change up and re-seeds the theme module's runtime registry,
  // so the new entry appears in the picker without a reload.
  let importingTheme = $state(false)
  async function importCustomTheme() {
    if (importingTheme) return
    importingTheme = true
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: 'CSS theme', extensions: ['css'] }],
      })
      if (!picked) return
      // tauri-plugin-dialog returns the path as a plain string when
      // `multiple: false, directory: false` is set.
      const path = picked
      const fileName = path.split(/[\\/]/).pop() ?? ''
      const stem = fileName.replace(/\.css$/i, '')
      // Reasonable default label — the user can rename later.
      const label = stem.replace(/[_-]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase())
      await invoke('import_custom_theme', {
        sourcePath: path,
        label,
      })
      // Re-pull the live settings snapshot — the import added
      // a row to `custom_themes`, and the picker reads it via
      // `pickerThemes` $derived.  Without this refresh the new
      // theme would only appear after a full reload.
      await reloadSettingsSnapshot()
    } catch (e) {
      console.warn('import_custom_theme failed', e)
    } finally {
      importingTheme = false
    }
  }
  async function removeCustomTheme(id: string) {
    if (!confirm('Remove this custom theme? The CSS file in the app data folder will be deleted.')) {
      return
    }
    try {
      await invoke('remove_custom_theme', { id })
      await reloadSettingsSnapshot()
    } catch (e) {
      console.warn('remove_custom_theme failed', e)
    }
  }
  /** Pull the just-saved AppSettings back into `appSettings`
   *  so derived state (the picker list, the From: header, …)
   *  recomputes against the new server-side truth without a
   *  full page reload. */
  async function reloadSettingsSnapshot() {
    try {
      const fresh = await invoke<AppSettings>('get_app_settings')
      appSettings = fresh
      onappprefschanged?.({ ...fresh })
    } catch (e) {
      console.warn('get_app_settings refresh failed', e)
    }
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

  /** Persist the emoji avatar (#115).  `null` clears it back to
   *  the initials fallback in the IconRail. */
  async function onEmojiChange(account: Account, next: string | null) {
    account.emoji = next
    try {
      await invoke('update_account', { account: { ...account, emoji: next } })
    } catch (e) {
      console.warn('failed to save account emoji', e)
    }
  }
  /** Which account currently has its emoji picker popover open. */
  let emojiPickerForAccount = $state<string | null>(null)
  /** Which account currently has its folder-icon-rule draft
   *  picker open (separate from the avatar picker — both can
   *  coexist on the same account row). */
  let iconDraftPickerFor = $state<string | null>(null)
  $effect(() => {
    if (!iconDraftPickerFor) return
    const onDoc = () => (iconDraftPickerFor = null)
    const handle = setTimeout(() => document.addEventListener('mousedown', onDoc), 0)
    return () => {
      clearTimeout(handle)
      document.removeEventListener('mousedown', onDoc)
    }
  })
  $effect(() => {
    if (!emojiPickerForAccount) return
    const onDoc = () => (emojiPickerForAccount = null)
    const handle = setTimeout(() => document.addEventListener('mousedown', onDoc), 0)
    return () => {
      clearTimeout(handle)
      document.removeEventListener('mousedown', onDoc)
    }
  })

  /** Persist the sender (person) name (#115). */
  async function onPersonNameChange(account: Account, raw: string) {
    const next = raw.trim() || null
    account.person_name = next
    try {
      await invoke('update_account', {
        account: { ...account, person_name: next },
      })
    } catch (e) {
      console.warn('failed to save sender name', e)
    }
  }

  /** Swap places with the next/previous account in the
   *  display-order ranking.  Re-numbers the affected pair so
   *  ties don't accumulate over time, and updates the local
   *  state immediately so the UI reorders without waiting on
   *  the round-trip. */
  async function moveAccount(account: Account, delta: -1 | 1) {
    const sorted = [...accounts].sort((a, b) => {
      const ao = a.sort_order ?? 0
      const bo = b.sort_order ?? 0
      if (ao !== bo) return ao - bo
      return a.id.localeCompare(b.id)
    })
    const idx = sorted.findIndex((a) => a.id === account.id)
    const target = idx + delta
    if (target < 0 || target >= sorted.length) return
    const other = sorted[target]
    // Re-rank from 0 so ties accumulated from older saves get
    // cleaned up.  Cheap — typical user has 1-3 accounts.
    sorted[idx] = other
    sorted[target] = account
    for (let i = 0; i < sorted.length; i++) {
      const a = sorted[i]
      if ((a.sort_order ?? 0) !== i) {
        a.sort_order = i
        try {
          await invoke('update_account', { account: { ...a, sort_order: i } })
        } catch (e) {
          console.warn('failed to save account sort order', e)
        }
      }
    }
    accounts = [...sorted]
  }

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

  <!-- Content — split into a category nav (left) + active panel
       (right) per #131.  The nav is sticky to the top of the
       scroll container so it stays visible as the right pane
       scrolls inside its own column. -->
  <div class="flex-1 overflow-hidden flex">
    <!-- Category nav.  Single source-of-truth list, keeps order
         identical between mobile-collapse + desktop layouts when
         that comes. -->
    <nav class="w-48 shrink-0 border-r border-surface-200 dark:border-surface-700 bg-surface-100/60 dark:bg-surface-800/40 overflow-y-auto p-3">
      <ul class="space-y-1">
        {#each CATEGORIES as cat (cat.id)}
          {@const active = activeCategory === cat.id}
          <li>
            <button
              type="button"
              class="w-full text-left px-3 py-2 rounded-md text-sm flex items-center gap-2 transition-colors {active
                ? 'bg-primary-500/15 text-primary-600 dark:text-primary-300 font-medium'
                : 'hover:bg-surface-200 dark:hover:bg-surface-700 text-surface-700 dark:text-surface-200'}"
              onclick={() => (activeCategory = cat.id)}
              aria-current={active ? 'page' : undefined}
            >
              <span aria-hidden="true">{cat.icon}</span>
              <span>{cat.label}</span>
            </button>
          </li>
        {/each}
      </ul>
    </nav>

    <!-- Active category's panel — scrolls independently of the
         nav so long sections (the account list) don't push the
         nav off-screen. -->
    <div class="flex-1 overflow-y-auto p-6 max-w-3xl w-full">
    {#if activeCategory === 'general'}
    <!-- App Preferences (Issue #16) — always visible, independent
         of per-account loading state so the user can tweak tray /
         sync / notification behaviour even before adding an account. -->
    <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg mb-6">
      <div class="flex items-center justify-between mb-3">
        <h2 class="text-base font-semibold">General</h2>
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

      <div class="space-y-3 text-sm">
        <div class="flex items-start gap-3">
          <Toggle
            bind:checked={appSettings.minimize_to_tray}
            label="Minimize to tray when closing the window"
            onchange={() => scheduleSave()}
          />
          <span>Minimize to tray when closing the window</span>
        </div>

        <div class="flex items-start gap-3">
          <Toggle
            bind:checked={appSettings.start_minimized}
            label="Start minimized to tray"
            onchange={() => scheduleSave()}
          />
          <span>Start minimized to tray</span>
        </div>

        <div class="flex items-start gap-3">
          <Toggle
            checked={appSettings.autostart_enabled}
            label="Launch Nimbus when I sign in"
            onchange={(v) => void onAutostartToggle(v)}
          />
          <span>
            Launch Nimbus when I sign in
            <span class="block text-xs text-surface-500">
              Adds an entry to your OS's autostart list. Combine with "Start minimized to tray" for a quiet boot.
            </span>
          </span>
        </div>

        <div class="flex items-start gap-3">
          <Toggle
            bind:checked={appSettings.auto_advance_after_remove}
            label="After delete / archive, open the next message automatically"
            onchange={() => scheduleSave()}
          />
          <span>After delete / archive, open the next message automatically</span>
        </div>
      </div>
    </div>
    {/if}

    {#if activeCategory === 'mail'}
    <!-- Mail-specific preferences: sync cadence + new-mail
         toast.  Account list is rendered by the gated section
         further down. -->
    <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg mb-6">
      <div class="flex items-center justify-between mb-3">
        <h2 class="text-base font-semibold">Mail preferences</h2>
        <div class="flex items-center gap-2">
          {#if prefsSaveStatus === 'saving'}
            <span class="text-xs text-surface-400">Saving…</span>
          {:else if prefsSaveStatus === 'saved'}
            <span class="text-xs text-success-500">Saved</span>
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
      <div class="space-y-3 text-sm">
        <div class="flex items-start gap-3">
          <Toggle
            bind:checked={appSettings.background_sync_enabled}
            label="Run background mail sync"
            onchange={() => scheduleSave()}
          />
          <span>Run background mail sync</span>
        </div>

        <label class="flex items-center gap-2 pl-12">
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

        <div class="flex items-start gap-3">
          <Toggle
            bind:checked={appSettings.notifications_enabled}
            label="Show desktop notifications for new mail"
            onchange={() => scheduleSave()}
          />
          <span>Show desktop notifications for new mail</span>
        </div>
      </div>
    </div>
    {/if}

    {#if activeCategory === 'calendar'}
    <!-- Calendar preferences: default calendar + Talk-room
         reminder toggle.  Both belong here because they're
         CalDAV / event-level concerns, not mail-app behaviour. -->
    <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg mb-6">
      <div class="flex items-center justify-between mb-3">
        <h2 class="text-base font-semibold">Calendar preferences</h2>
        {#if prefsSaveStatus === 'saving'}
          <span class="text-xs text-surface-400">Saving…</span>
        {:else if prefsSaveStatus === 'saved'}
          <span class="text-xs text-success-500">Saved</span>
        {/if}
      </div>
      <div class="space-y-3 text-sm">
        <div class="flex items-start gap-3">
          <Toggle
            bind:checked={appSettings.talk_reminder_enabled}
            label="Notify me before meetings with a Talk room"
            onchange={() => scheduleSave()}
          />
          <span>
            Notify me before meetings with a Talk room
            <span class="block text-xs text-surface-500">
              Lead time follows the event's own reminder.
            </span>
          </span>
        </label>

        <!-- Default calendar.  Used by the EventEditor as the
             pre-selected calendar in create-mode, and by the
             RSVP card as the default destination for accepted
             invites.  Hidden when no Nextcloud calendars are
             cached yet. -->
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
    {/if}

    {#if activeCategory === 'design'}
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
          <div class="flex items-center justify-between mb-2">
            <p class="font-medium">Theme</p>
            <button
              type="button"
              class="btn btn-sm preset-outlined-primary-500 text-xs"
              disabled={importingTheme}
              onclick={() => void importCustomTheme()}
              title="Pick a Skeleton-shape CSS file from disk. The Skeleton theme generator (skeleton.dev) exports compatible files; community themes also work."
            >{importingTheme ? 'Importing…' : '+ Import theme…'}</button>
          </div>
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
            {#each pickerThemes as theme (theme.id)}
              {@const active = appSettings.theme_name === theme.id}
              <div class="relative">
                <button
                  type="button"
                  class="w-full text-left p-3 rounded-md border transition-colors {active
                    ? 'border-primary-500 bg-primary-500/10'
                    : 'border-surface-300 dark:border-surface-700 hover:bg-surface-200 dark:hover:bg-surface-700'}"
                  onclick={() => onThemeChange(theme.id, appSettings.theme_mode)}
                >
                  <div class="font-medium flex items-center gap-2">
                    <span>{theme.label}</span>
                    {#if theme.custom}
                      <span class="text-[10px] uppercase tracking-wider font-semibold px-1 py-px rounded bg-primary-500/20 text-primary-600 dark:text-primary-300">
                        custom
                      </span>
                    {/if}
                  </div>
                  <div class="text-xs text-surface-500 mt-0.5">{theme.description}</div>
                </button>
                {#if theme.custom}
                  <button
                    type="button"
                    class="absolute top-1 right-1 w-6 h-6 rounded text-xs text-surface-500 hover:bg-error-500/20 hover:text-error-500"
                    title="Remove custom theme"
                    aria-label={`Remove ${theme.label}`}
                    onclick={(e) => {
                      e.stopPropagation()
                      void removeCustomTheme(theme.id)
                    }}
                  >×</button>
                {/if}
              </div>
            {/each}
          </div>
          <p class="text-xs text-surface-400 mt-2">
            Imported themes aren't validated — a poorly-tuned palette can hurt readability.
            Skeleton's theme generator at <span class="font-mono">skeleton.dev</span> exports compatible files.
          </p>
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
    {/if}

    {#if activeCategory === 'mail'}
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
      <!-- Account list — sorted by `sort_order` (#115) so the
           order in this panel matches the IconRail. -->
      {@const sortedRows = [...accounts].sort((a, b) => {
        const ao = a.sort_order ?? 0
        const bo = b.sort_order ?? 0
        if (ao !== bo) return ao - bo
        return a.id.localeCompare(b.id)
      })}
      <div class="space-y-4">
        {#each sortedRows as account, accountIdx (account.id)}
          <div class="card p-4 bg-surface-100 dark:bg-surface-800 rounded-lg">
            <div class="flex items-start justify-between">
              <div class="flex items-start gap-3">
                <!-- Reorder handle: ▲ / ▼ swap places with the
                     neighbouring row.  The sort_order field is
                     persisted via update_account so the IconRail
                     picks up the new order on its next render. -->
                <div class="flex flex-col gap-1 mt-1">
                  <button
                    type="button"
                    class="w-5 h-5 flex items-center justify-center rounded text-surface-500 hover:bg-surface-200 dark:hover:bg-surface-700 disabled:opacity-30"
                    disabled={accountIdx === 0}
                    title="Move up"
                    aria-label="Move account up"
                    onclick={() => void moveAccount(account, -1)}
                  >▲</button>
                  <button
                    type="button"
                    class="w-5 h-5 flex items-center justify-center rounded text-surface-500 hover:bg-surface-200 dark:hover:bg-surface-700 disabled:opacity-30"
                    disabled={accountIdx === sortedRows.length - 1}
                    title="Move down"
                    aria-label="Move account down"
                    onclick={() => void moveAccount(account, 1)}
                  >▼</button>
                </div>
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

            <!-- Identity fields: emoji avatar + person name (#115). -->
            <div class="mt-4 pt-4 border-t border-surface-200 dark:border-surface-700 grid grid-cols-[auto_1fr] gap-3 items-center">
              <label class="text-sm font-medium" for="emoji-{account.id}">Avatar emoji</label>
              <div class="relative">
                <button
                  id="emoji-{account.id}"
                  type="button"
                  class="input text-lg text-center w-16 px-2 py-1 rounded-md hover:bg-surface-200 dark:hover:bg-surface-700"
                  aria-label="Account emoji avatar"
                  onclick={(e) => {
                    e.stopPropagation()
                    emojiPickerForAccount = emojiPickerForAccount === account.id ? null : account.id
                  }}
                >{account.emoji || '📨'}</button>
                {#if emojiPickerForAccount === account.id}
                  <div
                    class="absolute left-0 top-full mt-1 z-50"
                    role="menu"
                    tabindex="-1"
                    onclick={(e) => e.stopPropagation()}
                    onmousedown={(e) => e.stopPropagation()}
                    onkeydown={(e) => { if (e.key === 'Escape') emojiPickerForAccount = null }}
                  >
                    <EmojiPicker
                      value={account.emoji ?? null}
                      onpick={(emoji) => {
                        emojiPickerForAccount = null
                        void onEmojiChange(account, emoji)
                      }}
                    />
                  </div>
                {/if}
              </div>
              <label class="text-sm font-medium" for="person-{account.id}">Sender name</label>
              <input
                id="person-{account.id}"
                type="text"
                placeholder={account.display_name}
                value={account.person_name ?? ''}
                onchange={(e) => void onPersonNameChange(account, (e.currentTarget as HTMLInputElement).value)}
                class="input flex-1 text-sm px-3 py-1 rounded-md"
                aria-label="Sender display name"
              />
            </div>
            <p class="text-xs text-surface-400 mt-1 ml-1">
              The sender name appears as <code>"Name" &lt;email&gt;</code> on outgoing mail. Defaults to the account name when empty.
            </p>

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
                  <div class="relative">
                    <button
                      type="button"
                      class="input text-lg text-center w-14 px-2 py-1 rounded-md hover:bg-surface-200 dark:hover:bg-surface-700"
                      aria-label="Icon"
                      onclick={(e) => {
                        e.stopPropagation()
                        iconDraftPickerFor = iconDraftPickerFor === account.id ? null : account.id
                      }}
                    >{iconDrafts[account.id].icon || '🏦'}</button>
                    {#if iconDraftPickerFor === account.id}
                      <div
                        class="absolute left-0 top-full mt-1 z-50"
                        role="menu"
                        tabindex="-1"
                        onclick={(e) => e.stopPropagation()}
                        onmousedown={(e) => e.stopPropagation()}
                        onkeydown={(e) => { if (e.key === 'Escape') iconDraftPickerFor = null }}
                      >
                        <EmojiPicker
                          value={iconDrafts[account.id].icon || null}
                          allowClear={false}
                          onpick={(emoji) => {
                            iconDrafts[account.id].icon = emoji ?? ''
                            iconDraftPickerFor = null
                          }}
                        />
                      </div>
                    {/if}
                  </div>
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
    {/if}

    {#if activeCategory === 'nextcloud'}
    <NextcloudSettings />
    {/if}

    {#if activeCategory === 'security'}
    <SecuritySettings />
    {/if}
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
