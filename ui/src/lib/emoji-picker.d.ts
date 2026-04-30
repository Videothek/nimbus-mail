/**
 * Type-only registration of the `<emoji-picker>` custom element
 * for Svelte's template type-checker.  The runtime registration
 * happens via the side-effect import in `EmojiPicker.svelte`;
 * this file just teaches `svelte-check` that the element + its
 * `class` and `style` attributes are valid template syntax.
 *
 * The package's own `index.d.ts` exports the Picker class but
 * doesn't extend `svelteHTML.IntrinsicElements`, so we patch
 * that here.
 */

import type { HTMLAttributes } from 'svelte/elements'

declare module 'svelte/elements' {
  interface SvelteHTMLElements {
    'emoji-picker': HTMLAttributes<HTMLElement> & {
      class?: string
      style?: string
    }
  }
}

export {}
