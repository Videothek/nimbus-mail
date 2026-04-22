//! Local mail cache backed by SQLite.
//!
//! # What lives here
//!
//! - **Envelopes** (light metadata shown in the mail list) and **bodies**
//!   (text/HTML, cached lazily on first open) of messages fetched from IMAP.
//! - **Folder listings** and **per-folder sync state** so the next launch
//!   can start displaying something before the network is touched.
//!
//! # What does not live here
//!
//! - **Accounts**: still in `accounts.json`. Moving them into the DB is a
//!   future exercise — worth doing once we need foreign keys from emails
//!   to accounts or transactional account edits. See the README for the
//!   motivation.
//! - **Passwords**: always in the OS keychain (`credentials.rs`). Never
//!   the DB, never disk.
//!
//! # Read strategy
//!
//! The UI loads from the cache first (instant, offline-safe) and then
//! kicks off a network refresh which write-throughs back to the cache.
//! The Tauri layer owns this dance — there are separate `get_cached_*`
//! commands for the cache path and `fetch_*` commands for the network
//! path. Keeping them distinct makes the strategy explicit in the UI
//! and lets future views (search, notifications) pick whichever they
//! need.
//!
//! # Thread-safety
//!
//! The cache holds an `r2d2` pool internally. Every method checks out a
//! connection, does its work, and returns it. The pool is internally
//! synchronised so `Cache` is cheap to `clone()` and share across tasks.

pub mod calendars;
pub mod contacts;
pub mod key;
pub mod pool;
pub mod schema;
pub mod search;

pub use calendars::{
    CachedCalendar, CalendarEventRow, CalendarEventServerHandle, CalendarRow, CalendarSyncState,
    ExpansionInput,
};
pub use contacts::{AddressbookSyncState, ContactRow, ContactServerHandle};
pub use search::{SearchFilters, SearchHit, SearchScope};

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use nimbus_core::NimbusError;
use nimbus_core::models::{Email, EmailEnvelope, Folder};
use rusqlite::{OptionalExtension, params};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::cache::pool::SqlitePool;

/// Errors specific to the cache layer. Converted to `NimbusError::Storage`
/// when crossing out of the crate so the rest of the app doesn't have to
/// care which database we happen to be using.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("failed to open cache: {0}")]
    Open(String),
    #[error("migration failed: {0}")]
    Migration(String),
    #[error("sqlite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("pool error: {0}")]
    Pool(#[from] r2d2::Error),
}

impl From<CacheError> for NimbusError {
    fn from(e: CacheError) -> Self {
        NimbusError::Storage(e.to_string())
    }
}

/// Cached per-folder sync bookmark.
///
/// `uidvalidity` is the IMAP server's guarantee that existing UIDs in the
/// folder are still valid. If the server ever returns a different value,
/// we must throw away everything cached for that folder and start over.
#[derive(Debug, Clone)]
pub struct SyncState {
    pub uidvalidity: Option<u32>,
    pub highest_uid_seen: Option<u32>,
    pub last_synced_at: Option<DateTime<Utc>>,
}

/// Handle to the local mail cache. Cheap to clone — under the hood it's
/// an `Arc` around a connection pool.
#[derive(Clone)]
pub struct Cache {
    pool: SqlitePool,
}

impl Cache {
    /// Open the app's default cache location:
    /// `<config-dir>/nimbus-mail/cache.db`, and run any pending migrations.
    ///
    /// The DB is encrypted via SQLCipher; the master key is fetched from
    /// (or freshly generated in) the OS keychain. See `key.rs`.
    pub fn open_default() -> Result<Self, NimbusError> {
        let path = default_cache_path()?;
        let key_hex = key::get_or_create_master_key()?;
        Self::open_with_key(&path, key_hex).map_err(Into::into)
    }

    /// Open a cache at an explicit path with a caller-supplied key.
    ///
    /// Used by the default opener above and by future multi-profile
    /// support. The key must be a 64-char lowercase hex string.
    ///
    /// Handles the pre-encryption → encryption upgrade: if a legacy
    /// unencrypted `cache.db` is found on disk, opening it with a key
    /// will fail at the first decrypt; we detect that, wipe the file,
    /// and recreate from scratch. The user loses their cache but
    /// re-sync fills it back in on next launch.
    pub fn open_with_key(path: &Path, key_hex: String) -> Result<Self, CacheError> {
        info!("Opening encrypted mail cache at {}", path.display());
        let pool = match pool::open_pool(path, key_hex.clone()) {
            Ok(p) => p,
            Err(e) if is_wrong_key_error(&e) && path.exists() => {
                warn!(
                    "Existing cache at {} could not be unlocked (likely an \
                     unencrypted cache from a pre-encryption build). Wiping \
                     and recreating — mail will re-sync on next launch.",
                    path.display()
                );
                wipe_cache_files(path)?;
                pool::open_pool(path, key_hex)?
            }
            Err(e) => return Err(e),
        };
        // Run migrations on a freshly checked-out connection so the pool
        // is available for use right after this call returns.
        let mut conn = pool.get()?;
        schema::run_migrations(&mut conn)?;
        Ok(Self { pool })
    }

    /// Clears the cache for a specific account — called when an account
    /// is removed, or when `UIDVALIDITY` changes and we need to start fresh.
    pub fn wipe_account(&self, account_id: &str) -> Result<(), CacheError> {
        let conn = self.pool.get()?;
        // `ON DELETE CASCADE` on message_bodies means deleting from
        // messages clears the bodies too. folders / folder_sync_state
        // don't have FKs, so we clear them explicitly.
        conn.execute("DELETE FROM messages WHERE account_id = ?1", [account_id])?;
        conn.execute("DELETE FROM folders WHERE account_id = ?1", [account_id])?;
        conn.execute(
            "DELETE FROM folder_sync_state WHERE account_id = ?1",
            [account_id],
        )?;
        info!("Wiped cache entries for account '{account_id}'");
        Ok(())
    }

    /// Clear all cached rows for a single folder — used when the server's
    /// `UIDVALIDITY` for that folder has changed, meaning every UID we had
    /// cached now refers to a different message (or none at all).
    ///
    /// `ON DELETE CASCADE` handles the bodies; we explicitly drop the
    /// `folder_sync_state` row too so the next sync starts from scratch.
    pub fn wipe_folder(&self, account_id: &str, folder: &str) -> Result<(), CacheError> {
        let conn = self.pool.get()?;
        conn.execute(
            "DELETE FROM messages WHERE account_id = ?1 AND folder = ?2",
            params![account_id, folder],
        )?;
        conn.execute(
            "DELETE FROM folder_sync_state WHERE account_id = ?1 AND folder = ?2",
            params![account_id, folder],
        )?;
        info!("Wiped cache for '{account_id}' / '{folder}' (UIDVALIDITY reset)");
        Ok(())
    }

    // ── Folders ─────────────────────────────────────────────────

    /// Replace the cached folder list for an account.
    ///
    /// Folder names can change (user renames, server-side mailbox removal),
    /// so we wipe-and-reinsert inside a transaction rather than trying to
    /// diff. The folder list is small (dozens of rows at most) so this
    /// is effectively free.
    pub fn upsert_folders(&self, account_id: &str, folders: &[Folder]) -> Result<(), CacheError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM folders WHERE account_id = ?1", [account_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO folders (account_id, name, delimiter, attributes, unread_count)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for f in folders {
                let attrs = serde_json::to_string(&f.attributes).unwrap_or_else(|_| "[]".into());
                stmt.execute(params![
                    account_id,
                    f.name,
                    f.delimiter,
                    attrs,
                    f.unread_count,
                ])?;
            }
        }
        tx.commit()?;
        debug!(
            "Cached {} folders for account '{account_id}'",
            folders.len()
        );
        Ok(())
    }

    /// Read the cached folder list for an account.
    ///
    /// Returns folders in the order they were inserted — the server's
    /// native order, which is usually alphabetical or provider-specific.
    /// Attributes are stored as a JSON array string and parsed back into
    /// a `Vec<String>` here so callers see the same shape as the live
    /// `list_folders` response.
    pub fn get_folders(&self, account_id: &str) -> Result<Vec<Folder>, CacheError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT name, delimiter, attributes, unread_count
             FROM folders
             WHERE account_id = ?1
             ORDER BY name",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            let attrs_json: String = r.get(2)?;
            let attributes: Vec<String> = serde_json::from_str(&attrs_json).unwrap_or_default();
            let unread: Option<i64> = r.get(3)?;
            Ok(Folder {
                name: r.get(0)?,
                delimiter: r.get(1)?,
                attributes,
                unread_count: unread.map(|v| v as u32),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ── Envelopes ───────────────────────────────────────────────

    /// Upsert a batch of envelopes, tagging each with the given `account_id`.
    ///
    /// `EmailEnvelope` doesn't carry an account id — the frontend never
    /// needs it, and the Tauri command always knows which account it
    /// connected to. We take the id once here instead of widening the
    /// shared struct.
    ///
    /// Uses `ON CONFLICT ... DO UPDATE` so re-fetching an existing message
    /// refreshes its flags (e.g. user marked-as-read on another device).
    /// Runs inside a transaction: either the whole batch lands or none
    /// of it does.
    pub fn upsert_envelopes_for_account(
        &self,
        account_id: &str,
        envelopes: &[EmailEnvelope],
    ) -> Result<(), CacheError> {
        if envelopes.is_empty() {
            return Ok(());
        }
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;
        let now = Utc::now().timestamp();
        {
            let mut stmt = tx.prepare(
                "INSERT INTO messages
                   (account_id, folder, uid, from_addr, subject, internal_date,
                    is_read, is_starred, cached_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (account_id, folder, uid) DO UPDATE SET
                   from_addr     = excluded.from_addr,
                   subject       = excluded.subject,
                   internal_date = excluded.internal_date,
                   is_read       = excluded.is_read,
                   is_starred    = excluded.is_starred,
                   cached_at     = excluded.cached_at",
            )?;
            for env in envelopes {
                stmt.execute(params![
                    account_id,
                    env.folder,
                    env.uid as i64,
                    env.from,
                    env.subject,
                    env.date.timestamp(),
                    env.is_read as i64,
                    env.is_starred as i64,
                    now,
                ])?;
            }
        }
        tx.commit()?;
        debug!(
            "Cached {} envelopes for '{account_id}' (first folder: {})",
            envelopes.len(),
            envelopes.first().map(|e| e.folder.as_str()).unwrap_or("-"),
        );
        Ok(())
    }

    /// Return the newest `limit` envelopes in a folder from the cache.
    ///
    /// Uses the `messages_by_folder_date` index to avoid a sort.
    pub fn get_envelopes(
        &self,
        account_id: &str,
        folder: &str,
        limit: u32,
    ) -> Result<Vec<EmailEnvelope>, CacheError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT uid, folder, from_addr, subject, internal_date, is_read, is_starred
             FROM messages
             WHERE account_id = ?1 AND folder = ?2
             ORDER BY internal_date DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![account_id, folder, limit as i64], |r| {
            let ts: i64 = r.get(4)?;
            let date = Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now);
            Ok(EmailEnvelope {
                uid: r.get::<_, i64>(0)? as u32,
                folder: r.get(1)?,
                from: r.get(2)?,
                subject: r.get(3)?,
                date,
                is_read: r.get::<_, i64>(5)? != 0,
                is_starred: r.get::<_, i64>(6)? != 0,
                account_id: account_id.to_string(),
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Return the newest `limit` envelopes in `folder` across **all**
    /// accounts. Powers the unified-inbox view: each row carries its
    /// owning `account_id` so the UI can render an account label and
    /// route the "open message" click to the right account.
    pub fn get_unified_envelopes(
        &self,
        folder: &str,
        limit: u32,
    ) -> Result<Vec<EmailEnvelope>, CacheError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT account_id, uid, folder, from_addr, subject, internal_date, is_read, is_starred
             FROM messages
             WHERE folder = ?1
             ORDER BY internal_date DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![folder, limit as i64], |r| {
            let ts: i64 = r.get(5)?;
            let date = Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now);
            Ok(EmailEnvelope {
                account_id: r.get(0)?,
                uid: r.get::<_, i64>(1)? as u32,
                folder: r.get(2)?,
                from: r.get(3)?,
                subject: r.get(4)?,
                date,
                is_read: r.get::<_, i64>(6)? != 0,
                is_starred: r.get::<_, i64>(7)? != 0,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mark a cached envelope as read (sets `is_read = 1`) and keep
    /// the folder's `unread_count` in sync by decrementing it iff the
    /// message was previously unread.
    ///
    /// Used by the "mark as read when opened" path: we flip the local
    /// cache immediately so the UI reflects the change without waiting
    /// for the network round-trip to the IMAP server. If the row isn't
    /// cached yet (message was never listed), the message-table UPDATE
    /// is a no-op and we don't decrement the folder count — there's
    /// nothing to subtract from.
    ///
    /// Wrapped in a transaction so the message flip and the folder
    /// count adjustment land atomically; an interrupted call can never
    /// leave `is_read = 1` next to an unchanged `unread_count`.
    pub fn mark_envelope_read(
        &self,
        account_id: &str,
        folder: &str,
        uid: u32,
    ) -> Result<(), CacheError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;

        let was_unread: bool = tx
            .query_row(
                "SELECT is_read = 0 FROM messages
                 WHERE account_id = ?1 AND folder = ?2 AND uid = ?3",
                params![account_id, folder, uid as i64],
                |r| r.get::<_, i64>(0).map(|v| v != 0),
            )
            .unwrap_or(false);

        tx.execute(
            "UPDATE messages SET is_read = 1
             WHERE account_id = ?1 AND folder = ?2 AND uid = ?3",
            params![account_id, folder, uid as i64],
        )?;

        if was_unread {
            // `MAX(unread_count - 1, 0)` guards against an off-by-one
            // dropping below zero when the cached folder count is
            // already stale (e.g. another client read the message,
            // a background poll lowered `unread_count`, then we read
            // it ourselves).
            tx.execute(
                "UPDATE folders
                 SET unread_count = MAX(COALESCE(unread_count, 0) - 1, 0)
                 WHERE account_id = ?1 AND name = ?2",
                params![account_id, folder],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Bump a folder's `unread_count` by `delta` (positive to add,
    /// negative to subtract). Treats a `NULL` stored count as `0`.
    /// Used by the poll path to credit newly-arrived unread mail
    /// against the badge without waiting for a fresh `STATUS` round-trip.
    pub fn bump_folder_unread(
        &self,
        account_id: &str,
        folder: &str,
        delta: i64,
    ) -> Result<(), CacheError> {
        if delta == 0 {
            return Ok(());
        }
        let conn = self.pool.get()?;
        conn.execute(
            "UPDATE folders
             SET unread_count = MAX(COALESCE(unread_count, 0) + ?3, 0)
             WHERE account_id = ?1 AND name = ?2",
            params![account_id, folder, delta],
        )?;
        Ok(())
    }

    /// Total unread messages across all accounts' INBOX folders.
    ///
    /// Feeds the tray tooltip ("Nimbus Mail — 3 unread") and any
    /// aggregate badge UI. We scope to INBOX only because other folders
    /// (Archive, Trash) aren't typically surfaced as "unread" to the
    /// user even when they technically have `is_read = 0` rows.
    pub fn total_unread_count(&self) -> Result<u32, CacheError> {
        let conn = self.pool.get()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE folder = 'INBOX' AND is_read = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(count as u32)
    }

    // ── Message bodies ──────────────────────────────────────────

    /// Upsert a cached message body alongside its envelope.
    ///
    /// Takes an `Email` since that's the shape the IMAP client returns — we
    /// split it into an envelope row (via `upsert_envelopes_for_account`)
    /// and a body row here, in a single transaction so partial rows never
    /// survive a failed write.
    pub fn upsert_message(&self, email: &Email) -> Result<(), CacheError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;
        let now = Utc::now().timestamp();

        // Envelope row — mirrors upsert_envelopes_for_account but inside
        // the same transaction as the body so the two can't drift.
        tx.execute(
            "INSERT INTO messages
                (account_id, folder, uid, from_addr, subject, internal_date,
                 is_read, is_starred, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (account_id, folder, uid) DO UPDATE SET
                from_addr     = excluded.from_addr,
                subject       = excluded.subject,
                internal_date = excluded.internal_date,
                is_read       = excluded.is_read,
                is_starred    = excluded.is_starred,
                cached_at     = excluded.cached_at",
            params![
                email.account_id,
                email.folder,
                // `id` from IMAP is formatted as "folder:uid" in the
                // fetch path — we don't rely on it here, the UID is
                // re-parsed by the caller.
                uid_from_email_id(&email.id) as i64,
                email.from,
                email.subject,
                email.date.timestamp(),
                email.is_read as i64,
                email.is_starred as i64,
                now,
            ],
        )?;

        // Addresses are stored as JSON arrays — see the v1 → v2 migration
        // note. `unwrap_or_else` fallbacks are defensive; serde_json on a
        // Vec<String> can only fail if allocation fails.
        let to_json = serde_json::to_string(&email.to).unwrap_or_else(|_| "[]".into());
        let cc_json = serde_json::to_string(&email.cc).unwrap_or_else(|_| "[]".into());
        // Attachment metadata as JSON — one record per attachment,
        // with the stable `part_id` the IMAP re-fetch uses. See v5 → v6.
        let attachments_json =
            serde_json::to_string(&email.attachments).unwrap_or_else(|_| "[]".into());

        tx.execute(
            "INSERT INTO message_bodies
                (account_id, folder, uid, body_text, body_html,
                 has_attachments, raw_size, cached_at, to_addrs, cc_addrs,
                 attachments)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (account_id, folder, uid) DO UPDATE SET
                body_text       = excluded.body_text,
                body_html       = excluded.body_html,
                has_attachments = excluded.has_attachments,
                raw_size        = excluded.raw_size,
                cached_at       = excluded.cached_at,
                to_addrs        = excluded.to_addrs,
                cc_addrs        = excluded.cc_addrs,
                attachments     = excluded.attachments",
            params![
                email.account_id,
                email.folder,
                uid_from_email_id(&email.id) as i64,
                email.body_text,
                email.body_html,
                email.has_attachments as i64,
                None::<i64>,
                now,
                to_json,
                cc_json,
                attachments_json,
            ],
        )?;
        tx.commit()?;
        debug!(
            "Cached message {}:{}:{} (text={}, html={}, atts={})",
            email.account_id,
            email.folder,
            uid_from_email_id(&email.id),
            email.body_text.is_some(),
            email.body_html.is_some(),
            email.has_attachments,
        );
        Ok(())
    }

    /// Look up a fully-hydrated cached message.
    ///
    /// Joins `messages` and `message_bodies`; returns `None` if we haven't
    /// fetched the body yet (envelope-only is not enough to render MailView,
    /// so the caller should treat envelope-only as "not cached" and go to
    /// the network).
    pub fn get_message(
        &self,
        account_id: &str,
        folder: &str,
        uid: u32,
    ) -> Result<Option<Email>, CacheError> {
        let conn = self.pool.get()?;
        let row = conn
            .query_row(
                "SELECT m.from_addr, m.subject, m.internal_date,
                        m.is_read, m.is_starred,
                        b.body_text, b.body_html, b.has_attachments,
                        b.to_addrs, b.cc_addrs, b.attachments
                 FROM messages m
                 INNER JOIN message_bodies b USING (account_id, folder, uid)
                 WHERE m.account_id = ?1 AND m.folder = ?2 AND m.uid = ?3",
                params![account_id, folder, uid as i64],
                |r| {
                    let ts: i64 = r.get(2)?;
                    let date = Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now);
                    let to_json: String = r.get(8)?;
                    let cc_json: String = r.get(9)?;
                    let attachments_json: String = r.get(10)?;
                    Ok(Email {
                        id: format!("{folder}:{uid}"),
                        account_id: account_id.to_string(),
                        folder: folder.to_string(),
                        from: r.get(0)?,
                        to: serde_json::from_str(&to_json).unwrap_or_default(),
                        cc: serde_json::from_str(&cc_json).unwrap_or_default(),
                        subject: r.get(1)?,
                        body_text: r.get(5)?,
                        body_html: r.get(6)?,
                        date,
                        is_read: r.get::<_, i64>(3)? != 0,
                        is_starred: r.get::<_, i64>(4)? != 0,
                        has_attachments: r.get::<_, i64>(7)? != 0,
                        attachments: serde_json::from_str(&attachments_json)
                            .unwrap_or_default(),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    // ── Sync state ──────────────────────────────────────────────

    pub fn get_sync_state(
        &self,
        account_id: &str,
        folder: &str,
    ) -> Result<Option<SyncState>, CacheError> {
        let conn = self.pool.get()?;
        let state = conn
            .query_row(
                "SELECT uidvalidity, highest_uid_seen, last_synced_at
                 FROM folder_sync_state
                 WHERE account_id = ?1 AND folder = ?2",
                params![account_id, folder],
                |r| {
                    let ts: Option<i64> = r.get(2)?;
                    let uv: Option<i64> = r.get(0)?;
                    let hi: Option<i64> = r.get(1)?;
                    Ok(SyncState {
                        uidvalidity: uv.map(|v| v as u32),
                        highest_uid_seen: hi.map(|v| v as u32),
                        last_synced_at: ts.and_then(|t| Utc.timestamp_opt(t, 0).single()),
                    })
                },
            )
            .optional()?;
        Ok(state)
    }

    pub fn set_sync_state(
        &self,
        account_id: &str,
        folder: &str,
        state: &SyncState,
    ) -> Result<(), CacheError> {
        let conn = self.pool.get()?;
        conn.execute(
            "INSERT INTO folder_sync_state
                (account_id, folder, uidvalidity, highest_uid_seen, last_synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (account_id, folder) DO UPDATE SET
                uidvalidity      = excluded.uidvalidity,
                highest_uid_seen = excluded.highest_uid_seen,
                last_synced_at   = excluded.last_synced_at",
            params![
                account_id,
                folder,
                state.uidvalidity.map(|v| v as i64),
                state.highest_uid_seen.map(|v| v as i64),
                state.last_synced_at.map(|t| t.timestamp()),
            ],
        )?;
        Ok(())
    }
}

/// Parse the IMAP UID out of an `Email.id` produced by the IMAP client.
///
/// `nimbus_imap` formats ids as `"{folder}:{uid}"` — folder names can
/// themselves contain `:` (rare but legal), so we split on the *last*
/// colon. A malformed id yields 0 with a warn log; this can only happen
/// if the upstream id format changes, in which case the cache row will
/// collide on uid=0 and the warning makes it discoverable.
fn uid_from_email_id(id: &str) -> u32 {
    let tail = id.rsplit_once(':').map(|(_, u)| u).unwrap_or(id);
    tail.parse().unwrap_or_else(|_| {
        tracing::warn!("could not parse uid from email id '{id}', defaulting to 0");
        0
    })
}

fn default_cache_path() -> Result<PathBuf, NimbusError> {
    let dir = dirs::config_dir()
        .ok_or_else(|| NimbusError::Storage("cannot determine config directory".into()))?;
    Ok(dir.join("nimbus-mail").join("cache.db"))
}

/// Does this pool-open error look like "wrong key / not a SQLCipher DB"?
///
/// r2d2 wraps the underlying rusqlite error once, and we re-wrap into
/// `CacheError::Open` with the message, so the sentinel strings bubble
/// up in the final `.to_string()`. SQLCipher returns either
/// `SQLITE_NOTADB` ("file is not a database") or `SQLITE_CORRUPT`
/// ("file is encrypted or is not a database") when the key is wrong.
fn is_wrong_key_error(err: &CacheError) -> bool {
    let msg = err.to_string();
    msg.contains("file is not a database") || msg.contains("file is encrypted")
}

/// Delete the cache DB plus its WAL sidecar files (`-wal`, `-shm`).
///
/// Leaving the sidecars behind would let SQLite partially replay the
/// old unencrypted WAL against the new encrypted file on next open.
fn wipe_cache_files(path: &Path) -> Result<(), CacheError> {
    for suffix in ["", "-wal", "-shm"] {
        let p = if suffix.is_empty() {
            path.to_path_buf()
        } else {
            let mut s = path.as_os_str().to_owned();
            s.push(suffix);
            PathBuf::from(s)
        };
        if p.exists() {
            std::fs::remove_file(&p)
                .map_err(|e| CacheError::Open(format!("remove {}: {e}", p.display())))?;
        }
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_envelope(uid: u32, folder: &str, offset_min: i64) -> EmailEnvelope {
        EmailEnvelope {
            uid,
            folder: folder.to_string(),
            from: format!("sender-{uid}@example.com"),
            subject: format!("Test subject {uid}"),
            date: Utc::now() - Duration::minutes(offset_min),
            is_read: false,
            is_starred: false,
            account_id: String::new(),
        }
    }

    fn open_test_cache() -> Cache {
        let pool = pool::open_memory_pool().expect("open memory pool");
        let mut conn = pool.get().expect("checkout");
        schema::run_migrations(&mut conn).expect("migrate");
        // Drop the conn before the Cache uses the pool.
        drop(conn);
        Cache { pool }
    }

    #[test]
    fn migrations_are_idempotent() {
        let cache = open_test_cache();
        // Running again against the same pool should be a no-op.
        let mut conn = cache.pool.get().unwrap();
        schema::run_migrations(&mut conn).expect("second migrate");
    }

    #[test]
    fn upsert_and_read_envelopes_newest_first() {
        let cache = open_test_cache();
        let envs = vec![
            make_envelope(1, "INBOX", 30), // older
            make_envelope(2, "INBOX", 10), // newer
            make_envelope(3, "INBOX", 20),
        ];
        cache
            .upsert_envelopes_for_account("acc-1", &envs)
            .expect("upsert");

        let got = cache.get_envelopes("acc-1", "INBOX", 10).expect("read");
        assert_eq!(got.len(), 3);
        // Newest first: uid 2, then 3, then 1
        assert_eq!(got[0].uid, 2);
        assert_eq!(got[1].uid, 3);
        assert_eq!(got[2].uid, 1);
    }

    #[test]
    fn upsert_refreshes_flags() {
        let cache = open_test_cache();
        let mut env = make_envelope(42, "INBOX", 5);
        cache
            .upsert_envelopes_for_account("acc", std::slice::from_ref(&env))
            .unwrap();

        env.is_read = true;
        env.is_starred = true;
        cache
            .upsert_envelopes_for_account("acc", std::slice::from_ref(&env))
            .unwrap();

        let got = cache.get_envelopes("acc", "INBOX", 5).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].is_read);
        assert!(got[0].is_starred);
    }

    fn make_email(uid: u32, folder: &str) -> Email {
        Email {
            id: format!("{folder}:{uid}"),
            account_id: "acc".to_string(),
            folder: folder.to_string(),
            from: "alice@example.com".into(),
            to: vec!["bob@example.com".into(), "carol@example.com".into()],
            cc: vec!["dave@example.com".into()],
            subject: format!("Hello {uid}"),
            body_text: Some("plain body".into()),
            body_html: Some("<p>html body</p>".into()),
            date: Utc::now(),
            is_read: false,
            is_starred: false,
            has_attachments: true,
            attachments: vec![],
        }
    }

    #[test]
    fn message_roundtrip() {
        let cache = open_test_cache();
        assert!(cache.get_message("acc", "INBOX", 7).unwrap().is_none());

        let email = make_email(7, "INBOX");
        cache.upsert_message(&email).unwrap();

        let got = cache.get_message("acc", "INBOX", 7).unwrap().unwrap();
        assert_eq!(got.subject, "Hello 7");
        assert_eq!(got.body_text.as_deref(), Some("plain body"));
        assert_eq!(got.body_html.as_deref(), Some("<p>html body</p>"));
        assert_eq!(got.to, vec!["bob@example.com", "carol@example.com"]);
        assert_eq!(got.cc, vec!["dave@example.com"]);
        assert!(got.has_attachments);

        // Envelope side is also populated by upsert_message.
        let envs = cache.get_envelopes("acc", "INBOX", 5).unwrap();
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].uid, 7);
    }

    #[test]
    fn wipe_account_clears_everything() {
        let cache = open_test_cache();
        cache.upsert_message(&make_email(1, "INBOX")).unwrap();

        cache.wipe_account("acc").unwrap();

        assert!(cache.get_envelopes("acc", "INBOX", 5).unwrap().is_empty());
        assert!(cache.get_message("acc", "INBOX", 1).unwrap().is_none());
    }

    #[test]
    fn folders_roundtrip() {
        let cache = open_test_cache();
        let folders = vec![
            Folder {
                name: "INBOX".into(),
                delimiter: Some("/".into()),
                attributes: vec!["\\HasNoChildren".into()],
                unread_count: Some(3),
            },
            Folder {
                name: "Sent".into(),
                delimiter: Some("/".into()),
                attributes: vec!["\\Sent".into(), "\\HasNoChildren".into()],
                unread_count: None,
            },
        ];
        cache.upsert_folders("acc", &folders).unwrap();

        let got = cache.get_folders("acc").unwrap();
        assert_eq!(got.len(), 2);
        // Ordered alphabetically: INBOX, Sent.
        assert_eq!(got[0].name, "INBOX");
        assert_eq!(got[0].unread_count, Some(3));
        assert_eq!(got[1].name, "Sent");
        assert_eq!(got[1].attributes, vec!["\\Sent", "\\HasNoChildren"]);

        // Replacing the list wipes the previous rows.
        cache
            .upsert_folders(
                "acc",
                &[Folder {
                    name: "Archive".into(),
                    delimiter: None,
                    attributes: vec![],
                    unread_count: None,
                }],
            )
            .unwrap();
        let got = cache.get_folders("acc").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "Archive");
    }

    #[test]
    fn wipe_folder_is_scoped() {
        let cache = open_test_cache();
        cache.upsert_message(&make_email(1, "INBOX")).unwrap();
        cache.upsert_message(&make_email(2, "Sent")).unwrap();
        cache
            .set_sync_state(
                "acc",
                "INBOX",
                &SyncState {
                    uidvalidity: Some(1),
                    highest_uid_seen: Some(1),
                    last_synced_at: Some(Utc::now()),
                },
            )
            .unwrap();

        cache.wipe_folder("acc", "INBOX").unwrap();

        // INBOX is gone…
        assert!(cache.get_envelopes("acc", "INBOX", 5).unwrap().is_empty());
        assert!(cache.get_message("acc", "INBOX", 1).unwrap().is_none());
        assert!(cache.get_sync_state("acc", "INBOX").unwrap().is_none());
        // …but Sent is untouched.
        assert_eq!(cache.get_envelopes("acc", "Sent", 5).unwrap().len(), 1);
        assert!(cache.get_message("acc", "Sent", 2).unwrap().is_some());
    }

    #[test]
    fn uid_from_email_id_handles_colons_in_folder() {
        assert_eq!(uid_from_email_id("INBOX:42"), 42);
        assert_eq!(uid_from_email_id("Foo:Bar:99"), 99);
        assert_eq!(uid_from_email_id("garbage"), 0);
    }

    #[test]
    fn sync_state_roundtrip() {
        let cache = open_test_cache();
        let now = Utc::now();
        let st = SyncState {
            uidvalidity: Some(1234),
            highest_uid_seen: Some(99),
            last_synced_at: Some(now),
        };
        cache.set_sync_state("acc", "INBOX", &st).unwrap();
        let got = cache.get_sync_state("acc", "INBOX").unwrap().unwrap();
        assert_eq!(got.uidvalidity, Some(1234));
        assert_eq!(got.highest_uid_seen, Some(99));
        // Timestamps round-trip to whole seconds.
        assert_eq!(got.last_synced_at.unwrap().timestamp(), now.timestamp());
    }
}
