//! Persistent Nextcloud-connection storage backed by a JSON file.
//!
//! Mirrors `account_store.rs` in shape: one JSON file at
//! `<config-dir>/nimbus-mail/nextcloud_accounts.json` containing the
//! full list, read-whole/write-whole on every mutation. That's fine for
//! a handful of connections and keeps this file dead-simple to inspect
//! by hand.
//!
//! # Why a second file
//!
//! Nextcloud connections are independent of mail accounts — see the
//! doc comment on `NextcloudAccount`. Keeping them in their own file
//! means removing a mail account can never accidentally nuke a
//! Nextcloud connection, and users can back them up separately.

use std::path::PathBuf;

use nimbus_core::NimbusError;
use nimbus_core::models::NextcloudAccount;
use tracing::{debug, info};

fn file_path() -> Result<PathBuf, NimbusError> {
    let data_dir = dirs::config_dir()
        .ok_or_else(|| NimbusError::Storage("cannot determine config directory".into()))?;
    Ok(data_dir
        .join("nimbus-mail")
        .join("nextcloud_accounts.json"))
}

/// Load all saved Nextcloud connections. Empty list on first run.
pub fn load_accounts() -> Result<Vec<NextcloudAccount>, NimbusError> {
    let path = file_path()?;
    if !path.exists() {
        debug!("No Nextcloud accounts file at {}", path.display());
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| NimbusError::Storage(format!("read nextcloud_accounts.json: {e}")))?;
    let accts: Vec<NextcloudAccount> = serde_json::from_str(&data)
        .map_err(|e| NimbusError::Storage(format!("parse nextcloud_accounts.json: {e}")))?;
    info!("Loaded {} Nextcloud connection(s)", accts.len());
    Ok(accts)
}

fn save_accounts(accts: &[NextcloudAccount]) -> Result<(), NimbusError> {
    let path = file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| NimbusError::Storage(format!("create config dir: {e}")))?;
    }
    let json = serde_json::to_string_pretty(accts)
        .map_err(|e| NimbusError::Storage(format!("serialize nextcloud accounts: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| NimbusError::Storage(format!("write nextcloud_accounts.json: {e}")))?;
    debug!("Saved {} Nextcloud connection(s)", accts.len());
    Ok(())
}

/// Add a new Nextcloud connection, or overwrite one with the same id.
///
/// Overwrite semantics are deliberate: re-running Login Flow v2 for the
/// same logical server/user produces a fresh app password; we want to
/// update in place rather than accumulating stale rows.
pub fn upsert_account(acct: NextcloudAccount) -> Result<(), NimbusError> {
    let mut accts = load_accounts()?;
    if let Some(existing) = accts.iter_mut().find(|a| a.id == acct.id) {
        info!(
            "Updating Nextcloud connection '{}' ({}@{})",
            acct.id, acct.username, acct.server_url
        );
        *existing = acct;
    } else {
        info!(
            "Adding Nextcloud connection '{}' ({}@{})",
            acct.id, acct.username, acct.server_url
        );
        accts.push(acct);
    }
    save_accounts(&accts)
}

/// Remove a Nextcloud connection by id.
pub fn remove_account(id: &str) -> Result<(), NimbusError> {
    let mut accts = load_accounts()?;
    let before = accts.len();
    accts.retain(|a| a.id != id);
    if accts.len() == before {
        return Err(NimbusError::Storage(format!(
            "no Nextcloud connection with id '{id}'"
        )));
    }
    info!("Removed Nextcloud connection '{id}'");
    save_accounts(&accts)
}
