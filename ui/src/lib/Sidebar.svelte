<script lang="ts">
  /**
   * Sidebar — account's folder list + integration shortcuts.
   *
   * Folders come from the IMAP server. We load the cached list first
   * so the sidebar paints instantly on launch, then refresh from the
   * network in parallel (same pattern as MailList / MailView).
   *
   * Clicking a folder calls `onselectfolder(name)` so App.svelte can
   * point MailList at the new folder.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  interface Folder {
    name: string
    delimiter: string | null
    attributes: string[]
    unread_count: number | null
  }

  interface Props {
    accountId: string
    selectedFolder: string
    onselectfolder: (name: string) => void
    onsettings: () => void
  }
  let { accountId, selectedFolder, onselectfolder, onsettings }: Props = $props()

  let folders = $state<Folder[]>([])
  let loading = $state(true)
  let refreshing = $state(false)
  let error = $state('')

  // Integrations are still hardcoded — those are planned features, not
  // derived from the mail server.
  const integrations = [
    { name: 'Calendar', icon: '\u{1F4C5}' },      // 📅
    { name: 'Contacts', icon: '\u{1F464}' },      // 👤
    { name: 'Nextcloud Talk', icon: '\u{1F4AC}' }, // 💬
    { name: 'Nextcloud Files', icon: '\u{1F4C1}' },// 📁
  ]

  $effect(() => {
    void load(accountId)
  })

  async function load(id: string) {
    loading = true
    refreshing = false
    error = ''

    try {
      const cached = await invoke<Folder[]>('get_cached_folders', { accountId: id })
      if (id === accountId) {
        folders = cached
        if (cached.length > 0) loading = false
      }
    } catch (e) {
      console.warn('get_cached_folders failed:', e)
    }

    refreshing = folders.length > 0
    try {
      const fresh = await invoke<Folder[]>('fetch_folders', { accountId: id })
      if (id === accountId) {
        folders = fresh
      }
    } catch (e) {
      if (folders.length === 0) {
        error = formatError(e) || 'Failed to load folders'
      } else {
        console.warn('fetch_folders failed (showing cached):', e)
      }
    } finally {
      loading = false
      refreshing = false
    }
  }

  // Map IMAP folder attributes (\Sent, \Drafts, \Trash, etc.) to an
  // emoji. Falls back to a plain folder icon. Attribute names come from
  // async-imap's Debug formatting, so they look like `Sent`, `Drafts`,
  // etc. — we match case-insensitively.
  function folderIcon(f: Folder): string {
    const name = f.name.toLowerCase()
    const attrs = f.attributes.map((a) => a.toLowerCase())

    const has = (k: string) => attrs.some((a) => a.includes(k))
    if (name === 'inbox' || has('inbox')) return '\u{1F4E5}' // 📥
    if (has('sent')) return '\u{1F4E4}' // 📤
    if (has('draft')) return '\u{1F4DD}' // 📝
    if (has('trash') || has('deleted')) return '\u{1F5D1}' // 🗑️
    if (has('junk') || has('spam')) return '\u{1F6AB}' // 🚫
    if (has('flagged') || has('starred')) return '\u{2B50}' // ⭐
    if (has('archive')) return '\u{1F5C3}' // 🗃️
    return '\u{1F4C1}' // 📁
  }

  // Short display name: strip the hierarchy prefix so "INBOX/Work" shows
  // as "Work". INBOX itself keeps its name but title-cased.
  function displayName(f: Folder): string {
    if (f.name.toUpperCase() === 'INBOX') return 'Inbox'
    const delim = f.delimiter ?? '/'
    const parts = f.name.split(delim)
    return parts[parts.length - 1] || f.name
  }
</script>

<aside class="w-56 shrink-0 border-r border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800 flex flex-col">
  <!-- App title -->
  <div class="p-4 border-b border-surface-200 dark:border-surface-700">
    <h1 class="text-lg font-bold text-primary-500">Nimbus Mail</h1>
  </div>

  <!-- Compose button -->
  <div class="p-3">
    <button class="btn preset-filled-primary-500 w-full">
      Compose
    </button>
  </div>

  <!-- Mail folders -->
  <nav class="flex-1 overflow-y-auto px-2">
    <p class="px-2 py-1 text-xs font-semibold text-surface-500 uppercase tracking-wider flex items-center justify-between">
      <span>Folders</span>
      {#if refreshing}
        <span class="text-[10px] font-normal normal-case tracking-normal text-surface-500">Refreshing…</span>
      {/if}
    </p>

    {#if loading}
      <p class="px-3 py-2 text-xs text-surface-500">Loading folders…</p>
    {:else if error}
      <p class="px-3 py-2 text-xs text-red-500">{error}</p>
    {:else if folders.length === 0}
      <p class="px-3 py-2 text-xs text-surface-500">No folders.</p>
    {:else}
      {#each folders as folder (folder.name)}
        <button
          class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors
            {selectedFolder === folder.name
              ? 'bg-primary-500/10 text-primary-500 font-medium'
              : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
          onclick={() => onselectfolder(folder.name)}
        >
          <span>{folderIcon(folder)}</span>
          <span class="flex-1 text-left truncate">{displayName(folder)}</span>
          {#if folder.unread_count && folder.unread_count > 0}
            <span class="badge preset-filled-primary-500 text-xs">{folder.unread_count}</span>
          {/if}
        </button>
      {/each}
    {/if}

    <hr class="my-3 border-surface-200 dark:border-surface-700" />

    <p class="px-2 py-1 text-xs font-semibold text-surface-500 uppercase tracking-wider">Integrations</p>
    {#each integrations as item (item.name)}
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm hover:bg-surface-200 dark:hover:bg-surface-700 transition-colors"
      >
        <span>{item.icon}</span>
        <span class="flex-1 text-left">{item.name}</span>
      </button>
    {/each}
  </nav>

  <!-- Account / Settings -->
  <div class="p-3 border-t border-surface-200 dark:border-surface-700">
    <button
      class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm hover:bg-surface-200 dark:hover:bg-surface-700 transition-colors text-surface-500"
      onclick={onsettings}
    >
      <span>&#9881;</span>
      <span>Account Settings</span>
    </button>
  </div>
</aside>
