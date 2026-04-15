//! Connection pool and PRAGMA tuning for the local mail cache.
//!
//! SQLite is a single-file embedded database. "Scaling" it means choosing
//! sane PRAGMAs and serialising writes; the library choice matters less
//! than those. The settings below are the standard high-throughput
//! desktop-app recipe:
//!
//! - **WAL (Write-Ahead Logging)** — readers never block writers and vice
//!   versa. Without WAL, every write takes an exclusive lock on the whole
//!   DB, which would freeze the UI while the sync thread writes envelopes.
//!
//! - **`synchronous = NORMAL`** — WAL is checkpointed at shutdown rather
//!   than on every commit. Safe (we still survive crashes, just not a
//!   hard OS-level power loss mid-commit) and dramatically faster.
//!
//! - **`busy_timeout = 5000ms`** — if a second connection finds the write
//!   lock held, wait up to 5s instead of failing immediately. This papers
//!   over brief contention between the sync thread and a UI query.
//!
//! - **`foreign_keys = ON`** — SQLite ignores FK constraints by default;
//!   we want `ON DELETE CASCADE` on `message_bodies` to actually fire.
//!
//! # Why r2d2
//!
//! A pool lets N reader connections run concurrently against a WAL
//! database — useful once we start doing searches or background sync
//! while the UI is also pulling envelopes. One pool, shared by the
//! whole app, created once at startup.

use std::path::Path;
use std::time::Duration;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

use crate::cache::CacheError;

pub type SqlitePool = Pool<SqliteConnectionManager>;

/// Apply the scaling-oriented PRAGMAs to a freshly opened connection.
///
/// r2d2 calls this for every new pooled connection via `.with_init(...)`,
/// so every connection in the pool starts with the same settings.
fn apply_pragmas(conn: &mut Connection) -> rusqlite::Result<()> {
    // WAL is persistent on the file — setting it on any connection applies
    // to the whole DB. Still, setting it per-connection is harmless and
    // makes the intent explicit.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    Ok(())
}

/// Open (or create) the cache database at `path` and return a ready-to-use pool.
pub fn open_pool(path: &Path) -> Result<SqlitePool, CacheError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CacheError::Open(format!("create cache dir: {e}")))?;
    }

    let manager = SqliteConnectionManager::file(path).with_init(apply_pragmas);

    // Small pool — a desktop app rarely benefits from more than a handful
    // of connections. The cost of a connection is ~100KB of SQLite state.
    Pool::builder()
        .max_size(8)
        .build(manager)
        .map_err(|e| CacheError::Open(format!("build pool: {e}")))
}

/// Convenience: open an in-memory pool with a unique, process-local name.
///
/// Each call gets a fresh memory DB that only its own pool can see — this
/// isolates tests that run in parallel. The `file::<name>:?mode=memory&
/// cache=shared` URI lets multiple pooled connections share one DB while
/// the counter-driven name prevents other tests from crashing into it.
#[cfg(test)]
pub(crate) fn open_memory_pool() -> Result<SqlitePool, CacheError> {
    use rusqlite::OpenFlags;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let uri = format!("file:nimbus_test_mem_{id}?mode=memory&cache=shared");
    // `SQLITE_OPEN_URI` is required for the URI form to be honoured.
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI;
    let manager = SqliteConnectionManager::file(uri)
        .with_flags(flags)
        .with_init(apply_pragmas);
    Pool::builder()
        .max_size(4)
        .build(manager)
        .map_err(|e| CacheError::Open(format!("build memory pool: {e}")))
}
