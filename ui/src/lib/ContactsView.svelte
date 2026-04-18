<script lang="ts">
  /**
   * ContactsView — list / edit / add / delete Nextcloud contacts.
   *
   * Reads from the local cache (`get_contacts`) so the list paints
   * instantly, then kicks off a fresh `sync_nextcloud_contacts` in the
   * background. Any edit the user makes writes through to both the
   * server (PUT / DELETE) and the cache, so we don't have to wait for
   * the next sync to see our own changes.
   */

  import { convertFileSrc, invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  interface Props {
    onclose: () => void
  }
  const { onclose }: Props = $props()

  // ── Types (mirror the Rust models) ──────────────────────────
  interface NextcloudAccount {
    id: string
    server_url: string
    username: string
    display_name?: string | null
  }
  interface Contact {
    id: string
    nextcloud_account_id: string
    display_name: string
    email: string[]
    phone: string[]
    organization: string | null
    photo_mime: string | null
    photo_data: number[] | null
  }
  interface ContactInput {
    display_name: string
    emails: string[]
    phones: string[]
    organization: string | null
    photo_mime: string | null
    photo_data: number[] | null
  }
  interface AddressbookSummary {
    path: string
    name: string
    display_name: string | null
  }

  // ── State ───────────────────────────────────────────────────
  let accounts = $state<NextcloudAccount[]>([])
  let contacts = $state<Contact[]>([])
  let loading = $state(true)
  let syncing = $state(false)
  let error = $state('')

  // Currently selected contact in the list. `null` = nothing selected.
  // `'new'` is a sentinel meaning "showing the create form" — easier
  // than a separate boolean since the editor pane is one component.
  let selectedId = $state<string | 'new' | null>(null)

  // Form state — bound to inputs. When `selectedId` changes we
  // copy the matching contact's fields into these so edits don't
  // mutate the cached row until the user saves.
  let formName = $state('')
  let formEmails = $state('') // newline-separated
  let formPhones = $state('') // newline-separated
  let formOrg = $state('')
  let formAccountId = $state('')       // only used for create
  let formAddressbookUrl = $state('')  // only used for create
  let formAddressbookName = $state('') // only used for create
  // Photo bytes for the selected contact, lazy-loaded via
  // `get_contact_photo`. Only fetched so we can round-trip them
  // through `update_contact` — display uses the `contact-photo://`
  // URI scheme, which streams bytes straight to `<img>` without
  // touching JSON IPC. Without this round-trip the vCard rebuild
  // on save would drop the avatar.
  let selectedPhotoBytes = $state<number[] | null>(null)
  let saving = $state(false)
  let formError = $state('')
  let deleteConfirm = $state(false)

  // Cache per-account addressbooks so switching the "save to" account
  // in the new-contact form doesn't re-hit the server.
  let addressbooksByAccount = $state<Record<string, AddressbookSummary[]>>({})

  // Naive free-text filter over the loaded list. Server-side search
  // isn't needed at this scale (addressbooks are usually hundreds,
  // not thousands, of contacts).
  let query = $state('')
  const filteredContacts = $derived.by(() => {
    const q = query.trim().toLowerCase()
    if (!q) return contacts
    return contacts.filter(
      (c) =>
        c.display_name.toLowerCase().includes(q) ||
        c.email.some((e) => e.toLowerCase().includes(q)) ||
        (c.organization ?? '').toLowerCase().includes(q),
    )
  })

  $effect(() => {
    void init()
  })

  async function init() {
    loading = true
    error = ''
    try {
      accounts = await invoke<NextcloudAccount[]>('get_nextcloud_accounts')
      if (accounts.length === 0) {
        error = 'Connect a Nextcloud account first to sync contacts.'
        loading = false
        return
      }
      await reloadContacts()
      // Default the create-form account selector to the first NC so
      // the user doesn't have to pick when they only have one.
      formAccountId = accounts[0].id
      void loadAddressbooksFor(formAccountId)
    } catch (e) {
      error = formatError(e) || 'Failed to load contacts'
    } finally {
      loading = false
    }
    // Kick off a refresh in the background so new/changed contacts
    // from other devices land without the user having to visit the
    // Nextcloud settings page.
    void syncInBackground()
  }

  async function reloadContacts() {
    contacts = await invoke<Contact[]>('get_contacts', { ncId: null })
    // Keep the list sorted by name so edits don't reshuffle it.
    contacts.sort((a, b) =>
      a.display_name.localeCompare(b.display_name, undefined, { sensitivity: 'base' }),
    )
  }

  async function syncInBackground() {
    if (syncing) return
    syncing = true
    try {
      for (const a of accounts) {
        try {
          await invoke('sync_nextcloud_contacts', { ncId: a.id })
        } catch (e) {
          console.warn('sync_nextcloud_contacts failed for', a.id, e)
        }
      }
      await reloadContacts()
    } finally {
      syncing = false
    }
  }

  async function loadAddressbooksFor(ncId: string) {
    if (addressbooksByAccount[ncId]) {
      applyAddressbookDefault(ncId)
      return
    }
    try {
      const books = await invoke<AddressbookSummary[]>(
        'list_nextcloud_addressbooks',
        { ncId },
      )
      addressbooksByAccount[ncId] = books
      applyAddressbookDefault(ncId)
    } catch (e) {
      console.warn('list_nextcloud_addressbooks failed', e)
    }
  }

  function applyAddressbookDefault(ncId: string) {
    const books = addressbooksByAccount[ncId] ?? []
    if (books.length > 0) {
      formAddressbookUrl = books[0].path
      formAddressbookName = books[0].name
    } else {
      formAddressbookUrl = ''
      formAddressbookName = ''
    }
  }

  function selectContact(id: string) {
    selectedId = id
    deleteConfirm = false
    formError = ''
    const c = contacts.find((x) => x.id === id)
    if (!c) return
    formName = c.display_name
    formEmails = c.email.join('\n')
    formPhones = c.phone.join('\n')
    formOrg = c.organization ?? ''
    selectedPhotoBytes = null
    // We still need the bytes (not just a URL) so save can re-emit
    // them in the vCard — without this, an edit drops the avatar.
    if (c.photo_mime) void loadSelectedPhotoBytes(id)
  }

  function startNew() {
    selectedId = 'new'
    deleteConfirm = false
    formError = ''
    formName = ''
    formEmails = ''
    formPhones = ''
    formOrg = ''
    selectedPhotoBytes = null
    if (!formAccountId && accounts.length > 0) {
      formAccountId = accounts[0].id
    }
    if (formAccountId) void loadAddressbooksFor(formAccountId)
  }

  function cancelEdit() {
    selectedId = null
    formError = ''
    deleteConfirm = false
    selectedPhotoBytes = null
  }

  // Pull just the bytes via IPC so we can round-trip them on save.
  // Display elsewhere uses `photoSrc()` against the URI scheme.
  async function loadSelectedPhotoBytes(id: string) {
    try {
      const photo = await invoke<{ mime: string; data: number[] } | null>(
        'get_contact_photo',
        { contactId: id },
      )
      if (selectedId !== id) return
      selectedPhotoBytes = photo?.data ?? null
    } catch (e) {
      console.warn('get_contact_photo failed', e)
    }
  }

  // URL for `<img src>` against the custom Tauri URI scheme. Bytes
  // are streamed straight from the cache to the webview — no JSON
  // bloat, browser handles caching, `loading="lazy"` defers off-
  // screen rows. Returns `null` when the contact has no photo so
  // callers can render the initial-letter placeholder instead.
  function photoSrc(c: Contact): string | null {
    if (!c.photo_mime) return null
    return convertFileSrc(c.id, 'contact-photo')
  }

  function onAccountChange() {
    void loadAddressbooksFor(formAccountId)
  }

  function onAddressbookChange(e: Event) {
    const sel = e.target as HTMLSelectElement
    const books = addressbooksByAccount[formAccountId] ?? []
    const picked = books.find((b) => b.path === sel.value)
    formAddressbookUrl = sel.value
    formAddressbookName = picked?.name ?? ''
  }

  // Split a textarea's contents into trimmed non-empty lines. vCard
  // emits one EMAIL / TEL per value, so the form's newline separation
  // maps 1:1 onto the backend shape.
  function splitLines(s: string): string[] {
    return s
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.length > 0)
  }

  function buildInput(): ContactInput {
    // Photo editing isn't in v1 — round-trip whatever the server
    // already has so saving the form doesn't wipe the avatar. The
    // bytes were lazy-loaded into `selectedPhotoBytes` when the
    // contact was opened (see `loadSelectedPhoto`).
    const existingMime =
      selectedId && selectedId !== 'new'
        ? (contacts.find((c) => c.id === selectedId)?.photo_mime ?? null)
        : null
    return {
      display_name: formName.trim(),
      emails: splitLines(formEmails),
      phones: splitLines(formPhones),
      organization: formOrg.trim() || null,
      photo_mime: existingMime,
      photo_data: existingMime ? selectedPhotoBytes : null,
    }
  }

  async function saveContact() {
    formError = ''
    const input = buildInput()
    if (!input.display_name) {
      formError = 'Please enter a name.'
      return
    }

    saving = true
    try {
      if (selectedId === 'new') {
        if (!formAccountId || !formAddressbookUrl || !formAddressbookName) {
          formError = 'Pick a Nextcloud account and addressbook first.'
          return
        }
        const created = await invoke<Contact>('create_contact', {
          ncId: formAccountId,
          addressbookUrl: formAddressbookUrl,
          addressbookName: formAddressbookName,
          input,
        })
        await reloadContacts()
        selectedId = created.id
      } else if (selectedId) {
        const updated = await invoke<Contact>('update_contact', {
          contactId: selectedId,
          input,
        })
        await reloadContacts()
        selectedId = updated.id
      }
    } catch (e) {
      formError = formatError(e) || 'Failed to save contact'
    } finally {
      saving = false
    }
  }

  async function deleteSelected() {
    if (!selectedId || selectedId === 'new') return
    saving = true
    formError = ''
    try {
      await invoke('delete_contact', { contactId: selectedId })
      await reloadContacts()
      selectedId = null
      deleteConfirm = false
    } catch (e) {
      formError = formatError(e) || 'Failed to delete contact'
    } finally {
      saving = false
    }
  }

  function accountLabel(id: string): string {
    const a = accounts.find((x) => x.id === id)
    if (!a) return id
    return a.display_name ?? a.username
  }

  const selectedContact = $derived(
    selectedId && selectedId !== 'new'
      ? (contacts.find((c) => c.id === selectedId) ?? null)
      : null,
  )
</script>

<div class="h-full flex bg-surface-50 dark:bg-surface-900">
  <!-- ── Left: contact list ──────────────────────────────── -->
  <aside class="w-80 shrink-0 border-r border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800 flex flex-col">
    <div class="p-3 border-b border-surface-200 dark:border-surface-700 flex items-center gap-2">
      <button
        class="btn-icon btn-icon-sm preset-tonal"
        aria-label="Back"
        onclick={onclose}
      >
        &larr;
      </button>
      <h2 class="text-base font-semibold flex-1">Contacts</h2>
      {#if syncing}
        <span class="text-[10px] text-surface-500">Syncing…</span>
      {/if}
    </div>
    <div class="p-3 flex flex-col gap-2">
      <input
        type="search"
        class="input"
        placeholder="Search"
        bind:value={query}
      />
      <button class="btn preset-filled-primary-500" onclick={startNew}>
        + New contact
      </button>
    </div>

    <div class="flex-1 overflow-y-auto px-2 pb-3">
      {#if loading}
        <p class="px-3 py-2 text-xs text-surface-500">Loading contacts…</p>
      {:else if error}
        <p class="px-3 py-2 text-xs text-red-500">{error}</p>
      {:else if contacts.length === 0}
        <p class="px-3 py-2 text-xs text-surface-500">
          No contacts yet. Click “New contact” to add one.
        </p>
      {:else if filteredContacts.length === 0}
        <p class="px-3 py-2 text-xs text-surface-500">No matches for “{query}”.</p>
      {:else}
        {#each filteredContacts as c (c.id)}
          <button
            class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors
              {selectedId === c.id
                ? 'bg-primary-500/10 text-primary-500 font-medium'
                : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
            onclick={() => selectContact(c.id)}
          >
            {#if photoSrc(c)}
              <img
                src={photoSrc(c)}
                alt=""
                loading="lazy"
                class="w-8 h-8 rounded-full object-cover shrink-0"
              />
            {:else}
              <span class="w-8 h-8 rounded-full bg-surface-300 dark:bg-surface-700 text-xs font-semibold flex items-center justify-center shrink-0">
                {c.display_name.slice(0, 1).toUpperCase()}
              </span>
            {/if}
            <span class="flex flex-col min-w-0 text-left">
              <span class="truncate">{c.display_name || '(no name)'}</span>
              {#if c.email.length > 0}
                <span class="text-xs text-surface-500 truncate normal-case">{c.email[0]}</span>
              {/if}
            </span>
          </button>
        {/each}
      {/if}
    </div>
  </aside>

  <!-- ── Right: detail / edit pane ──────────────────────────── -->
  <main class="flex-1 flex flex-col overflow-y-auto">
    {#if selectedId === null}
      <div class="flex-1 flex items-center justify-center text-surface-500 text-sm">
        Pick a contact on the left, or click “New contact”.
      </div>
    {:else}
      <div class="max-w-2xl w-full mx-auto p-6 flex flex-col gap-4">
        <div class="flex items-center gap-3">
          {#if selectedContact && photoSrc(selectedContact)}
            <img
              src={photoSrc(selectedContact)}
              alt=""
              class="w-16 h-16 rounded-full object-cover"
            />
          {:else}
            <div class="w-16 h-16 rounded-full bg-surface-300 dark:bg-surface-700 flex items-center justify-center text-xl font-semibold">
              {(formName || '?').slice(0, 1).toUpperCase()}
            </div>
          {/if}
          <div class="flex flex-col">
            <h3 class="text-lg font-semibold">
              {selectedId === 'new' ? 'New contact' : formName || '(no name)'}
            </h3>
            {#if selectedContact}
              <span class="text-xs text-surface-500">
                From {accountLabel(selectedContact.nextcloud_account_id)}
              </span>
            {/if}
          </div>
        </div>

        <label class="label">
          <span>Name</span>
          <input class="input" bind:value={formName} placeholder="Jane Doe" />
        </label>

        <label class="label">
          <span>Email addresses <span class="text-surface-500">(one per line)</span></span>
          <textarea
            class="textarea"
            rows="3"
            bind:value={formEmails}
            placeholder="jane@example.com"
          ></textarea>
        </label>

        <label class="label">
          <span>Phone numbers <span class="text-surface-500">(one per line)</span></span>
          <textarea
            class="textarea"
            rows="2"
            bind:value={formPhones}
            placeholder="+1 555 0100"
          ></textarea>
        </label>

        <label class="label">
          <span>Organization</span>
          <input class="input" bind:value={formOrg} placeholder="Example Corp" />
        </label>

        {#if selectedId === 'new'}
          <div class="grid grid-cols-2 gap-3">
            <label class="label">
              <span>Nextcloud account</span>
              <select class="select" bind:value={formAccountId} onchange={onAccountChange}>
                {#each accounts as a (a.id)}
                  <option value={a.id}>{a.display_name ?? a.username}</option>
                {/each}
              </select>
            </label>
            <label class="label">
              <span>Addressbook</span>
              <select
                class="select"
                value={formAddressbookUrl}
                onchange={onAddressbookChange}
              >
                {#each addressbooksByAccount[formAccountId] ?? [] as b (b.path)}
                  <option value={b.path}>{b.display_name ?? b.name}</option>
                {/each}
              </select>
            </label>
          </div>
        {/if}

        {#if formError}
          <p class="text-sm text-red-500">{formError}</p>
        {/if}

        <div class="flex items-center gap-2 pt-2">
          <button
            class="btn preset-filled-primary-500"
            disabled={saving}
            onclick={saveContact}
          >
            {saving ? 'Saving…' : selectedId === 'new' ? 'Create contact' : 'Save changes'}
          </button>
          <button class="btn preset-tonal" disabled={saving} onclick={cancelEdit}>
            Cancel
          </button>
          {#if selectedId !== 'new'}
            <div class="flex-1"></div>
            {#if deleteConfirm}
              <span class="text-xs text-surface-500">Really delete?</span>
              <button
                class="btn preset-filled-error-500"
                disabled={saving}
                onclick={deleteSelected}
              >
                Confirm delete
              </button>
              <button
                class="btn preset-tonal"
                disabled={saving}
                onclick={() => (deleteConfirm = false)}
              >
                Keep
              </button>
            {:else}
              <button
                class="btn preset-tonal text-red-500"
                disabled={saving}
                onclick={() => (deleteConfirm = true)}
              >
                Delete
              </button>
            {/if}
          {/if}
        </div>
      </div>
    {/if}
  </main>
</div>
