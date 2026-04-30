# Mail Client Icons — Set v3

21 additional icons. Combined with v1 (18) + v2 (31), you now have **70 icons** in one consistent family.

## What's new

**Presence** — `online`, `offline`, `typing`, `away`
**Status messages** — `help`, `info`, `warning`, `error`, `success`
**Writing tools** — `translate`, `spellcheck`, `dictate`
**Actions** — `save-draft`, `open-link`, `open-on-desktop`, `cloud`, `today`
**Navigation** — `nav-backward`, `nav-forward`, `arrow-left`, `arrow-right`

## Naming notes

The mail-action **`forward`** (curved arrow, from set v2) and the navigation **`nav-forward`** (chevron, new) are both included with distinct names so you can use them side-by-side without ambiguity. Same for the directional pair:

| Use case | Icon |
|---|---|
| Reply to message → forward to someone else | `forward` |
| History navigation: go back / go to next view | `nav-backward` / `nav-forward` |
| Pure directional indicators (e.g., carousel, pagination) | `arrow-left` / `arrow-right` |

The chevrons (`nav-*`) have no tail; the arrows have a tail. This is deliberate — chevrons read as "navigate to," arrows read as "this direction."

## Pattern reuse (per the design reference)

These icons reuse established family patterns rather than inventing new ones:

- **Presence** + **Status messages** all use the `circle + interior glyph` pattern from `do-not-disturb`, `block`, `rsvp-*`
- **Help** uses the same question-mark glyph as `rsvp-tentative` — they're visual cousins, in different contexts
- **Today** = the existing `calendar` base + a filled day-marker dot
- **Save draft** = the `notes` base + a paper-rim header (suggests a floppy/document hybrid that reads at 16px)
- **Open on desktop** = monitor outline + downward chevron (content arriving on the desktop)
- **Cloud** is a single closed-stroke path, drawn from common cloud icon proportions
- **Dictate** is the standard microphone shape (capsule + stand) shared with most desktop apps

## Installation

If you're adding to an existing install, copy the new components from `svelte/icons/` into your `src/lib/icons/` folder, then replace your `Icon.svelte` with the updated version in this zip — it now registers all 70 icons.

If you're starting fresh, drop the entire `svelte/` folder into `src/lib/`.

## Usage examples

```svelte
<!-- Presence indicator next to a contact name -->
<Icon name="online" size={12} class="text-green-500" />

<!-- Toast/banner status -->
<Icon name="success" class="text-green-600" /> Message sent
<Icon name="warning" class="text-amber-600" /> Connection unstable
<Icon name="error" class="text-red-600" /> Failed to send

<!-- Composer toolbar -->
<button><Icon name="spellcheck" /></button>
<button><Icon name="translate" /></button>
<button><Icon name="dictate" /></button>

<!-- Sidebar nav -->
<button><Icon name="nav-backward" /></button>
<button><Icon name="nav-forward" /></button>

<!-- "Today" button in the calendar header -->
<button><Icon name="today" /> Today</button>

<!-- External link in an email body -->
<a href="...">View report <Icon name="open-link" size={14} /></a>
```

## Color suggestions for status set

Same convention as RSVP icons — these are intentionally monochrome so context drives color:

| State | Suggested color |
|---|---|
| Online / Success | `#16a34a` (green) |
| Away | `#d97706` (amber) |
| Warning | `#d97706` (amber) |
| Error | `#dc2626` (red) |
| Offline | `#6b7280` (gray) |
| Info / Help | `#2563eb` (blue) |
| Typing | inherit (usually muted text color) |
