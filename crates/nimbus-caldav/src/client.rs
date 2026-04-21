//! Tiny HTTP layer for CalDAV: reqwest with the right headers and
//! basic auth, plus helpers for PROPFIND / REPORT.
//!
//! These two methods aren't in `reqwest::Method` (they're WebDAV
//! extensions), so we build them via `Method::from_bytes` and attach
//! headers ourselves. Mirrors the shape of `nimbus-carddav::client` —
//! only the default `Content-Type` and user agent change.

use reqwest::{Client, Method, Response};
use std::time::Duration;

use nimbus_core::NimbusError;

/// Build the shared HTTP client.
pub fn build() -> Result<Client, NimbusError> {
    Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("Nimbus Mail CalDAV/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| NimbusError::Network(format!("failed to build CalDAV HTTP client: {e}")))
}

/// PROPFIND with a given depth and XML body.
///
/// Depth `0` queries the resource itself; `1` queries it and direct
/// children. CalDAV calendar-home listing wants `1` (the home plus
/// each calendar collection).
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

/// REPORT against a calendar collection. `Depth: 1` is what every
/// calendar-scoped report (sync-collection, calendar-multiget,
/// calendar-query) wants — the collection plus its members.
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

/// PUT a `text/calendar` body to a calendar resource.
///
/// `if_match` carries the existing etag for an update — the server
/// returns 412 if the resource changed under us. For a fresh create,
/// pass `None` and set `if_none_match_star = true` so the PUT only
/// succeeds when the href is unused (basic two-client safety).
pub async fn put_ics(
    http: &Client,
    url: &str,
    username: &str,
    app_password: &str,
    body: &str,
    if_match: Option<&str>,
    if_none_match_star: bool,
) -> Result<Response, NimbusError> {
    let mut req = http
        .put(url)
        .basic_auth(username, Some(app_password))
        .header("Content-Type", "text/calendar; charset=utf-8")
        .body(body.to_string());
    if let Some(etag) = if_match {
        let v = if etag.starts_with('"') {
            etag.to_string()
        } else {
            format!("\"{etag}\"")
        };
        req = req.header("If-Match", v);
    }
    if if_none_match_star {
        req = req.header("If-None-Match", "*");
    }
    req.send()
        .await
        .map_err(|e| NimbusError::Network(format!("PUT {url}: {e}")))
}

/// DELETE a CalDAV resource at `url`. `if_match` is recommended (and
/// Nextcloud requires it) so we don't blow away an event someone else
/// just edited.
pub async fn delete_resource(
    http: &Client,
    url: &str,
    username: &str,
    app_password: &str,
    if_match: Option<&str>,
) -> Result<Response, NimbusError> {
    let mut req = http.delete(url).basic_auth(username, Some(app_password));
    if let Some(etag) = if_match {
        let v = if etag.starts_with('"') {
            etag.to_string()
        } else {
            format!("\"{etag}\"")
        };
        req = req.header("If-Match", v);
    }
    req.send()
        .await
        .map_err(|e| NimbusError::Network(format!("DELETE {url}: {e}")))
}

/// Strip a trailing `/` from a server URL.
pub fn normalize_server_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Resolve a possibly-relative `href` from a multistatus response
/// against the server's base URL. CalDAV servers usually return
/// absolute paths (no scheme/host), occasionally full URLs.
pub fn absolute_url(server_url: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_string()
    } else if href.starts_with('/') {
        format!("{}{}", normalize_server_url(server_url), href)
    } else {
        format!("{}/{}", normalize_server_url(server_url), href)
    }
}
