<script lang="ts">
  /**
   * SearchResults — replaces MailList when a search is active.
   *
   * Runs `search_emails` against the local FTS5 index, then offers
   * a "Search server too" button for IMAP SEARCH fallback when the
   * user suspects the mail they want isn't cached (e.g. old archive
   * messages they've never opened on this machine).
   *
   * Hit rows look like mail-list rows plus a highlighted snippet
   * rendered from `<mark>` the backend inserted around matches.
   */

  import { invoke } from '@tauri-apps/api/core'
  import type { SearchScope, SearchFilters } from './SearchBar.svelte'
  import { formatError } from './errors'

  interface SearchHit {
    accountId: string
    folder: string
    uid: number
    from: string
    subject: string
    date: string
    isRead: boolean
    isStarred: boolean
    hasAttachments: boolean
    snippet: string
  }

  interface Props {
    accountId: string
    /** Fallback folder for server-side search when scope is a single folder. */
    currentFolder: string
    query: string
    scope: SearchScope
    filters: SearchFilters
    selectedUid: number | null
    onselect: (uid: number, folder: string) => void
  }
  let {
    accountId,
    currentFolder,
    query,
    scope,
    filters,
    selectedUid,
    onselect,
  }: Props = $props()

  let hits = $state<SearchHit[]>([])
  let loading = $state(false)
  let error = $state('')
  let serverSearching = $state(false)
  let serverSearched = $state(false)

  // Re-run whenever the query / scope / filters change. We also
  // reset the "server search already done" flag so the button is
  // offered again for the new query.
  $effect(() => {
    // Touch the reactive inputs so Svelte re-runs this effect.
    query
    scope
    filters
    serverSearched = false
    void runLocal()
  })

  async function runLocal() {
    loading = true
    error = ''
    try {
      const result = await invoke<SearchHit[]>('search_emails', {
        query,
        scope,
        filters,
      })
      hits = result
    } catch (e: any) {
      error = formatError(e) || 'Search failed'
      hits = []
    } finally {
      loading = false
    }
  }

  async function runServer() {
    if (!query.trim()) return
    serverSearching = true
    try {
      const folder = scope.folder ?? currentFolder
      const serverHits = await invoke<
        Array<{
          uid: number
          folder: string
          from: string
          subject: string
          date: string
          is_read: boolean
          is_starred: boolean
        }>
      >('search_imap_server', {
        accountId,
        folder,
        query,
        limit: 100,
      })
      // Merge into the result set, dedupe on (folder, uid), keeping
      // the local hit if both sources returned it (local has the
      // snippet while server results don't).
      const seen = new Set(hits.map((h) => `${h.folder}:${h.uid}`))
      const merged = [...hits]
      for (const s of serverHits) {
        const key = `${s.folder}:${s.uid}`
        if (seen.has(key)) continue
        merged.push({
          accountId,
          folder: s.folder,
          uid: s.uid,
          from: s.from,
          subject: s.subject,
          date: s.date,
          isRead: s.is_read,
          isStarred: s.is_starred,
          hasAttachments: false,
          snippet: '',
        })
      }
      merged.sort(
        (a, b) => new Date(b.date).getTime() - new Date(a.date).getTime(),
      )
      hits = merged
      serverSearched = true
    } catch (e: any) {
      error = formatError(e) || 'Server search failed'
    } finally {
      serverSearching = false
    }
  }

  function formatDate(iso: string): string {
    const d = new Date(iso)
    const now = new Date()
    const sameDay = d.toDateString() === now.toDateString()
    if (sameDay) {
      return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    }
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
  }

  /** The backend already wraps matches in `<mark>…</mark>`. We emit
   *  the snippet as trusted HTML so the highlighting renders. The
   *  snippet never comes from user input directly — it's extracted
   *  by FTS5 from a value our own code already stored. No XSS risk
   *  as long as the upstream body/subject is treated safely, but
   *  we still belt-and-suspenders: strip `<` in non-mark positions
   *  by only allowing our known tags through. */
  function safeSnippet(raw: string): string {
    if (!raw) return ''
    // Escape everything, then unescape the marks we emit ourselves.
    const escaped = raw
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
    return escaped
      .replace(/&lt;mark&gt;/g, '<mark class="bg-yellow-200 dark:bg-yellow-700/50 rounded-sm px-0.5">')
      .replace(/&lt;\/mark&gt;/g, '</mark>')
  }
</script>

<div class="flex-1 flex flex-col min-w-0">
  <!-- Result count + server fallback button -->
  <div class="px-3 py-1.5 text-xs text-surface-600 dark:text-surface-300 flex items-center justify-between border-b border-surface-100 dark:border-surface-800">
    <span>
      {#if loading}
        Searching…
      {:else}
        {hits.length} result{hits.length === 1 ? '' : 's'}
      {/if}
    </span>
    {#if query.trim() && !serverSearched}
      <button
        type="button"
        class="text-primary-600 dark:text-primary-300 hover:underline disabled:opacity-50"
        disabled={serverSearching}
        onclick={runServer}
        title="Also search messages on the server (slower)"
      >
        {serverSearching ? 'Searching server…' : 'Search server too'}
      </button>
    {/if}
  </div>

  <div class="flex-1 overflow-y-auto">
    {#if error}
      <div class="p-4 text-sm text-red-500">{error}</div>
    {:else if !loading && hits.length === 0}
      <div class="p-6 text-center text-sm text-surface-500">
        No messages match.
      </div>
    {:else}
      {#each hits as hit (`${hit.folder}:${hit.uid}`)}
        <button
          class="w-full text-left px-4 py-3 border-b border-surface-100 dark:border-surface-800 transition-colors
            {selectedUid === hit.uid
            ? 'bg-primary-500/10'
            : 'hover:bg-surface-100 dark:hover:bg-surface-800'}"
          onclick={() => onselect(hit.uid, hit.folder)}
        >
          <div class="flex items-center justify-between mb-1">
            <span class="text-sm {!hit.isRead ? 'font-semibold' : 'font-normal'} truncate pr-2">
              {hit.from || '(unknown sender)'}
            </span>
            <span class="text-xs text-surface-500 shrink-0">{formatDate(hit.date)}</span>
          </div>
          <p class="text-sm {!hit.isRead ? 'font-medium' : ''} truncate">
            {hit.subject || '(no subject)'}
          </p>
          {#if hit.snippet}
            <!-- Trusted: see safeSnippet() comment. -->
            <p class="text-xs text-surface-500 mt-0.5 truncate">
              <!-- eslint-disable-next-line -->
              {@html safeSnippet(hit.snippet)}
            </p>
          {/if}
          <div class="flex items-center gap-1 mt-0.5">
            {#if hit.folder !== currentFolder}
              <span class="text-[10px] px-1.5 rounded bg-surface-200 dark:bg-surface-700 text-surface-600 dark:text-surface-300">
                {hit.folder}
              </span>
            {/if}
            {#if hit.hasAttachments}
              <span class="text-[10px]" title="Has attachment">&#x1F4CE;</span>
            {/if}
          </div>
        </button>
      {/each}
    {/if}
  </div>
</div>
