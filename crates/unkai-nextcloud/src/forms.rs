//! Nextcloud Forms via the Forms app's OCS API v3 (#572).
//!
//! # Why its own module
//!
//! Forms is a separate Nextcloud app (`apps/forms`) with its own OCS
//! namespace, its own share model (form shares are *not* Files
//! shares — different table, different ids, different link scheme)
//! and its own JSON conventions (request bodies are JSON, not
//! form-encoded like the Files Sharing API).  Keeping it apart from
//! `shares.rs` means neither module has to special-case the other.
//!
//! # Endpoint shape
//!
//! ```text
//!   GET    {server}/ocs/v2.php/apps/forms/api/v3/forms?type=owned
//!   POST   {server}/ocs/v2.php/apps/forms/api/v3/forms
//!   GET    {server}/ocs/v2.php/apps/forms/api/v3/forms/{id}
//!   PATCH  {server}/ocs/v2.php/apps/forms/api/v3/forms/{id}
//!            { "keyValuePairs": { "title": "…" } }
//!   DELETE {server}/ocs/v2.php/apps/forms/api/v3/forms/{id}
//!   POST   {server}/ocs/v2.php/apps/forms/api/v3/forms/{id}/shares
//!            { "shareType": 3, "permissions": ["submit"] }
//!   OCS-APIRequest: true
//!   Accept: application/json
//! ```
//!
//! Every response is the standard OCS envelope (`ocs.meta` +
//! `ocs.data`).  Like the Files Sharing API, Forms answers a denied
//! request with an envelope whose `data` is an empty array, so the
//! payload is held as opaque JSON until `meta` says "ok".
//!
//! # Links
//!
//! A form has two URLs the UI cares about:
//!
//! - **Editor / owner URL** — `{server}/apps/forms/{form.hash}`.
//!   Requires a logged-in Nextcloud session; the in-app popout uses
//!   it so the user can add questions.
//! - **Public URL** — `{server}/apps/forms/s/{share.shareWith}` for a
//!   *link share* (`shareType == 3`).  This is what goes into a mail.
//!   The hash in `shareWith` is minted server-side when the link
//!   share is created; a form has no public URL until then.
//!
//! # Scope
//!
//! Questions and submissions are deliberately out of scope — the
//! Forms web editor is far better at building a questionnaire than
//! anything we'd ship inside a mail client.  We create the shell
//! (title + public link), hand the editor to the user, and manage
//! the lifecycle (list / copy link / open / delete).

use serde::Deserialize;
use serde_json::json;

use unkai_core::UnkaiError;
use unkai_core::models::TrustedCert;

use crate::client;

/// Nextcloud share-type discriminator for a public link — same value
/// the Files Sharing API uses (`IShare::TYPE_LINK`).
pub const SHARE_TYPE_LINK: u8 = 3;

/// `Constants::FORM_STATE_*` on the Forms side.  Surfaced as a plain
/// `u8` so the UI can badge closed / archived forms without the
/// crate having to know every future state.
pub const FORM_STATE_ACTIVE: u8 = 0;
pub const FORM_STATE_CLOSED: u8 = 1;
pub const FORM_STATE_ARCHIVED: u8 = 2;

/// Condensed row from `GET /forms` (the Forms app calls this the
/// "partial" form — no questions, no shares).
#[derive(Debug, Clone, Deserialize)]
pub struct FormSummary {
    pub id: i64,
    pub hash: String,
    #[serde(default)]
    pub title: String,
    /// Unix timestamp; `0` = never expires.
    #[serde(default)]
    pub expires: i64,
    /// Unix timestamp of the last edit (questions included).
    #[serde(default, rename = "lastUpdated")]
    pub last_updated: i64,
    #[serde(default)]
    pub state: u8,
    /// Only present when the caller may see results — for owned
    /// forms that's always.
    #[serde(default, rename = "submissionCount")]
    pub submission_count: Option<i64>,
}

/// One share row from a full form (`GET /forms/{id}`).  For a link
/// share `share_with` holds the public hash.
#[derive(Debug, Clone, Deserialize)]
pub struct FormShare {
    pub id: i64,
    #[serde(rename = "shareType")]
    pub share_type: u8,
    #[serde(default, rename = "shareWith")]
    pub share_with: String,
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// Full form from `GET /forms/{id}` / `POST /forms`, minus the
/// question tree we never read.
#[derive(Debug, Clone, Deserialize)]
pub struct FormDetails {
    pub id: i64,
    pub hash: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Unix timestamp of creation.
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub expires: i64,
    #[serde(default, rename = "lastUpdated")]
    pub last_updated: i64,
    #[serde(default)]
    pub state: u8,
    #[serde(default, rename = "submissionCount")]
    pub submission_count: Option<i64>,
    #[serde(default)]
    pub shares: Vec<FormShare>,
}

impl FormDetails {
    /// The first public link share on the form, if any.
    pub fn link_share(&self) -> Option<&FormShare> {
        self.shares
            .iter()
            .find(|s| s.share_type == SHARE_TYPE_LINK && !s.share_with.is_empty())
    }
}

/// Owner-side editor URL for a form.
pub fn form_editor_url(server_url: &str, hash: &str) -> String {
    let server = client::normalize_server_url(server_url);
    format!("{server}/apps/forms/{hash}")
}

/// Owner-side results page for a form.
pub fn form_results_url(server_url: &str, hash: &str) -> String {
    let server = client::normalize_server_url(server_url);
    format!("{server}/apps/forms/{hash}/results")
}

/// Public URL for a link share's hash — what recipients open.
pub fn form_public_url(server_url: &str, share_hash: &str) -> String {
    let server = client::normalize_server_url(server_url);
    format!("{server}/apps/forms/s/{share_hash}")
}

fn api_base(server_url: &str) -> String {
    let server = client::normalize_server_url(server_url);
    format!("{server}/ocs/v2.php/apps/forms/api/v3")
}

// ── Wire format ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OcsRaw {
    ocs: OcsBodyRaw,
}

#[derive(Debug, Deserialize)]
struct OcsBodyRaw {
    meta: OcsMeta,
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OcsMeta {
    status: String,
    statuscode: u16,
    #[serde(default)]
    message: Option<String>,
}

/// Send a prepared request and unwrap the OCS envelope down to its
/// `data` payload, mapping auth / HTTP / OCS-level failures onto the
/// shared error variants.  Every Forms call funnels through here so
/// the "Forms app not installed" (404) and "app password revoked"
/// (401) cases read the same on every endpoint.
async fn send_ocs(
    req: reqwest::RequestBuilder,
    username: &str,
    app_password: &str,
    ctx: &str,
) -> Result<serde_json::Value, UnkaiError> {
    let resp = req
        .header("OCS-APIRequest", "true")
        .header("Accept", "application/json")
        .basic_auth(username, Some(app_password))
        .send()
        .await
        .map_err(|e| UnkaiError::Network(format!("{ctx} request failed: {e}")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(UnkaiError::Auth(
            "Nextcloud rejected app password (revoked or expired)".into(),
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        // The OCS router answers 404 for an app that isn't enabled
        // as well as for a form id that doesn't exist; the body
        // tells them apart when it's an OCS envelope.
        let body = resp.text().await.unwrap_or_default();
        return Err(UnkaiError::Nextcloud(ocs_message(&body).unwrap_or_else(
            || {
                "Nextcloud Forms is not available on this server (app not installed or disabled)"
                    .into()
            },
        )));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| UnkaiError::Network(format!("{ctx} body read failed: {e}")))?;

    if !status.is_success() {
        let detail = ocs_message(&body).unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.len() > 240 {
                format!("{}…", &trimmed[..240])
            } else {
                trimmed.to_string()
            }
        });
        return Err(UnkaiError::Nextcloud(format!("{ctx} failed: {detail}")));
    }

    parse_ocs_data(&body, ctx)
}

fn ocs_message(body: &str) -> Option<String> {
    let raw: OcsRaw = serde_json::from_str(body).ok()?;
    raw.ocs.meta.message.filter(|m| !m.is_empty())
}

/// Parse the envelope and return `data` once `meta` reports success.
/// Split out so tests can drive it with canned JSON.
fn parse_ocs_data(body: &str, ctx: &str) -> Result<serde_json::Value, UnkaiError> {
    let raw: OcsRaw = serde_json::from_str(body)
        .map_err(|e| UnkaiError::Protocol(format!("{ctx} bad JSON: {e}")))?;
    if raw.ocs.meta.status != "ok" || raw.ocs.meta.statuscode >= 400 {
        let msg = raw
            .ocs
            .meta
            .message
            .unwrap_or_else(|| "rejected by server".to_string());
        return Err(UnkaiError::Nextcloud(format!(
            "{ctx} failed (OCS {}): {msg}",
            raw.ocs.meta.statuscode
        )));
    }
    Ok(raw.ocs.data)
}

// ── Public API ─────────────────────────────────────────────────

/// List every form the user owns (`GET /forms?type=owned`).
pub async fn list_forms(
    server_url: &str,
    username: &str,
    app_password: &str,
    trusted_certs: &[TrustedCert],
) -> Result<Vec<FormSummary>, UnkaiError> {
    let url = format!("{}/forms?type=owned", api_base(server_url));
    tracing::debug!("GET {url}");
    let http = client::build(trusted_certs)?;
    let data = send_ocs(http.get(&url), username, app_password, "forms list").await?;
    serde_json::from_value(data)
        .map_err(|e| UnkaiError::Protocol(format!("forms list bad shape: {e}")))
}

/// Fetch one form with its shares (`GET /forms/{id}`).
pub async fn get_form(
    server_url: &str,
    username: &str,
    app_password: &str,
    form_id: i64,
    trusted_certs: &[TrustedCert],
) -> Result<FormDetails, UnkaiError> {
    let url = format!("{}/forms/{form_id}", api_base(server_url));
    tracing::debug!("GET {url}");
    let http = client::build(trusted_certs)?;
    let data = send_ocs(http.get(&url), username, app_password, "form fetch").await?;
    serde_json::from_value(data).map_err(|e| UnkaiError::Protocol(format!("form bad shape: {e}")))
}

/// Create an empty form, then set its title.
///
/// `POST /forms` takes no parameters on API v3 — the server mints an
/// untitled form and we `PATCH` the title straight after.  An empty
/// `title` skips the second call so the form stays "untitled" the
/// way the web UI would leave it.  The returned details reflect the
/// title we set (the `POST` response predates the `PATCH`).
pub async fn create_form(
    server_url: &str,
    username: &str,
    app_password: &str,
    title: &str,
    trusted_certs: &[TrustedCert],
) -> Result<FormDetails, UnkaiError> {
    let url = format!("{}/forms", api_base(server_url));
    tracing::debug!("POST {url}");
    let http = client::build(trusted_certs)?;
    let data = send_ocs(http.post(&url), username, app_password, "form create").await?;
    let mut form: FormDetails = serde_json::from_value(data)
        .map_err(|e| UnkaiError::Protocol(format!("form create bad shape: {e}")))?;

    let title = title.trim();
    if !title.is_empty() {
        update_form_title(
            server_url,
            username,
            app_password,
            form.id,
            title,
            trusted_certs,
        )
        .await?;
        form.title = title.to_string();
    }
    Ok(form)
}

/// Rename a form (`PATCH /forms/{id}` with `keyValuePairs.title`).
pub async fn update_form_title(
    server_url: &str,
    username: &str,
    app_password: &str,
    form_id: i64,
    title: &str,
    trusted_certs: &[TrustedCert],
) -> Result<(), UnkaiError> {
    let url = format!("{}/forms/{form_id}", api_base(server_url));
    tracing::debug!("PATCH {url} (title len {})", title.len());
    let http = client::build(trusted_certs)?;
    let body = json!({ "keyValuePairs": { "title": title } });
    send_ocs(
        http.patch(&url).json(&body),
        username,
        app_password,
        "form rename",
    )
    .await?;
    Ok(())
}

/// Delete a form and everything under it — questions, submissions,
/// and shares (`DELETE /forms/{id}`).  Irreversible server-side.
pub async fn delete_form(
    server_url: &str,
    username: &str,
    app_password: &str,
    form_id: i64,
    trusted_certs: &[TrustedCert],
) -> Result<(), UnkaiError> {
    let url = format!("{}/forms/{form_id}", api_base(server_url));
    tracing::debug!("DELETE {url}");
    let http = client::build(trusted_certs)?;
    send_ocs(http.delete(&url), username, app_password, "form delete").await?;
    Ok(())
}

/// Mint a public link share for a form (`POST /forms/{id}/shares`
/// with `shareType == 3`).  The server generates the hash and hands
/// it back in `shareWith`; `form_public_url` turns it into the URL
/// recipients open.  Link shares may only carry the `submit`
/// permission (plus `embed`), which is exactly what a mailed form
/// needs.
pub async fn create_link_share(
    server_url: &str,
    username: &str,
    app_password: &str,
    form_id: i64,
    trusted_certs: &[TrustedCert],
) -> Result<FormShare, UnkaiError> {
    let url = format!("{}/forms/{form_id}/shares", api_base(server_url));
    tracing::debug!("POST {url} (link share)");
    let http = client::build(trusted_certs)?;
    let body = json!({ "shareType": SHARE_TYPE_LINK, "permissions": ["submit"] });
    let data = send_ocs(
        http.post(&url).json(&body),
        username,
        app_password,
        "form link share",
    )
    .await?;
    serde_json::from_value(data)
        .map_err(|e| UnkaiError::Protocol(format!("form share bad shape: {e}")))
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_form_list() {
        let body = r#"{"ocs":{"meta":{"status":"ok","statuscode":200,"message":"OK"},
          "data":[{"id":3,"hash":"abc123","title":"Team lunch","expires":0,
                   "lastUpdated":1700000000,"permissions":["edit","results","submit"],
                   "partial":true,"state":0,"lockedBy":null,"lockedUntil":null,
                   "submissionCount":4}]}}"#;
        let data = parse_ocs_data(body, "t").unwrap();
        let rows: Vec<FormSummary> = serde_json::from_value(data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 3);
        assert_eq!(rows[0].hash, "abc123");
        assert_eq!(rows[0].title, "Team lunch");
        assert_eq!(rows[0].state, FORM_STATE_ACTIVE);
        assert_eq!(rows[0].submission_count, Some(4));
    }

    #[test]
    fn full_form_finds_link_share() {
        let body = r#"{"ocs":{"meta":{"status":"ok","statuscode":200,"message":"OK"},
          "data":{"id":3,"hash":"abc123","title":"Team lunch","description":"",
                  "ownerId":"alex","created":1699999000,"expires":0,"state":1,
                  "questions":[],"submissionCount":0,
                  "shares":[
                    {"id":7,"formId":3,"shareType":0,"shareWith":"jane","permissions":["submit"],"displayName":"Jane"},
                    {"id":8,"formId":3,"shareType":3,"shareWith":"pUbL1cHaSh","permissions":["submit"],"displayName":""}
                  ]}}}"#;
        let data = parse_ocs_data(body, "t").unwrap();
        let form: FormDetails = serde_json::from_value(data).unwrap();
        assert_eq!(form.state, FORM_STATE_CLOSED);
        let link = form.link_share().expect("link share");
        assert_eq!(link.id, 8);
        assert_eq!(
            form_public_url("https://cloud.example.com/", &link.share_with),
            "https://cloud.example.com/apps/forms/s/pUbL1cHaSh"
        );
        assert_eq!(
            form_editor_url("https://cloud.example.com", &form.hash),
            "https://cloud.example.com/apps/forms/abc123"
        );
    }

    #[test]
    fn ocs_failure_surfaces_message() {
        let body = r#"{"ocs":{"meta":{"status":"failure","statuscode":403,"message":"Forbidden"},"data":[]}}"#;
        let err = parse_ocs_data(body, "form delete").unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("403"), "{text}");
        assert!(text.contains("Forbidden"), "{text}");
    }

    #[test]
    fn missing_link_share_is_none() {
        let body = r#"{"ocs":{"meta":{"status":"ok","statuscode":200},
          "data":{"id":1,"hash":"h","shares":[]}}}"#;
        let data = parse_ocs_data(body, "t").unwrap();
        let form: FormDetails = serde_json::from_value(data).unwrap();
        assert!(form.link_share().is_none());
        assert_eq!(form.title, "");
    }
}
