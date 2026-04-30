# Mail Client Icons — Set v4

29 additional icons. Combined with v1+v2+v3, you now have **99 icons** in one consistent family.

## What's new

**Common UI** — `email-envelope`, `design-palette`, `more` (three dots), `more-info`, `full-screen`, `emoji`, `open-in-browser`

**Tables** — `table`, `insert-row-above`, `insert-row-below`, `delete-row`, `insert-column-left`, `insert-column-right`, `delete-column`

**Editor** — `clear`, `insert-image`, `bullet-list`, `numbered-list`, `align-left`, `align-center`, `align-right`, `justify`, `quote`, `text-color`, `highlight-text`

**History** — `undo`, `redo`

**Security** — `passphrase`, `security-key`

## Naming notes

A few of your requested names were renamed for consistency with the existing set conventions:

| You asked for | Final name | Why |
|---|---|---|
| three dots (options menu) | `more` | Common UI convention. Use this for `…` overflow menus. |
| open full screen (pop out) | `full-screen` | |
| emoji picker | `emoji` | The icon is the same whether picker or single-use. |
| open in office web | `open-in-browser` | I cannot reproduce Microsoft Office logos (copyrighted). The icon shows a globe + arrow, which is the universal "open in web/external app" gesture. |
| insert column (above/below) | `insert-column-left` / `insert-column-right` | Columns extend horizontally — left/right is correct directionality. |
| Insert Image | `insert-image` | kebab-case per the design reference. |
| left-align, center-align, right-align | `align-left`, `align-center`, `align-right` | Matches editor toolbar conventions. |
| Text color | `text-color` | |
| Highlight Text | `highlight-text` | |

## Pattern reuse (per the design reference)

- **`email-envelope`** = the `unread` base without the indicator dot — slots cleanly into the envelope family
- **`open-in-browser`** = a globe + the arrow from `open-link` (composed from existing vocabulary)
- **`more-info`** = the `info` glyph + an extra underline, suggesting "details below"
- **`emoji`** uses the established circle base (matches `do-not-disturb`, `block`, `rsvp-*`)
- **`delete-row` / `delete-column`** use a `stroke-width="2.4"` cut-through bar — this is the **legitimate exception** in the design reference for the "rotating arc" / weight-emphasis case
- **Alignment icons** are four parallel lines with varying lengths — the standard editor convention
- **`text-color` / `highlight-text`** include a small filled bar at the bottom representing the applied color (intentionally `currentColor` so you can override it via CSS to show the active color)

## Special: `text-color` and `highlight-text`

These icons are the only ones in the set where you might want to **override the indicator color independently** from the icon stroke. The bar at the bottom uses `currentColor` by default (so it inherits whatever color the icon is set to), but a common pattern is:

```svelte
<style>
  .text-color-btn :global(svg) {
    color: var(--text-stroke-color, var(--color-fg));
  }
  .text-color-btn :global(svg rect) {
    /* The colored bar showing currently selected text color */
    fill: var(--current-text-color, currentColor);
  }
</style>
```

If you want a version with the bar permanently in a different color, let me know and I'll generate variants.

## Microsoft Office note

Several apps have an "Open in Office Online" action that uses the Word/Excel/PowerPoint colored logos. Those are Microsoft trademarks and protected — I generated a generic "open in browser" icon instead. If you want product-specific buttons, you'll need to source those logos from Microsoft's brand assets directly.

## Installation

Same as before — drop `svelte/icons/*.svelte` into `src/lib/icons/` and replace `Icon.svelte` with the updated version that registers all 99 icons.
