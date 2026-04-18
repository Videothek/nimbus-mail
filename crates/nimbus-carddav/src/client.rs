//! Tiny HTTP layer for CardDAV: reqwest with the right headers and
//! basic auth, and a couple of helpers for sending PROPFIND / REPORT.
//!
//! These two methods are not in `reqwest::Method` (they're WebDAV
//! extensions), so we build the requests via `Method::from_bytes` and
//! attach the body / headers ourselves.

use reqwest::{Client, Method, Response};
use std::time::Duration;

use nimbus_core::NimbusError;

/// Build the shared HTTP client.
///
/// Same shape as the Nextcloud one — generous timeouts because some
/// self-hosted servers answer slowly under load, and a recognisable
/// user agent so server admins can spot us in logs.
pub fn build() -> Result<Client, NimbusError> {
    Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("Nimbus Mail CardDAV/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| NimbusError::Network(format!("failed to build CardDAV HTTP client: {e}")))
}

/// PROPFIND with a given depth and XML body.
///
/// `depth = 0` queries the resource itself, `1` queries it and direct
/// children. CardDAV addressbook listing wants 1 (the home + each book).
pub async fn propfind(
    http: &Client,
    url: &str,
    username: &str,
    app_password: &str,
    depth: u32,
    body: &str,
) -> Result<Response, NimbusError> {
    let method = Method::from_bytes(b"PROPFIND")
        .map_err(|e| NimbusError::Other(format!("PROPFIND method: {e}")))?;
    http.request(method, url)
        .basic_auth(username, Some(app_password))
        .header("Depth", depth.to_string())
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| NimbusError::Network(format!("PROPFIND {url}: {e}")))
}

/// REPORT with the standard CardDAV body. `Depth: 1` is what every
/// addressbook-scoped report wants (the collection + its members).
pub async fn report(
    http: &Client,
    url: &str,
    username: &str,
    app_password: &str,
    body: &str,
) -> Result<Response, NimbusError> {
    let method = Method::from_bytes(b"REPORT")
        .map_err(|e| NimbusError::Other(format!("REPORT method: {e}")))?;
    http.request(method, url)
        .basic_auth(username, Some(app_password))
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| NimbusError::Network(format!("REPORT {url}: {e}")))
}

/// Strip a trailing `/` from a server URL — same helper as in
/// `nimbus-nextcloud::client`. Duplicated rather than depended upon to
/// keep this crate from pulling in the whole NC integration.
pub fn normalize_server_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Resolve a possibly-relative `href` from a multistatus response
/// against the server's base URL. CardDAV servers usually return
/// absolute paths (no scheme/host), occasionally full URLs.
pub fn absolute_url(server_url: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_string()
    } else if href.starts_with('/') {
        format!("{}{}", normalize_server_url(server_url), href)
    } else {
        // Defensive: treat as relative to root.
        format!("{}/{}", normalize_server_url(server_url), href)
    }
}
