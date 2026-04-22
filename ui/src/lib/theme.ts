/**
 * Theme application helper.
 *
 * The Rust `AppSettings` struct stores two preferences:
 *   - `theme_name` — which Skeleton theme to use (cerberus, pine, …)
 *   - `theme_mode` — `"system" | "light" | "dark"`
 *
 * This module turns those into the `data-theme` and `data-mode`
 * attributes on `<html>` that Skeleton's CSS variables and the
 * Tailwind `dark:` variant (overridden in `app.css`) react to.
 *
 * `system` mode means "follow the OS". We don't hand that string to
 * the CSS — instead this module reads `prefers-color-scheme` and sets
 * `data-mode="light"` or `"dark"` itself, then keeps doing so as the
 * OS preference changes. That way the CSS only ever has to look at a
 * concrete `light` / `dark` value and never branches on "system".
 */

export type ThemeMode = 'system' | 'light' | 'dark'

export interface ThemeOption {
  /** Skeleton theme slug — matches the file name under
      `@skeletonlabs/skeleton/themes/<slug>.css`. */
  id: string
  /** Human-readable label for the picker. */
  label: string
  /** One-line description shown next to the label so the user knows
      what they're picking without trying it first. */
  description: string
}

/**
 * Themes the picker exposes. Adding more is two steps:
 *   1. Add an `@import` for the theme in `app.css`.
 *   2. Add an entry here.
 *
 * Skeleton ships ~22 themes total — we curate to keep the picker
 * scannable and the CSS bundle reasonable.
 */
export const THEMES: ThemeOption[] = [
  { id: 'cerberus', label: 'Cerberus', description: 'Bold default with warm accents' },
  { id: 'modern', label: 'Modern', description: 'Clean, neutral, business-like' },
  { id: 'pine', label: 'Pine', description: 'Calm forest greens' },
  { id: 'rose', label: 'Rose', description: 'Soft pink, warm and friendly' },
  { id: 'vintage', label: 'Vintage', description: 'Warm sepia, retro feel' },
]

/** Fallback if a saved `theme_name` no longer exists in `THEMES`
    (e.g. user downgraded to a build that dropped the theme). */
export const DEFAULT_THEME_ID = 'cerberus'

const DARK_MEDIA = '(prefers-color-scheme: dark)'

/**
 * Apply a theme + mode to the document. Idempotent — safe to call
 * on every settings change without diffing.
 *
 * For `system` mode this resolves the OS preference once and writes
 * a concrete `light`/`dark` value. The matchMedia listener installed
 * by `installSystemModeListener` takes care of the live updates as
 * the OS preference flips later.
 */
export function applyTheme(name: string, mode: ThemeMode): void {
  const themeId = THEMES.some((t) => t.id === name) ? name : DEFAULT_THEME_ID
  document.documentElement.dataset.theme = themeId

  const concreteMode: 'light' | 'dark' =
    mode === 'system' ? (prefersDark() ? 'dark' : 'light') : mode
  document.documentElement.dataset.mode = concreteMode
}

/**
 * Subscribe to OS theme changes so `system` mode follows them live.
 * Returns an unsubscribe function. Caller owns the lifecycle — the
 * App-level `$effect` in `App.svelte` re-installs whenever the user
 * switches mode (tearing down the old listener for `light`/`dark`).
 *
 * For non-system modes this is a no-op subscription that just hands
 * back a no-op cleanup, so callers don't have to branch.
 */
export function installSystemModeListener(
  mode: ThemeMode,
  themeName: string,
): () => void {
  if (mode !== 'system' || !window.matchMedia) return () => {}

  const mql = window.matchMedia(DARK_MEDIA)
  const handler = () => applyTheme(themeName, 'system')
  mql.addEventListener('change', handler)
  return () => mql.removeEventListener('change', handler)
}

function prefersDark(): boolean {
  return !!window.matchMedia && window.matchMedia(DARK_MEDIA).matches
}
