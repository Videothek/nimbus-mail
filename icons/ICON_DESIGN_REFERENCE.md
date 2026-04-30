# Icon Design Reference — Mail Client Icon Family

> **Audience:** AI agents (Claude, GPT, etc.) tasked with generating new icons that must match an existing set.
> **Goal:** Produce icons that are visually indistinguishable in style from the established 49-icon family.
> **Project context:** Svelte/Tauri desktop mail client.

---

## 1. Hard requirements (non-negotiable)

Every icon you produce **must** satisfy all of these. If any of these is wrong, the icon does not match the family.

| Property | Required value |
|---|---|
| `viewBox` | `"0 0 24 24"` (exactly) |
| `xmlns` | `"http://www.w3.org/2000/svg"` |
| `fill` (root) | `"none"` |
| `stroke` (root) | `"currentColor"` |
| `stroke-width` (root) | `"1.6"` |
| `stroke-linecap` (root) | `"round"` |
| `stroke-linejoin` (root) | `"round"` |

These attributes go on the `<svg>` element itself, not on individual paths. Inner shapes inherit them. Do **not** repeat `stroke="currentColor"` on every path — it's redundant and inflates file size.

### Forbidden

- ❌ Hardcoded colors (`#000`, `black`, `red`, etc.) — except `fill="currentColor"` on tiny indicator dots
- ❌ `stroke-width` other than 1.6 on the root, or per-path overrides (one rare exception below)
- ❌ Different viewBox sizes (no 16×16, 20×20, 32×32 — always 24×24)
- ❌ `<defs>`, `<style>`, `<linearGradient>`, `<filter>`, `<mask>` — none of it
- ❌ `<text>` elements
- ❌ Drop shadows, blurs, opacity tricks, gradients
- ❌ Bitmap embeds, base64 data URIs
- ❌ Inline `width`/`height` on the SVG file (the Svelte wrapper handles sizing)
- ❌ Comments inside the SVG

### Rare allowed exceptions

- **`fill="currentColor" stroke="none"`** on a single small `<circle>` used as an indicator dot (e.g., the dot under the exclamation in `important`, the unread badge dot). Maximum radius: ~1.1.
- **`stroke-width="2.4"`** on a single path used to create a "rotating arc" effect — only used in the `loading` spinner. Do not introduce this elsewhere.
- **`opacity="0.25"`** on a base ring behind a foreground arc — again, only the `loading` spinner pattern.

If you find yourself wanting another exception, you're probably designing the icon wrong. Simplify it instead.

---

## 2. Visual vocabulary

### Geometry

- **Corner radius:** 2px (`rx="2"`) on most rectangles. Use larger radii (4-8) only when the shape is a pill, badge, or screen with intentionally rounded corners (e.g., the calendar body uses `rx="2"`, the lock body uses `rx="2"`).
- **Circles:** Used liberally — for status containers (RSVP set), avatars (contacts), clock faces (snooze).
- **No diagonals shorter than ~3px** — they look like noise at 16px render size. If you need a tiny detail, use a dot.
- **No paths thinner than the stroke** — at 1.6 stroke, a feature smaller than ~3px will visually merge. Don't add detail you can't see at 16px.

### Composition

- Icons sit inside the 24×24 viewBox with **~2-3px of padding on all sides**. Most actual content lives in the 3..21 range on both axes.
- The "optical center" of an icon is usually around `(12, 12)` — but symmetry is more important than mathematical centering. A bell hangs from the top, so its visual mass is below center; that's correct.
- Single-shape icons (like `flag`, `star`, `sent`, `filter`) fill the canvas more aggressively — they reach to ~3px from the edges.
- Compound icons (icon + badge, like `move-to-folder` or `unread`) keep the primary shape smaller (~14-16px tall) to make room for the secondary element without crowding.

### Strokes vs. fills

- **Default style: outline only.** The icon is a stroke drawing on a transparent background.
- **Indicator dots are filled** with `currentColor` — these are tiny accent marks, never the primary shape.
- **Never fill the primary shape.** Even icons that look "filled" in some libraries (like `star`) are drawn here as a single closed stroke path with `fill="none"`. The `star` is the only icon where this might be tempting; resist it.

### Path direction & smoothness

- Use SVG path commands fluently: `M` to move, `L` for straight lines, `C` or `Q` for curves, `A` for arcs, `Z` to close.
- Prefer **arcs (`A`) over manual Bézier curves** when drawing circular segments — they're more predictable and produce cleaner shapes.
- Round numeric values to **0.1 precision maximum** (`12.5`, not `12.473`). Two decimals are noise.
- Close paths (`Z`) only when the shape is genuinely closed. A "C" shape (like the contacts torso) is open — don't close it.

---

## 3. Conceptual rules

### Metaphor selection

When designing a new icon, ask in this order:

1. **Is there an established metaphor in major mail/desktop apps?** (Apple Mail, Outlook, Gmail, Spark, Thunderbird) — if 3+ of them use the same shape, use that shape. Don't invent.
2. **Does the existing set already use a base shape that fits?** — for example, every shield-based icon (`spam`, `encrypted`, `verified`) uses the **same** shield path. Reuse it. Consistency across related icons matters more than individual cleverness.
3. **If it's genuinely novel, prefer abstract over literal.** A "snippet" icon should be brackets, not a tiny pair of scissors cutting a tiny piece of paper.

### Composition patterns

The set uses recurring patterns. New icons should reuse them:

- **Shield + interior glyph** = security/trust icons (`spam`, `encrypted`, `verified`)
- **Circle + interior glyph** = state/status icons (`rsvp-accept`, `rsvp-decline`, `do-not-disturb`, `block`)
- **Folder + arrow/glyph** = folder action icons (`move-to-folder`, `drafts`)
- **Envelope + modifier** = mail state icons (`read`, `unread`)
- **Bell + modifier** = notification icons (`notification`, `mute`)
- **Document + lines + modifier** = content type icons (`notes`, `drafts`)

If you're designing, say, an "archived important email" icon, you'd reach for the archive box base + an exclamation overlay. Don't draw a new metaphor when you can compose existing ones.

### Naming

- **File names:** `kebab-case.svg` (e.g., `reply-all.svg`, `do-not-disturb.svg`, `add-account.svg`)
- **Svelte component names:** `PascalCase.svelte` (e.g., `ReplyAll.svelte`, `DoNotDisturb.svelte`, `AddAccount.svelte`)
- **Icon name strings (for the `<Icon name="..." />` API):** kebab-case, matches the file name without extension
- **TypeScript type:** Add the new kebab name to the `IconName` union in `Icon.svelte`
- **Map entry:** Add the new entry to the `map` object in `Icon.svelte`

Compound concepts use a separator: `reply-all` not `replyall`, `move-to-folder` not `moveToFolder`. RSVP states use the `rsvp-` prefix: `rsvp-accept`, `rsvp-decline`. Stick with these prefixes for related new icons (e.g., a future `rsvp-pending` or `rsvp-forwarded`).

---

## 4. File templates

### Raw SVG file

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
  <!-- icon paths here, indented 2 spaces -->
</svg>
```

That's it. No `<title>`, no `<desc>`, no extra attributes. Single trailing newline.

### Svelte component (standard)

```svelte
<script lang="ts">
  export let size: number | string = 20;
  let className = '';
  export { className as class };
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.6"
  stroke-linecap="round"
  stroke-linejoin="round"
  class={className}
  aria-hidden="true"
>
  <!-- icon paths here, indented 2 spaces -->
</svg>
```

`aria-hidden="true"` is correct because icons are decorative — the button or link surrounding them carries the accessible name. If an icon is ever used standalone (no surrounding text label), the consumer sets `aria-label` on the wrapper.

### Svelte component (animated — only for spinner-style icons)

```svelte
<script lang="ts">
  export let size: number | string = 20;
  let className = '';
  export { className as class };
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.6"
  stroke-linecap="round"
  stroke-linejoin="round"
  class="loading {className}"
  aria-hidden="true"
>
  <!-- icon paths here -->
</svg>

<style>
  .loading {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
```

Reserve animations for icons that **inherently** spin/pulse (loading, syncing, recording). Don't animate static concepts.

---

## 5. Design process for a new icon

Follow this sequence. Do not skip steps.

### Step 1 — Define the concept in one sentence

"This icon represents [verb/noun] in the context of a mail client toolbar."

If you can't write that sentence, you don't know what you're drawing yet. Stop and ask the user.

### Step 2 — Choose the base metaphor

Check existing apps. Check the existing set. Choose a shape vocabulary (envelope, shield, circle+glyph, folder+glyph, etc.). Write down which existing icons share the base — your new icon must look like a sibling of those.

### Step 3 — Sketch in coordinates

Draft the path mentally on a 24×24 grid:

- Where does the bounding box of the main shape land? (Usually 3..21 on both axes)
- Are there secondary elements (badges, indicators)? Where do they attach?
- What's the optical weight? Is it bottom-heavy (envelope), top-heavy (bell), or centered (circle)?

### Step 4 — Write the SVG

Use the template. Keep it minimal — the average icon in this set is **2-4 path elements**. The longest is `settings` (gear, one path). The shortest is `filter` (one path). If you find yourself writing 8+ paths, you're over-detailing. Cut.

### Step 5 — Validate against the checklist

Before submitting:

- [ ] viewBox is exactly `"0 0 24 24"`
- [ ] All five stroke attributes on root SVG
- [ ] No hardcoded colors except `currentColor`
- [ ] No `stroke-width` overrides except in the explicit exceptions above
- [ ] Path coordinates rounded to 0.1 max
- [ ] Content stays within ~2-3px padding from each edge
- [ ] Visual weight matches sibling icons in the family
- [ ] No more than ~4-5 path elements (rough ceiling, not a hard rule)
- [ ] Renders cleanly at 16px (mental check: would the smallest detail merge into noise?)

### Step 6 — Generate both files

For every new icon, produce:

1. `svg/{kebab-name}.svg` — raw SVG using the standard template
2. `svelte/icons/{PascalName}.svelte` — Svelte component using the standard template
3. Update `svelte/Icon.svelte` — add to the import list, the `IconName` union, and the `map` object

### Step 7 — Document briefly

When delivering new icons to the user, include:

- A preview rendering all new icons together (use a tool like the `visualize:show_widget` SVG renderer)
- A one-line metaphor explanation per icon ("Archive: open box with a horizontal line, suggesting items being slotted in")
- Any noteworthy choices or alternatives the user might want instead

---

## 6. Common pitfalls (and how to avoid them)

**Pitfall:** Using a thinner stroke (1, 1.25, 1.5) because "it looks more refined."
**Fix:** No. The family is 1.6. A single mismatched icon will visually pop out against the others. If 1.6 looks too heavy in a specific context, it's the surrounding UI's color/spacing that needs adjusting, not the stroke.

**Pitfall:** Adding decorative details to make an icon "more interesting" — extra dots, sparkles, motion lines.
**Fix:** Mail-client icons live in dense toolbars at 16-20px. Detail you add is noise the user has to filter. Less is correct.

**Pitfall:** Using a different base shape than the established family for the same concept (e.g., drawing a new shield outline instead of reusing the existing one).
**Fix:** Copy the exact path from the existing icon. Shield in `spam` = shield in `encrypted` = shield in `verified`. Same coordinates. The interior glyph is what changes.

**Pitfall:** Drawing literal objects when an abstract symbol would read better at 16px.
**Fix:** A "PDF attachment" icon is not a tiny PDF document with the letters "PDF" — it's an attachment icon plus context, or a paper icon with a small badge. At 16px, text inside an icon is unreadable.

**Pitfall:** Forgetting to update `Icon.svelte` after adding a new component.
**Fix:** The Svelte component file alone is dead code unless registered. Always update the three places: import, type union, map object.

**Pitfall:** Inconsistent file/component naming.
**Fix:** `reply-all.svg` ↔ `ReplyAll.svelte` ↔ `'reply-all'` (the name string). All three must be derivable from each other by mechanical transformation.

**Pitfall:** Using emoji or Unicode symbols inside SVGs as a shortcut.
**Fix:** Never. All glyphs must be drawn as paths. Emoji rendering varies by platform and breaks `currentColor` theming.

**Pitfall:** Padding the icon by shrinking content instead of using the natural canvas.
**Fix:** The viewBox is 24×24, but the optical content area is ~3..21. Don't draw at 6..18 "to leave more room." It will look smaller than the other icons in the set.

---

## 7. Reference: the established palette of patterns

When in doubt, study these existing icons in the set. They define the visual language:

| Pattern | Reference icons | When to reuse |
|---|---|---|
| Plain envelope | `unread` | Any "incoming mail" concept |
| Open envelope | `read` | Any "viewed/processed mail" concept |
| Tray + dots | `global-inbox` | Aggregated/multi-source concepts |
| Folder | `files`, `move-to-folder`, `drafts` | Anything storage-related |
| Document + lines | `notes`, `drafts` | Content/text concepts |
| Circle + glyph | `rsvp-*`, `do-not-disturb`, `block` | State/status indicators |
| Shield + glyph | `spam`, `encrypted`, `verified` | Trust/security/safety |
| Bell | `notification`, `mute` | Alert concepts |
| Gear | `settings` | Configuration only — don't overload |
| Arrow + curve | `reply`, `forward` | Directional actions |
| Padlock | `lock`, `encrypted` (interior) | Secured states |
| Pen + line | `compose`, `signed` | Authoring/marking |
| Trash can | `delete`, `trash` | Destruction |
| Paper plane | `sent` | Transmission |

Reusing a base from this table is **always preferable** to inventing a new one.

---

## 8. When the user asks for "more icons in the same style"

Default workflow:

1. **Ask which categories they want** (use a multi-select if available). Don't guess.
2. **Render a preview first** showing all proposed icons together as a family — this is the user's chance to flag style mismatches before you commit to files.
3. **Generate both file formats** (raw SVG + Svelte component) for every icon.
4. **Update the registry** (`Icon.svelte`).
5. **Bundle as a zip** with the same folder structure as previous deliveries.
6. **Briefly note design decisions** the user might want to reconsider — alternative metaphors, color suggestions, edge cases — but don't bury the deliverable in caveats.

The user has already approved a style. Your job is to extend it faithfully, not to redesign it.
