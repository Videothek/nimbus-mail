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

use crate::fido::{KeychainEnvelope, WrappedKey, parse_envelope, serialize_envelope};

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
///
/// Reads the keychain envelope (#164).  In **plain mode** (or the
/// pre-FIDO format with a bare hex string), the envelope's
/// `plain_key` is the answer.  When the envelope is in FIDO-only
/// mode (no `plain_key`), the caller must instead unwrap one of the
/// stored wraps via [`unlock_with_prf`].
pub fn get_or_create_master_key() -> Result<String, NimbusError> {
    let entry = entry()?;
    match entry.get_password() {
        Ok(raw) => {
            let env = parse_envelope(&raw)?;
            if let Some(hex) = env.plain_key.as_deref() {
                if hex.len() != KEY_LEN * 2 {
                    return Err(NimbusError::Storage(format!(
                        "unexpected master key length: {} chars (expected {})",
                        hex.len(),
                        KEY_LEN * 2
                    )));
                }
                debug!("Loaded existing DB master key from keychain");
                Ok(hex.to_string())
            } else {
                Err(NimbusError::Auth(
                    "Database is in FIDO-only mode — call unlock_with_prf first".into(),
                ))
            }
        }
        Err(keyring::Error::NoEntry) => {
            info!("No DB master key in keychain — generating a new one");
            let hex_key = generate_hex_key()?;
            let env = KeychainEnvelope::new_plain(hex_key.clone());
            entry
                .set_password(&serialize_envelope(&env)?)
                .map_err(|e| NimbusError::Storage(format!("failed to store master key: {e}")))?;
            Ok(hex_key)
        }
        Err(e) => Err(NimbusError::Storage(format!(
            "failed to read master key: {e}"
        ))),
    }
}

/// Read the raw keychain envelope.  Used by FIDO management
/// commands (list / remove credentials) and by the boot path that
/// decides whether to show the lock screen.
pub fn load_envelope() -> Result<KeychainEnvelope, NimbusError> {
    let entry = entry()?;
    match entry.get_password() {
        Ok(raw) => parse_envelope(&raw),
        Err(keyring::Error::NoEntry) => Ok(KeychainEnvelope {
            version: 1,
            plain_key: None,
            wraps: Vec::new(),
        }),
        Err(e) => Err(NimbusError::Storage(format!(
            "failed to read master key: {e}"
        ))),
    }
}

/// Persist a mutated envelope back to the keychain.
pub fn save_envelope(env: &KeychainEnvelope) -> Result<(), NimbusError> {
    let entry = entry()?;
    entry
        .set_password(&serialize_envelope(env)?)
        .map_err(|e| NimbusError::Storage(format!("failed to store master key: {e}")))
}

/// Append (or replace) a wrap in the envelope, keyed on
/// `credential_id` so re-enrolling the same authenticator just
/// updates the existing entry.
pub fn add_wrap(new: WrappedKey) -> Result<(), NimbusError> {
    let mut env = load_envelope()?;
    env.wraps.retain(|w| w.credential_id != new.credential_id);
    env.wraps.push(new);
    save_envelope(&env)
}

/// Remove a wrap by credential id.  Returns whether something was
/// actually removed (caller may want to surface "no such credential"
/// to the user).
pub fn remove_wrap(credential_id_b64: &str) -> Result<bool, NimbusError> {
    let mut env = load_envelope()?;
    let before = env.wraps.len();
    env.wraps.retain(|w| w.credential_id != credential_id_b64);
    let removed = env.wraps.len() < before;
    if removed {
        save_envelope(&env)?;
    }
    Ok(removed)
}

fn generate_hex_key() -> Result<String, NimbusError> {
    let mut buf = [0u8; KEY_LEN];
    getrandom(&mut buf).map_err(|e| NimbusError::Storage(format!("RNG failed: {e}")))?;
    Ok(hex::encode(buf))
}
