<script lang="ts">
  // Reusable emoji picker — category tabs across the top, a
  // search box that filters across every category, and a grid
  // of buttons below.  The host owns positioning and the
  // current value; this component only emits picks.
  import { EMOJI_CATEGORIES, EMOJI_SEARCH_INDEX } from './emojiData'

  interface Props {
    /** Currently chosen emoji, used to highlight its tile. */
    value?: string | null
    /** Width of the popover in Tailwind units (defaults to w-80 = 20rem). */
    widthClass?: string
    /** Whether to show a "no emoji" / clear option in the grid. */
    allowClear?: boolean
    onpick: (emoji: string | null) => void
  }
  let { value = null, widthClass = 'w-80', allowClear = true, onpick }: Props = $props()

  let activeCategory = $state(EMOJI_CATEGORIES[0].id)
  let query = $state('')

  /** Visible emoji set: the active category's list, or — when
   *  the search box has anything — every emoji whose keyword
   *  blob matches.  Each query token must hit somewhere so
   *  multi-word searches like "red heart" actually narrow.
   *  Direct character match is checked first so pasting an
   *  emoji into the search field surfaces it instantly. */
  const visible = $derived.by(() => {
    const q = query.trim().toLowerCase()
    if (!q) {
      return EMOJI_CATEGORIES.find((c) => c.id === activeCategory)?.emojis ?? []
    }
    const tokens = q.split(/\s+/).filter(Boolean)
    return EMOJI_SEARCH_INDEX
      .filter((entry) => {
        if (entry.emoji.includes(q)) return true
        return tokens.every((t) => entry.search.includes(t))
      })
      .map((entry) => entry.emoji)
  })
</script>

<div class="{widthClass} bg-surface-50 dark:bg-surface-900 border border-surface-300 dark:border-surface-600 rounded-md shadow-lg flex flex-col">
  <!-- Category tab strip.  Tabs are distributed with flex-1 so
       they evenly fill the available width — at the picker's
       default w-80 (320px) the nine categories already fit in
       a single row, and at the wider w-full layout (used in the
       new-list modal) they spread out without leaving a stale
       horizontal scrollbar.  The strip falls back to overflow-x
       only on hosts that render the picker narrower than ~270px,
       which never happens in practice. -->
  <div class="flex items-center border-b border-surface-200 dark:border-surface-700 px-1 py-1 gap-0.5 overflow-x-auto">
    {#each EMOJI_CATEGORIES as cat (cat.id)}
      <button
        type="button"
        class="flex-1 min-w-0 px-1 py-1 text-base rounded-md hover:bg-surface-200 dark:hover:bg-surface-800 {activeCategory === cat.id && !query.trim() ? 'bg-primary-500/15 ring-1 ring-primary-500' : ''}"
        title={cat.label}
        aria-label={cat.label}
        onclick={() => { activeCategory = cat.id; query = '' }}
      >{cat.icon}</button>
    {/each}
  </div>

  <!-- Search field.  Cross-category, blanks the active
       category highlight while non-empty so the user
       understands they're seeing global results. -->
  <div class="px-2 pt-2">
    <input
      type="search"
      class="input w-full text-sm px-2 py-1 rounded-md"
      placeholder="Search emoji"
      bind:value={query}
    />
  </div>

  <!-- Grid.  Fixed 8-column layout, scrollable so even the
       big Symbols / Flags categories fit without resizing. -->
  <div class="p-2 grid grid-cols-8 gap-0.5 max-h-64 overflow-y-auto">
    {#if allowClear && !query.trim()}
      <button
        type="button"
        class="w-8 h-8 flex items-center justify-center text-xs rounded-md hover:bg-surface-200 dark:hover:bg-surface-800 {value === null ? 'bg-primary-500/15 ring-1 ring-primary-500' : ''}"
        title="No emoji"
        onclick={() => onpick(null)}
      >∅</button>
    {/if}
    {#each visible as e (e)}
      <button
        type="button"
        class="w-8 h-8 flex items-center justify-center text-lg rounded-md hover:bg-surface-200 dark:hover:bg-surface-800 {value === e ? 'bg-primary-500/15 ring-1 ring-primary-500' : ''}"
        title={e}
        onclick={() => onpick(e)}
      >{e}</button>
    {/each}
    {#if visible.length === 0}
      <p class="col-span-8 text-xs text-surface-500 italic px-2 py-3 text-center">
        No emoji matches "{query}".
      </p>
    {/if}
  </div>
</div>
