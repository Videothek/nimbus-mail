//! Persistent account storage backed by a JSON file.
//!
//! Accounts are stored in `<app-data-dir>/nimbus-mail/accounts.json`.
//! This is intentionally simple — we read the whole file on every
//! operation and write the whole file back. For a handful of accounts
//! this is perfectly fine and avoids the complexity of a database.

use std::path::PathBuf;

use nimbus_core::NimbusError;
use nimbus_core::models::Account;
use tracing::{debug, info};

/// Returns the path to the accounts JSON file.
///
/// On Windows this is typically:
///   `C:\Users\<you>\AppData\Roaming\nimbus-mail\accounts.json`
/// On macOS:
///   `~/Library/Application Support/nimbus-mail/accounts.json`
/// On Linux:
///   `~/.config/nimbus-mail/accounts.json`
fn accounts_file_path() -> Result<PathBuf, NimbusError> {
    let data_dir = dirs::config_dir()
        .ok_or_else(|| NimbusError::Storage("cannot determine config directory".into()))?;
    Ok(data_dir.join("nimbus-mail").join("accounts.json"))
}

/// Load all saved accounts from disk.
/// Returns an empty list if the file doesn't exist yet (first launch).
pub fn load_accounts() -> Result<Vec<Account>, NimbusError> {
    let path = accounts_file_path()?;

    if !path.exists() {
        debug!(
            "No accounts file found at {}, returning empty list",
            path.display()
        );
        return Ok(Vec::new());
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|e| NimbusError::Storage(format!("failed to read accounts file: {e}")))?;

    let accounts: Vec<Account> = serde_json::from_str(&data)
        .map_err(|e| NimbusError::Storage(format!("failed to parse accounts file: {e}")))?;

    info!(
        "Loaded {} account(s) from {}",
        accounts.len(),
        path.display()
    );
    Ok(accounts)
}

/// Save the full list of accounts to disk, creating directories if needed.
fn save_accounts(accounts: &[Account]) -> Result<(), NimbusError> {
    let path = accounts_file_path()?;

    // Ensure the parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| NimbusError::Storage(format!("failed to create config dir: {e}")))?;
    }

    let json = serde_json::to_string_pretty(accounts)
        .map_err(|e| NimbusError::Storage(format!("failed to serialize accounts: {e}")))?;

    std::fs::write(&path, json)
        .map_err(|e| NimbusError::Storage(format!("failed to write accounts file: {e}")))?;

    debug!("Saved {} account(s) to {}", accounts.len(), path.display());
    Ok(())
}

/// Add a new account and persist to disk.
pub fn add_account(account: Account) -> Result<(), NimbusError> {
    let mut accounts = load_accounts()?;

    // Prevent duplicate IDs
    if accounts.iter().any(|a| a.id == account.id) {
        return Err(NimbusError::Storage(format!(
            "account with id '{}' already exists",
            account.id
        )));
    }

    info!(
        "Adding account '{}' ({})",
        account.display_name, account.email
    );
    accounts.push(account);
    save_accounts(&accounts)
}

/// Remove an account by its ID.
pub fn remove_account(id: &str) -> Result<(), NimbusError> {
    let mut accounts = load_accounts()?;
    let before = accounts.len();
    accounts.retain(|a| a.id != id);

    if accounts.len() == before {
        return Err(NimbusError::Storage(format!(
            "no account found with id '{id}'"
        )));
    }

    info!("Removed account '{id}'");
    save_accounts(&accounts)
}

/// Update an existing account (matched by ID).
pub fn update_account(updated: Account) -> Result<(), NimbusError> {
    let mut accounts = load_accounts()?;

    let existing = accounts
        .iter_mut()
        .find(|a| a.id == updated.id)
        .ok_or_else(|| {
            NimbusError::Storage(format!("no account found with id '{}'", updated.id))
        })?;

    info!(
        "Updating account '{}' ({})",
        updated.display_name, updated.email
    );
    *existing = updated;
    save_accounts(&accounts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a test account.
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
        }
    }

    #[test]
    fn accounts_file_path_is_valid() {
        let path = accounts_file_path().expect("should resolve path");
        assert!(
            path.ends_with("nimbus-mail/accounts.json")
                || path.ends_with("nimbus-mail\\accounts.json")
        );
    }

    #[test]
    fn test_account_struct_is_serializable() {
        let acct = test_account("1");
        let json = serde_json::to_string(&acct).expect("should serialize");
        let parsed: Account = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(parsed.id, "1");
    }
}
