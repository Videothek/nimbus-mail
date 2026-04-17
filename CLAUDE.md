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
- **Frontend:** Svelte 5 + TypeScript + Vite
- **UI Library:** Skeleton UI v3 (Tailwind CSS-based component library, theme: cerberus)
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
└── ui/                     # Frontend (Svelte 5 + TypeScript + Vite)
    ├── src/
    │   ├── lib/            # Svelte components
    │   ├── app.css         # Global styles (Tailwind + Skeleton)
    │   ├── App.svelte      # Root component
    │   └── main.ts         # Entry point
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

### Windows build prerequisite: Strawberry Perl

The local mail cache is encrypted at rest via **SQLCipher**, which is
built through `rusqlite`'s `bundled-sqlcipher-vendored-openssl` feature.
That feature compiles OpenSSL from source as part of `cargo build`, and
OpenSSL's build scripts require a full Perl install.

The Perl that ships with Git Bash is a stripped-down MSYS2 Perl that
fails to find its own standard modules (`Locale::Maketext::Simple`),
so we need **Strawberry Perl**:

```powershell
# One-time install (powershell)
winget install StrawberryPerl.StrawberryPerl
```

Then make sure Strawberry Perl is found *before* Git's Perl. In Git Bash:

```bash
export PATH="/c/Strawberry/perl/bin:/c/Strawberry/c/bin:$PATH"
```

Add that to your `~/.bashrc` so every shell picks it up automatically.

**End users do not need Perl or OpenSSL installed.** The `vendored-openssl`
feature statically links the compiled OpenSSL into the final `.exe`, so
the shipped binary is self-contained. Perl is a build-time tool only.

### CI

GitHub Actions should install Strawberry Perl before `cargo tauri build`:

```yaml
- name: Install Strawberry Perl (Windows)
  if: runner.os == 'Windows'
  run: choco install strawberryperl -y
```

### Commands

```bash
# Install frontend dependencies
cd ui && npm install

# Run in development mode (starts both Vite dev server and Tauri)
cargo tauri dev

# Build for production — produces a self-contained installer/exe
cargo tauri build

# Run Rust tests
cargo test --workspace

# Lint Rust code
cargo clippy --workspace
```

## Project Status

**Phase: Scaffolding complete**
- Rust workspace with modular crates set up
- Tauri 2 + Svelte 5 + Skeleton UI frontend in place
- Basic mail client UI shell (sidebar, mail list, reading pane)
- Repository: https://github.com/Videothek/nimbus-mail
- Next: implement first protocol (IMAP), connect backend to frontend via Tauri commands

## Development Workflow

The team follows a simple loop for every issue:

1. **Pick an issue** — choose an open GitHub issue to work on
2. **Ask Claude** — use Claude Code to implement, explain, or debug. Claude uses this `CLAUDE.md` as project context, so keep it up to date
3. **Understand & revise** — review Claude's output, make sure you understand the code, adjust as needed
4. **Push to GitHub** — commit and push when the work is solid

This means Claude should:
- Always explain *what* the code does and *why* it's written that way
- Not just produce code — teach the team as you go
- Keep `CLAUDE.md` updated when the project evolves (new decisions, status changes, tech stack updates)

## Git Branching Strategy

```
main (stable, always compiles)
 ├── nick   (Nick's working branch)
 └── jannik (Jannik's working branch)
```

### Rules
- **Never push directly to `main`** — always merge via Pull Request
- **When your issue is done** — open a PR from your branch to `main`, the other person reviews and merges
- **When the other person merged to `main`** — pull `main` into your branch to get their changes:
  ```bash
  git pull origin main
  ```
- **Merge early, merge small** — don't wait until an entire issue is done. If you add a shared type to `nimbus-core`, merge that to `main` first so the other branch can use it

### When to merge to main
- A new model or type is added to `nimbus-core`
- A crate compiles and has basic functionality or tests
- A UI component works (even with mock data)
- **Do NOT merge** broken code or half-finished functions

### Claude reminder obligation
**When an issue or meaningful unit of work is completed and merged to `main`, Claude MUST remind the developer:**
> "This is now merged to main. Remind the other developer (Nick/Jannik) to pull main into their branch: `git pull origin main`"

This ensures both branches stay in sync and avoids painful merge conflicts.

## Team Context

- **Nick** and **Jannik** — two-person team, new to building a project of this scale
- AI assistance (Claude) is a core part of the development workflow for code generation, explanation, and architectural guidance
- Expect frequent questions about Rust idioms, protocol details, and design patterns — answer thoroughly with explanations
- Project management via GitHub Issues and milestones (Phases 1–3)
