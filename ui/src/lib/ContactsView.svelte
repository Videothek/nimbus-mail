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
  interface ContactAddress {
    kind: string
    street: string
    locality: string
    region: string
    postal_code: string
    country: string
  }
  interface ContactPhone {
    /** "home" / "work" / "cell" / "fax" / "other" — pulled from the
        vCard `TEL;TYPE=…` parameter. */
    kind: string
    value: string
  }
  interface ContactEmail {
    /** "home" / "work" / "other" — pulled from the vCard
        `EMAIL;TYPE=…` parameter. */
    kind: string
    value: string
  }
  interface Contact {
    id: string
    nextcloud_account_id: string
    display_name: string
    email: ContactEmail[]
    phone: ContactPhone[]
    organization: string | null
    photo_mime: string | null
    photo_data: number[] | null
    title?: string | null
    birthday?: string | null
    note?: string | null
    addresses?: ContactAddress[]
    urls?: string[]
    /** vCard CATEGORIES — the Kontaktgruppen the contact
     *  belongs to.  Mutated by drag-drop onto a Kontaktgruppe
     *  row in the sidebar; sync goes back to NC via
     *  `add_contact_to_category`. */
    categories?: string[]
  }
  interface ContactInput {
    display_name: string
    emails: ContactEmail[]
    phones: ContactPhone[]
    organization: string | null
    photo_mime: string | null
    photo_data: number[] | null
    /** Optional extended fields. The Rust side merges them over the
        cached vCard so omitting a field preserves whatever was on
        the server, instead of clearing it. */
    title?: string | null
    birthday?: string | null
    note?: string | null
    addresses?: ContactAddress[]
    urls?: string[]
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
  // Same per-row treatment as phones — each email carries a kind
  // picker (Home / Work / Other) so the vCard `EMAIL;TYPE=…`
  // round-trips and Nextcloud Contacts groups identically.
  let formEmails = $state<ContactEmail[]>([])
  // Phones are now per-row records so each carries a kind picker
  // ("home" / "work" / "mobile" / "fax" / "other"), matching what
  // Nextcloud Contacts shows.
  let formPhones = $state<ContactPhone[]>([])
  let formOrg = $state('')
  let formTitle = $state('')
  let formBirthday = $state('')
  let formNote = $state('')
  let formUrls = $state('') // newline-separated
  // Addresses are an array of records, edited in place. We model a
  // single concatenated free-text field per address keeping
  // street/locality/region/postal/country on separate lines so the
  // form stays readable without exploding into one input per slot.
  let formAddresses = $state<ContactAddress[]>([])
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

  // ── Categories + mailing lists (#133 redesign) ───────────────
  interface ContactCategoryView {
    name: string
    memberCount: number
    useAsMailingList: boolean
  }
  interface MailingListView {
    id: string
    source: 'category' | 'team' | 'manual'
    name: string
    members: { displayName: string; email: string }[]
    hiddenFromAutocomplete: boolean
  }
  type ContactsTab = 'contacts' | 'lists'
  let activeTab = $state<ContactsTab>('contacts')
  /** Distinct addressbook paths across the cached contacts —
   *  populated lazily once `accounts` resolves so the sidebar
   *  can render one row per CardDAV collection.  Each row is
   *  `{ ncId, path, name, displayName? }`. */
  let allAddressbooks = $state<
    { ncId: string; path: string; name: string; displayName: string | null }[]
  >([])
  let categories = $state<ContactCategoryView[]>([])
  let mailingLists = $state<MailingListView[]>([])
  /** Currently-active sidebar selection on the Contacts tab.
   *  Strings: `'all'` | `'addressbook:<path>'` | `'category:<name>'`. */
  let selectedScope = $state<string>('all')

  /** Drag state for the drop-a-contact-on-a-Kontaktgruppe
   *  flow.  Carries the *app-side* contact id (`nc::uid`)
   *  since the IPC takes the composite id, not the bare UID. */
  let draggedContactId = $state<string | null>(null)
  let dragHoverCategory = $state<string | null>(null)

  async function loadSidebarData() {
    // Addressbooks: list per NC account, dedupe by composite
    // (ncId, path).  We don't show empty addressbooks the user
    // hasn't synced yet — the contacts list is the source of
    // truth.
    try {
      const seen = new Set<string>()
      const rows: typeof allAddressbooks = []
      for (const a of accounts) {
        try {
          const books = await invoke<AddressbookSummary[]>(
            'list_nextcloud_addressbooks',
            { ncId: a.id },
          )
          for (const b of books) {
            const k = `${a.id}::${b.path}`
            if (seen.has(k)) continue
            seen.add(k)
            rows.push({
              ncId: a.id,
              path: b.path,
              name: b.name,
              displayName: b.display_name,
            })
          }
        } catch (e) {
          console.warn('list_nextcloud_addressbooks failed for', a.id, e)
        }
      }
      allAddressbooks = rows
    } catch (e) {
      console.warn('addressbooks load failed', e)
    }
    try {
      categories = await invoke<ContactCategoryView[]>('list_contact_categories')
    } catch (e) {
      console.warn('list_contact_categories failed', e)
    }
    try {
      mailingLists = await invoke<MailingListView[]>('list_mailing_lists')
    } catch (e) {
      console.warn('list_mailing_lists failed', e)
    }
  }

  // ── Kontaktgruppe (CATEGORIES) CRUD ────────────────────────
  async function createCategory() {
    const name = prompt('New Kontaktgruppe — name?')?.trim()
    if (!name) return
    if (contacts.length === 0) {
      formError = 'Add at least one contact before creating a Kontaktgruppe — a tag with no contacts vanishes on the next sync.'
      return
    }
    const seedRaw = prompt(
      `Seed members — paste contact emails separated by commas (or leave blank to add later via drag-drop).`,
    )
    if (seedRaw === null) return
    const seedEmails = new Set(
      seedRaw
        .split(',')
        .map((s) => s.trim().toLowerCase())
        .filter(Boolean),
    )
    const seedIds = contacts
      .filter((c) => c.email.some((e) => seedEmails.has(e.value.toLowerCase())))
      .map((c) => c.id)
    if (seedIds.length === 0) {
      // No matching contacts — the category would vanish.
      // Bail with a hint rather than silently doing nothing.
      formError = 'None of the pasted emails matched a cached contact — Kontaktgruppe not created.'
      return
    }
    for (const id of seedIds) {
      try {
        await invoke('add_contact_to_category', { contactId: id, category: name })
      } catch (e) {
        console.warn('seed category member failed', id, e)
      }
    }
    await reloadContacts()
    selectedScope = `category:${name}`
  }
  async function renameCategory(name: string) {
    const next = prompt('Rename Kontaktgruppe', name)?.trim()
    if (!next || next === name) return
    try {
      await invoke('rename_contact_category', { old: name, new: next })
      await reloadContacts()
      if (selectedScope === `category:${name}`) selectedScope = `category:${next}`
    } catch (e) {
      formError = formatError(e) || 'Failed to rename Kontaktgruppe'
    }
  }
  async function deleteCategory(name: string) {
    if (!confirm(`Remove the "${name}" tag from every contact carrying it? Contacts themselves are kept.`)) return
    try {
      await invoke('delete_contact_category', { name })
      await reloadContacts()
      if (selectedScope === `category:${name}`) selectedScope = 'all'
    } catch (e) {
      formError = formatError(e) || 'Failed to delete Kontaktgruppe'
    }
  }
  async function toggleCategoryAsList(name: string, currentlyOn: boolean) {
    try {
      await invoke('set_category_use_as_mailing_list', {
        name,
        enabled: !currentlyOn,
      })
      categories = categories.map((c) =>
        c.name === name ? { ...c, useAsMailingList: !currentlyOn } : c,
      )
      // Mailing-lists view depends on this flag, refresh once.
      try {
        mailingLists = await invoke<MailingListView[]>('list_mailing_lists')
      } catch (e) {
        console.warn('list_mailing_lists refresh failed', e)
      }
    } catch (e) {
      formError = formatError(e) || 'Failed to toggle "Use as mailing list"'
    }
  }
  async function addContactIdToCategory(contactId: string, name: string) {
    try {
      await invoke('add_contact_to_category', { contactId, category: name })
      await reloadContacts()
    } catch (e) {
      formError = formatError(e) || 'Failed to tag contact'
    }
  }

  // ── Manual mailing list CRUD ──────────────────────────────
  async function createManualMailingList() {
    const name = prompt('New mailing list — name?')?.trim()
    if (!name) return
    if (accounts.length === 0) return
    const ncId = accounts[0].id
    let books = addressbooksByAccount[ncId]
    if (!books) {
      try {
        books = await invoke<AddressbookSummary[]>(
          'list_nextcloud_addressbooks',
          { ncId },
        )
        addressbooksByAccount[ncId] = books
      } catch (e) {
        formError = formatError(e) || 'Failed to list addressbooks'
        return
      }
    }
    const book = books[0]
    if (!book) return
    try {
      await invoke('create_contact_group', {
        ncId,
        addressbookUrl: book.path,
        addressbookName: book.name,
        displayName: name,
        memberUids: [],
      })
      try {
        mailingLists = await invoke<MailingListView[]>('list_mailing_lists')
      } catch (e) {
        console.warn('list_mailing_lists refresh failed', e)
      }
    } catch (e) {
      formError = formatError(e) || 'Failed to create mailing list'
    }
  }
  async function deleteManualMailingList(id: string, name: string) {
    if (!confirm(`Delete mailing list "${name}"? Members are not affected.`)) return
    // Manual rows use `list:<vcard-uid>` ids; the underlying
    // group_id (composite contact id) is `nc::uid`.  Strip the
    // prefix to get back to the contact-handle id.
    const groupId = id.startsWith('list:') ? id.slice(5) : id
    try {
      await invoke('delete_contact_group', { groupId })
      mailingLists = mailingLists.filter((m) => m.id !== id)
    } catch (e) {
      formError = formatError(e) || 'Failed to delete mailing list'
    }
  }
  async function toggleMailingListHidden(id: string, currently: boolean) {
    try {
      await invoke('set_mailing_list_hidden', { id, hidden: !currently })
      mailingLists = mailingLists.map((m) =>
        m.id === id ? { ...m, hiddenFromAutocomplete: !currently } : m,
      )
    } catch (e) {
      formError = formatError(e) || 'Failed to toggle hide flag'
    }
  }

  // Naive free-text filter over the loaded list. Server-side search
  // isn't needed at this scale (addressbooks are usually hundreds,
  // not thousands, of contacts).
  let query = $state('')
  const filteredContacts = $derived.by(() => {
    const q = query.trim().toLowerCase()
    let scope = contacts
    if (selectedScope.startsWith('addressbook:')) {
      const path = selectedScope.slice('addressbook:'.length)
      // Composite ids are `nc::uid`, but the addressbook lives
      // server-side and we don't ship it on `Contact` directly.
      // Match via emailing through the cache row's
      // `nextcloud_account_id` + a separate IPC isn't worth it
      // for the typical user with one or two books — for now
      // we filter by the addressbook's nc-account at minimum
      // so each book row doesn't show every account's contacts.
      const book = allAddressbooks.find((b) => b.path === path)
      if (book) {
        scope = contacts.filter(
          (c) => c.nextcloud_account_id === book.ncId,
        )
      }
    } else if (selectedScope.startsWith('category:')) {
      const name = selectedScope.slice('category:'.length)
      scope = contacts.filter((c) => c.categories?.includes(name))
    }
    if (!q) return scope
    return scope.filter(
      (c) =>
        c.display_name.toLowerCase().includes(q) ||
        c.email.some((e) => e.value.toLowerCase().includes(q)) ||
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
    contacts.sort((a, b) =>
      a.display_name.localeCompare(b.display_name, undefined, { sensitivity: 'base' }),
    )
    await loadSidebarData()
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
    formEmails = c.email.map((e) => ({ ...e }))
    formPhones = c.phone.map((p) => ({ ...p }))
    formOrg = c.organization ?? ''
    formTitle = c.title ?? ''
    formBirthday = c.birthday ?? ''
    formNote = c.note ?? ''
    formUrls = (c.urls ?? []).join('\n')
    formAddresses = (c.addresses ?? []).map((a) => ({ ...a }))
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
    formEmails = []
    formPhones = []
    formOrg = ''
    formTitle = ''
    formBirthday = ''
    formNote = ''
    formUrls = ''
    formAddresses = []
    selectedPhotoBytes = null
    if (!formAccountId && accounts.length > 0) {
      formAccountId = accounts[0].id
    }
    if (formAccountId) void loadAddressbooksFor(formAccountId)
  }

  /** Add a blank address row. Defaults to "home" so the picker has
      something selected — RFC 6350's TYPE param is optional but
      Nextcloud Contacts always groups by it, so we may as well too. */
  function addAddress() {
    formAddresses = [
      ...formAddresses,
      {
        kind: 'home',
        street: '',
        locality: '',
        region: '',
        postal_code: '',
        country: '',
      },
    ]
  }

  function removeAddress(idx: number) {
    formAddresses = formAddresses.filter((_, i) => i !== idx)
  }

  /** Add a blank phone row. Defaults to "cell" — by far the most
      common kind for a freshly-added number on a personal contact. */
  function addPhone() {
    formPhones = [...formPhones, { kind: 'cell', value: '' }]
  }

  function removePhone(idx: number) {
    formPhones = formPhones.filter((_, i) => i !== idx)
  }

  /** Add a blank email row. Defaults to "home" — typical for a
      personal contact entry; the user can flip to Work / Other. */
  function addEmail() {
    formEmails = [...formEmails, { kind: 'home', value: '' }]
  }

  function removeEmail(idx: number) {
    formEmails = formEmails.filter((_, i) => i !== idx)
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
      // Drop empty-value rows the same way phones do — an unfilled
      // "Add email" slot shouldn't ship to the server as a blank.
      emails: formEmails
        .filter((e) => e.value.trim())
        .map((e) => ({ kind: e.kind, value: e.value.trim() })),
      // Drop empty-value rows so an unfilled "Add phone" slot
      // doesn't end up as a blank entry on the server.
      phones: formPhones
        .filter((p) => p.value.trim())
        .map((p) => ({ kind: p.kind, value: p.value.trim() })),
      organization: formOrg.trim() || null,
      photo_mime: existingMime,
      photo_data: existingMime ? selectedPhotoBytes : null,
      title: formTitle.trim() || null,
      birthday: formBirthday.trim() || null,
      note: formNote.trim() || null,
      urls: splitLines(formUrls),
      // Strip empty rows so the user can't end up with a phantom
      // address from forgetting to fill in the slots they added.
      addresses: formAddresses.filter(
        (a) =>
          a.street.trim() ||
          a.locality.trim() ||
          a.region.trim() ||
          a.postal_code.trim() ||
          a.country.trim(),
      ),
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
  <!-- ── Sidebar (Contacts tab) — addressbooks + Kontaktgruppen.
       Mailing-lists tab replaces the contacts list with the
       new mailing-list catalogue, so the sidebar only renders
       on the Contacts tab. ─────────────────────────────────── -->
  {#if activeTab === 'contacts'}
  <aside class="w-56 shrink-0 border-r border-surface-200 dark:border-surface-700 bg-surface-100/60 dark:bg-surface-800/40 flex flex-col">
    <div class="flex-1 overflow-y-auto px-2 py-3 space-y-1">
      <!-- "All" -->
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm text-left transition-colors {selectedScope === 'all'
          ? 'bg-primary-500/15 text-primary-600 dark:text-primary-300 font-medium'
          : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
        onclick={() => (selectedScope = 'all')}
      >
        <span class="w-6 text-center">👥</span>
        <span class="flex-1 truncate">All contacts</span>
        <span class="text-xs text-surface-500">{contacts.length}</span>
      </button>

      <!-- Addressbooks — one row per CardDAV collection.  Click
           filters the middle list to entries from that book's
           NC account (the contact row doesn't carry the
           addressbook path, so we approximate by NC account). -->
      {#if allAddressbooks.length > 0}
        <div class="px-3 pt-3 pb-1 text-[10px] uppercase tracking-wider text-surface-500">
          Addressbooks
        </div>
        {#each allAddressbooks as b (`${b.ncId}::${b.path}`)}
          {@const sel = selectedScope === `addressbook:${b.path}`}
          <button
            class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm text-left transition-colors
                   {sel
                     ? 'bg-primary-500/15 text-primary-600 dark:text-primary-300 font-medium'
                     : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
            onclick={() => (selectedScope = `addressbook:${b.path}`)}
          >
            <span class="w-6 text-center">📒</span>
            <span class="flex-1 truncate">{b.displayName ?? b.name}</span>
          </button>
        {/each}
      {/if}

      <!-- Kontaktgruppen — derived from CATEGORIES on every
           cached vCard.  Drag-drop a contact onto a row to add
           it; right-click opens rename / delete; the swatch
           toggles "Use as mailing list". -->
      <div class="px-3 pt-3 pb-1 flex items-center justify-between">
        <span class="text-[10px] uppercase tracking-wider text-surface-500">Kontaktgruppen</span>
        <button
          class="w-5 h-5 rounded-md flex items-center justify-center text-surface-500 hover:bg-surface-200 dark:hover:bg-surface-700"
          title="New Kontaktgruppe"
          aria-label="New Kontaktgruppe"
          onclick={() => void createCategory()}
        >+</button>
      </div>
      {#each categories as c (c.name)}
        {@const sel = selectedScope === `category:${c.name}`}
        {@const dragOver = dragHoverCategory === c.name}
        <!-- Container is a div, not a button, so the inline
             "Use as mailing list" swatch can stay a real
             <button> — nested <button> would otherwise trip
             the HTML parser's repair pass.  We add
             role="button" + tabindex + keyboard handler so the
             a11y story matches a regular button. -->
        <div
          role="button"
          tabindex="0"
          class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm text-left transition-colors cursor-pointer
                 {sel
                   ? 'bg-primary-500/15 text-primary-600 dark:text-primary-300 font-medium'
                   : 'hover:bg-surface-200 dark:hover:bg-surface-700'}
                 {dragOver ? 'ring-2 ring-primary-500' : ''}"
          oncontextmenu={(e) => {
            e.preventDefault()
            const action = prompt(
              `"${c.name}" — type: rename / delete`,
              '',
            )?.trim()
            if (action === 'rename') void renameCategory(c.name)
            else if (action === 'delete') void deleteCategory(c.name)
          }}
          ondragover={(e) => {
            if (!draggedContactId) return
            e.preventDefault()
            dragHoverCategory = c.name
          }}
          ondragleave={() => {
            if (dragHoverCategory === c.name) dragHoverCategory = null
          }}
          ondrop={(e) => {
            e.preventDefault()
            const id = draggedContactId
            dragHoverCategory = null
            draggedContactId = null
            if (id) void addContactIdToCategory(id, c.name)
          }}
          onclick={() => (selectedScope = `category:${c.name}`)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault()
              selectedScope = `category:${c.name}`
            }
          }}
        >
          <span class="w-6 text-center">{(c.name || '?').slice(0, 1).toUpperCase()}</span>
          <span class="flex-1 truncate">{c.name}</span>
          <!-- Per-row "Use as mailing list" swatch — filled
               primary when on, empty outline when off.  Click
               toggles WITHOUT propagating to the row's main
               click handler. -->
          <button
            class="w-3 h-3 rounded-sm shrink-0 border transition-colors cursor-pointer mr-1"
            style={c.useAsMailingList
              ? `background-color: var(--color-primary-500); border-color: var(--color-primary-500);`
              : `background-color: transparent; border-color: var(--color-surface-400);`}
            title={c.useAsMailingList
              ? 'Currently usable as a mailing list (click to disable)'
              : 'Currently NOT usable as a mailing list (click to enable)'}
            aria-label="Toggle use as mailing list"
            onclick={(e) => {
              e.stopPropagation()
              void toggleCategoryAsList(c.name, c.useAsMailingList)
            }}
          ></button>
          <span class="text-xs text-surface-500">{c.memberCount}</span>
        </div>
      {/each}
      {#if categories.length === 0}
        <p class="px-3 py-2 text-xs text-surface-500 italic">
          No Kontaktgruppen yet. Click <span class="font-semibold">+</span> to
          create one — drag contacts onto it after to tag them.
        </p>
      {/if}
    </div>
  </aside>
  {/if}

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
      <h2 class="text-base font-semibold flex-1">{activeTab === 'contacts' ? 'Contacts' : 'Mailing lists'}</h2>
      {#if syncing}
        <span class="text-[10px] text-surface-500">Syncing…</span>
      {/if}
    </div>

    <!-- Tab strip — Contacts / Mailing lists.  Same shell so
         the back button / sync indicator stays anchored, only
         the column body swaps. -->
    <div class="px-3 pt-2 flex gap-1 border-b border-surface-200 dark:border-surface-700">
      <button
        type="button"
        class="flex-1 px-3 py-2 text-sm rounded-t-md transition-colors {activeTab === 'contacts'
          ? 'bg-primary-500/15 text-primary-600 dark:text-primary-300 font-medium'
          : 'text-surface-600 dark:text-surface-300 hover:bg-surface-200 dark:hover:bg-surface-700'}"
        onclick={() => (activeTab = 'contacts')}
      >Contacts</button>
      <button
        type="button"
        class="flex-1 px-3 py-2 text-sm rounded-t-md transition-colors {activeTab === 'lists'
          ? 'bg-primary-500/15 text-primary-600 dark:text-primary-300 font-medium'
          : 'text-surface-600 dark:text-surface-300 hover:bg-surface-200 dark:hover:bg-surface-700'}"
        onclick={() => (activeTab = 'lists')}
      >Mailing lists</button>
    </div>

    {#if activeTab === 'contacts'}
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
            draggable="true"
            ondragstart={(e) => {
              draggedContactId = c.id
              e.dataTransfer?.setData('text/plain', c.display_name)
              if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copy'
            }}
            ondragend={() => {
              draggedContactId = null
              dragHoverCategory = null
            }}
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
                <span class="text-xs text-surface-500 truncate normal-case">{c.email[0].value}</span>
              {/if}
            </span>
          </button>
        {/each}
      {/if}
    </div>
    {:else}
    <!-- Mailing lists tab — virtual rows from categories
         (auto-mirrored), manual KIND:group cards (CRUD), and
         Teams (read-only).  Per-row hide swatch on non-category
         sources; categories use their sidebar swatch instead. -->
    <div class="p-3 flex flex-col gap-2">
      <button
        class="btn preset-filled-primary-500"
        onclick={() => void createManualMailingList()}
      >+ New mailing list</button>
    </div>
    <div class="flex-1 overflow-y-auto px-2 pb-3 space-y-1">
      {#if mailingLists.length === 0}
        <p class="px-3 py-2 text-xs text-surface-500 italic">
          No mailing lists yet. Click <span class="font-semibold">+ New mailing list</span> to
          create one, or tag contacts with a Kontaktgruppe and flip its
          "Use as mailing list" swatch on.
        </p>
      {/if}
      {#each mailingLists as ml (ml.id)}
        {@const isManual = ml.source === 'manual'}
        {@const isTeam = ml.source === 'team'}
        {@const isCategory = ml.source === 'category'}
        <div class="px-3 py-2 rounded-md hover:bg-surface-200 dark:hover:bg-surface-700">
          <div class="flex items-center gap-2">
            <span class="w-6 text-center">
              {isCategory ? '🏷️' : isTeam ? '⚡' : '📨'}
            </span>
            <div class="flex-1 min-w-0">
              <div class="flex items-center gap-2">
                <span class="font-medium truncate">{ml.name}</span>
                <span class="text-[10px] uppercase tracking-wider font-semibold px-1 py-px rounded
                             {isCategory
                               ? 'bg-primary-500/20 text-primary-600 dark:text-primary-300'
                               : isTeam
                                 ? 'bg-surface-300 dark:bg-surface-600 text-surface-700 dark:text-surface-200'
                                 : 'bg-success-500/20 text-success-600 dark:text-success-300'}">
                  {isCategory ? 'category' : isTeam ? 'team' : 'manual'}
                </span>
              </div>
              <p class="text-xs text-surface-500 truncate">
                {ml.members.filter((m) => m.email).length} member{ml.members.filter((m) => m.email).length === 1 ? '' : 's'} with email
              </p>
            </div>
            {#if !isCategory}
              <button
                class="w-3 h-3 rounded-sm shrink-0 border transition-colors cursor-pointer"
                style={ml.hiddenFromAutocomplete
                  ? `background-color: transparent; border-color: var(--color-surface-400);`
                  : `background-color: var(--color-primary-500); border-color: var(--color-primary-500);`}
                title={ml.hiddenFromAutocomplete
                  ? 'Currently hidden from autocomplete (click to show)'
                  : 'Currently shown in autocomplete (click to hide)'}
                aria-label="Toggle hidden from autocomplete"
                onclick={() => void toggleMailingListHidden(ml.id, ml.hiddenFromAutocomplete)}
              ></button>
            {/if}
            {#if isManual}
              <button
                class="text-xs text-surface-500 hover:text-error-500"
                title="Delete mailing list"
                aria-label="Delete mailing list"
                onclick={() => void deleteManualMailingList(ml.id, ml.name)}
              >×</button>
            {/if}
          </div>
        </div>
      {/each}
    </div>
    {/if}
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

        <!-- Email addresses — same per-row treatment as phones so
             each carries a Home / Work / Other picker. The kind
             round-trips to the vCard `EMAIL;TYPE=…` parameter. -->
        <div class="space-y-2">
          <div class="flex items-center justify-between">
            <span class="text-sm font-medium">Email addresses</span>
            <button type="button" class="btn btn-sm preset-tonal" onclick={addEmail}>
              + Add email
            </button>
          </div>
          {#each formEmails as email, i (i)}
            <div class="flex items-center gap-2">
              <select class="select w-28" bind:value={email.kind}>
                <option value="home">Home</option>
                <option value="work">Work</option>
                <option value="other">Other</option>
              </select>
              <input
                class="input flex-1"
                type="email"
                bind:value={email.value}
                placeholder="jane@example.com"
              />
              <button
                type="button"
                class="text-xs text-error-500 hover:underline"
                onclick={() => removeEmail(i)}
              >Remove</button>
            </div>
          {/each}
        </div>

        <!-- Phone numbers — per-row so each carries a kind picker
             (mobile / work / home / fax / other) and Nextcloud
             Contacts groups identically on its side. Same shape as
             the addresses block below. -->
        <div class="space-y-2">
          <div class="flex items-center justify-between">
            <span class="text-sm font-medium">Phone numbers</span>
            <button type="button" class="btn btn-sm preset-tonal" onclick={addPhone}>
              + Add phone
            </button>
          </div>
          {#each formPhones as phone, i (i)}
            <div class="flex items-center gap-2">
              <select class="select w-28" bind:value={phone.kind}>
                <option value="cell">Mobile</option>
                <option value="work">Work</option>
                <option value="home">Home</option>
                <option value="fax">Fax</option>
                <option value="other">Other</option>
              </select>
              <input
                class="input flex-1"
                bind:value={phone.value}
                placeholder="+1 555 0100"
              />
              <button
                type="button"
                class="text-xs text-error-500 hover:underline"
                onclick={() => removePhone(i)}
              >Remove</button>
            </div>
          {/each}
        </div>

        <div class="grid grid-cols-2 gap-3">
          <label class="label">
            <span>Organization</span>
            <input class="input" bind:value={formOrg} placeholder="Example Corp" />
          </label>
          <label class="label">
            <span>Job title</span>
            <input class="input" bind:value={formTitle} placeholder="Product Manager" />
          </label>
        </div>

        <label class="label">
          <span>Birthday</span>
          <input
            class="input"
            bind:value={formBirthday}
            placeholder="1985-10-31"
          />
        </label>

        <label class="label">
          <span>Websites <span class="text-surface-500">(one per line)</span></span>
          <textarea
            class="textarea"
            rows="2"
            bind:value={formUrls}
            placeholder="https://example.com"
          ></textarea>
        </label>

        <!-- Postal addresses. Variable-length so we render with an
             explicit add/remove instead of a free-text textarea —
             matches the Nextcloud Contacts UI's per-address card and
             keeps street/city/region/postal/country round-tripping
             cleanly through the vCard ADR field. -->
        <div class="space-y-2">
          <div class="flex items-center justify-between">
            <span class="text-sm font-medium">Addresses</span>
            <button type="button" class="btn btn-sm preset-tonal" onclick={addAddress}>
              + Add address
            </button>
          </div>
          {#each formAddresses as addr, i (i)}
            <div class="card p-3 bg-surface-50 dark:bg-surface-900/50 rounded-md space-y-2">
              <div class="flex items-center gap-2">
                <select class="select w-32" bind:value={addr.kind}>
                  <option value="home">Home</option>
                  <option value="work">Work</option>
                  <option value="other">Other</option>
                </select>
                <button
                  type="button"
                  class="ml-auto text-xs text-error-500 hover:underline"
                  onclick={() => removeAddress(i)}
                >Remove</button>
              </div>
              <input class="input" bind:value={addr.street} placeholder="Street" />
              <div class="grid grid-cols-2 gap-2">
                <input class="input" bind:value={addr.locality} placeholder="City" />
                <input class="input" bind:value={addr.region} placeholder="Region / State" />
              </div>
              <div class="grid grid-cols-2 gap-2">
                <input class="input" bind:value={addr.postal_code} placeholder="Postal code" />
                <input class="input" bind:value={addr.country} placeholder="Country" />
              </div>
            </div>
          {/each}
        </div>

        <label class="label">
          <span>Notes</span>
          <textarea
            class="textarea"
            rows="3"
            bind:value={formNote}
            placeholder="Anything you want to remember about this contact"
          ></textarea>
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
