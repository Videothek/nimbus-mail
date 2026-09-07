<script lang="ts">
  /**
   * FormsView — sidebar-routed full-pane Nextcloud Forms manager (#572).
   *
   * Same shell as SharesView (header + scrollable list) so the rail-
   * routed integration views read as one app.  Lists every form the
   * user owns on the selected Nextcloud account and lets them:
   *
   *   - create a new form (title here, questions in the Nextcloud
   *     editor which opens in an in-app window)
   *   - copy the public link (minting a link share if the form has
   *     none yet)
   *   - share the link in a new mail
   *   - open the form editor (click the title → in-app window;
   *     the open-link button → system browser)
   *   - delete the form
   *
   * # Why we don't cache forms
   *
   * The list is small and lives on the server; a form edited in the
   * web UI would leave a cache stale.  Same rationale as SharesView:
   * fetch on demand + a slow timer, no SQLite layer.
   *
   * # Why question editing stays in Nextcloud
   *
   * The Forms web editor already handles question types, ordering,
   * validation, and results.  Re-implementing that inside a mail
   * client would be a large surface for little gain, so Unkai creates
   * the shell and hands the editor to the user in a popout window
   * (the same `openExternalPopout` the share-document viewer uses).
   */

  import * as api from './api'
  import type { NextcloudFormRow } from './api/types'
  import type { ComposeInitial } from './Compose.svelte'
  import { isNextcloudSource } from './ncSources'
  import { openExternalPopout } from './standalonePopoutWindow'
  import { onDestroy, onMount } from 'svelte'
  import { formatError } from './errors'
  import Badge from './Badge.svelte'
  import Icon from './Icon.svelte'
  import SearchInput from './SearchInput.svelte'
  import { m } from '../paraglide/messages'

  interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
  }

  interface Props {
    /** Open Compose with a form-invite card in the body. */
    oncompose: (initial: ComposeInitial) => void
  }
  const { oncompose }: Props = $props()

  let accounts = $state<NextcloudAccount[]>([])
  let accountId = $state('')
  let forms = $state<NextcloudFormRow[]>([])
  let loading = $state(false)
  let error = $state('')
  let searchQuery = $state('')
  const filteredForms = $derived.by(() => {
    const q = searchQuery.trim().toLowerCase()
    if (!q) return forms
    return forms.filter(
      (f) =>
        f.title.toLowerCase().includes(q) ||
        f.description.toLowerCase().includes(q),
    )
  })

  const REFRESH_INTERVAL_MS = 60_000
  let pollTimer: number | null = null

  onMount(async () => {
    await loadAccounts()
  })

  onDestroy(() => {
    if (pollTimer !== null) window.clearInterval(pollTimer)
  })

  async function loadAccounts() {
    try {
      // Forms is a Nextcloud app — skip generic-DAV / local sources (#413).
      const list = (
        await api.nextcloud.getNextcloudAccounts()
      ).filter(isNextcloudSource)
      accounts = list
      if (list.length === 1 && !accountId) {
        accountId = list[0].id
        await refresh()
        startPolling()
      }
    } catch (e) {
      error = formatError(e) || m.forms_view_load_accounts_error()
    }
  }

  async function selectAccount(id: string) {
    accountId = id
    forms = []
    await refresh()
    startPolling()
  }

  function startPolling() {
    if (pollTimer !== null) window.clearInterval(pollTimer)
    pollTimer = window.setInterval(() => {
      void refresh({ silent: true })
    }, REFRESH_INTERVAL_MS)
  }

  async function refresh(opts: { silent?: boolean } = {}) {
    if (!accountId) return
    if (!opts.silent) {
      loading = true
      error = ''
    }
    try {
      const list = await api.nextcloud.listNextcloudForms({ ncId: accountId })
      // Most recently edited first — the form the user is working
      // on right now is the one they want on top.
      list.sort((a, b) => b.last_updated - a.last_updated)
      forms = list
    } catch (e) {
      if (!opts.silent) error = formatError(e) || m.forms_view_load_error()
    } finally {
      if (!opts.silent) loading = false
    }
  }

  function formatRelative(unix: number): string {
    if (!unix) return ''
    const now = Date.now() / 1000
    const delta = now - unix
    if (delta < 60) return m.forms_view_row_just_now()
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`
    if (delta < 7 * 86400) return `${Math.floor(delta / 86400)}d ago`
    return new Date(unix * 1000).toLocaleDateString()
  }

  function displayTitle(f: NextcloudFormRow): string {
    return f.title.trim() || m.forms_view_untitled()
  }

  /** Forms' `FORM_STATE_*`: 0 active, 1 closed, 2 archived. */
  function stateBadge(f: NextcloudFormRow): { label: string; tone: 'warning' | 'neutral' } | null {
    if (f.state === 1) return { label: m.forms_view_state_closed(), tone: 'warning' }
    if (f.state === 2) return { label: m.forms_view_state_archived(), tone: 'neutral' }
    return null
  }

  // ── Public link: resolve (mint on demand) ───────────────────
  // A form created in the web UI may have no link share yet.  The
  // backend's `ensure` call returns the existing link or mints one;
  // we patch the row so the chip flips without a full refresh.
  async function resolveLink(row: NextcloudFormRow): Promise<string> {
    if (row.public_url) return row.public_url
    const url = await api.nextcloud.ensureNextcloudFormLink({
      ncId: row.nc_id,
      formId: row.id,
    })
    forms = forms.map((f) => (f.id === row.id ? { ...f, public_url: url } : f))
    return url
  }

  let copiedId = $state<number | null>(null)
  let copyingId = $state<number | null>(null)
  let copiedTimer: number | null = null
  async function copyLink(row: NextcloudFormRow) {
    if (copyingId) return
    copyingId = row.id
    try {
      const url = await resolveLink(row)
      await navigator.clipboard.writeText(url)
      copiedId = row.id
      if (copiedTimer) window.clearTimeout(copiedTimer)
      copiedTimer = window.setTimeout(() => {
        copiedId = null
      }, 1600)
    } catch (e) {
      error = formatError(e) || m.forms_view_copy_error()
    } finally {
      copyingId = null
    }
  }

  let sharingId = $state<number | null>(null)
  async function shareInMail(row: NextcloudFormRow) {
    if (sharingId) return
    sharingId = row.id
    try {
      const url = await resolveLink(row)
      oncompose({
        subject: displayTitle(row),
        formLink: { title: displayTitle(row), url },
      })
    } catch (e) {
      error = formatError(e) || m.forms_view_copy_error()
    } finally {
      sharingId = null
    }
  }

  // ── Open ────────────────────────────────────────────────────
  // Title click opens the owner-side editor in an in-app window
  // (same surface the share-document viewer uses, so the user's NC
  // session carries over).  The open-link button hands the same URL
  // to the system browser for people who prefer it.
  function openEditor(row: NextcloudFormRow) {
    openExternalPopout('form', row.edit_url, {
      title: displayTitle(row),
      width: 1200,
      height: 800,
    })
  }

  function openInBrowser(row: NextcloudFormRow) {
    void api.system.openUrl({ url: row.edit_url })
  }

  // ── Delete ──────────────────────────────────────────────────
  let deletingId = $state<number | null>(null)
  async function deleteForm(row: NextcloudFormRow) {
    const ok = window.confirm(m.forms_view_delete_confirm({ name: displayTitle(row) }))
    if (!ok) return
    deletingId = row.id
    try {
      await api.nextcloud.deleteNextcloudForm({ ncId: row.nc_id, formId: row.id })
      forms = forms.filter((f) => f.id !== row.id)
    } catch (e) {
      error = formatError(e) || m.forms_view_delete_error()
    } finally {
      deletingId = null
    }
  }

  // ── Create modal ────────────────────────────────────────────
  // Title only: the questions belong to the Nextcloud editor, which
  // opens right after the shell is created.
  let showCreate = $state(false)
  let createTitle = $state('')
  let creating = $state(false)
  let createError = $state('')

  function openCreate() {
    createTitle = ''
    createError = ''
    creating = false
    showCreate = true
  }

  function cancelCreate() {
    if (creating) return
    showCreate = false
  }

  async function commitCreate() {
    if (!accountId || creating) return
    creating = true
    createError = ''
    try {
      const row = await api.nextcloud.createNextcloudForm({
        ncId: accountId,
        title: createTitle.trim(),
      })
      forms = [row, ...forms]
      showCreate = false
      openEditor(row)
    } catch (e) {
      createError = formatError(e) || m.forms_view_create_error()
    } finally {
      creating = false
    }
  }

  function onCreateKeydown(e: KeyboardEvent) {
    if (!showCreate) return
    if (e.key === 'Escape' && !creating) {
      e.preventDefault()
      cancelCreate()
    } else if (e.key === 'Enter' && !creating) {
      e.preventDefault()
      void commitCreate()
    }
  }
</script>

<svelte:window onkeydown={onCreateKeydown} />

<div class="h-full flex flex-col bg-surface-50 dark:bg-surface-900">
  <!-- Stacked header (#522): title above its icon-only actions,
       docked left; search centered with a mirrored right spacer. -->
  <div class="flex items-center gap-3 px-6 py-3 border-b glass-panel">
    <div class="flex-1 min-w-0 flex flex-col items-start gap-2">
      <h2 class="text-xl font-semibold truncate">{m.forms_view_title()}</h2>
      <div class="flex items-center gap-2 shrink-0">
        <button
          class="btn btn-sm preset-filled-primary-500 inline-flex items-center justify-center"
          disabled={!accountId}
          onclick={openCreate}
          title={m.forms_view_new_form_title()}
          aria-label={m.forms_view_new_form()}
        ><Icon name="plus" size={14} /></button>
        <button
          class="btn btn-sm preset-tonal-surface inline-flex items-center justify-center"
          disabled={!accountId || loading}
          onclick={() => refresh()}
          title={loading ? m.forms_view_refreshing() : m.forms_view_refresh_title()}
          aria-label={loading ? m.forms_view_refreshing() : m.forms_view_refresh()}
        ><Icon name={loading ? 'loading' : 'refresh'} size={14} /></button>
      </div>
    </div>
    <div class="flex-1 flex justify-center min-w-0">
      <SearchInput
        bind:value={searchQuery}
        placeholder={m.forms_view_search_placeholder()}
        class="w-full max-w-md"
      />
    </div>
    <div class="flex-1"></div>
  </div>

  {#if accounts.length === 0}
    <div class="p-6 text-sm text-surface-500">
      {@html m.forms_view_no_account_html()}
    </div>
  {:else}
    {#if accounts.length > 1}
      <div class="px-5 py-2 border-b border-surface-200 dark:border-surface-700 flex items-center gap-2">
        <label for="forms-account" class="text-xs text-surface-500">{m.forms_view_account_label()}</label>
        <select
          id="forms-account"
          class="select text-sm"
          value={accountId}
          onchange={(e) => selectAccount((e.target as HTMLSelectElement).value)}
        >
          <option value="" disabled>{m.forms_view_account_placeholder()}</option>
          {#each accounts as acc (acc.id)}
            <option value={acc.id}>{acc.display_name ?? acc.username} ({acc.server_url})</option>
          {/each}
        </select>
      </div>
    {/if}

    {#if error}
      <p class="px-5 py-2 text-sm text-error-500">{error}</p>
    {/if}

    {#if !accountId}
      <p class="p-6 text-sm text-surface-500">{m.forms_view_pick_account_hint()}</p>
    {:else if loading && forms.length === 0}
      <p class="p-6 text-sm text-surface-500">{m.forms_view_loading()}</p>
    {:else if forms.length === 0}
      <p class="p-6 text-sm text-surface-500">{m.forms_view_empty()}</p>
    {:else if filteredForms.length === 0}
      <!-- Non-empty list narrowed to nothing — its own empty state
           so the user knows the search hit zero, not the account. -->
      <p class="p-6 text-sm text-surface-500">{m.forms_view_no_matches()}</p>
    {:else}
      <div class="flex-1 overflow-y-auto">
        <ul class="divide-y divide-surface-200 dark:divide-surface-800">
          {#each filteredForms as row (row.id)}
            {@const badge = stateBadge(row)}
            <li class="px-5 py-3 flex items-center gap-3 hover:bg-primary-500/10">
              <span class="flex-shrink-0 text-surface-600 dark:text-surface-300">
                <Icon name="forms" size={20} />
              </span>

              <div class="flex-1 min-w-0">
                <div class="flex items-center gap-2">
                  <!-- The title is the open affordance — a text-
                       shaped button, primary tint on hover. -->
                  <button
                    type="button"
                    class="font-medium truncate min-w-0 text-left hover:text-primary-500 transition-colors duration-150 ease-out"
                    onclick={() => openEditor(row)}
                    title={m.forms_view_open_editor_title()}
                  >{displayTitle(row)}</button>
                  {#if badge}
                    <Badge label={badge.label} tone={badge.tone} />
                  {/if}
                </div>
                {#if row.description}
                  <p class="text-xs text-surface-500 truncate">{row.description}</p>
                {/if}
                <div class="flex items-center gap-3 mt-1 text-[11px] text-surface-500">
                  <span class="inline-flex items-center gap-1" title={m.forms_view_submissions_title()}>
                    <Icon name="tasks" size={12} />
                    {m.forms_view_submissions({ n: String(row.submission_count ?? 0) })}
                  </span>
                  <span
                    class="inline-flex items-center gap-1"
                    title={row.public_url ? m.forms_view_link_set_title() : m.forms_view_link_none_title()}
                  >
                    <Icon name={row.public_url ? 'share-links' : 'unlocked'} size={12} />
                    {row.public_url ? m.forms_view_link_set() : m.forms_view_link_none()}
                  </span>
                  {#if row.last_updated}
                    <span class="inline-flex items-center gap-1" title={m.forms_view_updated_title()}>
                      <Icon name="time" size={12} />
                      {formatRelative(row.last_updated)}
                    </span>
                  {/if}
                </div>
              </div>

              <!-- Per-row icon-only actions (CLAUDE.md shape); only
                   Delete gets the destructive hover overlay. -->
              <button
                class="btn btn-sm preset-outlined-surface-500 inline-flex items-center justify-center"
                disabled={copyingId === row.id}
                onclick={() => void copyLink(row)}
                title={copiedId === row.id ? m.forms_view_copied() : m.forms_view_copy_title()}
                aria-label={copiedId === row.id ? m.forms_view_copied() : m.forms_view_copy_link()}
              ><Icon name={copyingId === row.id ? 'loading' : copiedId === row.id ? 'success' : 'copy'} size={14} /></button>
              <button
                class="btn btn-sm preset-outlined-surface-500 inline-flex items-center justify-center"
                disabled={sharingId === row.id}
                onclick={() => void shareInMail(row)}
                title={m.forms_view_share_mail_title()}
                aria-label={m.forms_view_share_mail()}
              ><Icon name={sharingId === row.id ? 'loading' : 'email-envelope'} size={14} /></button>
              <button
                class="btn btn-sm preset-outlined-surface-500 inline-flex items-center justify-center"
                onclick={() => openInBrowser(row)}
                title={m.forms_view_open_browser_title()}
                aria-label={m.forms_view_open_browser()}
              ><Icon name="open-link" size={14} /></button>
              <button
                class="btn btn-sm preset-outlined-surface-500 inline-flex items-center justify-center hover:bg-error-500/15 hover:text-error-500 hover:border-error-500/40"
                disabled={deletingId === row.id}
                onclick={() => void deleteForm(row)}
                title={deletingId === row.id ? m.forms_view_deleting() : m.forms_view_delete_title()}
                aria-label={deletingId === row.id ? m.forms_view_deleting() : m.forms_view_delete()}
              ><Icon name={deletingId === row.id ? 'loading' : 'trash'} size={14} /></button>
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  {/if}
</div>

{#if showCreate}
  <div
    class="fixed inset-0 flex items-center justify-center bg-black/50"
    style="z-index: 50"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onmousedown={(e) => {
      if (e.target === e.currentTarget && !creating) cancelCreate()
    }}
  >
    <div class="glass-float rounded-2xl w-[28rem] max-w-full p-5">
      <h3 class="text-base font-semibold mb-1">{m.forms_create_title()}</h3>
      <p class="text-xs text-on-glass-muted mb-3">{m.forms_create_hint()}</p>

      <label class="block text-xs text-on-glass-muted mb-1" for="forms-create-title">
        {m.forms_create_name_label()}
      </label>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        id="forms-create-title"
        class="input w-full text-sm px-2 py-1.5 rounded-lg mb-3"
        placeholder={m.forms_create_name_placeholder()}
        bind:value={createTitle}
        disabled={creating}
        autofocus
      />

      {#if createError}
        <p class="text-xs text-error-500 mb-3 wrap-break-word">{createError}</p>
      {/if}

      <!-- Icon-only footer, same vocabulary as the per-row buttons:
           `close` cancels, `plus` (filled primary) creates; the
           icon swaps to `loading` mid-action so the width holds. -->
      <div class="flex justify-end gap-2">
        <button
          class="btn btn-sm preset-outlined-surface-500 inline-flex items-center justify-center"
          disabled={creating}
          onclick={cancelCreate}
          title={m.forms_create_cancel()}
          aria-label={m.forms_create_cancel()}
        ><Icon name="close" size={14} /></button>
        <button
          class="btn btn-sm preset-filled-primary-500 inline-flex items-center justify-center"
          disabled={creating}
          onclick={() => void commitCreate()}
          title={m.forms_create_submit()}
          aria-label={m.forms_create_submit()}
        ><Icon name={creating ? 'loading' : 'plus'} size={14} /></button>
      </div>
    </div>
  </div>
{/if}
