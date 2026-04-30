# Mail Client Icons — Set v2

31 additional icons matching the original 18-icon set. Together: **49 icons total** in one consistent family.

## What's new

**Reply & forward** — `reply`, `reply-all`, `forward`
**Organize** — `archive`, `snooze`, `flag`, `star`
**Find** — `search`, `filter`, `sort`
**Attachments** — `attachment`, `download`, `print`
**Safety** — `spam`, `block`, `important`
**Folders** — `drafts`, `sent`, `trash`
**Sync** — `refresh`, `sync`, `loading` (with built-in spin animation)
**Account** — `add-account`, `sign-out`, `lock`
**Notifications** — `notification`, `mute`, `do-not-disturb`
**Security** — `encrypted`, `signed`, `verified`

## Design system (unchanged)

- 24×24 viewBox
- 1.6px stroke width
- Round caps and joins
- `currentColor` for stroke — control via CSS `color` property
- 2px corner radius on rectangular elements

## Installation

If you haven't installed the first set yet, drop both `svelte/icons/*.svelte` files into `src/lib/icons/` and replace `src/lib/Icon.svelte` with the version in this zip — it includes all 49 icons.

If you already have set v1 installed, just copy the new components from `svelte/icons/` into your existing `src/lib/icons/` folder and replace your `Icon.svelte` with the updated one.

## Usage

```svelte
<script>
  import Icon from '$lib/Icon.svelte';
</script>

<!-- Toolbar -->
<button><Icon name="reply" /> Reply</button>
<button><Icon name="reply-all" /> Reply all</button>
<button><Icon name="forward" /> Forward</button>

<!-- Status indicators -->
<Icon name="encrypted" class="text-green-600" />
<Icon name="verified" class="text-blue-600" />

<!-- Loading spinner (animation built in) -->
{#if loading}
  <Icon name="loading" />
{/if}
```

## Notes on specific icons

**Loading** ships with a built-in CSS `@keyframes spin` animation — drop it in and it spins on its own. If you want a static version, use `refresh` or `sync` instead.

**Star** is filled-style (single closed path) so it renders cleanly at small sizes. Toggle it with two states by swapping `fill="none"` to `fill="currentColor"` in your wrapper, or use a parent class.

**Important** is intentionally minimal — a thin vertical line and dot, like an exclamation mark without the circle. Pairs well in red for urgent flags.

**Encrypted** is a shield with a padlock inside; **Verified** is a shield with a checkmark; **Signed** is a pen-on-paper mark. All three can sit side-by-side as message metadata badges.

**Mute** is the notification bell with a diagonal slash. **Do not disturb** is a circle with a horizontal bar (the universal "no" symbol) — distinct enough not to be confused with the mute icon at small sizes.

## Adding even more icons

Same recipe as before:
1. 24×24 viewBox
2. 1.6 stroke width
3. `stroke-linecap="round" stroke-linejoin="round"`
4. `stroke="currentColor"` and `fill="none"`
5. Use `fill="currentColor" stroke="none"` only for tiny indicator dots
