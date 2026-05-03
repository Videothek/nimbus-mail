/**
 * UI-scale plumbing (#191).
 *
 * Applies a global zoom factor to the running webview so the
 * whole UI scales uniformly — text, icons, SVGs, and pixel-
 * pinned widths all included.  We use CSS `zoom` rather than
 * a `transform: scale()` wrapper because zoom keeps the layout
 * box model honest (hit-testing, scrollbars, intersection
 * observers all behave correctly), while a transform would
 * half-bake everything.  `zoom` is a non-standard CSS property
 * but is universally supported by every webview engine we
 * ship to (WebView2 on Windows, WKWebView on macOS,
 * WebKitGTK on Linux).
 *
 * Source of truth flow:
 *   1. AppSettings carries `ui_scale` + `ui_scale_auto`.
 *   2. Settings come back from the backend on launch + after
 *      every save.
 *   3. `effectiveScale(...)` decides what to actually apply:
 *      - if `ui_scale_auto` is true, derive from screen width;
 *      - otherwise use `ui_scale` clamped to the safe range.
 *   4. `applyUiScale(scale)` writes `document.documentElement
 *      .style.zoom`.
 *   5. Any user gesture that picks an explicit scale (manual
 *      slider, Ctrl+wheel) flips `ui_scale_auto` off so the
 *      auto-derivation stops fighting the user's choice on
 *      the next launch.
 */

/** Minimum + maximum scale we'll ever apply.  Below ~0.7 the
 *  10-px font sizes round down to single-pixel artifacts; above
 *  ~1.5 the fixed-width modal dialogs start spilling off the
 *  edge of a 1280×720 screen.  Adjust if a user reports either
 *  symptom in practice. */
export const MIN_UI_SCALE = 0.7
export const MAX_UI_SCALE = 1.5
/** Step size used by both the settings slider and Ctrl+wheel. */
export const UI_SCALE_STEP = 0.05

/**
 * Auto-derive a scale from the current screen.  Bigger
 * monitors get a bump; small laptops stay at 1.0.  Numbers
 * tuned against a 1.5 px-density spread (1080p / 1440p / 4K)
 * so the absolute UI size feels consistent.
 *
 * Bands are intentionally coarse: a few discrete steps reads
 * better than a continuous formula because users get a
 * predictable "switching monitors changes the size" mental
 * model rather than fractional drift.
 */
export function autoUiScale(): number {
  if (typeof window === 'undefined') return 1.0
  const w = window.screen.availWidth
  if (w >= 2560) return 1.25
  if (w >= 1920) return 1.10
  if (w >= 1366) return 1.0
  if (w >= 1024) return 0.95
  return 0.90
}

/** Resolve the scale to apply given the user's settings. */
export function effectiveScale(
  uiScale: number | null | undefined,
  uiScaleAuto: boolean | null | undefined,
): number {
  if (uiScaleAuto ?? true) return autoUiScale()
  return clampScale(uiScale ?? 1.0)
}

/** Clamp a candidate scale to the supported range, then snap
 *  to the nearest `UI_SCALE_STEP`.  Snapping keeps the value
 *  serialised cleanly in the JSON settings file (no `1.0500001`
 *  drift after a few wheel ticks). */
export function clampScale(s: number): number {
  if (!Number.isFinite(s)) return 1.0
  const stepped = Math.round(s / UI_SCALE_STEP) * UI_SCALE_STEP
  return Math.min(MAX_UI_SCALE, Math.max(MIN_UI_SCALE, stepped))
}

/** Write the scale onto the document root.  Idempotent — calling
 *  with the same value twice is cheap.  We set `zoom` rather
 *  than `font-size` because zoom propagates to icons / SVGs /
 *  arbitrary-pixel widths the way the user expects. */
export function applyUiScale(scale: number): void {
  if (typeof document === 'undefined') return
  // Use `String(scale)` rather than `${scale}rem`-style: zoom
  // takes a unitless number (1.0 = 100%).
  document.documentElement.style.zoom = String(scale)
}
