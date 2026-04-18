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

  interface Contact {
    id: string
    nextcloud_account_id: string
    display_name: string
    email: string[]
    phone: string[]
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
  let suggestions = $state<Contact[]>([])
  let open = $state(false)
  let activeIndex = $state(0)
  let inputEl: HTMLInputElement | undefined = $state()

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
      suggestions = rows
      activeIndex = 0
      open = rows.length > 0
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

  /** Pick the first non-empty email address. */
  function primaryEmail(c: Contact): string {
    return c.email.find((e) => e.length > 0) ?? ''
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

  function pick(c: Contact) {
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
      {#each suggestions as c, i (c.id)}
        {@const url = photoUrl(c)}
        <li
          role="option"
          aria-selected={i === activeIndex}
          class="flex items-center gap-3 px-3 py-2 cursor-pointer text-sm
                 {i === activeIndex ? 'bg-primary-500/15' : 'hover:bg-surface-200 dark:hover:bg-surface-800'}"
          onmousedown={(e) => { e.preventDefault(); pick(c) }}
          onmouseenter={() => (activeIndex = i)}
        >
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
        </li>
      {/each}
    </ul>
  {/if}
</div>
