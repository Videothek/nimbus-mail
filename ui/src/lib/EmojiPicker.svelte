<script lang="ts">
  // Reusable emoji picker — category tabs across the top, a
  // search box that filters across every category, and a grid
  // of buttons below.  The host owns positioning and the
  // current value; this component only emits picks.
  import { EMOJI_CATEGORIES, ALL_EMOJIS } from './emojiData'

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
   *  the search box has anything — every category filtered by
   *  a substring match against the emoji's CLDR-ish name.  We
   *  don't ship per-emoji names, so the filter falls back to
   *  matching the emoji *character* (useful when the user
   *  pastes one) plus a small heuristic on category names. */
  const visible = $derived.by(() => {
    const q = query.trim().toLowerCase()
    if (!q) {
      return EMOJI_CATEGORIES.find((c) => c.id === activeCategory)?.emojis ?? []
    }
    const direct = ALL_EMOJIS.filter((e) => e.includes(q))
    if (direct.length > 0) return direct
    const cat = EMOJI_CATEGORIES.find((c) =>
      c.label.toLowerCase().includes(q) || c.id.toLowerCase().includes(q),
    )
    return cat ? cat.emojis : []
  })
</script>

<div class="{widthClass} bg-surface-50 dark:bg-surface-900 border border-surface-300 dark:border-surface-600 rounded-md shadow-lg flex flex-col">
  <!-- Category tab strip.  Each tab is a single emoji; the
       active one gets a primary underline so the eye can
       jump back to it without reading the label. -->
  <div class="flex items-center border-b border-surface-200 dark:border-surface-700 px-1 py-1 overflow-x-auto">
    {#each EMOJI_CATEGORIES as cat (cat.id)}
      <button
        type="button"
        class="px-2 py-1 text-base rounded-md hover:bg-surface-200 dark:hover:bg-surface-800 {activeCategory === cat.id && !query.trim() ? 'bg-primary-500/15 ring-1 ring-primary-500' : ''}"
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
