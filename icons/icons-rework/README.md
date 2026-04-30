# Icon Rework — 6 replacements

Drop-in replacements for 6 icons that needed clearer rendering. **Same names, same API** — just better paths.

## What changed

### `attachment` (set v2)
**Before:** Two overlapping path segments that looked tangled at small sizes.
**After:** Single fluid path tracing a paperclip curl. Reads cleanly at 16px.

### `numbered-list` (set v4)
**Before:** The "2" character was constructed in a way that didn't read as a 2.
**After:** Properly drawn "1" with serif foot and "2" with curl + base — both legible at 16px.

### `quote` (set v4)
**Before:** Tangled mix of corner blocks and lines that didn't clearly read as quotation marks.
**After:** Standard blockquote convention — two opening quote marks (`❝ ❝`) on the left, vertical indent bar with three text lines on the right. Universally recognizable.

### `delete-row` (set v4)
**Before:** Heavy strikethrough bar floated between rows in an ambiguous position.
**After:** Bar (2.4 stroke) bisects the **middle row at its center**, with break gaps in the cell dividers above and below — clearly shows "this row is being removed."

### `delete-column` (set v4)
**Before:** Same problem as delete-row.
**After:** Bar bisects the **middle column at its center**, with gaps in the cell dividers on each side.

### `text-color` (set v4)
**Before:** Cramped "A" with a too-narrow color bar.
**After:** Properly proportioned A taking advantage of the full canvas height, with a wider color bar at the bottom that's easier to spot.

> Note: I asked about "cell color" in your message but interpreted it as `text-color` (the only color-applying icon in the set). If you actually want a separate `cell-color` icon (background fill for table cells), let me know — I'd draw it as a small filled rect with the color bar underneath.

## Installation

Replace these 6 files in your project:

```
src/lib/icons/Attachment.svelte
src/lib/icons/NumberedList.svelte
src/lib/icons/Quote.svelte
src/lib/icons/DeleteRow.svelte
src/lib/icons/DeleteColumn.svelte
src/lib/icons/TextColor.svelte
```

And the corresponding raw SVGs if you have them:

```
svg/attachment.svg
svg/numbered-list.svg
svg/quote.svg
svg/delete-row.svg
svg/delete-column.svg
svg/text-color.svg
```

**No changes needed to `Icon.svelte`** — the names and exports are identical.
