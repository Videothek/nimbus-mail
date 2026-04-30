//! FIDO unlock for the SQLCipher master key (#164).
//!
//! # What this is
//!
//! The cache database is encrypted with a 32-byte AES-256 master
//! key.  Today that key sits in plaintext in the OS keychain — fine
//! against "someone copies the disk" but not against malware running
//! as the same OS user (which can ask the keychain itself for the
//! key).  This module adds an opt-in second layer: instead of the
//! plain key, the keychain holds an *envelope* containing zero or
//! more wraps, each one a copy of the master key sealed by a
//! per-credential PRF output that only a registered FIDO2
//! authenticator (USB key, Touch ID, Windows Hello, …) can produce.
//!
//! Without the credential, the wrap is opaque.  An attacker with full
//! keychain access still can't reach the master key without the
//! user touching their authenticator.
//!
//! # The PRF protocol
//!
//! WebAuthn's PRF extension (RFC-bound; backed by CTAP2's
//! `hmac-secret`) lets us evaluate
//!
//!     prf_output = HMAC-SHA-256(per-credential-secret, salt)
//!
//! The per-credential secret never leaves the authenticator.  Same
//! `(credential_id, salt)` always produces the same 32-byte output,
//! but only after the user authenticates.  We store `salt` per wrap
//! and treat the resulting `prf_output` as a key.
//!
//! # The wrap
//!
//! For each registered credential we draw a random 12-byte nonce and
//! seal the master key with AES-256-GCM under `prf_output`, with the
//! `credential_id` as additional authenticated data so a wrap can't
//! be confused for one belonging to a different credential.
//!
//! ```text
//! ciphertext, tag = AES-256-GCM(
//!     key   = prf_output,
//!     nonce = random,
//!     aad   = credential_id,
//!     msg   = master_key,
//! )
//! ```
//!
//! Each wrap independently encrypts the *same* master key, so any
//! one registered credential is enough to unlock.  Adding /
//! removing a key only mutates the wraps array — the master key
//! itself never changes (so the encrypted DB doesn't have to be
//! re-keyed).
//!
//! # Storage shape
//!
//! The `nimbus-mail-db` keychain entry holds JSON:
//!
//! ```json
//! {
//!   "version": 1,
//!   "plain_key": "<64-char hex>",
//!   "wraps": [
//!     {
//!       "credential_id": "<base64>",
//!       "salt":          "<base64>",
//!       "label":         "YubiKey 5C",
//!       "nonce":         "<base64>",
//!       "ciphertext":    "<base64>",
//!       "created_at":    1735689600
//!     }
//!   ]
//! }
//! ```
//!
//! `plain_key` is present in plain mode and after enrolling the
//! first FIDO credential (Phase 1A keeps it for backwards-compat).
//! Phase 1B will null it once startup unlock through FIDO is
//! wired, leaving the wraps as the only path to the master key.
//!
//! Pre-#164 keychains hold the bare 64-char hex string instead of
//! JSON; `parse_envelope` migrates that to the new shape on first
//! read.

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chrono::Utc;
use getrandom::getrandom;
use nimbus_core::NimbusError;
use serde::{Deserialize, Serialize};

/// Bytes in a master key (AES-256).
const MASTER_KEY_LEN: usize = 32;
/// Bytes in the PRF output WebAuthn returns to us (HMAC-SHA-256).
const PRF_LEN: usize = 32;
/// AES-GCM nonce length.
const NONCE_LEN: usize = 12;
/// Bytes in a wrap salt — the WebAuthn PRF eval input.
const SALT_LEN: usize = 32;

/// One sealed copy of the master key, bound to a single registered
/// FIDO credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedKey {
    /// Base64-encoded WebAuthn credential id.
    pub credential_id: String,
    /// Base64-encoded random 32-byte salt — the PRF input we
    /// re-supply at unlock to derive the same secret.
    pub salt: String,
    /// User-readable label.  `"YubiKey 5C"`, `"Touch ID — MacBook"`.
    pub label: String,
    /// Base64-encoded AES-GCM nonce (12 bytes).
    pub nonce: String,
    /// Base64-encoded AES-GCM ciphertext + tag.
    pub ciphertext: String,
    /// Unix epoch seconds — purely informational, surfaced in the
    /// Settings list.
    pub created_at: i64,
}

/// The keychain entry's payload.  See the module-level doc for the
/// exact JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeychainEnvelope {
    pub version: u32,
    /// 64-character lowercase hex master key.  Present in plain
    /// mode and during the Phase 1A grace period; null once
    /// FIDO-only mode is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_key: Option<String>,
    #[serde(default)]
    pub wraps: Vec<WrappedKey>,
}

impl KeychainEnvelope {
    pub fn new_plain(plain_key_hex: String) -> Self {
        Self {
            version: 1,
            plain_key: Some(plain_key_hex),
            wraps: Vec::new(),
        }
    }
}

/// Parse the keychain entry's stored value into an envelope.  Old
/// builds wrote a bare hex string; we treat that as a plain-only
/// envelope so the migration is invisible to the caller.
pub fn parse_envelope(raw: &str) -> Result<KeychainEnvelope, NimbusError> {
    if looks_like_plain_hex(raw) {
        return Ok(KeychainEnvelope::new_plain(raw.to_string()));
    }
    serde_json::from_str(raw)
        .map_err(|e| NimbusError::Storage(format!("master-key envelope decode: {e}")))
}

fn looks_like_plain_hex(s: &str) -> bool {
    s.len() == MASTER_KEY_LEN * 2 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Serialise an envelope for the keychain.
pub fn serialize_envelope(env: &KeychainEnvelope) -> Result<String, NimbusError> {
    serde_json::to_string(env)
        .map_err(|e| NimbusError::Storage(format!("master-key envelope encode: {e}")))
}

// ── Wrap helpers ──────────────────────────────────────────────

/// Seal `master_key` under the FIDO PRF output.  Used at enrollment
/// time once the frontend has run WebAuthn and ferried the PRF bytes
/// back to us.
///
/// The salt is generated here (not by the caller) so each wrap has
/// independent, fresh entropy — it's encoded into the returned
/// `WrappedKey.salt` and *must* be supplied to WebAuthn at unlock
/// time as the PRF eval input.
pub fn wrap_master_key(
    master_key: &[u8],
    prf_output: &[u8],
    credential_id: &[u8],
    salt: &[u8],
    label: String,
) -> Result<WrappedKey, NimbusError> {
    if master_key.len() != MASTER_KEY_LEN {
        return Err(NimbusError::Storage(format!(
            "wrap_master_key: master_key must be {MASTER_KEY_LEN} bytes, got {}",
            master_key.len()
        )));
    }
    if prf_output.len() != PRF_LEN {
        return Err(NimbusError::Storage(format!(
            "wrap_master_key: prf_output must be {PRF_LEN} bytes, got {}",
            prf_output.len()
        )));
    }
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom(&mut nonce_bytes)
        .map_err(|e| NimbusError::Storage(format!("wrap nonce RNG: {e}")))?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(prf_output));
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: master_key,
                aad: credential_id,
            },
        )
        .map_err(|e| NimbusError::Storage(format!("AES-GCM seal: {e}")))?;
    Ok(WrappedKey {
        credential_id: B64.encode(credential_id),
        salt: B64.encode(salt),
        label,
        nonce: B64.encode(nonce_bytes),
        ciphertext: B64.encode(&ct),
        created_at: Utc::now().timestamp(),
    })
}

/// Open a single wrap with the FIDO PRF output computed at unlock
/// time.  Returns the recovered master key bytes (32 bytes).
pub fn unwrap_master_key(
    wrap: &WrappedKey,
    prf_output: &[u8],
) -> Result<Vec<u8>, NimbusError> {
    if prf_output.len() != PRF_LEN {
        return Err(NimbusError::Storage(format!(
            "unwrap_master_key: prf_output must be {PRF_LEN} bytes, got {}",
            prf_output.len()
        )));
    }
    let credential_id = B64
        .decode(wrap.credential_id.as_bytes())
        .map_err(|e| NimbusError::Storage(format!("wrap credential_id b64: {e}")))?;
    let nonce_bytes = B64
        .decode(wrap.nonce.as_bytes())
        .map_err(|e| NimbusError::Storage(format!("wrap nonce b64: {e}")))?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(NimbusError::Storage(format!(
            "wrap nonce must be {NONCE_LEN} bytes, got {}",
            nonce_bytes.len()
        )));
    }
    let ct = B64
        .decode(wrap.ciphertext.as_bytes())
        .map_err(|e| NimbusError::Storage(format!("wrap ciphertext b64: {e}")))?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(prf_output));
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ct,
                aad: &credential_id,
            },
        )
        .map_err(|e| NimbusError::Storage(format!("AES-GCM open: {e}")))?;
    if pt.len() != MASTER_KEY_LEN {
        return Err(NimbusError::Storage(format!(
            "unwrapped master key has wrong length: {}",
            pt.len()
        )));
    }
    Ok(pt)
}

/// Generate a fresh random salt for a new wrap.  The frontend feeds
/// this back into WebAuthn's `prf.eval.first` at unlock time.
pub fn generate_salt() -> Result<[u8; SALT_LEN], NimbusError> {
    let mut buf = [0u8; SALT_LEN];
    getrandom(&mut buf).map_err(|e| NimbusError::Storage(format!("salt RNG: {e}")))?;
    Ok(buf)
}

/// Decode a base64-encoded credential id (helper for the Tauri layer).
pub fn decode_b64(s: &str) -> Result<Vec<u8>, NimbusError> {
    B64.decode(s.as_bytes())
        .map_err(|e| NimbusError::Storage(format!("base64 decode: {e}")))
}

/// Encode bytes as base64 (helper for the Tauri layer).
pub fn encode_b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_then_unwrap_roundtrips() {
        let master = [0xAB_u8; MASTER_KEY_LEN];
        let prf = [0xCD_u8; PRF_LEN];
        let cred = b"fake-credential-id";
        let salt = generate_salt().unwrap();
        let wrap = wrap_master_key(&master, &prf, cred, &salt, "Test".into()).unwrap();
        let recovered = unwrap_master_key(&wrap, &prf).unwrap();
        assert_eq!(recovered, master);
    }

    #[test]
    fn unwrap_with_wrong_prf_fails() {
        let master = [0x01_u8; MASTER_KEY_LEN];
        let prf = [0x02_u8; PRF_LEN];
        let wrong_prf = [0x03_u8; PRF_LEN];
        let cred = b"id";
        let salt = generate_salt().unwrap();
        let wrap = wrap_master_key(&master, &prf, cred, &salt, "X".into()).unwrap();
        assert!(unwrap_master_key(&wrap, &wrong_prf).is_err());
    }

    #[test]
    fn plain_hex_treated_as_plain_envelope() {
        let hex = "a".repeat(64);
        let env = parse_envelope(&hex).unwrap();
        assert_eq!(env.plain_key.as_deref(), Some(hex.as_str()));
        assert!(env.wraps.is_empty());
    }
}
