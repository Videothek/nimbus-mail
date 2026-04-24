//! Persistent account storage backed by the SQLCipher cache.
//!
//! Accounts used to live in a plaintext `accounts.json` next to the
//! database. Issue #60 moved them into the encrypted cache so the
//! whole account record (hosts, signatures, future TLS-trust state)
//! is encrypted at rest under the same key as the message bodies.
//!
//! # Migration
//!
//! Existing installs already have an `accounts.json`. The first call
//! to [`load_accounts`] after the upgrade detects an empty `accounts`
//! table next to a present-and-non-empty JSON file and imports it,
//! then renames the JSON to `accounts.json.bak` so the user can roll
//! back if anything goes wrong. Subsequent calls find a non-empty
//! table and skip the import. The whole dance is gated behind the
//! emptiness check, so nothing happens on fresh installs.

use std::path::PathBuf;

use nimbus_core::NimbusError;
use nimbus_core::models::Account;
use rusqlite::params;
use tracing::{debug, info, warn};

use crate::Cache;

/// Path of the legacy plaintext accounts file. Kept here (rather
/// than deleted with the rest of the JSON code) so the import
/// migration in [`load_accounts`] knows where to look.
fn legacy_json_path() -> Result<PathBuf, NimbusError> {
    let data_dir = dirs::config_dir()
        .ok_or_else(|| NimbusError::Storage("cannot determine config directory".into()))?;
    Ok(data_dir.join("nimbus-mail").join("accounts.json"))
}

/// Load all saved accounts. Returns an empty list on first launch
/// (no JSON, no rows). On the upgrade boundary — empty table next
/// to a populated `accounts.json` — runs the one-time JSON-to-SQLite
/// import and renames the JSON file to `.bak` so we don't try again.
///
/// The legacy-JSON probe is compiled out of tests so unit tests
/// against an in-memory cache don't accidentally pick up the
/// developer's real `accounts.json` from `dirs::config_dir()`.
pub fn load_accounts(cache: &Cache) -> Result<Vec<Account>, NimbusError> {
    let accounts = read_all(cache)?;
    #[cfg(not(test))]
    if accounts.is_empty() {
        if let Some(imported) = migrate_from_legacy_json(cache)? {
            return Ok(imported);
        }
    }
    Ok(accounts)
}

/// Read every row out of the cache, ordered by insertion. Used by
/// both `load_accounts` and the dedup check in `add_account`.
fn read_all(cache: &Cache) -> Result<Vec<Account>, NimbusError> {
    let conn = conn(cache)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, display_name, email, imap_host, imap_port,
                    smtp_host, smtp_port, use_jmap, jmap_url, signature,
                    folder_icons_json, trusted_certs_json,
                    folder_icon_overrides_json
             FROM accounts
             ORDER BY rowid",
        )
        .map_err(|e| NimbusError::Storage(format!("prepare load_accounts: {e}")))?;

    let rows = stmt
        .query_map([], row_to_account)
        .map_err(|e| NimbusError::Storage(format!("query load_accounts: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| NimbusError::Storage(format!("row load_accounts: {e}")))?);
    }
    debug!("Loaded {} account(s) from cache", out.len());
    Ok(out)
}

fn row_to_account(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let folder_icons_json: String = r.get(10)?;
    let folder_icons = serde_json::from_str(&folder_icons_json).unwrap_or_default();
    let trusted_certs_json: String = r.get(11)?;
    let trusted_certs = serde_json::from_str(&trusted_certs_json).unwrap_or_default();
    let folder_icon_overrides_json: String = r.get(12)?;
    let folder_icon_overrides =
        serde_json::from_str(&folder_icon_overrides_json).unwrap_or_default();
    Ok(Account {
        id: r.get(0)?,
        display_name: r.get(1)?,
        email: r.get(2)?,
        imap_host: r.get(3)?,
        imap_port: r.get::<_, i64>(4)? as u16,
        smtp_host: r.get(5)?,
        smtp_port: r.get::<_, i64>(6)? as u16,
        use_jmap: r.get::<_, i64>(7)? != 0,
        jmap_url: r.get(8)?,
        signature: r.get(9)?,
        folder_icons,
        trusted_certs,
        folder_icon_overrides,
    })
}

/// Add a new account. Errors if an account with this id already exists
/// — same contract the JSON-backed implementation had, so callers don't
/// need to change their handling.
pub fn add_account(cache: &Cache, account: Account) -> Result<(), NimbusError> {
    if read_all(cache)?.iter().any(|a| a.id == account.id) {
        return Err(NimbusError::Storage(format!(
            "account with id '{}' already exists",
            account.id
        )));
    }

    info!(
        "Adding account '{}' ({})",
        account.display_name, account.email
    );
    insert_one(cache, &account)
}

fn insert_one(cache: &Cache, account: &Account) -> Result<(), NimbusError> {
    let folder_icons_json = serde_json::to_string(&account.folder_icons)
        .map_err(|e| NimbusError::Storage(format!("serialize folder_icons: {e}")))?;
    let trusted_certs_json = serde_json::to_string(&account.trusted_certs)
        .map_err(|e| NimbusError::Storage(format!("serialize trusted_certs: {e}")))?;
    let folder_icon_overrides_json = serde_json::to_string(&account.folder_icon_overrides)
        .map_err(|e| NimbusError::Storage(format!("serialize folder_icon_overrides: {e}")))?;
    let conn = conn(cache)?;
    conn.execute(
        "INSERT INTO accounts
            (id, display_name, email, imap_host, imap_port,
             smtp_host, smtp_port, use_jmap, jmap_url, signature,
             folder_icons_json, trusted_certs_json,
             folder_icon_overrides_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            account.id,
            account.display_name,
            account.email,
            account.imap_host,
            account.imap_port as i64,
            account.smtp_host,
            account.smtp_port as i64,
            account.use_jmap as i64,
            account.jmap_url,
            account.signature,
            folder_icons_json,
            trusted_certs_json,
            folder_icon_overrides_json,
            chrono::Utc::now().timestamp(),
        ],
    )
    .map_err(|e| NimbusError::Storage(format!("insert account: {e}")))?;
    Ok(())
}

/// Remove an account by id. Returns an error if the id wasn't found.
pub fn remove_account(cache: &Cache, id: &str) -> Result<(), NimbusError> {
    let conn = conn(cache)?;
    let removed = conn
        .execute("DELETE FROM accounts WHERE id = ?1", params![id])
        .map_err(|e| NimbusError::Storage(format!("delete account: {e}")))?;

    if removed == 0 {
        return Err(NimbusError::Storage(format!(
            "no account found with id '{id}'"
        )));
    }
    info!("Removed account '{id}'");
    Ok(())
}

/// Update an existing account in place. The id must already exist —
/// matches the JSON-backed contract so callers don't have to branch.
pub fn update_account(cache: &Cache, updated: Account) -> Result<(), NimbusError> {
    let folder_icons_json = serde_json::to_string(&updated.folder_icons)
        .map_err(|e| NimbusError::Storage(format!("serialize folder_icons: {e}")))?;
    let trusted_certs_json = serde_json::to_string(&updated.trusted_certs)
        .map_err(|e| NimbusError::Storage(format!("serialize trusted_certs: {e}")))?;
    let folder_icon_overrides_json = serde_json::to_string(&updated.folder_icon_overrides)
        .map_err(|e| NimbusError::Storage(format!("serialize folder_icon_overrides: {e}")))?;
    let conn = conn(cache)?;
    let changed = conn
        .execute(
            "UPDATE accounts
             SET display_name               = ?2,
                 email                      = ?3,
                 imap_host                  = ?4,
                 imap_port                  = ?5,
                 smtp_host                  = ?6,
                 smtp_port                  = ?7,
                 use_jmap                   = ?8,
                 jmap_url                   = ?9,
                 signature                  = ?10,
                 folder_icons_json          = ?11,
                 trusted_certs_json         = ?12,
                 folder_icon_overrides_json = ?13
             WHERE id = ?1",
            params![
                updated.id,
                updated.display_name,
                updated.email,
                updated.imap_host,
                updated.imap_port as i64,
                updated.smtp_host,
                updated.smtp_port as i64,
                updated.use_jmap as i64,
                updated.jmap_url,
                updated.signature,
                folder_icons_json,
                trusted_certs_json,
                folder_icon_overrides_json,
            ],
        )
        .map_err(|e| NimbusError::Storage(format!("update account: {e}")))?;

    if changed == 0 {
        return Err(NimbusError::Storage(format!(
            "no account found with id '{}'",
            updated.id
        )));
    }
    info!(
        "Updated account '{}' ({})",
        updated.display_name, updated.email
    );
    Ok(())
}

/// One-time import from the legacy JSON file. Called from
/// [`load_accounts`] when the cache is empty — covers the upgrade
/// path where a user already had `accounts.json` populated.
///
/// On success the JSON file is renamed to `accounts.json.bak` so the
/// next launch finds an empty file-or-no-file and skips the import.
/// We rename rather than delete so the user can manually restore if
/// anything goes wrong with the import (we already saw a CalDAV-403
/// regression in #56 from a similar boundary).
fn migrate_from_legacy_json(cache: &Cache) -> Result<Option<Vec<Account>>, NimbusError> {
    let path = legacy_json_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            warn!("legacy accounts.json present but unreadable: {e}");
            return Ok(None);
        }
    };
    let imported: Vec<Account> = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            warn!("legacy accounts.json present but unparsable: {e}");
            return Ok(None);
        }
    };

    if imported.is_empty() {
        // Empty file — still rename it so we don't try every launch.
        if let Err(e) = std::fs::rename(&path, path.with_extension("json.bak")) {
            warn!("could not rename empty legacy accounts.json: {e}");
        }
        return Ok(None);
    }

    info!(
        "Migrating {} account(s) from {} into the encrypted cache",
        imported.len(),
        path.display()
    );
    for account in &imported {
        if let Err(e) = insert_one(cache, account) {
            // Best-effort: log and continue. A single bad row shouldn't
            // block the rest of the user's accounts from coming over.
            warn!(
                "skipping account '{}' during migration: {e}",
                account.display_name
            );
        }
    }

    if let Err(e) = std::fs::rename(&path, path.with_extension("json.bak")) {
        warn!("could not rename migrated accounts.json to .bak: {e}");
    } else {
        info!("Renamed legacy accounts.json → accounts.json.bak");
    }

    Ok(Some(read_all(cache)?))
}

/// Borrow a pooled connection. Wrapper around `Cache::pool().get()`
/// that converts the pool error into our own `NimbusError` so
/// callers can use `?` without sprinkling `.map_err` everywhere.
fn conn(
    cache: &Cache,
) -> Result<
    r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>,
    NimbusError,
> {
    cache
        .pool()
        .get()
        .map_err(|e| NimbusError::Storage(format!("checkout cache conn: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_cache() -> Cache {
        Cache::open_in_memory().expect("open in-memory cache")
    }

    fn test_account(id: &str) -> Account {
        Account {
            id: id.to_string(),
            display_name: "Test User".into(),
            email: "test@example.com".into(),
            imap_host: "imap.example.com".into(),
            imap_port: 993,
            smtp_host: "smtp.example.com".into(),
            smtp_port: 587,
            use_jmap: false,
            jmap_url: None,
            signature: None,
            folder_icons: Vec::new(),
            trusted_certs: Vec::new(),
            folder_icon_overrides: Default::default(),
        }
    }

    #[test]
    fn add_load_remove_roundtrip() {
        let cache = open_test_cache();
        assert!(load_accounts(&cache).unwrap().is_empty());

        add_account(&cache, test_account("a")).unwrap();
        add_account(
            &cache,
            Account {
                id: "b".into(),
                signature: Some("Best,\nNick".into()),
                ..test_account("b")
            },
        )
        .unwrap();

        let listed = load_accounts(&cache).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "a"); // insertion order preserved
        assert_eq!(listed[1].signature.as_deref(), Some("Best,\nNick"));

        // Duplicate id is rejected.
        assert!(add_account(&cache, test_account("a")).is_err());

        remove_account(&cache, "a").unwrap();
        let after_remove = load_accounts(&cache).unwrap();
        assert_eq!(after_remove.len(), 1);
        assert_eq!(after_remove[0].id, "b");

        // Removing an unknown id surfaces an error.
        assert!(remove_account(&cache, "missing").is_err());
    }

    #[test]
    fn update_replaces_fields() {
        let cache = open_test_cache();
        add_account(&cache, test_account("a")).unwrap();

        let renamed = Account {
            display_name: "Renamed".into(),
            email: "new@example.com".into(),
            signature: Some("sig".into()),
            ..test_account("a")
        };
        update_account(&cache, renamed).unwrap();

        let listed = load_accounts(&cache).unwrap();
        assert_eq!(listed[0].display_name, "Renamed");
        assert_eq!(listed[0].email, "new@example.com");
        assert_eq!(listed[0].signature.as_deref(), Some("sig"));

        // Updating an unknown id surfaces an error.
        assert!(update_account(&cache, test_account("missing")).is_err());
    }
}
