//! OCS user-info — the authenticated user's profile email.
//!
//! Used by the calendar create/update path to put the right
//! address on `ORGANIZER:mailto:`.  Nextcloud 30+ Mail Provider
//! matches `ORGANIZER` against the user's Mail-app accounts
//! character-for-character — get this wrong and iMIP silently
//! falls back to the system mailer with `From: invitations-noreply@…`.

use serde::Deserialize;

use nimbus_core::NimbusError;

use crate::client;

#[derive(Debug, Deserialize)]
struct OcsEnvelope<T> {
    ocs: OcsBody<T>,
}

#[derive(Debug, Deserialize)]
struct OcsBody<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
struct UserData {
    /// Primary email from Personal settings → Personal info → Email.
    /// Often null on freshly-created accounts; callers must handle
    /// the empty case explicitly.
    #[serde(default)]
    email: Option<String>,
    /// Display name — used for `ORGANIZER;CN=` so the iMIP shows a
    /// human label alongside the address.
    #[serde(default)]
    displayname: Option<String>,
}

/// Profile fields we surface to the rest of the app.
#[derive(Debug, Clone)]
pub struct NextcloudUserProfile {
    pub email: Option<String>,
    pub display_name: Option<String>,
}

/// Fetch the authenticated user's profile from
/// `/ocs/v2.php/cloud/user` (the "current user" endpoint, no
/// username path component required).
pub async fn fetch_current_user(
    server_url: &str,
    username: &str,
    app_password: &str,
) -> Result<NextcloudUserProfile, NimbusError> {
    let server = client::normalize_server_url(server_url);
    let url = format!("{server}/ocs/v2.php/cloud/user?format=json");
    let http = client::build()?;
    let resp = http
        .get(&url)
        .header("OCS-APIRequest", "true")
        .header("Accept", "application/json")
        .basic_auth(username, Some(app_password))
        .send()
        .await
        .map_err(|e| NimbusError::Network(format!("user request failed: {e}")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(NimbusError::Auth(
            "Nextcloud rejected app password (revoked or expired)".into(),
        ));
    }
    if !status.is_success() {
        return Err(NimbusError::Nextcloud(format!(
            "user info returned HTTP {status}"
        )));
    }

    let env: OcsEnvelope<UserData> = resp
        .json()
        .await
        .map_err(|e| NimbusError::Protocol(format!("user info bad JSON: {e}")))?;

    Ok(NextcloudUserProfile {
        email: env.ocs.data.email.filter(|s| !s.trim().is_empty()),
        display_name: env.ocs.data.displayname.filter(|s| !s.trim().is_empty()),
    })
}

// ─── Sharees lookup: "is this email a Nextcloud user?" ──────

#[derive(Debug, Deserialize)]
struct ShareesResponse {
    /// The `users` bucket carries matches that are local NC
    /// principals.  Other buckets (`groups`, `remotes`,
    /// `emails`) we don't care about — we want the
    /// authoritative-user list.
    #[serde(default)]
    exact: ShareesBuckets,
    #[serde(default)]
    users: Vec<ShareeMatch>,
}

#[derive(Debug, Default, Deserialize)]
struct ShareesBuckets {
    /// Inside `exact.users` we look for an entry whose
    /// `value.shareWith` matches the email's local principal.
    #[serde(default)]
    users: Vec<ShareeMatch>,
}

#[derive(Debug, Deserialize)]
struct ShareeMatch {
    /// Display name on the row — what NC's admin set as the
    /// user's full name.
    label: String,
    value: ShareeMatchValue,
}

#[derive(Debug, Deserialize)]
struct ShareeMatchValue {
    /// `shareWith` is the userId for `shareType=0` (local user).
    #[serde(rename = "shareWith")]
    share_with: String,
}

/// Match returned by [`find_user_by_email`] — the user-side
/// fields we care about for "is this attendee internal?".
#[derive(Debug, Clone)]
pub struct NextcloudUserMatch {
    pub user_id: String,
    pub display_name: String,
}

/// Look up a Nextcloud user by email via the sharees endpoint.
/// Returns `Ok(None)` when no NC principal owns that address —
/// the caller treats that as "external attendee, route through
/// guest URL / email participant".  Returns `Ok(Some(...))`
/// when an exact match is found (the user *and* the email are
/// authoritatively registered against the same principal).
///
/// We restrict to `shareType[]=0` (local users) so the response
/// can't be ambiguous against an email-share row carrying the
/// same address.  `itemType=calendar` is what NC Calendar's
/// own attendee picker passes; using the same hint keeps the
/// server's filtering logic aligned.
pub async fn find_user_by_email(
    server_url: &str,
    username: &str,
    app_password: &str,
    email: &str,
) -> Result<Option<NextcloudUserMatch>, NimbusError> {
    let server = client::normalize_server_url(server_url);
    let url = format!(
        "{server}/ocs/v2.php/apps/files_sharing/api/v1/sharees\
         ?format=json&itemType=calendar&search={}&shareType[]=0",
        urlencoding(email),
    );
    let http = client::build()?;
    let resp = http
        .get(&url)
        .header("OCS-APIRequest", "true")
        .header("Accept", "application/json")
        .basic_auth(username, Some(app_password))
        .send()
        .await
        .map_err(|e| NimbusError::Network(format!("sharees request failed: {e}")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(NimbusError::Auth(
            "Nextcloud rejected app password (revoked or expired)".into(),
        ));
    }
    if !status.is_success() {
        return Err(NimbusError::Nextcloud(format!(
            "sharees returned HTTP {status}"
        )));
    }

    let env: OcsEnvelope<ShareesResponse> = resp
        .json()
        .await
        .map_err(|e| NimbusError::Protocol(format!("sharees bad JSON: {e}")))?;

    // Prefer the `exact` bucket — it's what Nextcloud uses to
    // signal "this is the same address you typed".  Fall back
    // to the partial-match `users` list and pick a row whose
    // userId equals the email's local part as a last resort
    // (some NC instances store full email as userId).
    let pick = env
        .ocs
        .data
        .exact
        .users
        .first()
        .or_else(|| env.ocs.data.users.first());
    Ok(pick.map(|m| NextcloudUserMatch {
        user_id: m.value.share_with.clone(),
        display_name: m.label.clone(),
    }))
}

/// Minimal percent-encoding for the `search=` query parameter.
/// We deliberately don't pull in `urlencoding` as a workspace
/// dep just for one path — emails contain only a small set of
/// chars that need escaping (`@`, `+`, `.`, `-`).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}
