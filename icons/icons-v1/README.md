# Mail Client Icons

A cohesive set of 18 modern line icons designed for a Svelte/Tauri mail client.

## Design system

- **24×24 viewBox** — pixel-perfect at 16px, 20px, 24px, 32px
- **1.6px stroke width** — slightly heavier than 1.5 for better visibility on Hi-DPI screens
- **Round caps & joins** — softer, modern feel
- **`currentColor`** — icons inherit text color, perfect for theming
- **No fills** (except indicator dots) — clean line aesthetic

## Folder structure

```
src/lib/
  Icon.svelte                    — generic <Icon name="..." /> component
  icons/
    Compose.svelte
    Contacts.svelte
    Calendar.svelte
    Meetings.svelte
    GlobalInbox.svelte
    Files.svelte
    Notes.svelte
    Tasks.svelte
    ShareLinks.svelte
    Settings.svelte
    Read.svelte
    Unread.svelte
    MoveToFolder.svelte
    Delete.svelte
    RsvpAccept.svelte
    RsvpTentative.svelte
    RsvpDecline.svelte
    RsvpCanceled.svelte
```

## Usage

### Option 1: Generic component (recommended)

```svelte
<script>
  import Icon from '$lib/Icon.svelte';
</script>

<button class="toolbar-btn">
  <Icon name="compose" size={20} />
  Compose
</button>

<Icon name="rsvp-accept" size={24} class="text-green-500" />
```

### Option 2: Direct import (better tree-shaking)

```svelte
<script>
  import Compose from '$lib/icons/Compose.svelte';
  import Delete from '$lib/icons/Delete.svelte';
</script>

<Compose size={20} />
<Delete size={20} class="text-red-500" />
```

## Theming

Because every icon uses `stroke="currentColor"`, you control color via standard CSS:

```css
.sidebar-icon {
  color: var(--text-secondary);
}
.sidebar-icon:hover {
  color: var(--text-primary);
}

/* Dark mode just works */
[data-theme="dark"] .sidebar-icon {
  color: rgba(255, 255, 255, 0.7);
}
```

## RSVP color suggestions

The RSVP icons are intentionally monochrome so you can color them via context:

| State | Suggested color |
|-------|----------------|
| Accept | `#16a34a` (green) |
| Tentative | `#d97706` (amber) |
| Decline | `#dc2626` (red) |
| Canceled | `#6b7280` (gray) |

## Tauri notes

These icons are pure SVG inlined into Svelte components — no runtime asset loading, so they work identically in `tauri dev` and the bundled production app. No need to register anything in `tauri.conf.json`.

If you'd prefer to ship raw `.svg` files (e.g., for the Tauri tray icon or window icon), the `svg/` folder contains all icons as standalone files.

## Adding more icons later

To stay consistent with this set:

1. Use a 24×24 viewBox
2. Stroke width of 1.6
3. `stroke-linecap="round"` and `stroke-linejoin="round"`
4. `stroke="currentColor"` and `fill="none"`
5. Keep visual weight similar — no overly thin or thick paths
6. Sentence-case the icon name in the component file (e.g., `Archive.svelte`)
