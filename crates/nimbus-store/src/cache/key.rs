//! Master key management for the SQLCipher-encrypted mail cache.
//!
//! # What this solves
//!
//! The cache DB is encrypted at rest with AES-256 via SQLCipher. For that
//! to work, every connection has to be unlocked with a key before it can
//! read or write. We don't want to prompt the user for a passphrase every
//! time the app starts — so instead we generate a high-entropy random key
//! once and store it in the OS keychain, which is already protected by
//! the user's login session.
//!
//! # Threat model
//!
//! What this protects against:
//! - Another user on the same machine copying `cache.db` — the key lives
//!   in *their* keychain, not the file, so they see gibberish.
//! - A stolen laptop where the disk is readable but the account is
//!   locked — keychain entries are gated on the OS user session.
//! - Backup drives, cloud sync, malware exfiltrating files from the app
//!   data directory.
//!
//! What this does *not* protect against:
//! - Malware running as the current user — it can ask the keychain for
//!   the key just like we do. (Folder-level encryption with a separate
//!   passphrase would cover this, and is planned as a follow-up.)
//! - A forensic image of RAM while the app is running.
//!
//! # Key format
//!
//! 32 random bytes (256 bits), hex-encoded (64 chars) for keychain
//! storage and for SQLCipher's `PRAGMA key = "x'<hex>'"` syntax.
//! SQLCipher treats a hex literal of the right length as a raw key and
//! skips PBKDF2 derivation, which is both faster and — since we already
//! have cryptographic randomness — just as secure as a derived key.

use getrandom::getrandom;
use keyring::Entry;
use nimbus_core::NimbusError;
use tracing::{debug, info};

/// Keychain service name for the DB master key. Separate from the IMAP
/// service so the entry is easy to spot in Credential Manager / Keychain
/// Access, and so revoking the DB key can't touch account passwords.
const DB_SERVICE: &str = "nimbus-mail-db";

/// Singleton account name inside the keychain service. There's exactly
/// one master key per install — we don't key per-account because the
/// accounts table itself lives in this DB.
const DB_ACCOUNT: &str = "master-key";

/// Byte length of the raw AES-256 key. Hex-encoded this becomes 64 chars.
const KEY_LEN: usize = 32;

fn entry() -> Result<Entry, NimbusError> {
    Entry::new(DB_SERVICE, DB_ACCOUNT)
        .map_err(|e| NimbusError::Storage(format!("keychain entry init failed: {e}")))
}

/// Fetch the master key, generating and persisting a new one on first run.
///
/// Returned as a 64-character lowercase hex string, ready to be embedded
/// directly into a `PRAGMA key = "x'<hex>'"` statement.
pub fn get_or_create_master_key() -> Result<String, NimbusError> {
    let entry = entry()?;
    match entry.get_password() {
        Ok(hex_key) if hex_key.len() == KEY_LEN * 2 => {
            debug!("Loaded existing DB master key from keychain");
            Ok(hex_key)
        }
        Ok(other) => {
            // Somehow a wrong-length value ended up in the slot — almost
            // certainly a past-version bug or manual tampering. Refuse to
            // proceed rather than silently generate a new one, which would
            // orphan whatever encrypted DB the old key was unlocking.
            Err(NimbusError::Storage(format!(
                "unexpected master key length in keychain: {} chars (expected {})",
                other.len(),
                KEY_LEN * 2
            )))
        }
        Err(keyring::Error::NoEntry) => {
            info!("No DB master key in keychain — generating a new one");
            let hex_key = generate_hex_key()?;
            entry
                .set_password(&hex_key)
                .map_err(|e| NimbusError::Storage(format!("failed to store master key: {e}")))?;
            Ok(hex_key)
        }
        Err(e) => Err(NimbusError::Storage(format!(
            "failed to read master key: {e}"
        ))),
    }
}

fn generate_hex_key() -> Result<String, NimbusError> {
    let mut buf = [0u8; KEY_LEN];
    getrandom(&mut buf).map_err(|e| NimbusError::Storage(format!("RNG failed: {e}")))?;
    Ok(hex::encode(buf))
}
