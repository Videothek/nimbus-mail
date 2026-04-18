//! Database schema and migrations for the local mail cache.
//!
//! # How migrations work
//!
//! The `MIGRATIONS` array is an ordered list of SQL blocks. Each entry is
//! one "version" of the schema; applying entry `N` moves the database from
//! version `N` to version `N+1`. The `schema_version` table stores the
//! current version as a single integer row.
//!
//! On startup we read `schema_version`, then run every migration whose
//! index is `>=` the current version. Each migration runs inside a
//! transaction so a failure leaves the DB untouched.
//!
//! # Why hand-rolled instead of a crate
//!
//! A project this size has a handful of migrations over its lifetime.
//! A crate like `refinery` or `rusqlite_migration` is fine, but brings
//! its own opinions; keeping the list right here is easier to reason
//! about while we're still exploring the schema. We can swap to a
//! migration crate later without disruption.
//!
//! # Adding a new migration
//!
//! **Only ever append** — never edit an existing entry, since users will
//! have the old one applied. Bump the schema by pushing another `&str`
//! onto `MIGRATIONS`. Keep each migration self-contained (all statements
//! needed to move from version N → N+1).

use rusqlite::Connection;

use crate::cache::CacheError;

/// Ordered migration scripts. Index `i` migrates schema version `i` → `i+1`.
///
/// The initial migration sets up the whole cache schema:
///
/// - `folders`: one row per mailbox per account (mirrors IMAP LIST output).
///   Primary key is `(account_id, name)` so a folder name is unique per
///   account but two accounts can both have an "INBOX".
///
/// - `messages`: envelope-level metadata — the light-weight fields shown
///   in the mail list. `uid` alone is not unique across folders (IMAP UIDs
///   are scoped per folder), so the natural key is `(account_id, folder, uid)`.
///   `internal_date` is indexed descending because the mail list view sorts
///   newest-first, which is the hot query path.
///
/// - `message_bodies`: the heavy fields (plain text, HTML, size) kept in a
///   separate table so envelope scans never drag MIME blobs through the
///   page cache. 1:1 with `messages`, same composite key, `ON DELETE CASCADE`.
///
/// - `folder_sync_state`: per-folder IMAP sync bookmarks. `uidvalidity` from
///   the server tells us whether our cached UIDs are still valid — if the
///   server returns a new value, everything for that folder must be wiped
///   and re-fetched. `highest_uid_seen` lets us do incremental syncs with
///   `UID FETCH highest+1:*`.
const MIGRATIONS: &[&str] = &[
    // ─────────────────────────────────────────────────────────────
    // v0 → v1: initial schema
    // ─────────────────────────────────────────────────────────────
    r#"
    CREATE TABLE folders (
        account_id     TEXT NOT NULL,
        name           TEXT NOT NULL,
        delimiter      TEXT,
        attributes     TEXT NOT NULL DEFAULT '[]',  -- JSON array of IMAP flags
        unread_count   INTEGER,
        PRIMARY KEY (account_id, name)
    );

    CREATE TABLE messages (
        account_id     TEXT    NOT NULL,
        folder         TEXT    NOT NULL,
        uid            INTEGER NOT NULL,
        from_addr      TEXT    NOT NULL DEFAULT '',
        subject        TEXT    NOT NULL DEFAULT '',
        internal_date  INTEGER NOT NULL,  -- unix epoch seconds
        is_read        INTEGER NOT NULL DEFAULT 0,
        is_starred     INTEGER NOT NULL DEFAULT 0,
        cached_at      INTEGER NOT NULL,  -- unix epoch seconds
        PRIMARY KEY (account_id, folder, uid)
    );

    -- Hot path: "newest 50 in this folder" — composite index ordered
    -- descending on internal_date so SQLite can satisfy the query
    -- with an index scan, no sort needed.
    CREATE INDEX messages_by_folder_date
        ON messages (account_id, folder, internal_date DESC);

    CREATE TABLE message_bodies (
        account_id       TEXT    NOT NULL,
        folder           TEXT    NOT NULL,
        uid              INTEGER NOT NULL,
        body_text        TEXT,
        body_html        TEXT,
        has_attachments  INTEGER NOT NULL DEFAULT 0,
        raw_size         INTEGER,
        cached_at        INTEGER NOT NULL,
        PRIMARY KEY (account_id, folder, uid),
        FOREIGN KEY (account_id, folder, uid)
            REFERENCES messages (account_id, folder, uid)
            ON DELETE CASCADE
    );

    CREATE TABLE folder_sync_state (
        account_id        TEXT    NOT NULL,
        folder            TEXT    NOT NULL,
        uidvalidity       INTEGER,
        highest_uid_seen  INTEGER,
        last_synced_at    INTEGER,
        PRIMARY KEY (account_id, folder)
    );
    "#,
    // ─────────────────────────────────────────────────────────────
    // v1 → v2: cache recipient headers so MailView can render from
    // the cache alone (no network round-trip on reopen).
    //
    // Stored as JSON-encoded arrays. IMAP address lists can get
    // genuinely weird (groups, nested comments, encoded words) so
    // a text blob is safer than trying to model rows per address —
    // and recipients are a display-only field for now, never
    // queried on.
    // ─────────────────────────────────────────────────────────────
    r#"
    ALTER TABLE message_bodies ADD COLUMN to_addrs TEXT NOT NULL DEFAULT '[]';
    ALTER TABLE message_bodies ADD COLUMN cc_addrs TEXT NOT NULL DEFAULT '[]';
    "#,
    // ─────────────────────────────────────────────────────────────
    // v2 → v3: CardDAV contacts cache.
    //
    // - `contacts`: one row per vCard. Keyed by app-side `id`
    //   (`{nc_id}::{vcard_uid}`) so the UI has a single string handle,
    //   plus the natural `(nextcloud_account_id, addressbook, vcard_uid)`
    //   triple as a UNIQUE constraint to keep imports idempotent.
    //   `vcard_raw` is kept so we can re-extract fields if the model
    //   evolves without re-syncing every contact from the server.
    //
    // - `addressbook_sync_state`: per-collection bookmark for RFC 6578
    //   sync-collection. `sync_token` is what the server gave us last;
    //   we send it back to ask "what changed since". `ctag` is the
    //   pre-RFC-6578 cheap-check for "did anything change at all" —
    //   Nextcloud exposes both, we use ctag as the early-out and the
    //   sync token to enumerate the actual delta.
    //
    // Indexes:
    //   - display_name COLLATE NOCASE for the autocomplete LIKE scan
    //   - (nc_id, addressbook) for the per-addressbook sync upserts
    // ─────────────────────────────────────────────────────────────
    r#"
    CREATE TABLE contacts (
        id                    TEXT PRIMARY KEY,
        nextcloud_account_id  TEXT NOT NULL,
        addressbook           TEXT NOT NULL,
        vcard_uid             TEXT NOT NULL,
        href                  TEXT NOT NULL,
        etag                  TEXT NOT NULL,
        display_name          TEXT NOT NULL DEFAULT '',
        emails_json           TEXT NOT NULL DEFAULT '[]',
        phones_json           TEXT NOT NULL DEFAULT '[]',
        organization          TEXT,
        photo_mime            TEXT,
        photo_data            BLOB,
        vcard_raw             TEXT NOT NULL,
        cached_at             INTEGER NOT NULL,
        UNIQUE (nextcloud_account_id, addressbook, vcard_uid)
    );

    CREATE INDEX contacts_by_display_name
        ON contacts (display_name COLLATE NOCASE);

    CREATE INDEX contacts_by_addressbook
        ON contacts (nextcloud_account_id, addressbook);

    CREATE TABLE addressbook_sync_state (
        nextcloud_account_id  TEXT NOT NULL,
        addressbook           TEXT NOT NULL,
        display_name          TEXT,
        sync_token            TEXT,
        ctag                  TEXT,
        last_synced_at        INTEGER,
        PRIMARY KEY (nextcloud_account_id, addressbook)
    );
    "#,
];

const SCHEMA_VERSION_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS schema_version (
        id      INTEGER PRIMARY KEY CHECK (id = 1),
        version INTEGER NOT NULL
    );
    INSERT OR IGNORE INTO schema_version (id, version) VALUES (1, 0);
"#;

/// Bring the database up to the latest schema version.
///
/// Runs every pending migration inside its own transaction, bumping the
/// recorded `schema_version` after each one. If a migration fails, the
/// transaction rolls back and we return the error — the DB stays at the
/// previous version rather than landing in a half-migrated state.
pub fn run_migrations(conn: &mut Connection) -> Result<(), CacheError> {
    // Ensure the version table exists before we try to read from it.
    conn.execute_batch(SCHEMA_VERSION_SQL)
        .map_err(|e| CacheError::Migration(format!("failed to init schema_version: {e}")))?;

    let current: i64 = conn
        .query_row("SELECT version FROM schema_version WHERE id = 1", [], |r| {
            r.get(0)
        })
        .map_err(|e| CacheError::Migration(format!("failed to read schema_version: {e}")))?;

    let target = MIGRATIONS.len() as i64;
    if current == target {
        tracing::debug!("Cache schema already at version {current}");
        return Ok(());
    }

    tracing::info!("Migrating cache schema v{current} → v{target}");

    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn
            .transaction()
            .map_err(|e| CacheError::Migration(format!("begin tx: {e}")))?;

        tx.execute_batch(sql)
            .map_err(|e| CacheError::Migration(format!("migration v{} → v{}: {e}", i, i + 1)))?;

        tx.execute(
            "UPDATE schema_version SET version = ?1 WHERE id = 1",
            [(i + 1) as i64],
        )
        .map_err(|e| CacheError::Migration(format!("bump version: {e}")))?;

        tx.commit()
            .map_err(|e| CacheError::Migration(format!("commit tx: {e}")))?;

        tracing::debug!("Applied migration v{} → v{}", i, i + 1);
    }

    Ok(())
}
