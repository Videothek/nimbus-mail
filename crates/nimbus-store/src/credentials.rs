//! Credential storage via the OS keychain.
//!
//! Passwords are never written to `accounts.json` — they live in:
//!   - **Windows**: Credential Manager
//!   - **macOS**: Keychain
//!   - **Linux**: Secret Service (GNOME Keyring, KWallet, ...)
//!
//! We key each secret by `(service, account_id)` where `service` is a
//! constant string and `account_id` is the account's UUID. Using the UUID
//! means the keychain entry is stable even if the user changes their email
//! address, and avoids collisions when two accounts share an email.

use keyring::Entry;
use nimbus_core::NimbusError;
use tracing::{debug, info};

/// Keychain service name for IMAP passwords.
/// Shown to the user in Credential Manager / Keychain Access as the "service".
const IMAP_SERVICE: &str = "nimbus-mail-imap";

fn entry(account_id: &str) -> Result<Entry, NimbusError> {
    Entry::new(IMAP_SERVICE, account_id)
        .map_err(|e| NimbusError::Storage(format!("keychain entry init failed: {e}")))
}

/// Store (or overwrite) the IMAP password for an account.
pub fn store_imap_password(account_id: &str, password: &str) -> Result<(), NimbusError> {
    entry(account_id)?
        .set_password(password)
        .map_err(|e| NimbusError::Storage(format!("failed to store password: {e}")))?;
    info!("Stored IMAP password for account '{account_id}' in OS keychain");
    Ok(())
}

/// Retrieve the IMAP password for an account.
pub fn get_imap_password(account_id: &str) -> Result<String, NimbusError> {
    entry(account_id)?.get_password().map_err(|e| {
        NimbusError::Auth(format!("no password found for account '{account_id}': {e}"))
    })
}

/// Remove the IMAP password for an account. Silently succeeds if the entry
/// doesn't exist — useful during account removal where we can't be sure
/// whether a password was ever stored.
pub fn delete_imap_password(account_id: &str) -> Result<(), NimbusError> {
    match entry(account_id)?.delete_credential() {
        Ok(()) => {
            info!("Deleted IMAP password for account '{account_id}'");
            Ok(())
        }
        Err(keyring::Error::NoEntry) => {
            debug!("No password to delete for account '{account_id}' (ok)");
            Ok(())
        }
        Err(e) => Err(NimbusError::Storage(format!(
            "failed to delete password: {e}"
        ))),
    }
}
