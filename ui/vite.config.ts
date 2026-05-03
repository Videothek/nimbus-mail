import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { paraglideVitePlugin } from '@inlang/paraglide-js'

// Paraglide compiles messages from `messages/{locale}.json` into
// `src/paraglide/` on every dev tick + at build time.  See #190.
//   * `outdir` is the generated module the rest of the app imports
//     via `import * as m from './paraglide/messages'`.
//   * `strategy` decides where the active locale comes from at
//     runtime — `cookie` would default to a cookie, `url-pattern`
//     to a path prefix.  We're a desktop app with no URLs, so
//     `localStorage` (persisted) + a `baseLocale` fallback is
//     simplest; the runtime exposes `setLocale()` which the
//     Settings UI calls when the user picks German / English.
export default defineConfig({
  plugins: [
    paraglideVitePlugin({
      project: './project.inlang',
      outdir: './src/paraglide',
      strategy: ['localStorage', 'preferredLanguage', 'baseLocale'],
    }),
    svelte(),
    tailwindcss(),
  ],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: 'dist',
  },
})
