# Nimbus Mail

A modern, native desktop mail client written in Rust — built to stand out through
deep **Nextcloud integration**. Nimbus combines standard email protocols (IMAP,
SMTP, JMAP) with first-class support for Nextcloud Talk, Files, Contacts, and
Calendar, so mail lives alongside the rest of your collaboration stack instead
of in a silo.

> ⚠️ **Project status:** early development. The IMAP fetch path and account
> setup flow work end-to-end, but the app is not yet feature-complete or
> suitable as a daily driver. See [Roadmap](#roadmap) below.

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
  (Credential Manager / Keychain / Secret Service), no plaintext secrets
  on disk.

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

### Install and run

```bash
# Install frontend dependencies
cd ui && npm install && cd ..

# Run in development mode (starts Vite + the Tauri shell, with hot reload)
cargo tauri dev
```

On first launch you'll see the account setup wizard. Enter your IMAP / SMTP
server details and password; the password is stored in your OS keychain, not
on disk.

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
- Account setup wizard + keychain-backed credentials
- Mail list + reading pane wired to live IMAP data

**Next up**
- Local storage: cache emails and account data offline ([#4](https://github.com/Videothek/nimbus-mail/issues/4))
- Tauri command surface: round out the Rust ↔ Svelte bridge ([#5](https://github.com/Videothek/nimbus-mail/issues/5))
- UI: inbox view refinements ([#7](https://github.com/Videothek/nimbus-mail/issues/7))
- UI: compose and send ([#8](https://github.com/Videothek/nimbus-mail/issues/8))

**Later**
- Nextcloud authentication ([#9](https://github.com/Videothek/nimbus-mail/issues/9))
- CardDAV contacts ([#10](https://github.com/Videothek/nimbus-mail/issues/10)) and CalDAV calendars ([#11](https://github.com/Videothek/nimbus-mail/issues/11))
- Nextcloud Files ([#12](https://github.com/Videothek/nimbus-mail/issues/12)) and Talk ([#13](https://github.com/Videothek/nimbus-mail/issues/13)) integration
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
