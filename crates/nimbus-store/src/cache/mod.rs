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
//! # Write-through pattern
//!
//! For now the Tauri commands still fetch from the IMAP server on every
//! call and call into the cache to *write* what they got. A follow-up PR
//! will flip this: reads hit the cache first and the server refresh runs
//! in the background. Keeping the two changes separate makes it easy to
//! back out if the UI path has bugs.
//!
//! # Thread-safety
//!
//! The cache holds an `r2d2` pool internally. Every method checks out a
//! connection, does its work, and returns it. The pool is internally
//! synchronised so `Cache` is cheap to `clone()` and share across tasks.

pub mod pool;
pub mod schema;

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use nimbus_core::NimbusError;
use nimbus_core::models::{EmailEnvelope, Folder};
use rusqlite::{OptionalExtension, params};
use thiserror::Error;
use tracing::{debug, info};

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
    pub fn open_default() -> Result<Self, NimbusError> {
        let path = default_cache_path()?;
        Self::open(&path).map_err(Into::into)
    }

    /// Open a cache at an explicit path (used by tests and for future
    /// multi-profile support).
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        info!("Opening mail cache at {}", path.display());
        let pool = pool::open_pool(path)?;
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

    // ── Folders ─────────────────────────────────────────────────

    /// Replace the cached folder list for an account.
    ///
    /// Folder names can change (user renames, server-side mailbox removal),
    /// so we wipe-and-reinsert inside a transaction rather than trying to
    /// diff. The folder list is small (dozens of rows at most) so this
    /// is effectively free.
    pub fn upsert_folders(
        &self,
        account_id: &str,
        folders: &[Folder],
    ) -> Result<(), CacheError> {
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
        debug!("Cached {} folders for account '{account_id}'", folders.len());
        Ok(())
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
            let date = Utc
                .timestamp_opt(ts, 0)
                .single()
                .unwrap_or_else(Utc::now);
            Ok(EmailEnvelope {
                uid: r.get::<_, i64>(0)? as u32,
                folder: r.get(1)?,
                from: r.get(2)?,
                subject: r.get(3)?,
                date,
                is_read: r.get::<_, i64>(5)? != 0,
                is_starred: r.get::<_, i64>(6)? != 0,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ── Message bodies ──────────────────────────────────────────

    /// Upsert a cached message body. Run in the same transaction-by-default
    /// SQLite mode as everything else; the `messages` row must already exist
    /// (foreign key) so callers should upsert the envelope first.
    #[allow(clippy::too_many_arguments)] // one positional per column is clearer than a wrapper struct for a single call site
    pub fn upsert_body(
        &self,
        account_id: &str,
        folder: &str,
        uid: u32,
        body_text: Option<&str>,
        body_html: Option<&str>,
        has_attachments: bool,
        raw_size: Option<usize>,
    ) -> Result<(), CacheError> {
        let conn = self.pool.get()?;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO message_bodies
                (account_id, folder, uid, body_text, body_html,
                 has_attachments, raw_size, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (account_id, folder, uid) DO UPDATE SET
                body_text       = excluded.body_text,
                body_html       = excluded.body_html,
                has_attachments = excluded.has_attachments,
                raw_size        = excluded.raw_size,
                cached_at       = excluded.cached_at",
            params![
                account_id,
                folder,
                uid as i64,
                body_text,
                body_html,
                has_attachments as i64,
                raw_size.map(|n| n as i64),
                now,
            ],
        )?;
        Ok(())
    }

    /// Look up a cached body by `(account_id, folder, uid)`.
    ///
    /// Returns `None` if we haven't fetched this message's body yet — the
    /// caller then falls back to the network.
    pub fn get_body(
        &self,
        account_id: &str,
        folder: &str,
        uid: u32,
    ) -> Result<Option<CachedBody>, CacheError> {
        let conn = self.pool.get()?;
        let row = conn
            .query_row(
                "SELECT body_text, body_html, has_attachments
                 FROM message_bodies
                 WHERE account_id = ?1 AND folder = ?2 AND uid = ?3",
                params![account_id, folder, uid as i64],
                |r| {
                    Ok(CachedBody {
                        body_text: r.get(0)?,
                        body_html: r.get(1)?,
                        has_attachments: r.get::<_, i64>(2)? != 0,
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
                        last_synced_at: ts
                            .and_then(|t| Utc.timestamp_opt(t, 0).single()),
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

/// A cached message body — the fields MailView needs to render without a
/// network round-trip. `is_read` / `is_starred` come from the `messages`
/// envelope row, not this one.
#[derive(Debug, Clone)]
pub struct CachedBody {
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub has_attachments: bool,
}

fn default_cache_path() -> Result<PathBuf, NimbusError> {
    let dir = dirs::config_dir()
        .ok_or_else(|| NimbusError::Storage("cannot determine config directory".into()))?;
    Ok(dir.join("nimbus-mail").join("cache.db"))
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

    #[test]
    fn body_roundtrip() {
        let cache = open_test_cache();
        let env = make_envelope(7, "INBOX", 0);
        cache
            .upsert_envelopes_for_account("acc", std::slice::from_ref(&env))
            .unwrap();

        assert!(cache.get_body("acc", "INBOX", 7).unwrap().is_none());

        cache
            .upsert_body("acc", "INBOX", 7, Some("hello"), None, true, Some(1234))
            .unwrap();

        let b = cache.get_body("acc", "INBOX", 7).unwrap().unwrap();
        assert_eq!(b.body_text.as_deref(), Some("hello"));
        assert!(b.body_html.is_none());
        assert!(b.has_attachments);
    }

    #[test]
    fn wipe_account_clears_everything() {
        let cache = open_test_cache();
        let env = make_envelope(1, "INBOX", 5);
        cache
            .upsert_envelopes_for_account("acc", std::slice::from_ref(&env))
            .unwrap();
        cache
            .upsert_body("acc", "INBOX", 1, Some("body"), None, false, None)
            .unwrap();

        cache.wipe_account("acc").unwrap();

        assert!(cache.get_envelopes("acc", "INBOX", 5).unwrap().is_empty());
        assert!(cache.get_body("acc", "INBOX", 1).unwrap().is_none());
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
        assert_eq!(
            got.last_synced_at.unwrap().timestamp(),
            now.timestamp()
        );
    }
}
