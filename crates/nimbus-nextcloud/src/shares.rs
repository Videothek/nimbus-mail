//! Nextcloud public share links via the OCS Files Sharing API.
//!
//! # Why a separate module from `files`
//!
//! `files.rs` speaks **WebDAV** — a low-level resource protocol on
//! `/remote.php/dav/...`. Sharing speaks **OCS** — a higher-level JSON
//! API on `/ocs/v2.php/apps/files_sharing/...`. Different endpoint,
//! different content type, different response envelope. Keeping them
//! apart means each module's auth/error/parsing pattern stays small
//! and the next person finding "the share code" doesn't have to skim
//! a 600-line WebDAV file.
//!
//! # Endpoint shape
//!
//! ```text
//!   POST {server}/ocs/v2.php/apps/files_sharing/api/v1/shares?format=json
//!   OCS-APIRequest: true
//!   Accept: application/json
//!   Content-Type: application/x-www-form-urlencoded
//!
//!   path=/Documents/foo.pdf
//!   shareType=3            # 3 = public link
//!   permissions=1          # 1 = read-only (default for public links)
//! ```
//!
//! The response is the standard OCS envelope:
//!
//! ```json
//! {
//!   "ocs": {
//!     "meta": { "status": "ok", "statuscode": 200, "message": "OK" },
//!     "data": {
//!       "id": "42",
//!       "url": "https://cloud.example.com/s/abc123",
//!       "token": "abc123",
//!       ...
//!     }
//!   }
//! }
//! ```
//!
//! The `url` field is what we hand back to the UI to paste into an
//! email body.
//!
//! # MVP scope
//!
//! For Phase 2 of issue #12 we create the simplest possible share —
//! read-only, no password, no expiry. Password / expiry / per-share
//! permissions can each be added as form fields later without breaking
//! the function signature; we'd just expand `ShareOptions` and pass it
//! through.

use serde::Deserialize;

use nimbus_core::NimbusError;

use crate::client;

/// Nextcloud share-type discriminator. We only ever create type 3
/// (public link) here — user/group/team shares are a different feature
/// and a different UI gesture.
const SHARE_TYPE_PUBLIC_LINK: u8 = 3;

/// Default read-only permission bitmask. Nextcloud's bitfield is:
/// 1=read, 2=update, 4=create, 8=delete, 16=share. For email
/// attachments "read" is what we want — recipients shouldn't be able
/// to overwrite the file in your drive.
const PERM_READ_ONLY: u8 = 1;

/// What the caller gets back after creating a share. Just the URL for
/// now — that's all the compose UI needs to drop into the body. If we
/// later want to display the share in a "Manage shares" panel, we can
/// surface the share id and token here without breaking callers.
#[derive(Debug, Clone)]
pub struct PublicShare {
    /// Public URL the recipient opens, e.g. `https://cloud.example.com/s/abc123`.
    pub url: String,
}

// ── Wire format ────────────────────────────────────────────────
//
// We can't use a single `OcsEnvelope<ShareData>` like capabilities.rs
// does, because on failure Nextcloud sends `"data": []` (an array, not
// the expected object) — strict serde fails on the data field before
// we ever get to inspect meta. So we deserialize meta first, then
// conditionally pull data into the right shape.

#[derive(Debug, Deserialize)]
struct OcsRaw {
    ocs: OcsBodyRaw,
}

#[derive(Debug, Deserialize)]
struct OcsBodyRaw {
    meta: OcsMeta,
    /// Held as opaque JSON until we know meta said "ok"; then we
    /// re-deserialize into the concrete payload type.
    #[serde(default)]
    data: serde_json::Value,
}

/// `statuscode` is the OCS-level status (separate from HTTP status).
/// On a successful share, `status == "ok"` and `statuscode == 200`.
/// On a denied share (e.g. sharing disabled by admin) Nextcloud may
/// still return HTTP 200 but `statuscode == 403` — so we have to
/// inspect this even after a 2xx HTTP response.
#[derive(Debug, Deserialize)]
struct OcsMeta {
    status: String,
    statuscode: u16,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShareData {
    url: String,
}

/// Create a public share link for a file in the user's Nextcloud.
///
/// `path` is the same `/Documents/foo.pdf`-shaped path the file picker
/// produces. Returns the public URL on success.
///
/// # Errors
/// - `NimbusError::Auth` — app password rejected (401).
/// - `NimbusError::Nextcloud` — non-2xx HTTP, or OCS-level failure
///   (e.g. sharing globally disabled, target not found, file not in
///   user's scope). The OCS message is included where available so
///   the UI can show something specific.
/// - `NimbusError::Protocol` — JSON didn't match the expected shape.
pub async fn create_public_share(
    server_url: &str,
    username: &str,
    app_password: &str,
    path: &str,
    password: Option<&str>,
) -> Result<PublicShare, NimbusError> {
    let server = client::normalize_server_url(server_url);
    let url = format!("{server}/ocs/v2.php/apps/files_sharing/api/v1/shares?format=json");

    tracing::debug!(
        "POST {url} for path {path} (password: {})",
        if password.is_some() { "yes" } else { "no" }
    );

    let http = client::build()?;
    // Build the form pairs dynamically — `password` is only added when
    // the caller actually supplied one. Passing an empty `password=`
    // makes Nextcloud reject the request with "Password too short" on
    // some configurations, so omitting the field entirely is safer
    // than sending an empty value.
    let share_type = SHARE_TYPE_PUBLIC_LINK.to_string();
    let permissions = PERM_READ_ONLY.to_string();
    let mut form: Vec<(&str, &str)> = vec![
        ("path", path),
        ("shareType", &share_type),
        ("permissions", &permissions),
    ];
    if let Some(pw) = password {
        if !pw.is_empty() {
            form.push(("password", pw));
        }
    }

    let resp = http
        .post(&url)
        .header("OCS-APIRequest", "true")
        .header("Accept", "application/json")
        .basic_auth(username, Some(app_password))
        // The form encoder URL-encodes for us, so we pass the raw path
        // (with spaces / unicode) and Nextcloud receives the right thing.
        .form(&form)
        .send()
        .await
        .map_err(|e| NimbusError::Network(format!("share request failed: {e}")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(NimbusError::Auth(
            "Nextcloud rejected app password (revoked or expired)".into(),
        ));
    }

    // Read the body up front (success or failure) so a 4xx still
    // surfaces Nextcloud's actual reason. Password-policy rejections
    // come back as HTTP 400 with an OCS envelope whose `meta.message`
    // says e.g. "Password is too short" — pulling that into the
    // error makes the bad-password case actionable instead of "share
    // returned HTTP 400".
    let body = resp
        .text()
        .await
        .map_err(|e| NimbusError::Network(format!("share body read failed: {e}")))?;

    if !status.is_success() {
        let detail = ocs_message(&body).unwrap_or_else(|| {
            // Truncate so a verbose HTML error page doesn't blow up
            // the toast — 240 chars is enough to expose the gist.
            let trimmed = body.trim();
            if trimmed.len() > 240 {
                format!("{}…", &trimmed[..240])
            } else {
                trimmed.to_string()
            }
        });
        return Err(NimbusError::Nextcloud(format!(
            "share returned HTTP {status}: {detail}"
        )));
    }

    parse_share_response(&body)
}

/// Try to lift the human-readable `meta.message` out of an OCS
/// response body. Returns `None` if the body isn't OCS-shaped JSON
/// or doesn't carry a message — caller falls back to the raw body
/// in that case.
fn ocs_message(body: &str) -> Option<String> {
    let raw: OcsRaw = serde_json::from_str(body).ok()?;
    raw.ocs.meta.message.filter(|m| !m.is_empty())
}

/// Parse the OCS envelope and surface either the URL or a meaningful
/// error. Split out so tests can drive it with canned JSON.
fn parse_share_response(body: &str) -> Result<PublicShare, NimbusError> {
    let raw: OcsRaw = serde_json::from_str(body)
        .map_err(|e| NimbusError::Protocol(format!("share bad JSON: {e}")))?;

    // OCS-level failure even though HTTP was 2xx — surface the server's
    // message so the user sees "Sharing is disabled" rather than a
    // generic error. Check meta first; on failure `data` is an empty
    // array and would never deserialize into ShareData.
    if raw.ocs.meta.status != "ok" || raw.ocs.meta.statuscode >= 400 {
        let msg = raw
            .ocs
            .meta
            .message
            .unwrap_or_else(|| "share rejected by server".to_string());
        return Err(NimbusError::Nextcloud(format!(
            "share failed (OCS {}): {}",
            raw.ocs.meta.statuscode, msg
        )));
    }

    let data: ShareData = serde_json::from_value(raw.ocs.data)
        .map_err(|e| NimbusError::Protocol(format!("share data bad shape: {e}")))?;
    Ok(PublicShare { url: data.url })
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal slice of a real Nextcloud 28 share response. The actual
    /// `data` object has 30+ fields; we only care about `url`.
    const OK_RESPONSE: &str = r#"{
      "ocs": {
        "meta": {
          "status": "ok",
          "statuscode": 200,
          "message": "OK"
        },
        "data": {
          "id": "42",
          "url": "https://cloud.example.com/s/abc123",
          "token": "abc123",
          "share_type": 3,
          "permissions": 1
        }
      }
    }"#;

    #[test]
    fn parses_successful_share() {
        let share = parse_share_response(OK_RESPONSE).unwrap();
        assert_eq!(share.url, "https://cloud.example.com/s/abc123");
    }

    /// Sharing globally disabled — Nextcloud returns HTTP 200 but
    /// `statuscode: 403`. We must surface that as a Nextcloud error so
    /// the user sees something actionable.
    #[test]
    fn surfaces_ocs_level_failure() {
        let body = r#"{
          "ocs": {
            "meta": {
              "status": "failure",
              "statuscode": 403,
              "message": "Public upload disabled by the administrator"
            },
            "data": []
          }
        }"#;
        let err = parse_share_response(body).unwrap_err();
        match err {
            NimbusError::Nextcloud(msg) => {
                assert!(msg.contains("403"));
                assert!(msg.contains("Public upload disabled"));
            }
            other => panic!("expected Nextcloud error, got {other:?}"),
        }
    }

    /// Malformed JSON — should land in Protocol, not Network/Nextcloud.
    #[test]
    fn surfaces_bad_json_as_protocol_error() {
        let err = parse_share_response("not json at all").unwrap_err();
        assert!(matches!(err, NimbusError::Protocol(_)));
    }
}
