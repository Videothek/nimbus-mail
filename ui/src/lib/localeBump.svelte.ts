/**
 * Cross-component locale-change bump (#190 follow-up).
 *
 * Paraglide's `m.<key>()` calls aren't Svelte reactive state —
 * Svelte has no signal that the active locale flipped, so
 * components that already mounted with one locale keep showing
 * its strings until they're recreated.
 *
 * The standard pattern in Svelte 5 is a `{#key …}` block: when
 * the keyed value changes, Svelte unmounts and re-mounts the
 * subtree, which causes every `m.<key>()` inside to re-evaluate
 * with the current locale.
 *
 * This module exports a single shared `$state` counter that
 * any component can bump after calling `setLocale(...)`.
 * `App.svelte` keys its main view branch on it, so a locale
 * change re-mounts the visible app without reloading the page —
 * the user stays on whichever settings tab they were on, and
 * the explicit `set_app_settings` save can complete cleanly
 * without racing a `window.location.reload()`.
 *
 * `.svelte.ts` rather than plain `.ts` because Svelte runes
 * (`$state`, `$derived`, …) are only valid in `.svelte` /
 * `.svelte.ts` files; both extensions are processed by the
 * Svelte compiler.
 */

let bump = $state(0)

export const localeBump = {
  /** Reactive counter — bumps every time `bump()` is called.
   *  Read this inside a `{#key …}` block to subscribe a
   *  subtree to locale changes. */
  get value(): number {
    return bump
  },
  /** Increment the counter, forcing any `{#key localeBump.value}`
   *  block to remount its children. */
  bump(): void {
    bump++
  },
}
