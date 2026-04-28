<script lang="ts">
  /**
   * AddressAutocomplete — a comma/semicolon-separated address input
   * with a dropdown of matching contacts.
   *
   * # Why a wrapper around <input>
   *
   * The compose form has three recipient fields (To / Cc / Bcc) and we
   * want the same behaviour on all of them: type `ali`, see Alice
   * Example pop up with her photo thumbnail, arrow-down + Enter fills
   * in `Alice Example <alice@example.com>, ` and keeps the caret in
   * the field ready for the next address. Factoring it into one
   * component beats duplicating the plumbing three times.
   *
   * # What the backend does vs. what we do
   *
   * The Rust `search_contacts` command does the LIKE query over the
   * local cache and returns fully hydrated Contact rows. We add UI
   * polish on top: debouncing (so we don't hit the IPC bridge on
   * every keystroke), token parsing (the query is only the text
   * after the last `,` or `;`), and keyboard navigation.
   *
   * Photos render via the `contact-photo://` URI scheme — the
   * webview fetches the bytes straight from the Rust cache, so the
   * dropdown payload stays tiny (no JSON byte-array bloat) and the
   * browser caches per-id automatically.
   */

  import { convertFileSrc, invoke } from '@tauri-apps/api/core'
  import { onDestroy } from 'svelte'

  interface ContactEmail {
    kind: string
    value: string
  }
  interface ContactPhone {
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
  }

  interface Props {
    /** The full field value (`"alice@x, bob@y"`). Two-way via bind. */
    value: string
    placeholder?: string
    id?: string
  }
  let { value = $bindable(''), placeholder = '', id = '' }: Props = $props()

  // ── Dropdown state ─────────────────────────────────────────
  /** A suggestion row — either an individual contact (`kind:
   *  'contact'`) or a group / mailing list (#133, #113), in
   *  which case selecting it expands every member into the
   *  field rather than inserting a single address. */
  type Suggestion =
    | { kind: 'contact'; contact: Contact }
    | { kind: 'group'; group: GroupSuggestion }
  interface GroupSuggestion {
    id: string
    displayName: string
    emoji: string | null
    members: { id: string; displayName: string; email: string }[]
    hidden: boolean
  }
  let suggestions = $state<Suggestion[]>([])
  let open = $state(false)
  let activeIndex = $state(0)
  let inputEl: HTMLInputElement | undefined = $state()
  /** All non-hidden groups, fetched once and refreshed when
   *  `list_contact_groups` is available — we filter client-side
   *  on every keystroke so the dropdown stays snappy. */
  let allGroups = $state<GroupSuggestion[]>([])
  void invoke<GroupSuggestion[]>('list_contact_groups')
    .then((rows) => {
      allGroups = rows.filter((g) => !g.hidden)
    })
    .catch((e) => console.warn('list_contact_groups failed', e))

  // 150ms debounce keeps the UI snappy without firing a DB query on
  // every keystroke of a fast typer. The timer is a setTimeout handle
  // that we cancel-and-restart on each input event.
  let debounceTimer: number | null = null
  const DEBOUNCE_MS = 150
  const LIMIT = 8  // max dropdown rows; bigger lists scroll

  /** Extract the trailing token (the part after the last , or ;). */
  function currentToken(s: string): { token: string; prefix: string } {
    // Find the last separator; everything after is what the user is
    // actively typing. Everything before (+ separator + trailing
    // whitespace) is preserved verbatim when we commit a selection.
    const match = s.match(/^(.*[,;]\s*)?(.*)$/s)
    const prefix = match?.[1] ?? ''
    const token = (match?.[2] ?? '').trimStart()
    return { token, prefix }
  }

  async function runSearch(query: string) {
    if (query.length < 2) {
      suggestions = []
      open = false
      return
    }
    try {
      const rows = await invoke<Contact[]>('search_contacts', {
        query,
        limit: LIMIT,
      })
      // Match groups client-side from the cached `allGroups` —
      // they're typically a handful per user, so a substring
      // scan beats round-tripping a dedicated IPC.
      const q = query.toLowerCase()
      const groupHits = allGroups
        .filter((g) => g.displayName.toLowerCase().includes(q))
        .slice(0, LIMIT)
      // Groups go first — a group typed by name is almost
      // always the user's intent, and putting them at the top
      // matches Outlook / Apple Mail's autocomplete ordering.
      const merged: Suggestion[] = [
        ...groupHits.map((g) => ({ kind: 'group' as const, group: g })),
        ...rows.map((c) => ({ kind: 'contact' as const, contact: c })),
      ]
      suggestions = merged.slice(0, LIMIT)
      activeIndex = 0
      open = suggestions.length > 0
    } catch (e) {
      // Autocomplete is a nice-to-have — never surface errors here,
      // just collapse the dropdown silently.
      console.warn('search_contacts failed', e)
      suggestions = []
      open = false
    }
  }

  function onInput() {
    const { token } = currentToken(value)
    if (debounceTimer !== null) window.clearTimeout(debounceTimer)
    debounceTimer = window.setTimeout(() => runSearch(token), DEBOUNCE_MS)
  }

  /** Pick the first non-empty email address. Each entry now
      carries a kind hint (Home / Work / Other from vCard
      `EMAIL;TYPE=…`); the autocomplete only needs the value. */
  function primaryEmail(c: Contact): string {
    return c.email.find((e) => e.value.length > 0)?.value ?? ''
  }

  /**
   * Format a contact as an RFC-style address. We prefer
   * `"Display Name" <addr@x>` when a display name is present so the
   * SMTP send path gets a friendly From header; bare address if not.
   */
  function formatAddress(c: Contact): string {
    const addr = primaryEmail(c)
    if (!addr) return ''
    if (c.display_name && c.display_name !== addr) {
      // Escape embedded quotes; most names don't have them but be safe.
      const safe = c.display_name.replace(/"/g, '\\"')
      return `"${safe}" <${addr}>`
    }
    return addr
  }

  function pickContact(c: Contact) {
    const { prefix } = currentToken(value)
    const formatted = formatAddress(c)
    if (!formatted) return
    // Insert the selected address and a trailing `, ` so the user can
    // keep typing the next one without extra keystrokes.
    value = `${prefix}${formatted}, `
    suggestions = []
    open = false
    // Restore focus in case the click stole it.
    inputEl?.focus()
  }

  /** Expand a group selection: drop every member's email into
   *  the field as its own RFC-shaped address.  Members with no
   *  email (phone/photo-only contacts that ended up in a group)
   *  are silently skipped — they wouldn't survive an SMTP send
   *  anyway. */
  function pickGroup(g: GroupSuggestion) {
    const { prefix } = currentToken(value)
    const formatted = g.members
      .filter((m) => m.email)
      .map((m) => {
        if (m.displayName && m.displayName !== m.email) {
          const safe = m.displayName.replace(/"/g, '\\"')
          return `"${safe}" <${m.email}>`
        }
        return m.email
      })
      .join(', ')
    if (!formatted) return
    value = `${prefix}${formatted}, `
    suggestions = []
    open = false
    inputEl?.focus()
  }

  function pick(s: Suggestion) {
    if (s.kind === 'contact') pickContact(s.contact)
    else pickGroup(s.group)
  }

  function onKeydown(e: KeyboardEvent) {
    if (!open || suggestions.length === 0) return
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      activeIndex = (activeIndex + 1) % suggestions.length
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      activeIndex = (activeIndex - 1 + suggestions.length) % suggestions.length
    } else if (e.key === 'Enter') {
      // Intercept Enter only when dropdown is open — otherwise Enter
      // should fall through to any form submit handler.
      e.preventDefault()
      pick(suggestions[activeIndex])
    } else if (e.key === 'Escape') {
      e.preventDefault()
      open = false
    } else if (e.key === 'Tab') {
      // Tab behaves like Enter for autocomplete — lets power users
      // blast through a list of names quickly.
      if (suggestions.length > 0) {
        e.preventDefault()
        pick(suggestions[activeIndex])
      }
    }
  }

  function onBlur() {
    // Close on a tick so a click on a suggestion can fire first;
    // onmousedown on the suggestion will have already committed.
    setTimeout(() => {
      open = false
    }, 120)
  }

  /**
   * URL for `<img src>` against the custom `contact-photo://` URI
   * scheme. Returns `null` for contacts with no photo so callers
   * can render the initials placeholder. The browser caches per-id
   * so a contact that pops in and out of the dropdown only fetches
   * its avatar once.
   */
  function photoUrl(c: Contact): string | null {
    if (!c.photo_mime) return null
    return convertFileSrc(c.id, 'contact-photo')
  }

  function initials(name: string): string {
    const parts = name.trim().split(/\s+/).filter(Boolean)
    if (parts.length === 0) return '?'
    if (parts.length === 1) return parts[0][0].toUpperCase()
    return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase()
  }

  onDestroy(() => {
    if (debounceTimer !== null) window.clearTimeout(debounceTimer)
  })
</script>

<div class="relative flex-1">
  <input
    {id}
    {placeholder}
    bind:this={inputEl}
    bind:value
    class="input w-full px-3 py-2 text-sm rounded-md"
    oninput={onInput}
    onkeydown={onKeydown}
    onblur={onBlur}
    onfocus={() => {
      if (suggestions.length > 0) open = true
    }}
    autocomplete="off"
  />

  {#if open && suggestions.length > 0}
    <ul
      class="absolute left-0 right-0 top-full mt-1 z-50 max-h-72 overflow-y-auto
             bg-surface-50 dark:bg-surface-900 border border-surface-300
             dark:border-surface-700 rounded-md shadow-lg"
      role="listbox"
    >
      {#each suggestions as s, i (s.kind === 'contact' ? s.contact.id : `g:${s.group.id}`)}
        <li
          role="option"
          aria-selected={i === activeIndex}
          class="flex items-center gap-3 px-3 py-2 cursor-pointer text-sm
                 {i === activeIndex ? 'bg-primary-500/15' : 'hover:bg-surface-200 dark:hover:bg-surface-800'}"
          onmousedown={(e) => { e.preventDefault(); pick(s) }}
          onmouseenter={() => (activeIndex = i)}
        >
          {#if s.kind === 'contact'}
            {@const c = s.contact}
            {@const url = photoUrl(c)}
            {#if url}
              <img
                src={url}
                alt=""
                loading="lazy"
                class="w-8 h-8 rounded-full object-cover flex-shrink-0"
              />
            {:else}
              <div class="w-8 h-8 rounded-full bg-surface-300 dark:bg-surface-700
                          flex items-center justify-center text-xs font-semibold flex-shrink-0">
                {initials(c.display_name)}
              </div>
            {/if}
            <div class="flex-1 min-w-0">
              <p class="font-medium truncate">{c.display_name}</p>
              <p class="text-xs text-surface-500 truncate">
                {primaryEmail(c)}
                {#if c.organization}· {c.organization}{/if}
              </p>
            </div>
          {:else}
            {@const g = s.group}
            {@const sendable = g.members.filter((m) => m.email).length}
            <div class="w-8 h-8 rounded-full bg-primary-500/20 text-primary-600 dark:text-primary-300
                        flex items-center justify-center text-base font-semibold flex-shrink-0">
              {g.emoji && g.emoji.trim() ? g.emoji : (g.displayName || '?').slice(0, 1).toUpperCase()}
            </div>
            <div class="flex-1 min-w-0">
              <p class="font-medium truncate flex items-center gap-2">
                <span class="truncate">{g.displayName}</span>
                <span class="text-[10px] uppercase tracking-wider font-semibold px-1 py-px rounded bg-primary-500/20 text-primary-600 dark:text-primary-300">
                  group
                </span>
              </p>
              <p class="text-xs text-surface-500 truncate">
                {sendable} member{sendable === 1 ? '' : 's'} with email
              </p>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>
