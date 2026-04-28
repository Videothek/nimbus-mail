# Nimbus Mail

A modern, native desktop mail client written in Rust — built to stand out through
deep **Nextcloud integration**. Nimbus combines standard email protocols (IMAP,
SMTP, JMAP) with first-class support for Nextcloud Talk, Files, Contacts, and
Calendar, so mail lives alongside the rest of your collaboration stack instead
of in a silo.

> ⚠️ **Project status:** early development. Mail (IMAP / SMTP) works
> end-to-end with an encrypted local cache, and Nextcloud server
> authentication is in place — but the app is not yet feature-complete
> or suitable as a daily driver. See [Roadmap](#roadmap) below.

## Key differentiators

- **Nextcloud Talk integration** — create and join Talk rooms directly from the
  mail client (similar to how Teams plugs into Outlook).
- **Nextcloud Files integration** — attach, share, and browse files from
  Nextcloud without leaving the app.
- **Contact & Calendar sync** — full CardDAV / CalDAV sync with Nextcloud
  (and any other compliant server).
- **Modern protocol support** — JMAP alongside the classic IMAP / SMTP stack.
- **Native performance** — a Rust core with a Tauri shell, not a packaged
  Electron app.
- **Security-first** — TLS everywhere, passwords stored in the OS keychain
  (Credential Manager / Keychain / Secret Service), and the local mail
  cache is encrypted at rest with **SQLCipher** (AES-256) using a master
  key that also lives in the keychain. No plaintext secrets on disk.

## Tech stack

| Layer | Choice |
|---|---|
| Core logic & protocols | Rust |
| Desktop shell | [Tauri 2](https://tauri.app) (Rust backend + native webview) |
| Frontend | Svelte 5 + TypeScript + Vite |
| UI components | [Skeleton UI](https://www.skeleton.dev) v3 on Tailwind (theme: cerberus) |
| Platforms | Windows, macOS, Linux |

## Project structure

```
nimbus-mail/
├── Cargo.toml              # Rust workspace root
├── crates/
│   ├── nimbus-core/        # Shared types, models, error handling
│   ├── nimbus-imap/        # IMAP mail retrieval
│   ├── nimbus-smtp/        # SMTP mail sending
│   ├── nimbus-jmap/        # JMAP modern mail access
│   ├── nimbus-caldav/      # CalDAV calendar sync
│   ├── nimbus-carddav/     # CardDAV contact sync
│   ├── nimbus-nextcloud/   # Nextcloud API (Talk, Files, OCS)
│   └── nimbus-store/       # Local storage, caching, OS keychain
├── src-tauri/              # Tauri app (Rust entry point + config)
└── ui/                     # Frontend (Svelte 5 + TypeScript + Vite)
    └── src/
        ├── lib/            # Svelte components
        ├── App.svelte      # Root component
        └── main.ts         # Entry point
```

Each protocol lives in its own crate so it can be tested and reused
independently. The Tauri layer is deliberately thin — it only exposes
commands; all logic lives in the Rust core.

## Protocols & integrations

| Protocol / API | Purpose | Crate |
|---|---|---|
| IMAP | Mail retrieval | `nimbus-imap` |
| SMTP | Mail sending | `nimbus-smtp` |
| JMAP | Modern mail access (where supported) | `nimbus-jmap` |
| CalDAV | Calendar sync | `nimbus-caldav` |
| CardDAV | Contact sync | `nimbus-carddav` |
| Nextcloud OCS / API | Talk, Files, app integrations | `nimbus-nextcloud` |

## Getting started

### Prerequisites

- **Rust** (stable, edition 2024) — install via [rustup](https://rustup.rs)
- **Node.js** 20+ and npm
- **Tauri system dependencies** — see the
  [Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/)
  for your OS (WebView2 on Windows, WebKitGTK on Linux, etc.)
- **`cargo tauri` CLI** — `cargo install tauri-cli`
- **Windows only: Strawberry Perl** — the encrypted cache pulls in
  SQLCipher with a vendored OpenSSL, and OpenSSL's build scripts need
  a full Perl install that Git Bash's bundled Perl can't provide:
  ```powershell
  winget install StrawberryPerl.StrawberryPerl
  ```
  Then make sure Strawberry Perl is on `PATH` before Git's Perl (in
  Git Bash: `export PATH="/c/Strawberry/perl/bin:/c/Strawberry/c/bin:$PATH"`).
  **End users don't need Perl or OpenSSL** — both are static-linked
  into the shipped binary.

### Install and run

```bash
# Install frontend dependencies
cd ui && npm install && cd ..

# Run in development mode (starts Vite + the Tauri shell, with hot reload)
cargo tauri dev
```

On first launch you'll see the account setup wizard. Enter your IMAP / SMTP
server details and password; the password is stored in your OS keychain, not
on disk. Nextcloud servers are connected separately from the Settings
screen via a browser-based login — see [Nextcloud](#nextcloud) below.

### Build a release

```bash
cargo tauri build
```

The installer / bundle for your platform ends up in `src-tauri/target/release/bundle/`.

### Tests & lint

```bash
cargo test --workspace     # Run all Rust tests
cargo clippy --workspace   # Lint
cd ui && npm run check     # Svelte / TypeScript type-check
```

## Nextcloud

Nextcloud connections are managed independently of mail accounts — one
Nextcloud server can back any number of IMAP/SMTP identities. From the
Account Settings screen, enter your server URL and click *Connect*:

1. Nimbus opens your system browser at your Nextcloud login page.
2. You authorise Nimbus there — any IdP / SSO in front of Nextcloud
   (Keycloak, Authelia, Entra ID, …) works, because the login happens
   in the browser, not inside Nimbus.
3. Nextcloud generates a **revocable app password** and hands it back;
   Nimbus stores it in the OS keychain. You can revoke Nimbus at any
   time from Nextcloud → Personal settings → Security without changing
   your real password.

Once connected, Nimbus queries `/ocs/v2.php/cloud/capabilities` and
shows which Nextcloud apps are available (Talk, Files, Calendar,
Contacts). The feature-specific integrations that consume these
capabilities are tracked in the [Roadmap](#roadmap).

## Themes

Nimbus uses [Skeleton UI](https://www.skeleton.dev) for theming. Out of
the box you can pick any of Skeleton's 22 stock themes from
*Settings → Design*, plus a Light / Dark / Follow-OS toggle that applies
on top of any theme.

### Importing a custom theme

Skeleton themes are plain CSS — a single file declaring the colour
tokens under a `[data-theme="<slug>"]` selector. Nimbus can load any
file that follows that shape, so community themes and the output of
Skeleton's [Theme Generator](https://themes.skeleton.dev) both work.

1. **Get a theme file.** Either:
   - Open the **[Skeleton Theme Generator](https://themes.skeleton.dev)**
     in your browser, tweak the palette and properties, then click
     *Export* → *CSS*. Save the file (e.g. `aurora.css`).
   - Or download a community theme as a `.css` file from anywhere
     you trust.
2. **Open Nimbus → Settings → Design.**
3. Click **`+ Import theme…`** in the *Theme* section.
4. Pick the `.css` file in the native file dialog.
5. The new theme appears in the picker grid with a small **`custom`**
   tag. Click it to switch to it — the change applies live, no
   restart.
6. To remove a custom theme, click the small **`×`** in the top-right
   corner of its picker tile. The CSS file is deleted from
   `<config>/nimbus-mail/themes/`. If the removed theme was active,
   Nimbus falls back to the default *Cerberus*.

### Anatomy of a Skeleton theme file

If you want to author one by hand, the minimum shape is:

```css
[data-theme='aurora'] {
  --color-primary-500: #6c5ce7;
  --color-secondary-500: #00b894;
  --color-tertiary-500: #fd79a8;
  --color-success-500: #2ecc71;
  --color-warning-500: #f39c12;
  --color-error-500:   #e74c3c;
  --color-surface-50:  #ffffff;
  /* …surface-100..-900, contrast tokens, type scale, … */
}
```

The slug inside `[data-theme='…']` becomes the theme's id in the
picker — a single CSS file therefore declares exactly one theme.
Skeleton's docs cover the [full token list](https://www.skeleton.dev/docs/design/themes)
including dark-mode variants. Imported themes aren't validated, so a
missing or low-contrast token can hurt readability — pick from the
generator if in doubt.

### Where the files live

| Platform | Path                                                  |
|----------|-------------------------------------------------------|
| Linux    | `~/.config/nimbus-mail/themes/`                       |
| macOS    | `~/Library/Application Support/nimbus-mail/themes/`   |
| Windows  | `%APPDATA%\nimbus-mail\themes\`                       |

You can drop CSS files there directly and re-launch Nimbus, but the
*Import* button is the supported path — it parses the slug, copies
the file, and registers the theme with the picker in one step.

## Architecture principles

- **Separation of concerns** — the Rust core library handles all
  protocol / business logic; the UI is a thin presentation layer.
- **Offline-first** — local caching so the client works without
  constant connectivity (in progress).
- **Security-first** — TLS everywhere, OS-keychain credentials,
  no plaintext secrets anywhere on disk.
- **Modular design** — each protocol is its own crate for testability
  and reuse.
- **Stay responsive** — heavy work goes on async background tasks,
  never on the UI thread.

## Roadmap

Tracked in [GitHub Issues](https://github.com/Videothek/nimbus-mail/issues).
Current state at a glance:

**Done**
- IMAP: connect, list folders ([#1](https://github.com/Videothek/nimbus-mail/issues/1))
- IMAP: fetch envelopes + full messages with MIME parsing ([#2](https://github.com/Videothek/nimbus-mail/issues/2))
- SMTP: send messages ([#3](https://github.com/Videothek/nimbus-mail/issues/3))
- Local storage: cache emails and account data offline, encrypted at
  rest via SQLCipher ([#4](https://github.com/Videothek/nimbus-mail/issues/4))
- Tauri command surface: Rust ↔ Svelte bridge ([#5](https://github.com/Videothek/nimbus-mail/issues/5))
- Account setup wizard with real IMAP probe + keychain-backed credentials ([#6](https://github.com/Videothek/nimbus-mail/issues/6))
- UI: inbox view wired to live IMAP data ([#7](https://github.com/Videothek/nimbus-mail/issues/7))
- UI: compose and send with rich-text editor ([#8](https://github.com/Videothek/nimbus-mail/issues/8))
- Nextcloud: browser-based login (Login Flow v2) + capability detection ([#9](https://github.com/Videothek/nimbus-mail/issues/9))

**Next up**
- CardDAV contacts ([#10](https://github.com/Videothek/nimbus-mail/issues/10)) and CalDAV calendars ([#11](https://github.com/Videothek/nimbus-mail/issues/11))
- Nextcloud Files ([#12](https://github.com/Videothek/nimbus-mail/issues/12)) and Talk ([#13](https://github.com/Videothek/nimbus-mail/issues/13)) integration

**Later**
- JMAP support ([#14](https://github.com/Videothek/nimbus-mail/issues/14))
- Full-text search ([#15](https://github.com/Videothek/nimbus-mail/issues/15))
- System tray + notifications ([#16](https://github.com/Videothek/nimbus-mail/issues/16))
- Theming ([#17](https://github.com/Videothek/nimbus-mail/issues/17))
- Multi-account management ([#18](https://github.com/Videothek/nimbus-mail/issues/18))

## Contributing

This is a two-person project (Nick and Jannik) in its scaffolding phase,
but issues and pull requests are welcome. If you're curious about the
internals, `CLAUDE.md` in the repo root has the working context document
used during development.

### Branching

- `main` is stable and always compiles.
- Feature work happens on personal branches (`Nick`, `Jannik`) and lands
  via pull request.
- Never push directly to `main`.

## License

GPL-3.0. See [LICENSE](LICENSE).
