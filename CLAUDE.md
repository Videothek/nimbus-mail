# Nimbus Mail

## Vision

A modern, native desktop mail client built in Rust that stands out through deep **Nextcloud integration** — targeting both businesses and end users. The goal is to be more appealing and capable than existing alternatives by combining standard email protocols with modern APIs and tight collaboration features.

## Key Differentiators

- **Nextcloud Talk integration** — create and join Talk rooms directly from the mail client (similar to Teams integration in Outlook)
- **Nextcloud Files integration** — attach, share, and browse files from Nextcloud directly within the client
- **Contact & Calendar sync** — full sync with Nextcloud Contacts and Calendar
- **Modern protocol support** — JMAP and direct API calls alongside traditional protocols

## Tech Stack

- **Language:** Rust (core logic, protocol handling, backend)
- **UI Framework:** Tauri 2 (native desktop app with Rust backend + system webview for UI)
- **Frontend:** Vite + JavaScript/TypeScript (framework TBD — React, Svelte, or SolidJS)
- **Platform targets:** Windows, macOS, Linux

## Project Structure

```
nimbus-mail/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── nimbus-core/        # Shared types, models, error handling
│   ├── nimbus-imap/        # IMAP mail retrieval
│   ├── nimbus-smtp/        # SMTP mail sending
│   ├── nimbus-jmap/        # JMAP modern mail access
│   ├── nimbus-caldav/      # CalDAV calendar sync
│   ├── nimbus-carddav/     # CardDAV contact sync
│   ├── nimbus-nextcloud/   # Nextcloud API (Talk, Files, OCS)
│   └── nimbus-store/       # Local storage, caching, keychain
├── src-tauri/              # Tauri app (Rust entry point + config)
└── ui/                     # Frontend (Vite + JS/TS)
    ├── src/
    │   ├── components/     # Reusable UI components
    │   ├── views/          # Page-level views (inbox, calendar, etc.)
    │   ├── stores/         # State management
    │   └── styles/         # CSS / design tokens
    └── public/             # Static assets
```

## Protocols & Integrations

| Protocol/API | Purpose | Crate |
|---|---|---|
| IMAP | Mail retrieval | `nimbus-imap` |
| SMTP | Mail sending | `nimbus-smtp` |
| JMAP | Modern mail access (where supported) | `nimbus-jmap` |
| CalDAV | Calendar sync (Nextcloud + others) | `nimbus-caldav` |
| CardDAV | Contact sync (Nextcloud + others) | `nimbus-carddav` |
| Nextcloud OCS/API | Talk rooms, file sharing, app integrations | `nimbus-nextcloud` |

## Architecture Principles

- **Separation of concerns** — Rust core library handles all protocol/business logic; UI layer is a thin presentation layer
- **Offline-first** — local caching and sync so the client works without constant connectivity
- **Security-first** — TLS everywhere, credential storage via OS keychain, no plaintext secrets
- **Modular design** — each protocol as its own crate for testability and reuse

## Development Guidelines

- Write clear, well-documented code — the team is learning as they build
- Prefer existing, well-maintained Rust crates over reimplementing protocols from scratch
- Write tests for protocol handling and data sync logic
- Use `clippy` and `rustfmt` on all Rust code
- Commit messages should explain *why*, not just *what*
- Keep the UI responsive — heavy work belongs in async background tasks, never on the UI thread

## Build & Run

```bash
# Install frontend dependencies
cd ui && npm install

# Run in development mode (starts both Vite dev server and Tauri)
cargo tauri dev

# Build for production
cargo tauri build

# Run Rust tests
cargo test --workspace

# Lint Rust code
cargo clippy --workspace
```

## Project Status

**Phase: Initial scaffolding**
- Project structure created
- Workspace with modular crates set up
- Tauri 2 + Vite frontend scaffolding in place
- Next: choose frontend framework, implement first protocol (IMAP), build basic UI shell

## Team Context

- The team is new to building a project of this scale
- AI assistance (Claude) is a core part of the development workflow for code generation, explanation, and architectural guidance
- Expect frequent questions about Rust idioms, protocol details, and design patterns — answer thoroughly with explanations
