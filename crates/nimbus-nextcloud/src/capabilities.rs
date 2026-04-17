//! Capability detection — what does this Nextcloud server actually support?
//!
//! Nextcloud returns a deeply nested capability tree from
//! `/ocs/v2.php/cloud/capabilities`. The structure changes between
//! versions and apps, so we parse only the fields we care about and
//! ignore the rest via `#[serde(default)]` — future NC versions that
//! move things around won't break auth.
//!
//! We detect four apps:
//!
//! | Flag    | Marker in the JSON tree                                |
//! |---------|--------------------------------------------------------|
//! | `talk`  | `capabilities.spreed` present (Nextcloud Talk's app id)|
//! | `files` | `capabilities.files` present                           |
//! | `caldav`| `capabilities.dav.chunking` or `bulkupload` (as proxy) |
//! | `carddav`| same — CalDAV/CardDAV are both under `dav`            |
//!
//! The CalDAV/CardDAV detection is a heuristic: `/ocs/v2.php/cloud/capabilities`
//! doesn't list them explicitly, but they're bundled with Nextcloud core
//! and any server exposing the `dav` capability supports both. If a
//! future issue needs stricter detection (e.g. probing the DAV
//! endpoints), we can refine here without touching the UI.

use serde::Deserialize;

use nimbus_core::NimbusError;
use nimbus_core::models::NextcloudCapabilities;

use crate::client;

// ── Wire format ────────────────────────────────────────────────
// These mirror Nextcloud's JSON literally. Everything is Option so a
// missing field is just "not supported", never an error.

#[derive(Debug, Deserialize)]
struct OcsEnvelope<T> {
    ocs: OcsBody<T>,
}

#[derive(Debug, Deserialize)]
struct OcsBody<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
struct CapabilitiesData {
    version: Option<Version>,
    capabilities: Capabilities,
}

#[derive(Debug, Deserialize)]
struct Version {
    /// Dotted version string, e.g. "28.0.4"
    string: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Capabilities {
    /// Nextcloud Talk uses the internal app id `spreed`.
    spreed: Option<serde_json::Value>,
    /// Files app (usually always present).
    files: Option<serde_json::Value>,
    /// DAV capabilities — presence implies CalDAV + CardDAV are reachable.
    dav: Option<serde_json::Value>,
}

/// Query `/ocs/v2.php/cloud/capabilities` and map it to our flat
/// `NextcloudCapabilities` shape.
///
/// Uses Basic auth with the app password obtained from Login Flow v2.
pub async fn fetch_capabilities(
    server_url: &str,
    username: &str,
    app_password: &str,
) -> Result<NextcloudCapabilities, NimbusError> {
    let server = client::normalize_server_url(server_url);
    let url = format!("{server}/ocs/v2.php/cloud/capabilities?format=json");
    tracing::debug!("Fetching Nextcloud capabilities from {url}");

    let http = client::build()?;
    let resp = http
        .get(&url)
        .header("OCS-APIRequest", "true")
        .header("Accept", "application/json")
        .basic_auth(username, Some(app_password))
        .send()
        .await
        .map_err(|e| NimbusError::Network(format!("capabilities request failed: {e}")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(NimbusError::Auth(
            "Nextcloud rejected app password (revoked or expired)".into(),
        ));
    }
    if !status.is_success() {
        return Err(NimbusError::Nextcloud(format!(
            "capabilities returned HTTP {status}"
        )));
    }

    let env: OcsEnvelope<CapabilitiesData> = resp
        .json()
        .await
        .map_err(|e| NimbusError::Protocol(format!("capabilities bad JSON: {e}")))?;

    let dav_present = env.ocs.data.capabilities.dav.is_some();
    let caps = NextcloudCapabilities {
        version: env.ocs.data.version.and_then(|v| v.string),
        talk: env.ocs.data.capabilities.spreed.is_some(),
        files: env.ocs.data.capabilities.files.is_some(),
        caldav: dav_present,
        carddav: dav_present,
    };
    tracing::info!(
        "Nextcloud capabilities: version={:?} talk={} files={} dav={}",
        caps.version,
        caps.talk,
        caps.files,
        dav_present
    );
    Ok(caps)
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal but realistic slice of what Nextcloud 28 returns.
    const SAMPLE: &str = r#"{
      "ocs": {
        "data": {
          "version": { "string": "28.0.4" },
          "capabilities": {
            "spreed": { "features": [] },
            "files":  { "bigfilechunking": true },
            "dav":    { "chunking": "1.0" }
          }
        }
      }
    }"#;

    #[test]
    fn parses_full_caps() {
        let env: OcsEnvelope<CapabilitiesData> = serde_json::from_str(SAMPLE).unwrap();
        assert!(env.ocs.data.capabilities.spreed.is_some());
        assert!(env.ocs.data.capabilities.files.is_some());
        assert!(env.ocs.data.capabilities.dav.is_some());
    }

    #[test]
    fn missing_apps_are_false() {
        let json = r#"{
          "ocs": { "data": { "capabilities": {} } }
        }"#;
        let env: OcsEnvelope<CapabilitiesData> = serde_json::from_str(json).unwrap();
        assert!(env.ocs.data.capabilities.spreed.is_none());
        assert!(env.ocs.data.capabilities.files.is_none());
        assert!(env.ocs.data.capabilities.dav.is_none());
    }
}
