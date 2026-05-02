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

## UI Conventions

These are project-wide affordances we expect Claude to apply automatically when adding new list rows, sidebar items, or any other repeating element that has actions attached:

- **Left swatch is the visibility / enabled toggle.** When a row can be hidden, muted, suppressed-from-autocomplete, or otherwise disabled without removing it, expose the toggle as a small coloured square on the *left* edge of the row. Filled with the row's accent colour = enabled; outlined (transparent fill, same border colour) = disabled. The row's name text greys out (`text-surface-400 dark:text-surface-500`) when the toggle is off so the row reads as "still here, just inert" at a glance. Calendars (`CalendarView` mute swatch) and mailing lists (`ContactsView` hide-from-autocomplete swatch) are the canonical references — copy that shape rather than inventing a new disabled-state visual.
- **Three-dot button (⋯) signals "this row has actions."** Whenever a row carries any action beyond its primary click (rename, delete, change emoji/icon, hide, etc.), surface a `⋯` button on the right side of the row. Default to opacity-0 with `group-hover:opacity-100` (and persistent when its own menu is open) so resting rows stay quiet. The button must be keyboard-focusable and the menu must dismiss on outside-click and Escape.
- **Right-click does the same thing.** Every row that has a three-dot button must also respond to `oncontextmenu` by opening the *exact same* menu, anchored at the cursor position. The two surfaces share one menu component — never let them drift. This is our compatibility contract for trackpad / touchscreen users (who get the dots) versus mouse users (who reach for right-click).
- **Menu anchor pattern.** Use `position: fixed` with coordinates from `getBoundingClientRect()` (three-dot trigger) or `e.clientX/Y` (right-click). Stop `mousedown` from propagating out of the menu div — the document-level mousedown listener that dismisses it fires *before* a click, and without `stopPropagation` the menu unmounts before its item handlers run.
- **Inline edits over modals where possible.** "Rename" should swap the row's label for an `<input>` (Enter commits, Escape cancels, blur commits) — not a modal. Modals are reserved for create flows and destructive confirms.
- **Shared `EmojiPicker` for any emoji input.** Never build a one-off grid. Use `ui/src/lib/EmojiPicker.svelte` (categories + search + clear). Set `allowClear={false}` only when "no emoji" is meaningless (e.g. inserting into a text editor).
- **Outside-click dismissal idiom.** When you open a popover, register `document.addEventListener('mousedown', close)` *inside an `$effect` that depends on the open state*, with a one-tick delay (`setTimeout(..., 0)`) so the click that opened it doesn't immediately close it. Tear down on close.

When in doubt, look at how `ContactsView` (mailing-list rows) and `Sidebar` (mail-folder rows) implement these — they're the canonical reference.

## Email-rendering conventions

The Talk + meeting invite cards we drop into outgoing mail (`ui/src/lib/inviteHtml.ts`, used by Compose for the "Insert Talk link" and "Respond with meeting" flows) have a few non-obvious rules that go beyond normal HTML:

- **Inline styles only.** Gmail, Outlook, Yahoo, etc. all strip `<style>` blocks from received mail; class names carry no meaning across clients. Every visual property has to live on the element via `style="..."`. No external CSS, no `@import`, no `@font-face`.
- **System font stack.** `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif`. Crisp on every OS without a font fetch.
- **Detail-row glyphs are emoji** (📅 🕐 📍 📝 💬 🔗). Universal client support. SVG / icon fonts inside email are unreliable across Outlook desktop and conservative Gmail setups.
- **No images in the chrome.** The brand header is a typography-only wordmark in a soft pill — no `<img>`. We tried both:
  - **Remote URL** (`raw.githubusercontent.com/...`): hit "block remote images by default" in Gmail / Apple Mail / Outlook and the recipient saw a broken-icon until they trusted the sender. (Also: the original path I picked pointed at the v2 set, but storm is a v1 style — easy mistake to repeat. v2 ships `copper / forest / midnight / ocean / rose / slate / sunset`; storm lives at `logos/nimbus-logo/png/storm/...`.)
  - **Inline `data:image/png;base64,…` URI**: many corporate / hardened mail filters (Outlook in particular) strip `<img src="data:…">` for security, again leaving a broken-icon.
  Both paths ate the logo. Don't reintroduce an `<img>` in the chrome unless you've solved this for the worst client your users will receive mail in. The `PUBLIC_NIMBUS_LOGO_URL` export is now an empty-string compatibility stub for any leftover importers.
- **The editor's `NimbusBlock` extension** (`ui/src/lib/RichTextEditor.svelte`) recognises `<div data-nimbus-block="…">` wrappers as atom nodes so the styled cards survive Tiptap's schema. If you add a new card kind, stamp the wrapper with that data attribute and the editor will render it via the existing NodeView path — no new extension needed.

When in doubt, render the card to a local HTML file and open it in `outlook.com`, `mail.google.com`, and Apple Mail — those three are the dominant surfaces and have the strictest sanitisers.

## Development Guidelines

- Write clear, well-documented code — the team is learning as they build
- Prefer existing, well-maintained Rust crates over reimplementing protocols from scratch
- Write tests for protocol handling and data sync logic
- Use `clippy` and `rustfmt` on all Rust code
- Commit messages should explain *why*, not just *what*
- Keep the UI responsive — heavy work belongs in async background tasks, never on the UI thread
- **No other-mail-client references in code, comments, or commit messages.** Do not name Outlook, Apple Mail, Thunderbird, Gmail (as a UX comparison — the literal `gmail.com` hostname is fine in autoconfig / discovery code), Yahoo Mail, Fastmail, Spark, Airmail, Hey, ProtonMail, Tutanota, etc. Describe the *behaviour* generically ("the standard mail-client triage gesture", "the operator-prefixed search syntax") instead of comparing to a specific product. Where a comment is anchoring on a real protocol or RFC quirk, name the protocol or the RFC, not the client whose implementation first surfaced it. Applies retroactively — if you spot a leftover reference, rewrite it. Hostnames inside string literals (`gmail.com`, `[gmail]/trash`, `autoconfig.thunderbird.net`) are factual data and stay; the rule is about comments and prose.
- **Maintain `SBOM.md` AND `License.md` on every dependency change.** Adding, removing, or upgrading a package in any `Cargo.toml` (workspace or per-crate) or in `ui/package.json` requires edits to both:
  - `SBOM.md` — list the package, its licence, what category that licence falls into (permissive / weak copyleft / strong copyleft), and update the "Last manual reconciliation" date at the bottom of the inventory section.
  - `License.md` — add the package to the section matching its licence, or create a new section if that licence isn't represented yet (and add the licence's notice text inline if so).

  `SBOM.md` is the marketing-implications document (what each licence forces our distribution model to look like); `License.md` is the legal attribution document we ship next to binaries to satisfy each upstream's notice obligations. Introducing a stronger copyleft licence than what's already in the tree (e.g. AGPL-3.0 when we currently top out at GPL-3.0) is a project-level decision — surface it explicitly to Nick / Jannik before merging, don't slip it into a routine PR.

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

We use **short-lived feature branches, one per issue.** This keeps PRs focused, reviews small, and avoids the long-running merge conflicts that come with permanent personal branches.

```
main (stable, always compiles)
 ├── feature/10-contacts-view       (short-lived, deleted after merge)
 ├── feature/14-settings-panel      (short-lived, deleted after merge)
 └── feature/17-imap-idle           (short-lived, deleted after merge)
```

### Rules
- **Never push directly to `main`** — always merge via Pull Request
- **One branch per issue** — name it `feature/<issue-number>-<short-slug>` (e.g. `feature/10-contacts-view`)
- **Branch from the latest `main`** — always start a new feature branch from an up-to-date `main`:
  ```bash
  git checkout main
  git pull origin main
  git checkout -b feature/<issue-number>-<short-slug>
  ```
- **When your issue is done** — open a PR from your feature branch to `main`, the other person reviews and merges
- **After the PR is merged** — delete the branch (locally and on GitHub), then start the next issue with a fresh branch off the new `main`:
  ```bash
  git checkout main
  git pull origin main
  git branch -d feature/<old-branch>
  git push origin --delete feature/<old-branch>
  ```
- **Merge early, merge small** — if you add a shared type to `nimbus-core` that the other person needs, split it into its own tiny PR first so the other feature branch can use it

### When to merge to main
- The issue is complete (or a clean slice of it is)
- A new model or type is added to `nimbus-core` that other work depends on
- A crate compiles and has basic functionality or tests
- A UI component works (even with mock data)
- **Do NOT merge** broken code or half-finished functions

### Claude reminder obligations
Claude MUST proactively remind the developer in these situations:

**Before opening a PR:**
> "Ready to open a PR? Double-check: you're on a feature branch named `feature/<issue-number>-<slug>`, branched from an up-to-date `main`, and this branch covers exactly one issue. If you're on `main` or a long-lived personal branch, stop and move the commits onto a proper feature branch first."

**After an issue is merged to `main`:**
> "This is now merged to main. Delete the feature branch (`git branch -d feature/<name>` locally, `git push origin --delete feature/<name>` on GitHub), then remind the other developer (Nick/Jannik) to pull main before starting their next branch: `git pull origin main`."

Together these keep both developers starting every issue from the same clean base and avoid painful merge conflicts.

## Team Context

- **Nick** and **Jannik** — two-person team, new to building a project of this scale
- AI assistance (Claude) is a core part of the development workflow for code generation, explanation, and architectural guidance
- Expect frequent questions about Rust idioms, protocol details, and design patterns — answer thoroughly with explanations
- Project management via GitHub Issues and milestones (Phases 1–3)
