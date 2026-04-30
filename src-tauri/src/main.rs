//! Nimbus — a modern mail client with Nextcloud integration.
//!
//! This is the Tauri application entry point. It registers Tauri
//! commands (the IPC bridge between Rust and Svelte) and launches
//! the native window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod badge;

use nimbus_caldav::{
    Calendar as CaldavCalendar, RawEvent, build_ics as caldav_build_ics,
    create_calendar as caldav_create_calendar, create_event as caldav_create_event,
    delete_calendar as caldav_delete_calendar, delete_event as caldav_delete_event,
    list_calendars as caldav_list_calendars, sync_calendar as caldav_sync_calendar,
    update_calendar as caldav_update_calendar, update_event as caldav_update_event,
};
use nimbus_carddav::{
    Addressbook, ParsedVcard, RawContact, build_vcard, create_contact as carddav_create_contact,
    delete_contact as carddav_delete_contact, list_addressbooks, sync_addressbook,
    update_contact as carddav_update_contact,
};
use nimbus_core::NimbusError;
use nimbus_core::models::{
    Account, AppSettings, CalendarEvent, Contact, CustomTheme, Email, EmailEnvelope, EventAttendee,
    EventReminder, Folder, NextcloudAccount, OutgoingEmail,
};
use nimbus_imap::ImapClient;
use nimbus_jmap::JmapClient;
use nimbus_nextcloud::{
    FileEntry, LoginFlowInit, LoginFlowResult, fetch_capabilities, poll_login, start_login,
};
use nimbus_smtp::{SmtpClient, build_outgoing_message};
use nimbus_store::cache::{
    CalendarEventRow, CalendarEventServerHandle, CalendarRow, ContactRow, ContactServerHandle,
    SearchFilters, SearchHit, SearchScope, SyncState,
};
use nimbus_store::{Cache, account_store, app_settings, credentials, nextcloud_store};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, UriSchemeContext, WindowEvent};
use tokio::sync::RwLock;

/// Shared, mutable app preferences. Held as Tauri managed state so the
/// background loop can snapshot under a read lock on every tick while
/// `update_app_settings` swaps in a new value under the write lock.
type SharedSettings = Arc<RwLock<AppSettings>>;

/// Minimum enforced sync interval — guards against a hand-edited
/// `app_settings.json` DOSing the user's mail server.
const MIN_SYNC_INTERVAL_SECS: u64 = 30;

/// Captured-once raw RGBA of the base tray icon. We hold this so the
/// badge renderer can re-composite a fresh badge on every unread-count
/// change without re-reading the on-disk PNG. The window's default
/// icon is the source of truth at startup.
struct TrayBaseIcon {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// Absolute filesystem path to a PNG of our app icon, written at
/// startup. Returned to the frontend via `get_notification_icon_path`
/// so `sendNotification` calls can pass it through to libnotify /
/// the Windows toast / NSUserNotification on macOS, ensuring our
/// own icon shows up in the toast instead of a generic placeholder
/// (especially in dev builds where no .desktop / Start-Menu shortcut
/// exists yet to lend the OS a registered icon).
struct NotificationIconPath(std::path::PathBuf);

/// Bytes of `icons/icon.png`, baked in at compile time so we can
/// drop them onto disk on first launch without having to resolve a
/// runtime resource path that differs between `cargo tauri dev` and
/// bundled builds.
const NOTIFICATION_ICON_PNG: &[u8] = include_bytes!("../icons/icon.png");

/// Write the embedded icon to a stable temp-dir path and return it.
/// Idempotent — overwriting on every launch is cheap (~10 KB) and
/// keeps the file in sync with whatever's currently bundled.
fn install_notification_icon() -> Result<std::path::PathBuf, NimbusError> {
    let dir = std::env::temp_dir().join("nimbus-mail");
    std::fs::create_dir_all(&dir)
        .map_err(|e| NimbusError::Other(format!("notification icon mkdir failed: {e}")))?;
    let path = dir.join("nimbus-mail-icon.png");
    std::fs::write(&path, NOTIFICATION_ICON_PNG)
        .map_err(|e| NimbusError::Other(format!("notification icon write failed: {e}")))?;
    Ok(path)
}

#[tauri::command]
fn get_notification_icon_path(state: State<'_, NotificationIconPath>) -> String {
    state.0.to_string_lossy().into_owned()
}

/// Linux-only: send a desktop notification through libnotify with
/// the `DesktopEntry` + `Category` hints set, so the notification
/// daemon (GNOME Shell / KDE Plasma / mako / dunst) tracks it under
/// our app identity and keeps it in its notification center.
///
/// `tauri-plugin-notification` uses notify-rust under the hood but
/// doesn't expose hint APIs in JS, which left dev-build toasts as
/// "anonymous" — they showed up briefly but weren't kept in the
/// notification history. Wrapping the builder ourselves with the
/// hints set is enough to make them persist.
///
/// Returns `Ok(true)` when the call succeeded so the JS side can
/// fall back to the regular plugin if anything goes wrong (e.g.
/// no notification daemon running).
#[cfg(target_os = "linux")]
#[tauri::command]
fn send_native_notification(
    title: String,
    body: String,
    icon: State<'_, NotificationIconPath>,
) -> Result<bool, NimbusError> {
    use notify_rust::{Hint, Notification};
    let mut n = Notification::new();
    n.summary(&title)
        .body(&body)
        .appname("Nimbus Mail")
        .hint(Hint::DesktopEntry("com.nimbus.mail".to_string()))
        .hint(Hint::Category("email".to_string()));
    let icon_path = icon.0.to_string_lossy();
    if !icon_path.is_empty() {
        n.icon(&icon_path);
    }
    n.show()
        .map(|_| true)
        .map_err(|e| NimbusError::Other(format!("notify-rust failed: {e}")))
}

/// Stub on non-Linux platforms — the JS side is expected to fall
/// back to `sendNotification` from the Tauri plugin when this
/// returns `Ok(false)`. Keeps the JS branch code platform-agnostic
/// without needing to ask the OS layer about the platform.
#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn send_native_notification(_title: String, _body: String) -> Result<bool, NimbusError> {
    Ok(false)
}

/// Tells Windows that this process should attribute its toast
/// notifications to a specific AUMID instead of inheriting the
/// launching process's (which surfaces as "PowerShell" / "cmd" /
/// "Git Bash" depending on how the dev binary was started).
///
/// The string MUST match the AUMID baked into the installer's
/// Start-Menu shortcut for the toast's display name + icon to
/// resolve correctly in installed builds; we use the same bundle
/// identifier (`com.nimbus.mail`) the Tauri config sets so the two
/// stay in lockstep.
#[cfg(windows)]
fn set_app_user_model_id() {
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::core::HSTRING;

    let aumid = HSTRING::from("com.nimbus.mail");
    // SAFETY: the function takes a PCWSTR derived from a live
    // HSTRING; the call has no preconditions beyond a valid
    // null-terminated wide string, which `HSTRING` guarantees.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&aumid) } {
        tracing::warn!("SetCurrentProcessExplicitAppUserModelID failed: {e}");
    }
}

// ── Tauri commands ──────────────────────────────────────────────
//
// Each `#[tauri::command]` function becomes callable from the
// Svelte frontend via `invoke("command_name", { args })`.
//
// Tauri serialises the return value as JSON and sends it to the
// frontend. Errors must implement `Serialize` (which NimbusError
// does) so Tauri can send them back as structured error objects.

/// Return all configured accounts.
#[tauri::command]
fn get_accounts(cache: State<'_, Cache>) -> Result<Vec<Account>, NimbusError> {
    account_store::load_accounts(&cache)
}

/// Add a new email account and store its password in the OS keychain.
///
/// The frontend sends an `Account` object plus a `password`. The account
/// metadata lands in the encrypted SQLite cache; the password goes to
/// the OS keychain. Separating them keeps secrets off disk and lets the
/// `accounts` table be inspected without exposing credentials.
#[tauri::command]
fn add_account(
    account: Account,
    password: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    credentials::store_imap_password(&account.id, &password)?;
    account_store::add_account(&cache, account)
}

/// Remove an account and its stored password.
///
/// Order matters: keychain → cached message data → account record.
/// If any step fails, the remaining state is still consistent with
/// the account being present (the user can retry). The account row
/// is deleted last so the rest of the app's "this account exists"
/// queries stay truthful right up until the cleanup completes.
#[tauri::command]
fn remove_account(id: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    credentials::delete_imap_password(&id)?;
    // Best-effort: a failure here leaves orphaned cache rows but doesn't
    // block account removal. Log and continue.
    if let Err(e) = cache.wipe_account(&id) {
        tracing::warn!("failed to wipe cache for account '{id}': {e}");
    }
    account_store::remove_account(&cache, &id)
}

/// Update an existing account's settings.
#[tauri::command]
fn update_account(account: Account, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    account_store::update_account(&cache, account)
}

/// Replace the IMAP/SMTP password stored in the OS keychain for
/// an existing account.  Kept separate from `update_account` so
/// the password never has to round-trip through the account
/// metadata struct (which lives in the encrypted SQLite cache).
/// `store_imap_password` overwrites in place, so the same call
/// covers initial setup and rotation.
#[tauri::command]
fn set_account_password(id: String, password: String) -> Result<(), NimbusError> {
    if password.is_empty() {
        return Err(NimbusError::Other("password must not be empty".into()));
    }
    credentials::store_imap_password(&id, &password)
}

/// Pin (or clear) a per-folder icon override for an account.
///
/// Passing `Some(emoji)` sets the override; `None` removes it so the
/// folder falls back through the normal icon-resolution chain
/// (special-use attributes → user keyword rules → 📁). The command
/// loads the full `Account` server-side, mutates just
/// `folder_icon_overrides`, and writes back — cheaper than round-
/// tripping the whole struct through the UI, and avoids the UI
/// having to know every field on `Account` just to change one map
/// entry.
#[tauri::command]
fn set_folder_icon(
    account_id: String,
    folder_name: String,
    icon: Option<String>,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let mut account = load_account(&cache, &account_id)?;
    match icon {
        Some(e) if !e.trim().is_empty() => {
            account
                .folder_icon_overrides
                .insert(folder_name, e.trim().to_string());
        }
        _ => {
            account.folder_icon_overrides.remove(&folder_name);
        }
    }
    account_store::update_account(&cache, account)
}

/// Probe Mozilla autoconfig and DNS SRV records for the email's
/// domain and return any IMAP/SMTP server settings discovered.
/// Used by the AccountSetup wizard to prefill the form so most
/// users only need to type their email + password.
///
/// Returns `Ok(None)` when nothing is found — the wizard falls back
/// to manual entry. `Err` only on argument validation failures
/// (e.g. malformed email); transient network errors during the
/// individual probes are swallowed inside the discovery crate so
/// one flaky route doesn't kill the whole flow.
#[tauri::command]
async fn discover_account_settings(
    email: String,
) -> Result<Option<nimbus_discovery::DiscoveredAccount>, NimbusError> {
    match nimbus_discovery::discover(&email).await {
        Ok(found) => Ok(Some(found)),
        Err(nimbus_discovery::DiscoveryError::NotFound) => Ok(None),
        Err(nimbus_discovery::DiscoveryError::Parse(msg)) => Err(NimbusError::Other(msg)),
        Err(nimbus_discovery::DiscoveryError::Network(msg)) => Err(NimbusError::Network(msg)),
    }
}

/// One cert in a probed chain — DER bytes plus its SHA-256
/// fingerprint formatted for display. The frontend uses `der` to
/// build a `TrustedCert` entry and `sha256` to render the
/// "compare this against your server" prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProbedCertEntry {
    der: Vec<u8>,
    sha256: String,
}

/// Shape returned to the UI by [`probe_server_certificate`]. The
/// full chain (leaf first, then intermediates) is round-tripped
/// back so the UI can trust every cert the server presented — not
/// just the leaf. This survives chain reordering and reissues of
/// the leaf under the same intermediate without a re-prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProbedCert {
    /// Probed certificates in handshake order (leaf at index 0).
    chain: Vec<ProbedCertEntry>,
    host: String,
}

/// Open a no-verify TLS handshake to a mail server and capture the
/// presented certificate chain. Used by the AccountSetup wizard's
/// "trust this server?" path and AccountSettings' re-trust button:
/// when [`test_connection`] fails because the cert isn't trusted,
/// the UI calls this to get the fingerprints, asks the user, and on
/// confirm passes every DER back into `add_account` /
/// `update_account` as `trusted_certs` entries.
///
/// **Safety**: the captured certs are never used for actual mail
/// traffic — the connection is dropped immediately after the
/// handshake. The user explicitly chooses whether to trust them.
#[tauri::command]
async fn probe_server_certificate(host: String, port: u16) -> Result<ProbedCert, NimbusError> {
    let chain_der = nimbus_imap::probe_server_certificate(&host, port).await?;
    let chain = chain_der
        .into_iter()
        .map(|der| {
            let sha256 = nimbus_core::tls::fingerprint_sha256(&der);
            ProbedCertEntry { der, sha256 }
        })
        .collect();
    Ok(ProbedCert { chain, host })
}

/// Validate IMAP credentials by actually logging in.
///
/// The setup wizard calls this before it asks the store to persist the
/// account — an early TCP/TLS/LOGIN round-trip surfaces wrong hostnames,
/// wrong ports, and bad passwords as a structured `NimbusError` with a
/// specific variant (`Network`, `Auth`, `Protocol`) so the UI can phrase
/// the failure clearly instead of saving a dead account and confusing
/// the user on first fetch.
///
/// The session is immediately torn down — this is a probe, not a real
/// fetch; nothing is cached.
#[tauri::command]
async fn test_connection(
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_certs: Option<Vec<nimbus_core::models::TrustedCert>>,
) -> Result<String, NimbusError> {
    tracing::info!("Testing IMAP connection to {host}:{port} as {username}");
    let trusted = trusted_certs.unwrap_or_default();
    let client = ImapClient::connect(&host, port, &username, &password, &trusted).await?;
    let _ = client.logout().await;
    Ok(format!("IMAP login to {host}:{port} succeeded"))
}

// ── Nextcloud ───────────────────────────────────────────────────
//
// Nextcloud connections are independent of mail accounts: one user can
// have many mail accounts but a single Nextcloud that backs Talk,
// attachments, calendar and contacts. So these commands live on their
// own command family and their own JSON store.
//
// Auth is via Login Flow v2: the UI opens a browser URL, the user
// authorises, and the UI polls `poll_nextcloud_login` until the server
// returns the app password. Nothing in the app ever sees the real
// password — app passwords are revocable from the NC security page.

/// Begin Login Flow v2 — returns the URL to open in the browser plus a
/// polling handle the UI should use to drive `poll_nextcloud_login`.
#[tauri::command]
async fn start_nextcloud_login(server_url: String) -> Result<LoginFlowInit, NimbusError> {
    start_login(&server_url).await
}

/// Poll once for Login Flow v2 completion.
///
/// On success, this stores the app password in the OS keychain, queries
/// the server's capabilities, and persists a `NextcloudAccount` record.
/// The UI then just needs to refresh its `get_nextcloud_accounts` view.
///
/// Return shape matches Login Flow v2's own contract so the UI can
/// distinguish "not yet" (`Ok(None)`) from real errors.
#[tauri::command]
async fn poll_nextcloud_login(
    poll_endpoint: String,
    poll_token: String,
) -> Result<Option<NextcloudAccount>, NimbusError> {
    let Some(LoginFlowResult {
        server,
        login_name,
        app_password,
    }) = poll_login(&poll_endpoint, &poll_token).await?
    else {
        return Ok(None);
    };

    // Stable id derived from server + user so reconnecting updates
    // in place rather than duplicating. Escapes are unnecessary here —
    // `#` can't appear in a hostname or a reasonable NC login name.
    let id = format!("{server}#{login_name}");

    // Store the app password before persisting the account record: if
    // password storage fails the user gets a fresh error with no dead
    // account record left behind.
    credentials::store_nextcloud_password(&id, &app_password)?;

    // Best-effort capability snapshot. A working login with a broken
    // capabilities endpoint shouldn't block saving the account — we
    // can always refetch later.
    let capabilities = match fetch_capabilities(&server, &login_name, &app_password).await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("capabilities fetch failed, saving without: {e}");
            None
        }
    };

    let account = NextcloudAccount {
        id,
        server_url: server,
        username: login_name,
        display_name: None,
        capabilities,
    };
    nextcloud_store::upsert_account(account.clone())?;
    Ok(Some(account))
}

/// List all saved Nextcloud connections.
#[tauri::command]
fn get_nextcloud_accounts() -> Result<Vec<NextcloudAccount>, NimbusError> {
    nextcloud_store::load_accounts()
}

/// Re-probe `/ocs/v2.php/cloud/capabilities` for one account and
/// persist the fresh snapshot. Called by Settings on mount so newly-
/// installed Nextcloud apps (Office, Talk, …) light up their
/// indicator chip without the user having to disconnect + reconnect.
///
/// Soft-fails: a flaky network or revoked password returns the
/// account's previously-cached capabilities unchanged rather than
/// erroring out the whole settings panel.
#[tauri::command]
async fn refresh_nextcloud_capabilities(nc_id: String) -> Result<NextcloudAccount, NimbusError> {
    let mut account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    match fetch_capabilities(&account.server_url, &account.username, &app_password).await {
        Ok(caps) => {
            account.capabilities = Some(caps);
            nextcloud_store::upsert_account(account.clone())?;
        }
        Err(e) => {
            tracing::warn!("refresh_nextcloud_capabilities for {nc_id}: {e}");
        }
    }
    Ok(account)
}

/// Fetch the configured email address of the Nextcloud user owning
/// the given account. This is the same `email` field NC's Mail
/// Provider keys against for iMIP, so it's the right value to use as
/// `ORGANIZER` / CHAIR in calendar invites — making the calendar's
/// owning NC identity (not the user's first IMAP account) drive the
/// organizer line in the editor's attendee list.
///
/// Returns `None` when the user hasn't set an email in Personal info
/// or when the OCS lookup fails — caller should fall back to a
/// reasonable default (e.g. the first mail account).
#[tauri::command]
async fn get_nextcloud_user_email(nc_id: String) -> Result<Option<String>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    match nimbus_nextcloud::user::fetch_current_user(
        &account.server_url,
        &account.username,
        &app_password,
    )
    .await
    {
        Ok(profile) => Ok(profile.email),
        Err(e) => {
            tracing::warn!("get_nextcloud_user_email for {nc_id}: {e}");
            Ok(None)
        }
    }
}

/// Remove a Nextcloud connection and its stored app password.
///
/// Does **not** attempt to revoke the app password on the server —
/// that would require the password itself and we want removal to be
/// local-only, fast, and offline-safe. Users who want to fully revoke
/// can delete the app password from their NC security settings.
///
/// Also drops cached contacts, calendars, and their DAV sync state for
/// this account; a best-effort failure there is logged but doesn't
/// block removal.
#[tauri::command]
fn remove_nextcloud_account(id: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    credentials::delete_nextcloud_password(&id)?;
    if let Err(e) = cache.wipe_nextcloud_contacts(&id) {
        tracing::warn!("failed to wipe contacts for NC account '{id}': {e}");
    }
    if let Err(e) = cache.wipe_nextcloud_calendars(&id) {
        tracing::warn!("failed to wipe calendars for NC account '{id}': {e}");
    }
    nextcloud_store::remove_account(&id)
}

/// Open an arbitrary URL in the system's default browser.
///
/// Used by the Nextcloud login flow to hand the user off to their NC
/// server's login page, which happens outside our webview so the
/// browser can handle any SSO / IdP redirects the user's NC is wired
/// up with (Keycloak, OIDC, SAML, etc.).
#[tauri::command]
fn open_url(url: String) -> Result<(), NimbusError> {
    open::that(&url).map_err(|e| NimbusError::Other(format!("failed to open '{url}': {e}")))
}

// ── Nextcloud Files (browse + download) ────────────────────────
//
// WebDAV is stateless and per-folder: the UI asks for the children of
// a path, gets a listing, and asks again when the user navigates. We
// don't cache the tree — Nextcloud's PROPFIND is cheap, and cached
// listings go stale the moment a co-worker drops a new file in a
// shared folder. The picker lives entirely in memory.

/// List the immediate children of a folder in the user's Nextcloud.
///
/// `path` is relative to the user's root (e.g. `/` or `/Documents`).
/// Returns directories and files mixed, in the order the server sent
/// them — the UI sorts if it wants a particular display order.
#[tauri::command]
async fn list_nextcloud_files(nc_id: String, path: String) -> Result<Vec<FileEntry>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::list_directory(&account.server_url, &account.username, &app_password, &path)
        .await
}

/// Download a single file from Nextcloud.
///
/// Returns the raw bytes for the UI to stuff into a compose attachment
/// (or save wherever the caller needs). Large files are held in memory
/// for now — matches how locally-picked attachments work. A streaming
/// path is a separate future issue once compose itself streams.
#[tauri::command]
async fn download_nextcloud_file(nc_id: String, path: String) -> Result<Vec<u8>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::download_file(&account.server_url, &account.username, &app_password, &path)
        .await
}

/// Fetch a server-rendered preview thumbnail for a Nextcloud
/// file.  Used by the file picker to render inline thumbnails
/// for image / video rows.  Returns `None` (`Ok(None)`) when the
/// server has no preview for this file (404) so the frontend
/// silently falls back to the typed icon instead of surfacing an
/// error to the user.
#[tauri::command]
async fn nextcloud_file_preview(
    nc_id: String,
    path: String,
    size: Option<u32>,
) -> Result<Option<Vec<u8>>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let s = size.unwrap_or(96);
    match nimbus_nextcloud::fetch_preview(
        &account.server_url,
        &account.username,
        &app_password,
        &path,
        s,
    )
    .await
    {
        Ok(bytes) => Ok(Some(bytes)),
        // The 404 ("no preview available") path is legitimate —
        // surface as None so the picker just shows the icon.
        Err(NimbusError::Nextcloud(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Result of `create_nextcloud_share` — both the public URL (for
/// pasting into the email body) and the share id (for later
/// label updates via `update_nextcloud_share_label`).
#[derive(serde::Serialize)]
struct NextcloudShareResult {
    id: String,
    url: String,
}

/// Create a public share link for a Nextcloud file and return the
/// id + URL.
///
/// The compose UI uses this to insert a "click here to download" link
/// into the email body — a lighter alternative to attaching the bytes
/// for big files or files the recipient might want to re-download.
///
/// - `password`: optional, share is gated behind it on the recipient
///   side. The OCS endpoint enforces the user's configured password
///   policy.
/// - `label`: optional human-readable name for the share, visible
///   in Nextcloud's "Shared with others" list (#91).  Compose passes
///   the recipient string for an audit trail.  Empty / `None` leaves
///   Nextcloud's auto-naming intact.
/// - `permissions`: Nextcloud's permission bitmask
///   (1=read, 2=update, 4=create, 8=delete, 16=share).  The Compose
///   share modal exposes the common combinations as a dropdown.
#[tauri::command]
async fn create_nextcloud_share(
    nc_id: String,
    path: String,
    password: Option<String>,
    label: Option<String>,
    permissions: Option<u8>,
) -> Result<NextcloudShareResult, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let share = nimbus_nextcloud::create_public_share(
        &account.server_url,
        &account.username,
        &app_password,
        &path,
        password.as_deref(),
        label.as_deref(),
        permissions.unwrap_or(nimbus_nextcloud::shares::PERM_READ_ONLY),
    )
    .await?;
    Ok(NextcloudShareResult {
        id: share.id,
        url: share.url,
    })
}

/// Update the human-readable label of an existing Nextcloud share
/// (#91 follow-up).  Compose calls this when the user edits the
/// recipient list after a share link has already been minted —
/// otherwise the audit trail in Nextcloud's "Shared with others"
/// list freezes whatever the recipients were at click time.
#[tauri::command]
async fn update_nextcloud_share_label(
    nc_id: String,
    share_id: String,
    label: String,
) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::update_share_label(
        &account.server_url,
        &account.username,
        &app_password,
        &share_id,
        &label,
    )
    .await
}

/// Write raw bytes to a local file.
///
/// Used by the attachment Download flow: the frontend opens a native
/// "Save As" dialog (via `tauri-plugin-dialog`), the user picks a
/// destination, and the chosen absolute path + the attachment bytes
/// come back here. We use `std::fs::write` which truncates any file
/// already at that path — the native save dialog already asked the
/// user about overwrites, so we don't need a second confirmation.
#[tauri::command]
async fn save_bytes_to_path(path: String, data: Vec<u8>) -> Result<(), NimbusError> {
    // `write` is synchronous and the payload is typically a few MB — the
    // Tauri command runtime already runs us on a worker thread, so we
    // don't need to spawn_blocking.
    std::fs::write(&path, &data)
        .map_err(|e| NimbusError::Other(format!("Failed to write {path}: {e}")))
}

/// Upload raw bytes to a file in the user's Nextcloud.
///
/// The "Save to Nextcloud" action on a received email attachment calls
/// this with `path = <chosen folder>/<attachment filename>`. Existing
/// files at the same path are overwritten — the UI confirms with the
/// user before calling when that might be surprising.
#[tauri::command]
async fn upload_to_nextcloud(
    nc_id: String,
    path: String,
    data: Vec<u8>,
    content_type: Option<String>,
) -> Result<String, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::upload_file(
        &account.server_url,
        &account.username,
        &app_password,
        &path,
        data,
        content_type.as_deref(),
    )
    .await
}

/// Create a new (empty) folder in the user's Nextcloud.
///
/// `path` is the full path of the folder to create, relative to the
/// user's root (e.g. `/Documents/New Folder`). The parent must already
/// exist. The file picker calls this when the user clicks "New folder"
/// inside the currently-open directory; on success the picker re-lists
/// the parent so the new entry shows up.
#[tauri::command]
async fn create_nextcloud_directory(nc_id: String, path: String) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::create_directory(&account.server_url, &account.username, &app_password, &path)
        .await
}

// ── Office viewer (issue #65) ────────────────────────────────
//
// Click an Office-compatible attachment in MailView → upload its
// bytes to a per-user temp folder in the user's Nextcloud → return
// the deep-link URL the frontend opens in a Tauri webview window.
// On close, the frontend fires `office_close_attachment` which
// expunges the temp file. A separate `office_sweep_temp` runs at
// connect-time to clean up anything left behind by a crash mid-edit.
//
// Folder layout:
//   /Nimbus Mail/temp/<uuid>-<filename>
//
// The UUID prefix lets concurrent edits coexist without filename
// collisions and gives the sweeper an obvious "is-this-ours" gate
// (only delete files inside the temp folder).

/// Root path for Nimbus's per-user temp area on the user's
/// Nextcloud. Files-app-visible (no leading dot) so the user can
/// recover anything we somehow lose track of, but tucked under our
/// app's branded folder so the home screen stays uncluttered.
const NIMBUS_TEMP_ROOT: &str = "/Nimbus Mail";
const NIMBUS_TEMP_DIR: &str = "/Nimbus Mail/temp";

/// Result of `office_open_attachment` — the URL the frontend opens
/// in a fresh webview window plus the temp path it should pass back
/// to `office_close_attachment` on close.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OfficeOpenResult {
    /// Absolute URL into Nextcloud's Files app, which routes the
    /// file id to whichever app is registered as its handler —
    /// Collabora for Office docs, the PDF viewer for `.pdf`. Pasted
    /// directly into a `WebviewWindow` `url` arg.
    url: String,
    /// Path on the user's Nextcloud (relative to the user root).
    /// Round-trips back to `office_close_attachment` so the cleanup
    /// targets the file we just uploaded, not "all temp files".
    temp_path: String,
}

/// Best-effort `MKCOL` of `/Nimbus Mail` and `/Nimbus Mail/temp`.
/// Both are idempotent: `create_directory` returns "folder already
/// exists" as `NimbusError::Nextcloud` which we swallow so a
/// pre-existing folder doesn't fail the open. Anything else
/// propagates so quota / 401 / network errors surface to the user.
async fn ensure_temp_dir(
    account: &NextcloudAccount,
    app_password: &str,
) -> Result<(), NimbusError> {
    for dir in [NIMBUS_TEMP_ROOT, NIMBUS_TEMP_DIR] {
        match nimbus_nextcloud::create_directory(
            &account.server_url,
            &account.username,
            app_password,
            dir,
        )
        .await
        {
            Ok(()) => {}
            Err(NimbusError::Nextcloud(msg)) if msg.contains("already exists") => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Upload an attachment to the user's Nextcloud temp folder and
/// return the URL to open it in. Used by MailView when the user
/// clicks a `cid:` link or a tray button on an Office-compatible
/// attachment.
#[tauri::command]
async fn office_open_attachment(
    nc_id: String,
    filename: String,
    data: Vec<u8>,
    content_type: Option<String>,
) -> Result<OfficeOpenResult, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    ensure_temp_dir(&account, &app_password).await?;

    // UUID prefix dodges filename collisions between concurrent
    // viewer windows, and gives the sweeper a way to recognise our
    // own files without a metadata round-trip.
    let safe_name = filename.replace(['/', '\\'], "_");
    let temp_path = format!("{}/{}-{}", NIMBUS_TEMP_DIR, uuid::Uuid::new_v4(), safe_name);

    nimbus_nextcloud::upload_file(
        &account.server_url,
        &account.username,
        &app_password,
        &temp_path,
        data,
        content_type.as_deref(),
    )
    .await?;

    // Resolve the freshly-uploaded file's `oc:fileid` so we can
    // build the canonical `index.php/f/<id>` deep link. That URL
    // routes through Nextcloud's "open with default app" — Files
    // hands `.docx` etc. to Collabora, `.pdf` to the PDF viewer,
    // so the same code path works for both document types without
    // app-specific URL templating on our side.
    let file_id = nimbus_nextcloud::propfind_fileid(
        &account.server_url,
        &account.username,
        &app_password,
        &temp_path,
    )
    .await?;

    let server = account.server_url.trim_end_matches('/');
    let url = format!("{server}/index.php/f/{file_id}");

    Ok(OfficeOpenResult { url, temp_path })
}

/// Delete a temp file the frontend opened earlier. Best-effort:
/// 404 is swallowed by `delete_path`, network blips bubble up but
/// the frontend logs and moves on — leftover files get caught by
/// `office_sweep_temp` at next connect.
#[tauri::command]
async fn office_close_attachment(nc_id: String, temp_path: String) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::delete_path(
        &account.server_url,
        &account.username,
        &app_password,
        &temp_path,
    )
    .await
}

/// Result of `pdf_open_attachment`. Mirrors `OfficeOpenResult` so
/// the frontend can treat both viewers identically — same webview-
/// open + cleanup-on-close shape, the only difference is which URL
/// it points at.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfOpenResult {
    url: String,
    temp_path: String,
}

/// Open a PDF attachment in Nextcloud's built-in PDF viewer.
/// Same temp-upload + cleanup-on-close machinery as the Office flow:
///
///   - Bytes go to `/Nimbus Mail/temp/<uuid>-<filename>` on the user's
///     Nextcloud.
///   - We use the same `index.php/f/<fileid>` deep link the Office
///     viewer uses; Files routes the fileid to its registered
///     handler, which for `.pdf` is Nextcloud's built-in PDF
///     viewer.
///
/// On `pdf_close_attachment` (or the startup sweep) the temp file
/// is DAV-DELETED so the viewer URL stops resolving once the
/// viewer window closes.
#[tauri::command]
async fn pdf_open_attachment(
    nc_id: String,
    filename: String,
    data: Vec<u8>,
    content_type: Option<String>,
) -> Result<PdfOpenResult, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    ensure_temp_dir(&account, &app_password).await?;

    let safe_name = filename.replace(['/', '\\'], "_");
    let temp_path = format!("{}/{}-{}", NIMBUS_TEMP_DIR, uuid::Uuid::new_v4(), safe_name);

    nimbus_nextcloud::upload_file(
        &account.server_url,
        &account.username,
        &app_password,
        &temp_path,
        data,
        content_type.as_deref(),
    )
    .await?;

    let file_id = nimbus_nextcloud::propfind_fileid(
        &account.server_url,
        &account.username,
        &app_password,
        &temp_path,
    )
    .await?;
    let server = account.server_url.trim_end_matches('/');
    let url = format!("{server}/index.php/f/{file_id}");

    Ok(PdfOpenResult { url, temp_path })
}

/// DELETE the temp PDF the frontend opened. Same cleanup path as
/// Office — kept as its own command so the frontend's per-viewer
/// dispatch stays straightforward.
#[tauri::command]
async fn pdf_close_attachment(nc_id: String, temp_path: String) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::delete_path(
        &account.server_url,
        &account.username,
        &app_password,
        &temp_path,
    )
    .await
}

/// Clean up anything stuck in `/Nimbus Mail/temp` from a previous
/// session — say the user closed Nimbus mid-edit, or `office_close_
/// attachment` errored on the way out. We list the directory and
/// DELETE every entry whose `last_modified` is older than the cutoff,
/// so an in-flight viewer window in another Nimbus instance doesn't
/// have its file pulled out from under it.
#[tauri::command]
async fn office_sweep_temp(nc_id: String) -> Result<u32, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    // If the temp dir doesn't exist yet (fresh install / first
    // attachment click) treat that as "nothing to sweep". Anything
    // else propagates.
    let entries = match nimbus_nextcloud::list_directory(
        &account.server_url,
        &account.username,
        &app_password,
        NIMBUS_TEMP_DIR,
    )
    .await
    {
        Ok(e) => e,
        Err(NimbusError::Nextcloud(msg)) if msg.contains("not found") => return Ok(0),
        Err(e) => return Err(e),
    };

    let cutoff = chrono::Utc::now() - chrono::Duration::hours(1);
    let mut swept = 0u32;
    for entry in entries {
        let stale = entry.modified.map(|t| t < cutoff).unwrap_or(true);
        if !stale {
            continue;
        }
        let target = format!("{NIMBUS_TEMP_DIR}/{}", entry.name);
        match nimbus_nextcloud::delete_path(
            &account.server_url,
            &account.username,
            &app_password,
            &target,
        )
        .await
        {
            Ok(()) => swept += 1,
            Err(e) => tracing::warn!("office_sweep_temp: failed to delete {target}: {e}"),
        }
    }
    if swept > 0 {
        tracing::info!("office_sweep_temp: cleaned {swept} stale file(s)");
    }
    Ok(swept)
}

/// Open an attachment in its OS-default app so the user can print
/// it from there with the app's own print dialog. Used by the
/// "🖨 Open to print…" entry in the attachment dropdown.
///
/// Why this shape: the *generic* OS print dialog (Windows'
/// `PrintDialog`, the WinForms printer chooser) is just a printer
/// picker — it doesn't show the file, and it relies on each
/// file type's `PrintTo` verb being registered (Edge doesn't
/// register PrintTo for PDFs, so calling it for `.pdf` from a
/// fresh Windows install silently fails). The webview-rendered
/// Chromium print preview is brittle inside Tauri's sandbox.
///
/// What works reliably: open the file in its default handler
/// (Edge / Acrobat for PDF, Word for `.docx`, Photos for images,
/// Notepad for text, etc.) and let the user press **Ctrl/Cmd-P**.
/// Each app's own print dialog shows a real preview of the file
/// alongside the printer chooser — strictly better UX than the
/// generic OS dialog. The trade-off is one extra keystroke,
/// which the menu label calls out so the user expects it.
///
/// The temp file is kept for 10 minutes so the user has time
/// to actually print before we clean up.
#[tauri::command]
async fn print_attachment(file_name: String, bytes: Vec<u8>) -> Result<(), NimbusError> {
    let mut dir = std::env::temp_dir();
    dir.push(format!("nimbus-print-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| NimbusError::Other(format!("create print temp dir: {e}")))?;

    // Strip path separators / NUL from the filename so the spooler
    // sees a flat name in our temp dir, not a path traversal.
    let safe_name: String = file_name
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '_',
            _ => c,
        })
        .collect();
    let safe_name = if safe_name.trim().is_empty() {
        "attachment".to_string()
    } else {
        safe_name
    };
    let mut path = dir.clone();
    path.push(&safe_name);
    std::fs::write(&path, &bytes)
        .map_err(|e| NimbusError::Other(format!("write print temp file: {e}")))?;

    // `open::that_detached` is the cross-platform "default verb"
    // launcher: ShellExecute open on Windows, `open` on macOS,
    // `xdg-open` (and friends) on Linux. `_detached` so we don't
    // hold a child handle the user could orphan by closing Nimbus.
    if let Err(e) = open::that_detached(&path) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(NimbusError::Other(format!(
            "failed to open '{}' for printing: {e}",
            path.display()
        )));
    }

    let cleanup_dir = dir;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        if let Err(e) = tokio::fs::remove_dir_all(&cleanup_dir).await {
            tracing::debug!(
                "print_attachment cleanup: failed to remove {}: {e}",
                cleanup_dir.display()
            );
        }
    });

    Ok(())
}

// ── Nextcloud Talk ──────────────────────────────────────────────
//
// Three commands, mirroring the file/share pattern: each call loads
// the account + app password from local state and forwards to the
// matching `nimbus_nextcloud::talk::*` function. We don't cache the
// room list — Talk's `/room` is cheap and unread counts go stale the
// moment a colleague sends a message anyway. The sidebar polls on a
// timer instead.

/// List every Talk room the connected Nextcloud user is a participant
/// of. Drives the sidebar's "Talk Rooms" group.
#[tauri::command]
async fn list_talk_rooms(nc_id: String) -> Result<Vec<nimbus_nextcloud::TalkRoom>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::list_rooms(&account.server_url, &account.username, &app_password).await
}

/// Create a new group Talk room and invite `participants` to it.
///
/// `participants` carries a tagged enum (`{kind: "user"|"email", value: ...}`)
/// per invitee — `kind=email` triggers Talk's guest-invite flow so
/// recipients without a Nextcloud account get an emailed link. The
/// frontend builds this list from the email's To/Cc by treating
/// addresses matching the connected NC server's user list as `user`
/// and the rest as `email`. (For the MVP we always send `email` and
/// let Talk match users on the server side.)
#[tauri::command]
// `object_type` / `object_id` mirror Nextcloud Calendar's "Make
// it a Talk conversation" flow — pass `objectType: "event"` plus
// any random unique id to have Talk categorise the room as a
// meeting room.  Plain Compose-side "create Talk room" flows
// leave both `None`.
//
// `room_type` controls who can join: `2` = group/private (NC
// users only), `3` = public (anyone with the URL joins as
// guest).  Event-bound rooms default to `3` so externals
// invited via the calendar invite can click through without
// hitting the NC login wall.
async fn create_talk_room(
    nc_id: String,
    room_name: String,
    participants: Vec<nimbus_nextcloud::ParticipantSource>,
    object_type: Option<String>,
    object_id: Option<String>,
    room_type: Option<u8>,
) -> Result<nimbus_nextcloud::TalkRoom, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::create_room(
        &account.server_url,
        &account.username,
        &app_password,
        &room_name,
        &participants,
        nimbus_nextcloud::CreateRoomOptions {
            room_type,
            object_type: object_type.as_deref(),
            object_id: object_id.as_deref(),
        },
    )
    .await
}

/// Surgical PARTSTAT update for an event already in the user's
/// cache — the EventEditor's RSVP dropdown lands here when an
/// attendee changes their response on a meeting that's already
/// on the calendar.
///
/// Why we don't just route this through `update_calendar_event`:
/// regenerating the VEVENT body from form fields drops X-* lines
/// and re-orders properties, which Sabre's iTIP broker reads as
/// a "noisy" diff and silently suppresses the REPLY iMIP.  The
/// inbox card's `respond_to_invite` already implements the
/// byte-preserving surgical path; this command is a thin wrapper
/// that pulls the cached `ics_raw` for an existing event id and
/// hands it to `respond_to_invite` so the same flow applies.
#[tauri::command]
async fn rsvp_existing_event(
    event_id: String,
    partstat: String,
    attendee_hint: Option<String>,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let handle = load_event_handle(&cache, &event_id)?;
    let calendar_id = handle.calendar_id.clone();
    let raw_ics = handle.ics_raw.clone();
    respond_to_invite(calendar_id, raw_ics, partstat, attendee_hint, cache).await
}

/// Toggle a Talk room's public/private visibility.  Used by
/// the EventEditor save flow to downgrade a room from public
/// to private once we've confirmed every attendee is an
/// internal NC user — the externals-only flag is no longer
/// needed and the room shouldn't be join-by-URL after that
/// point.
#[tauri::command]
async fn set_talk_room_public(
    nc_id: String,
    room_token: String,
    public: bool,
) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::set_room_public(
        &account.server_url,
        &account.username,
        &app_password,
        &room_token,
        public,
    )
    .await
}

/// Look up a Nextcloud user by email address.  Returns the
/// matching userId + display name when the address is registered
/// against an NC principal on this server, or `None` otherwise.
/// Used by the EventEditor's chip badge ("internal" pill on
/// attendees who are NC users) and by the Talk participant-add
/// path (internal users get added as `users` source for an
/// in-NC notification, externals get added as `emails` source
/// so Talk emails them a guest URL).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NextcloudUserLookup {
    user_id: String,
    display_name: String,
}
#[tauri::command]
async fn find_nextcloud_user_by_email(
    nc_id: String,
    email: String,
) -> Result<Option<NextcloudUserLookup>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let m = nimbus_nextcloud::find_user_by_email(
        &account.server_url,
        &account.username,
        &app_password,
        &email,
    )
    .await?;
    Ok(m.map(|m| NextcloudUserLookup {
        user_id: m.user_id,
        display_name: m.display_name,
    }))
}

/// Promote an `Email`-source participant to a `User`-source one
/// whenever the address belongs to a real Nextcloud account on
/// this server (issue #124).  The internal user lands in the
/// room as themselves with an in-NC notification instead of
/// receiving a guest invite link via email — better UX, native
/// rights, and no second mail in the recipient's inbox.
///
/// Lookup is fail-soft: a network blip or an admin-restricted
/// sharees endpoint falls through to the original `Email`
/// source so the invite still gets out, just as a guest.  An
/// in-batch cache (`HashMap<lowercased-addr, ParticipantSource>`)
/// keeps duplicate addresses across the To/Cc list to a single
/// OCS round-trip.
async fn promote_email_to_user_if_internal(
    server_url: &str,
    username: &str,
    app_password: &str,
    src: &nimbus_nextcloud::ParticipantSource,
    cache: &mut std::collections::HashMap<String, nimbus_nextcloud::ParticipantSource>,
) -> nimbus_nextcloud::ParticipantSource {
    use nimbus_nextcloud::ParticipantSource;
    let addr = match src {
        ParticipantSource::User(_) => return src.clone(),
        ParticipantSource::Email(a) => a,
    };
    let key = addr.to_lowercase();
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let resolved = match nimbus_nextcloud::find_user_by_email(
        server_url,
        username,
        app_password,
        addr,
    )
    .await
    {
        Ok(Some(m)) => ParticipantSource::User(m.user_id),
        Ok(None) => src.clone(),
        Err(e) => {
            tracing::warn!(
                "talk-invite: NC user lookup failed for {addr}: {e}; \
                 falling back to email guest"
            );
            src.clone()
        }
    };
    cache.insert(key, resolved.clone());
    resolved
}

/// Add a single participant to an existing Talk room. Exposed so the
/// UI can grow an "Add participant" affordance later without a
/// backend round-trip.  Email-source participants whose address
/// matches a Nextcloud user on this server are silently promoted
/// to `User` source (issue #124).
#[tauri::command]
async fn add_talk_participant(
    nc_id: String,
    room_token: String,
    participant: nimbus_nextcloud::ParticipantSource,
) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let mut cache = std::collections::HashMap::new();
    let resolved = promote_email_to_user_if_internal(
        &account.server_url,
        &account.username,
        &app_password,
        &participant,
        &mut cache,
    )
    .await;
    nimbus_nextcloud::add_participant(
        &account.server_url,
        &account.username,
        &app_password,
        &room_token,
        &resolved,
    )
    .await
}

/// Batched add — invite a whole list of participants on a single
/// auth handshake.  Used by Compose's deferred-invite flow (#86):
/// we create the Talk room empty at compose-time and only invite
/// the recipients once `Send` actually goes out, so a discarded
/// draft doesn't leave a room full of strangers in the recipient's
/// Talk list.  Sequential (not parallel) so the first failure halts
/// the batch and surfaces as a single error.  Email-source entries
/// whose address matches a Nextcloud user on this server are
/// promoted to `User` source per issue #124 — internal recipients
/// join natively, externals still get the email-guest flow.
#[tauri::command]
async fn add_talk_participants(
    nc_id: String,
    room_token: String,
    participants: Vec<nimbus_nextcloud::ParticipantSource>,
) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let mut cache = std::collections::HashMap::new();
    for p in &participants {
        let resolved = promote_email_to_user_if_internal(
            &account.server_url,
            &account.username,
            &app_password,
            p,
            &mut cache,
        )
        .await;
        nimbus_nextcloud::add_participant(
            &account.server_url,
            &account.username,
            &app_password,
            &room_token,
            &resolved,
        )
        .await?;
    }
    Ok(())
}

/// Tear down a Talk room (#86).  Compose's `cancel` flow calls this
/// whenever the user discards a draft that minted a room earlier
/// in the session — without it, the room would dangle empty in the
/// user's Talk list with no context.
#[tauri::command]
async fn delete_talk_room(nc_id: String, room_token: String) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::delete_room(
        &account.server_url,
        &account.username,
        &app_password,
        &room_token,
    )
    .await
}

/// Rename an existing Talk room. Used by the Compose "Add Event"
/// flow to keep the auto-created Talk room's name in sync with the
/// final event title once the user saves the event.
#[tauri::command]
async fn rename_talk_room(
    nc_id: String,
    room_token: String,
    new_name: String,
) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::rename_room(
        &account.server_url,
        &account.username,
        &app_password,
        &room_token,
        &new_name,
    )
    .await
}

// ── Nextcloud Notes (issue #67) ────────────────────────────────
//
// Five thin commands wrapping `nimbus_nextcloud::notes`. Same
// shape as the Talk block above: each call loads the chosen NC
// account + app password and forwards. The Notes API is plain
// REST under `/index.php/apps/notes/api/v1/notes`, so there's no
// envelope unpacking — the wire types come straight back.
//
// We deliberately don't cache notes locally: the Notes web UI is
// the canonical editor and we want NotesView to reflect what the
// user just typed there without a sync-roundtrip dance. Cost is
// one HTTP call per list-refresh, which is cheap.

/// List every note the connected Nextcloud user can see.
#[tauri::command]
async fn list_nextcloud_notes(nc_id: String) -> Result<Vec<nimbus_nextcloud::Note>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::list_notes(&account.server_url, &account.username, &app_password).await
}

/// Fetch a single note, primarily to refresh the etag right before
/// an edit lands so we don't trip a 412 on a note the user looked
/// at long ago.
#[tauri::command]
async fn get_nextcloud_note(
    nc_id: String,
    note_id: u64,
) -> Result<nimbus_nextcloud::Note, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::get_note(
        &account.server_url,
        &account.username,
        &app_password,
        note_id,
    )
    .await
}

/// Create a new note. Title can be empty — the server derives it
/// from the first content line in that case, matching the behaviour
/// of the Notes web UI.
#[tauri::command]
async fn create_nextcloud_note(
    nc_id: String,
    title: String,
    content: String,
    category: String,
) -> Result<nimbus_nextcloud::Note, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::create_note(
        &account.server_url,
        &account.username,
        &app_password,
        &nimbus_nextcloud::NewNote {
            title: &title,
            content: &content,
            category: &category,
        },
    )
    .await
}

/// Apply a partial update. Each field is optional — the frontend
/// sends only the ones the user touched so a category-only edit
/// doesn't have to round-trip body bytes the user didn't change.
#[tauri::command]
async fn update_nextcloud_note(
    nc_id: String,
    note_id: u64,
    etag: String,
    title: Option<String>,
    content: Option<String>,
    category: Option<String>,
    favorite: Option<bool>,
) -> Result<nimbus_nextcloud::Note, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::update_note(
        &account.server_url,
        &account.username,
        &app_password,
        note_id,
        &etag,
        &nimbus_nextcloud::NoteUpdate {
            title: title.as_deref(),
            content: content.as_deref(),
            category: category.as_deref(),
            favorite,
        },
    )
    .await
}

/// Delete a note. Called by the trash button in NotesView; the
/// frontend confirms in JS first so we don't need a confirm here.
#[tauri::command]
async fn delete_nextcloud_note(nc_id: String, note_id: u64) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::delete_note(
        &account.server_url,
        &account.username,
        &app_password,
        note_id,
    )
    .await
}

// ── CardDAV contacts ────────────────────────────────────────────
//
// Contact sync is driven from a single entry point: the UI calls
// `sync_nextcloud_contacts(nc_id)` (a "Sync now" button in settings,
// or a background tick after login). That command walks the user's
// addressbooks, runs one incremental sync per book via sync-collection
// REPORT, and applies each delta to the local cache transactionally.
//
// The UI never sees hrefs, etags, or sync tokens — it reads fully
// hydrated `Contact` records from the cache via `get_contacts` (list
// view) and `search_contacts` (autocomplete).

/// Summary returned to the UI after a contacts sync run.
///
/// Per-addressbook counts let the UI say something more useful than
/// "done" — e.g. "Contacts: 12 new, 1 removed". `errors` carries the
/// list of addressbooks that failed so the overall sync doesn't look
/// green when one book silently fell over.
#[derive(Debug, Clone, Serialize)]
struct SyncContactsReport {
    nc_account_id: String,
    books_synced: u32,
    upserted: u32,
    deleted: u32,
    errors: Vec<String>,
}

/// Pull the latest contacts from a Nextcloud account.
///
/// Two-step: list addressbooks (PROPFIND on the user's home), then
/// run an incremental sync-collection REPORT against each. Each
/// addressbook's delta is committed to the local cache in its own
/// transaction, so a failure on book N+1 doesn't roll back book N.
/// Per-book errors are logged and accumulated into the report rather
/// than aborting the whole run.
#[tauri::command]
async fn sync_nextcloud_contacts(
    nc_id: String,
    cache: State<'_, Cache>,
) -> Result<SyncContactsReport, NimbusError> {
    let account = nextcloud_store::load_accounts()?
        .into_iter()
        .find(|a| a.id == nc_id)
        .ok_or_else(|| NimbusError::Other(format!("no Nextcloud account with id '{nc_id}'")))?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    let books = list_addressbooks(&account.server_url, &account.username, &app_password).await?;
    tracing::info!(
        "CardDAV: {} addressbook(s) to sync for {}",
        books.len(),
        nc_id
    );

    let mut report = SyncContactsReport {
        nc_account_id: nc_id.clone(),
        books_synced: 0,
        upserted: 0,
        deleted: 0,
        errors: Vec::new(),
    };

    for book in books {
        // Prior token (if any) makes the REPORT incremental; missing
        // state means first sync and the CardDAV layer handles that.
        let prev_token = cache
            .get_addressbook_sync_state(&nc_id, &book.name)
            .ok()
            .flatten()
            .and_then(|s| s.sync_token);

        let delta = match sync_addressbook(
            &account.server_url,
            &book.path,
            &account.username,
            &app_password,
            prev_token.as_deref(),
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("CardDAV sync failed for book '{}': {e}", book.name);
                report.errors.push(format!("{}: {e}", book.name));
                continue;
            }
        };

        let upserts: Vec<ContactRow> = delta.upserts.iter().map(raw_contact_to_row).collect();

        if let Err(e) = cache.apply_contact_delta(
            &nc_id,
            &book.name,
            book.display_name.as_deref(),
            &upserts,
            &delta.deleted_hrefs,
            delta.new_sync_token.as_deref(),
            book.ctag.as_deref(),
        ) {
            tracing::warn!("apply_contact_delta failed for '{}': {e}", book.name);
            report.errors.push(format!("{}: {e}", book.name));
            continue;
        }

        report.books_synced += 1;
        report.upserted += upserts.len() as u32;
        report.deleted += delta.deleted_hrefs.len() as u32;
    }

    Ok(report)
}

/// Cache-only list of contacts, optionally scoped to a single NC account.
#[tauri::command]
fn get_contacts(
    nc_id: Option<String>,
    cache: State<'_, Cache>,
) -> Result<Vec<Contact>, NimbusError> {
    cache.list_contacts(nc_id.as_deref()).map_err(Into::into)
}

/// Substring search over cached contacts — feeds the compose
/// autocomplete dropdown. `limit` caps the row count so a stray
/// single-character query can't return the whole address book.
#[tauri::command]
fn search_contacts(
    query: String,
    limit: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<Contact>, NimbusError> {
    cache.search_contacts(&query, limit).map_err(Into::into)
}

/// Aggregate sync status for the Settings UI's Contacts and
/// Calendars rows. Both surfaces want the same shape: when did we
/// last successfully sync, and what's the cached count? — so we
/// share the struct and reuse the `SyncStatusRow` Svelte component.
#[derive(Debug, Clone, Serialize)]
struct SyncStatus {
    /// RFC 3339 timestamp of the most recent successful sync across
    /// every addressbook / calendar for this account, or `None` if
    /// the account has never finished one. The frontend formats it
    /// relative ("12m ago" / "Synced just now").
    last_synced_at: Option<String>,
    /// Cached row count for this account (contacts or calendars).
    /// Mostly informational — the row title carries the meaningful
    /// "are we up to date?" signal.
    count: u32,
}

#[tauri::command]
fn get_contacts_sync_status(
    nc_id: String,
    cache: State<'_, Cache>,
) -> Result<SyncStatus, NimbusError> {
    let last = cache
        .latest_addressbook_sync_at(&nc_id)
        .map_err(NimbusError::from)?
        .map(|t| t.to_rfc3339());
    let count = cache.count_contacts(&nc_id).map_err(NimbusError::from)?;
    Ok(SyncStatus {
        last_synced_at: last,
        count,
    })
}

#[tauri::command]
fn get_calendars_sync_status(
    nc_id: String,
    cache: State<'_, Cache>,
) -> Result<SyncStatus, NimbusError> {
    let last = cache
        .latest_calendar_sync_at(&nc_id)
        .map_err(NimbusError::from)?
        .map(|t| t.to_rfc3339());
    let count = cache
        .list_calendars(&nc_id)
        .map(|cs| cs.len() as u32)
        .unwrap_or(0);
    Ok(SyncStatus {
        last_synced_at: last,
        count,
    })
}

/// Fetched separately from `get_contacts` because photo bytes are
/// huge and Tauri serialises them as JSON number arrays — shipping
/// every photo with the list payload made the contacts view feel
/// laggy. The UI requests photos only for rows it actually paints.
#[derive(Debug, Clone, Serialize)]
struct ContactPhoto {
    mime: String,
    data: Vec<u8>,
}

#[tauri::command]
fn get_contact_photo(
    contact_id: String,
    cache: State<'_, Cache>,
) -> Result<Option<ContactPhoto>, NimbusError> {
    Ok(cache
        .get_contact_photo(&contact_id)
        .map_err(NimbusError::from)?
        .map(|(mime, data)| ContactPhoto { mime, data }))
}

/// Field-for-field copy between the CardDAV crate's `RawContact` and
/// the store crate's `ContactRow`. Kept as a free function so neither
/// crate has to depend on the other — the Tauri layer is the only
/// place both are in scope.
fn raw_contact_to_row(c: &RawContact) -> ContactRow {
    ContactRow {
        href: c.href.clone(),
        etag: c.etag.clone(),
        vcard_uid: c.vcard_uid.clone(),
        display_name: c.display_name.clone(),
        emails: c
            .emails
            .iter()
            .map(|e| nimbus_core::models::ContactEmail {
                kind: e.kind.clone(),
                value: e.value.clone(),
            })
            .collect(),
        phones: c
            .phones
            .iter()
            .map(|p| nimbus_core::models::ContactPhone {
                kind: p.kind.clone(),
                value: p.value.clone(),
            })
            .collect(),
        organization: c.organization.clone(),
        photo_mime: c.photo_mime.clone(),
        photo_data: c.photo_data.clone(),
        title: c.title.clone(),
        birthday: c.birthday.clone(),
        note: c.note.clone(),
        addresses: c
            .addresses
            .iter()
            .map(|a| nimbus_core::models::ContactAddress {
                kind: a.kind.clone(),
                street: a.street.clone(),
                locality: a.locality.clone(),
                region: a.region.clone(),
                postal_code: a.postal_code.clone(),
                country: a.country.clone(),
            })
            .collect(),
        urls: c.urls.clone(),
        vcard_raw: c.vcard_raw.clone(),
        kind: c.kind.clone(),
        member_uids: c.member_uids.clone(),
        categories: c.categories.clone(),
    }
}

// ── CardDAV writes (create / update / delete) ───────────────────
//
// These three commands are the UI's entry points for editing
// contacts. They each do the same three-step dance:
//
// 1. Build a vCard 4.0 body from the form input.
// 2. PUT / DELETE against the CardDAV server with the right
//    precondition (If-None-Match for create, If-Match for
//    update/delete) so conflicting writes surface as a structured
//    error rather than silently clobbering remote state.
// 3. Write through to the local cache so the UI reflects the
//    change immediately — we don't wait for the next sync tick.
//
// For update/delete we look up the server bookkeeping (href, etag,
// addressbook) by contact id; the UI never has to carry those around.

/// Editable fields for a contact, shared by create and update.
/// The "extended" block (title, birthday, note, addresses, urls)
/// is optional so older UI versions that don't surface those
/// fields keep working — `update_contact` merges over the cached
/// vCard, so missing fields preserve whatever's on the server
/// instead of clobbering it.
#[derive(Debug, Clone, Deserialize)]
struct ContactInput {
    display_name: String,
    emails: Vec<nimbus_core::models::ContactEmail>,
    phones: Vec<nimbus_core::models::ContactPhone>,
    organization: Option<String>,
    photo_mime: Option<String>,
    photo_data: Option<Vec<u8>>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    birthday: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    addresses: Option<Vec<nimbus_core::models::ContactAddress>>,
    #[serde(default)]
    urls: Option<Vec<String>>,
}

/// Create a new contact on Nextcloud and cache it locally.
///
/// `addressbook_url` is the absolute URL of the target book (the
/// `path` field on `Addressbook`). The UI picks it up from the
/// sync report or a dedicated listing command.
///
/// Generates a fresh UUID for the vCard's UID so callers don't
/// have to, and returns the newly cached `Contact` so the UI can
/// slot it straight into its list without re-fetching.
#[tauri::command]
async fn create_contact(
    nc_id: String,
    addressbook_url: String,
    addressbook_name: String,
    input: ContactInput,
    cache: State<'_, Cache>,
) -> Result<Contact, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    let uid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let parsed = input_to_parsed(&uid, &input);
    let vcard = build_vcard(&parsed);

    let outcome = carddav_create_contact(
        &account.server_url,
        &addressbook_url,
        &account.username,
        &app_password,
        &uid,
        &vcard,
    )
    .await?;

    let row = parsed_to_row(&outcome.href, &outcome.etag, &uid, &parsed, vcard);
    cache
        .upsert_single_contact(&nc_id, &addressbook_name, &row)
        .map_err(NimbusError::from)?;

    Ok(row_to_contact(&nc_id, &addressbook_name, &row))
}

/// Replace an existing contact on the server with the form's new
/// values. `If-Match` on the cached etag means a concurrent edit
/// on another device surfaces as a 412 (mapped to a readable error)
/// rather than silently overwriting.
#[tauri::command]
async fn update_contact(
    contact_id: String,
    input: ContactInput,
    cache: State<'_, Cache>,
) -> Result<Contact, NimbusError> {
    let handle = load_contact_handle(&cache, &contact_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;

    // Merge the form fields over the existing parsed vCard so fields
    // the edit form doesn't surface (addresses, birthday, urls, note,
    // title, …) round-trip instead of being silently wiped on every
    // edit. The form-editable fields below replace whatever was there.
    let mut parsed = match nimbus_carddav::parse_vcard(&handle.vcard_raw) {
        Ok(p) => p,
        Err(_) => ParsedVcard {
            uid: handle.vcard_uid.clone(),
            ..Default::default()
        },
    };
    parsed.uid = handle.vcard_uid.clone();
    parsed.display_name = input.display_name.clone();
    parsed.emails = input
        .emails
        .iter()
        .map(|e| nimbus_carddav::VcardEmail {
            kind: e.kind.clone(),
            value: e.value.clone(),
        })
        .collect();
    parsed.phones = input
        .phones
        .iter()
        .map(|p| nimbus_carddav::VcardPhone {
            kind: p.kind.clone(),
            value: p.value.clone(),
        })
        .collect();
    parsed.organization = input.organization.clone();
    if input.photo_data.is_some() {
        parsed.photo_mime = input.photo_mime.clone();
        parsed.photo_data = input.photo_data.clone();
    }
    // Extended fields: a UI that surfaces them sends the new value
    // (or `None` to clear); a UI that doesn't sends `Option::None`
    // for the *whole field*, in which case we leave the cached
    // value alone. The distinction is made via `serde(default)` on
    // `ContactInput` — `None` only ever appears when the JSON omits
    // the key entirely, never when the user explicitly cleared it.
    if let Some(t) = &input.title {
        parsed.title = if t.is_empty() { None } else { Some(t.clone()) };
    }
    if let Some(b) = &input.birthday {
        parsed.birthday = if b.is_empty() { None } else { Some(b.clone()) };
    }
    if let Some(n) = &input.note {
        parsed.note = if n.is_empty() { None } else { Some(n.clone()) };
    }
    if let Some(addrs) = &input.addresses {
        parsed.addresses = addrs
            .iter()
            .map(|a| nimbus_carddav::VcardAddress {
                kind: a.kind.clone(),
                street: a.street.clone(),
                locality: a.locality.clone(),
                region: a.region.clone(),
                postal_code: a.postal_code.clone(),
                country: a.country.clone(),
            })
            .collect();
    }
    if let Some(urls) = &input.urls {
        parsed.urls = urls.clone();
    }
    let vcard = build_vcard(&parsed);

    let outcome = carddav_update_contact(
        &handle.href,
        &account.username,
        &app_password,
        &handle.etag,
        &vcard,
    )
    .await?;

    let row = parsed_to_row(
        &outcome.href,
        &outcome.etag,
        &handle.vcard_uid,
        &parsed,
        vcard,
    );
    cache
        .upsert_single_contact(&handle.nextcloud_account_id, &handle.addressbook, &row)
        .map_err(NimbusError::from)?;

    Ok(row_to_contact(
        &handle.nextcloud_account_id,
        &handle.addressbook,
        &row,
    ))
}

/// Delete a contact from the server and the local cache. The
/// server delete is gated on the cached etag; if that fails we
/// leave the cache row alone so the UI can show the user the
/// fresh state on the next sync.
#[tauri::command]
async fn delete_contact(contact_id: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    let handle = load_contact_handle(&cache, &contact_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;

    carddav_delete_contact(&handle.href, &account.username, &app_password, &handle.etag).await?;

    cache
        .delete_contact_by_id(&contact_id)
        .map_err(NimbusError::from)?;
    Ok(())
}

// ── Reserved Kontaktgruppe (#133 redesign) ────────────────────
//
// Manual mailing lists (KIND:group vCards) are auto-tagged with
// this CATEGORY so iOS / Apple Contacts / NC Contacts surface
// them in a dedicated "Mailing Lists" group.  The
// `list_mailing_lists` IPC filters this exact name out of the
// virtual-row derivation so we don't end up with a circular
// "Mailing Lists" mailing list of mailing lists.
const MAILING_LISTS_CATEGORY: &str = "Mailing Lists";

// ── Categories / Kontaktgruppen (#133 redesign) ──────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContactCategoryView {
    /// CATEGORY name as written on the vCards.
    name: String,
    /// Number of cached contacts carrying this CATEGORY.
    member_count: u32,
    /// True when the user has flipped "Use as mailing list"
    /// off on this category — drives both "no virtual row in
    /// the Mailing Lists tab" and "no autocomplete suggestion".
    use_as_mailing_list: bool,
}

/// Distinct CATEGORIES across every cached contact, with the
/// per-row "use as mailing list" overlay applied.
///
/// First call after the v17 → v18 migration backfills the
/// `categories_json` column from the cached `vcard_raw` for
/// every row whose tag list is still empty.  Idempotent —
/// once a row has a non-empty `categories_json` it's skipped.
#[tauri::command]
fn list_contact_categories(
    cache: State<'_, Cache>,
) -> Result<Vec<ContactCategoryView>, NimbusError> {
    let _ = cache.backfill_categories(|raw| {
        nimbus_carddav::parse_vcard(raw)
            .map(|p| p.categories)
            .unwrap_or_default()
    });
    let cats = cache.list_contact_categories().map_err(NimbusError::from)?;
    let suppressed = cache
        .get_mailing_list_suppressed()
        .map_err(NimbusError::from)?;
    Ok(cats
        .into_iter()
        .filter(|(name, _)| name != MAILING_LISTS_CATEGORY)
        .map(|(name, member_count)| {
            let id = format!("cat:{name}");
            ContactCategoryView {
                use_as_mailing_list: !suppressed.contains(&id),
                name,
                member_count,
            }
        })
        .collect())
}

/// Toggle "use as mailing list" for one Kontaktgruppe.
#[tauri::command]
fn set_category_use_as_mailing_list(
    name: String,
    enabled: bool,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let id = format!("cat:{name}");
    cache
        .set_mailing_list_suppressed(&id, !enabled)
        .map_err(NimbusError::from)
}

/// Add a CATEGORIES tag to one contact's vCard, sync to the
/// server.  Idempotent — a contact already in the category is
/// left alone (no spurious PUT).
#[tauri::command]
async fn add_contact_to_category(
    contact_id: String,
    category: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    rewrite_contact_categories(&contact_id, &cache, |cats| {
        if !cats.iter().any(|c| c == &category) {
            cats.push(category.clone());
            true
        } else {
            false
        }
    })
    .await
}

/// Remove one CATEGORIES tag from a contact's vCard.
#[tauri::command]
async fn remove_contact_from_category(
    contact_id: String,
    category: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    rewrite_contact_categories(&contact_id, &cache, |cats| {
        let before = cats.len();
        cats.retain(|c| c != &category);
        cats.len() != before
    })
    .await
}

/// Rename a category across every contact carrying it.  Loops
/// each tagged contact, rewrites the CATEGORIES list, PUTs.
/// Best-effort per-contact: a failure on one row logs and
/// continues so a flaky network doesn't strand the rename
/// half-applied (the next sync would heal anyway).
#[tauri::command]
async fn rename_contact_category(
    old: String,
    new: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let new = new.trim().to_string();
    if new.is_empty() {
        return Err(NimbusError::Other("new category name is empty".into()));
    }
    let contacts = cache
        .list_contacts_with_category(&old)
        .map_err(NimbusError::from)?;
    for c in contacts {
        if let Err(e) = rewrite_contact_categories_inner(&c.id, &cache, |cats| {
            let mut changed = false;
            for cat in cats.iter_mut() {
                if cat == &old {
                    *cat = new.clone();
                    changed = true;
                }
            }
            if !cats.iter().any(|c| c == &new) {
                cats.push(new.clone());
                changed = true;
            }
            cats.retain(|c| c != &old);
            changed
        })
        .await
        {
            tracing::warn!("rename category on {}: {e}", c.id);
        }
    }
    // Carry the suppressed flag over to the new id so the
    // user's "use as mailing list" choice doesn't reset.
    let suppressed = cache
        .get_mailing_list_suppressed()
        .map_err(NimbusError::from)?;
    if suppressed.contains(&format!("cat:{old}")) {
        cache
            .set_mailing_list_suppressed(&format!("cat:{old}"), false)
            .map_err(NimbusError::from)?;
        cache
            .set_mailing_list_suppressed(&format!("cat:{new}"), true)
            .map_err(NimbusError::from)?;
    }
    // Carry the per-list emoji overlay across the rename too.
    cache
        .rename_mailing_list_setting(&format!("cat:{old}"), &format!("cat:{new}"))
        .map_err(NimbusError::from)?;
    Ok(())
}

/// Delete a category — strips the tag from every contact.  The
/// underlying contacts are untouched, just no longer tagged.
#[tauri::command]
async fn delete_contact_category(name: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    let contacts = cache
        .list_contacts_with_category(&name)
        .map_err(NimbusError::from)?;
    for c in contacts {
        if let Err(e) = rewrite_contact_categories_inner(&c.id, &cache, |cats| {
            let before = cats.len();
            cats.retain(|cc| cc != &name);
            cats.len() != before
        })
        .await
        {
            tracing::warn!("delete category on {}: {e}", c.id);
        }
    }
    Ok(())
}

/// Public wrapper that takes a `State<'_, Cache>` and forwards
/// to the private inner — keeps the create/rename/delete IPCs
/// tidy without making them all duplicate the cache extraction.
async fn rewrite_contact_categories<F>(
    contact_id: &str,
    cache: &State<'_, Cache>,
    f: F,
) -> Result<(), NimbusError>
where
    F: FnOnce(&mut Vec<String>) -> bool,
{
    rewrite_contact_categories_inner(contact_id, cache, f).await
}

/// Pull the cached vCard for `contact_id`, mutate its
/// CATEGORIES list via `f`, and PUT the rewritten body back to
/// CardDAV.  Returns early when `f` reports no change so we
/// don't burn a round-trip on a no-op.
async fn rewrite_contact_categories_inner<F>(
    contact_id: &str,
    cache: &Cache,
    f: F,
) -> Result<(), NimbusError>
where
    F: FnOnce(&mut Vec<String>) -> bool,
{
    let handle = load_contact_handle(cache, contact_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;
    let mut parsed = match nimbus_carddav::parse_vcard(&handle.vcard_raw) {
        Ok(p) => p,
        Err(_) => ParsedVcard {
            uid: handle.vcard_uid.clone(),
            ..Default::default()
        },
    };
    parsed.uid = handle.vcard_uid.clone();
    let changed = f(&mut parsed.categories);
    if !changed {
        return Ok(());
    }
    let vcard = build_vcard(&parsed);
    let outcome = carddav_update_contact(
        &handle.href,
        &account.username,
        &app_password,
        &handle.etag,
        &vcard,
    )
    .await?;
    let row = parsed_to_row(
        &outcome.href,
        &outcome.etag,
        &handle.vcard_uid,
        &parsed,
        vcard,
    );
    cache
        .upsert_single_contact(&handle.nextcloud_account_id, &handle.addressbook, &row)
        .map_err(NimbusError::from)?;
    Ok(())
}

// ── Unified mailing lists (#133 redesign) ─────────────────────
//
// Single IPC the Mailing Lists tab + AddressAutocomplete read
// from.  Combines four sources into one flat list:
//   * `cat:<name>`  — a Kontaktgruppe (CATEGORY tag) with
//     `use_as_mailing_list = true`.
//   * `group:<id>`  — an OCS user group.
//   * `team:<id>`   — a Circles / Teams entry.
//   * `list:<uid>`  — a manual KIND:group vCard.
// The reserved `Mailing Lists` category is filtered out so the
// auto-tag we put on every manual list doesn't generate a
// circular row.

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MailingListView {
    /// Unified id — see source-prefix list above.
    id: String,
    /// `category` | `nc-group` | `team` | `manual`.  Drives the
    /// pill colour + the CRUD affordances.
    source: String,
    name: String,
    members: Vec<MailingListMemberView>,
    /// Local-only flag — when true the row is suppressed from
    /// AddressAutocomplete.  Categories use the same flag for
    /// the "Use as mailing list" toggle (off → suppressed).
    hidden_from_autocomplete: bool,
    /// Local-only emoji avatar override; `None` falls back to
    /// the source's default icon (🏷️/📨/⚡).
    emoji: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MailingListMemberView {
    display_name: String,
    email: String,
}

/// Build the unified mailing-list view across every source.
/// Read-heavy but cheap — categories are aggregated in one
/// SQL pass and the NC group / team list reuses the existing
/// list_nextcloud_groups path.
#[tauri::command]
async fn list_mailing_lists(cache: State<'_, Cache>) -> Result<Vec<MailingListView>, NimbusError> {
    // Same lazy backfill list_contact_categories does — this
    // IPC is the entry point the autocomplete uses on first
    // launch, before the contacts UI was opened, so we have to
    // rehydrate categories here too or the category-derived
    // rows would surface with zero members.
    let _ = cache.backfill_categories(|raw| {
        nimbus_carddav::parse_vcard(raw)
            .map(|p| p.categories)
            .unwrap_or_default()
    });
    let suppressed = cache
        .get_mailing_list_suppressed()
        .map_err(NimbusError::from)?;
    let emojis = cache.get_mailing_list_emojis().map_err(NimbusError::from)?;
    let mut out: Vec<MailingListView> = Vec::new();

    // 1. Categories.  Skip the reserved one we use as a holder
    // for KIND:group vCards.
    let cats = cache.list_contact_categories().map_err(NimbusError::from)?;
    for (name, _count) in cats {
        if name == MAILING_LISTS_CATEGORY {
            continue;
        }
        let id = format!("cat:{name}");
        // Category rows stay in the Lists tab regardless of
        // the hide flag, so the user can toggle them back on
        // from the same swatch they used to turn them off.
        // The autocomplete client-side filter is what actually
        // suppresses suggestions; the row carries the flag so
        // the UI can render it greyed-out.
        let hidden_from_autocomplete = suppressed.contains(&id);
        let contacts = cache.list_contacts_with_category(&name).unwrap_or_default();
        // Drop members that have no email — a category-derived
        // mailing list is only useful as a sending target, and
        // a row with empty email would just be noise (and
        // would expand to an unaddressable entry in compose
        // autocomplete).  Contacts without email still show
        // up in the Contacts tab under their Contact Group;
        // they only get hidden here in the mailing-list view.
        let members: Vec<MailingListMemberView> = contacts
            .into_iter()
            .filter_map(|c| {
                let email = c
                    .email
                    .into_iter()
                    .next()
                    .map(|e| e.value)
                    .unwrap_or_default();
                if email.is_empty() {
                    None
                } else {
                    Some(MailingListMemberView {
                        display_name: c.display_name,
                        email,
                    })
                }
            })
            .collect();
        let emoji = emojis.get(&id).cloned();
        out.push(MailingListView {
            id,
            source: "category".to_string(),
            name,
            members,
            hidden_from_autocomplete,
            emoji,
        });
    }

    // 2. Manual KIND:group vCards.  These already auto-tag the
    // reserved category so they show up in the Mailing Lists
    // Kontaktgruppe in NC; here we render them directly.
    if let Ok(groups) = cache.list_contact_groups() {
        for g in groups {
            let id = format!("list:{}", g.id);
            let suppressed_row = suppressed.contains(&id);
            let resolved = cache
                .resolve_group_members(&g.nextcloud_account_id, &g.member_uids)
                .unwrap_or_default();
            let members = resolved
                .into_iter()
                .map(|(_id, name, email)| MailingListMemberView {
                    display_name: name,
                    email,
                })
                .collect();
            let emoji = emojis.get(&id).cloned().or_else(|| g.emoji.clone());
            out.push(MailingListView {
                id,
                source: "manual".to_string(),
                name: g.display_name,
                members,
                hidden_from_autocomplete: suppressed_row,
                emoji,
            });
        }
    }

    // 3. Teams.  list_nextcloud_groups already returns OCS
    // user groups + Circles unified under `source = "team"`
    // with cleaned display names — we just forward each row
    // verbatim.  These refresh every call (typically a handful
    // per server, so live OCS round-trip is fine).
    let nc_groups = list_nextcloud_groups(cache).await.unwrap_or_default();
    for g in nc_groups {
        let id = g.id;
        let suppressed_row = suppressed.contains(&id);
        let members = g
            .members
            .into_iter()
            .map(|m| MailingListMemberView {
                display_name: m.display_name,
                email: m.email,
            })
            .collect();
        let emoji = emojis.get(&id).cloned();
        out.push(MailingListView {
            id,
            source: "team".to_string(),
            name: g.display_name,
            members,
            hidden_from_autocomplete: suppressed_row,
            emoji,
        });
    }

    Ok(out)
}

/// Toggle the local hide-from-autocomplete flag for one
/// mailing-list row.  Used by the per-row swatch on
/// non-category rows (manual / NC group / team) — categories
/// use `set_category_use_as_mailing_list` which writes to the
/// same table under the `cat:` id space.
#[tauri::command]
fn set_mailing_list_hidden(
    id: String,
    hidden: bool,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    cache
        .set_mailing_list_suppressed(&id, hidden)
        .map_err(NimbusError::from)
}

/// Set (or clear) the per-list emoji avatar override.  An empty
/// string clears the override so the row falls back to its
/// source icon.  Works for category / manual / team rows alike,
/// keyed by the unified id.
#[tauri::command]
fn set_mailing_list_emoji(
    id: String,
    emoji: Option<String>,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    cache
        .set_mailing_list_emoji(&id, emoji.as_deref().filter(|s| !s.is_empty()))
        .map_err(NimbusError::from)
}

/// Rename a mailing list, dispatched on the unified id prefix.
/// `cat:<name>` rewrites the CATEGORIES tag on every member
/// contact; `list:<uid>` updates the KIND:group vCard's
/// `display_name`.  Teams are read-only and rejected.
#[tauri::command]
async fn rename_mailing_list(
    id: String,
    new_name: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() {
        return Err(NimbusError::Other("new name is empty".into()));
    }
    if let Some(old) = id.strip_prefix("cat:") {
        rename_contact_category(old.to_string(), new_name, cache).await
    } else if let Some(group_id) = id.strip_prefix("list:") {
        // Reuse update_contact_group with the existing member
        // list — passing None for member_uids keeps them intact.
        update_contact_group(group_id.to_string(), Some(new_name), None, cache)
            .await
            .map(|_| ())
    } else {
        Err(NimbusError::Other("teams cannot be renamed".into()))
    }
}

// ── Contact groups / mailing lists (#133, #113) ───────────────
//
// Groups are stored on the server as plain `KIND:group` vCards.
// The CardDAV layer doesn't care — they sync just like
// individuals — so the IPCs here are thin wrappers that build the
// right vCard shape, route writes through the same
// create/update/delete CardDAV path the contacts use, and surface
// the local-only `group_emoji` / `group_hidden` overlay from the
// cache.

/// Snapshot of a group, hydrated for the UI.  `members` is the
/// expanded list of contact rows so the picker / chip strip can
/// render names + first emails without a second round-trip.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContactGroupView {
    id: String,
    nextcloud_account_id: String,
    display_name: String,
    member_uids: Vec<String>,
    members: Vec<GroupMemberView>,
    emoji: Option<String>,
    hidden: bool,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupMemberView {
    /// Composite contact id (`{nc}::{uid}`) — matches what
    /// `get_contacts` / `search_contacts` already expose.
    id: String,
    display_name: String,
    /// First email address, or empty when the underlying vCard
    /// has none — the UI shows "no email" in that case rather
    /// than failing the expand.
    email: String,
}

/// List every contact group across every connected NC account,
/// each with its members already resolved to (id, name, email)
/// triples so the UI doesn't have to chase referenced UIDs.
#[tauri::command]
fn list_contact_groups(cache: State<'_, Cache>) -> Result<Vec<ContactGroupView>, NimbusError> {
    let groups = cache.list_contact_groups().map_err(NimbusError::from)?;
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let resolved = cache
            .resolve_group_members(&g.nextcloud_account_id, &g.member_uids)
            .map_err(NimbusError::from)?;
        let members = resolved
            .into_iter()
            .map(|(id, display_name, email)| GroupMemberView {
                id,
                display_name,
                email,
            })
            .collect();
        out.push(ContactGroupView {
            id: g.id,
            nextcloud_account_id: g.nextcloud_account_id,
            display_name: g.display_name,
            member_uids: g.member_uids,
            members,
            emoji: g.emoji,
            hidden: g.hidden,
        });
    }
    Ok(out)
}

/// Create a new `KIND:group` vCard on the server and cache it.
/// `member_uids` is the bare-UID list (no `urn:uuid:` prefix);
/// the writer wraps each in the canonical URI form.
#[tauri::command]
async fn create_contact_group(
    nc_id: String,
    addressbook_url: String,
    addressbook_name: String,
    display_name: String,
    member_uids: Vec<String>,
    cache: State<'_, Cache>,
) -> Result<ContactGroupView, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    let uid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let parsed = ParsedVcard {
        uid: uid.clone(),
        display_name: display_name.clone(),
        kind: "group".to_string(),
        members: member_uids
            .iter()
            .map(|u| {
                if u.starts_with("urn:uuid:") {
                    u.clone()
                } else {
                    format!("urn:uuid:{u}")
                }
            })
            .collect(),
        // Auto-tag manual mailing lists with the reserved
        // CATEGORY so iOS / NC Contacts surface them in a
        // dedicated Kontaktgruppe.  The list_mailing_lists IPC
        // filters this name out of the virtual-row derivation
        // so we don't end up with a circular "Mailing Lists"
        // mailing list of mailing lists.
        categories: vec![MAILING_LISTS_CATEGORY.to_string()],
        ..Default::default()
    };
    let vcard = build_vcard(&parsed);
    let outcome = carddav_create_contact(
        &account.server_url,
        &addressbook_url,
        &account.username,
        &app_password,
        &uid,
        &vcard,
    )
    .await?;
    let row = parsed_to_row(&outcome.href, &outcome.etag, &uid, &parsed, vcard);
    cache
        .upsert_single_contact(&nc_id, &addressbook_name, &row)
        .map_err(NimbusError::from)?;
    let id = format!("{nc_id}::{uid}");
    Ok(ContactGroupView {
        id,
        nextcloud_account_id: nc_id,
        display_name,
        member_uids,
        members: Vec::new(),
        emoji: None,
        hidden: false,
    })
}

/// Edit an existing group — rename, swap members, both, neither.
/// `display_name` and `member_uids` are optional to keep the IPC
/// usable for partial updates from drag-and-drop (members only)
/// versus the rename modal (name only).
#[tauri::command]
async fn update_contact_group(
    group_id: String,
    display_name: Option<String>,
    member_uids: Option<Vec<String>>,
    cache: State<'_, Cache>,
) -> Result<ContactGroupView, NimbusError> {
    let handle = load_contact_handle(&cache, &group_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;

    let mut parsed = match nimbus_carddav::parse_vcard(&handle.vcard_raw) {
        Ok(p) => p,
        Err(_) => ParsedVcard {
            uid: handle.vcard_uid.clone(),
            ..Default::default()
        },
    };
    parsed.uid = handle.vcard_uid.clone();
    parsed.kind = "group".to_string();
    if let Some(n) = display_name {
        parsed.display_name = n;
    }
    if let Some(uids) = member_uids {
        parsed.members = uids
            .iter()
            .map(|u| {
                if u.starts_with("urn:uuid:") {
                    u.clone()
                } else {
                    format!("urn:uuid:{u}")
                }
            })
            .collect();
    }
    let vcard = build_vcard(&parsed);
    let outcome = carddav_update_contact(
        &handle.href,
        &account.username,
        &app_password,
        &handle.etag,
        &vcard,
    )
    .await?;
    let row = parsed_to_row(
        &outcome.href,
        &outcome.etag,
        &handle.vcard_uid,
        &parsed,
        vcard,
    );
    cache
        .upsert_single_contact(&handle.nextcloud_account_id, &handle.addressbook, &row)
        .map_err(NimbusError::from)?;
    // Re-pull the group with members hydrated so callers can
    // refresh their UI from a single response.
    let groups = cache.list_contact_groups().map_err(NimbusError::from)?;
    let g = groups
        .into_iter()
        .find(|g| g.id == group_id)
        .ok_or_else(|| NimbusError::Other(format!("group '{group_id}' missing after update")))?;
    let resolved = cache
        .resolve_group_members(&g.nextcloud_account_id, &g.member_uids)
        .map_err(NimbusError::from)?;
    Ok(ContactGroupView {
        id: g.id,
        nextcloud_account_id: g.nextcloud_account_id,
        display_name: g.display_name,
        member_uids: g.member_uids,
        members: resolved
            .into_iter()
            .map(|(id, display_name, email)| GroupMemberView {
                id,
                display_name,
                email,
            })
            .collect(),
        emoji: g.emoji,
        hidden: g.hidden,
    })
}

/// Delete a contact group from the server + local cache.
#[tauri::command]
async fn delete_contact_group(
    group_id: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let handle = load_contact_handle(&cache, &group_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;
    carddav_delete_contact(&handle.href, &account.username, &app_password, &handle.etag).await?;
    cache
        .delete_contact_by_id(&group_id)
        .map_err(NimbusError::from)?;
    Ok(())
}

/// Local-only "hide this group" toggle — drives the contacts
/// sidebar's hidden state and excludes the group from the
/// AddressAutocomplete search.
#[tauri::command]
fn set_contact_group_hidden(
    group_id: String,
    hidden: bool,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    cache
        .set_contact_group_hidden(&group_id, hidden)
        .map_err(NimbusError::from)
}

/// Local-only emoji avatar overlay for a group.  `None` clears
/// it back to the initials fallback.
#[tauri::command]
fn set_contact_group_emoji(
    group_id: String,
    emoji: Option<String>,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let val = emoji.as_deref().filter(|s| !s.is_empty());
    cache
        .set_contact_group_emoji(&group_id, val)
        .map_err(NimbusError::from)
}

// ── Nextcloud user groups + Teams (#133 follow-up) ────────────
//
// These are *identity / access* groups, separate from the vCard
// `KIND:group` records above.  Members are NC user IDs
// (provisioning-API speak), not vCard UIDs, so the contacts UI
// renders them in their own read-only sections — Nimbus can't
// add or remove members (admin task) but it can surface the
// groups the user already belongs to and resolve their members
// to email addresses for the Compose autocomplete.

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NextcloudGroupView {
    /// Nextcloud account this group lives on.
    nextcloud_account_id: String,
    /// Group / circle identifier — used as the picker id; UNIQUE
    /// per (`nextcloud_account_id`, `source`).
    id: String,
    /// `"group"` for OCS user groups, `"team"` for Circles /
    /// Teams.  Rendered as a colored pill in the sidebar.
    source: String,
    display_name: String,
    members: Vec<NextcloudGroupMemberView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NextcloudGroupMemberView {
    user_id: String,
    display_name: String,
    /// Empty when the NC user has no email set in Personal info.
    email: String,
}

/// Strip the SAML / LDAP prefixes some NC instances bake into
/// group ids when they sync from an upstream IdP — the user
/// sees a clean display name instead of `SAML_Engineering`.
/// Idempotent and case-insensitive on the prefix; everything
/// else passes through untouched.
fn humanize_nc_group_name(raw: &str) -> String {
    const PREFIXES: &[&str] = &[
        "SAML_", "saml_", "saml-", "SAML-", "LDAP_", "ldap_", "ldap-", "LDAP-", "OIDC_", "oidc_",
        "oidc-", "OIDC-",
    ];
    for p in PREFIXES {
        if let Some(rest) = raw.strip_prefix(p) {
            return rest.to_string();
        }
    }
    raw.to_string()
}

/// Pull every NC user group and Circle / Team the user belongs
/// to across every connected NC account, hydrating each with
/// (display_name, email) per member.  Soft-fails per group so
/// one restricted group doesn't block the rest.
#[tauri::command]
async fn list_nextcloud_groups(
    cache: State<'_, Cache>,
) -> Result<Vec<NextcloudGroupView>, NimbusError> {
    let accounts = nextcloud_store::load_accounts().unwrap_or_default();
    let mut out: Vec<NextcloudGroupView> = Vec::new();
    // Build a uid → email fallback map from the local CardDAV
    // cache.  Most NC instances sync the system addressbook into
    // CardDAV with each user's vCard UID == their NC user_id, so
    // this lets us recover emails even when the OCS user-profile
    // endpoint hides them (regular users querying other users
    // only get a display name, not the email field).
    let cache_uid_email: std::collections::HashMap<String, (String, String)> = cache
        .list_contacts(None)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| {
            let email = c
                .email
                .into_iter()
                .next()
                .map(|e| e.value)
                .unwrap_or_default();
            if email.is_empty() {
                return None;
            }
            // Composite id is `nc::uid` — split off the bare UID.
            let uid = c.id.split("::").nth(1).unwrap_or(&c.id).to_string();
            Some((uid, (c.display_name, email)))
        })
        .collect();
    for acc in &accounts {
        let app_password = match credentials::get_nextcloud_password(&acc.id) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("nc-groups: skipping {} ({e})", acc.id);
                continue;
            }
        };
        // OCS user groups -------------------------------------------------
        let group_ids =
            match nimbus_nextcloud::fetch_my_groups(&acc.server_url, &acc.username, &app_password)
                .await
            {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!("fetch_my_groups failed for {}: {e}", acc.id);
                    Vec::new()
                }
            };
        for gid in group_ids {
            let members = collect_group_members(acc, &app_password, &gid, &cache_uid_email).await;
            // OCS groups + Circles both surface as "team" so
            // the UI presents a single Teams section.  We keep
            // the raw `gid` in the unified id (`team:<gid>`) so
            // the per-row hide swatch can still target this
            // exact NC group across reloads.
            out.push(NextcloudGroupView {
                nextcloud_account_id: acc.id.clone(),
                id: format!("team:{gid}"),
                source: "team".to_string(),
                display_name: humanize_nc_group_name(&gid),
                members,
            });
        }
        // Circles / Teams ------------------------------------------------
        let circles =
            match nimbus_nextcloud::fetch_my_circles(&acc.server_url, &acc.username, &app_password)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("fetch_my_circles failed for {}: {e}", acc.id);
                    Vec::new()
                }
            };
        for c in circles {
            let mids = match nimbus_nextcloud::fetch_circle_member_ids(
                &acc.server_url,
                &acc.username,
                &app_password,
                &c.id,
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("circle-members failed: {e}");
                    Vec::new()
                }
            };
            let members = resolve_member_profiles(acc, &app_password, mids, &cache_uid_email).await;
            out.push(NextcloudGroupView {
                nextcloud_account_id: acc.id.clone(),
                id: format!("team:{}", c.id),
                source: "team".to_string(),
                display_name: humanize_nc_group_name(&c.display_name),
                members,
            });
        }
    }
    Ok(out)
}

/// Resolve every NC user id in a group to a (display_name,
/// email) tuple via the OCS user-profile endpoint.  Soft-fails
/// individual lookups (a deleted user surfaces with their bare
/// id and an empty email rather than failing the whole call).
async fn collect_group_members(
    acc: &NextcloudAccount,
    app_password: &str,
    group_id: &str,
    cache_uid_email: &std::collections::HashMap<String, (String, String)>,
) -> Vec<NextcloudGroupMemberView> {
    let ids = match nimbus_nextcloud::fetch_group_member_ids(
        &acc.server_url,
        &acc.username,
        app_password,
        group_id,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("fetch_group_member_ids({group_id}) failed: {e}");
            return Vec::new();
        }
    };
    resolve_member_profiles(acc, app_password, ids, cache_uid_email).await
}

/// Resolve a list of NC user-ids to (display_name, email) tuples
/// in parallel.  Falls back to the local CardDAV cache (system
/// addressbook) when OCS hides the email field — that's the
/// default for non-admin users querying other accounts.
async fn resolve_member_profiles(
    acc: &NextcloudAccount,
    app_password: &str,
    ids: Vec<String>,
    cache_uid_email: &std::collections::HashMap<String, (String, String)>,
) -> Vec<NextcloudGroupMemberView> {
    let futs = ids.into_iter().map(|uid| async move {
        let prof = nimbus_nextcloud::fetch_user_profile(
            &acc.server_url,
            &acc.username,
            app_password,
            &uid,
        )
        .await;
        (uid, prof)
    });
    let results = futures::future::join_all(futs).await;
    results
        .into_iter()
        .map(|(uid, prof)| {
            let (display_name, email_from_ocs) = match prof {
                Ok(p) => (p.display_name, p.email.unwrap_or_default()),
                Err(_) => (uid.clone(), String::new()),
            };
            // Fall back to the local CardDAV cache when OCS didn't
            // return an email (regular-user privacy default) — the
            // system addressbook entry usually has it.
            let (display_name, email) = if email_from_ocs.is_empty() {
                match cache_uid_email.get(&uid) {
                    Some((cached_name, cached_email)) => {
                        let dn = if display_name == uid && !cached_name.is_empty() {
                            cached_name.clone()
                        } else {
                            display_name
                        };
                        (dn, cached_email.clone())
                    }
                    None => (display_name, String::new()),
                }
            } else {
                (display_name, email_from_ocs)
            };
            NextcloudGroupMemberView {
                user_id: uid,
                display_name,
                email,
            }
        })
        .collect()
}

/// A trimmed-down addressbook record for the UI's "save new contact
/// to…" dropdown. We don't ship ctags or sync tokens — those are
/// sync-layer bookkeeping the frontend has no business touching.
#[derive(Debug, Clone, Serialize)]
struct AddressbookSummary {
    path: String,
    name: String,
    display_name: Option<String>,
}

/// List the user's addressbooks on a Nextcloud account. Used by
/// the Contacts view to populate a target-addressbook dropdown
/// when creating a new contact. Hits the server (PROPFIND) because
/// the list can change between logins and we want a fresh view.
#[tauri::command]
async fn list_nextcloud_addressbooks(
    nc_id: String,
) -> Result<Vec<AddressbookSummary>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let books: Vec<Addressbook> =
        list_addressbooks(&account.server_url, &account.username, &app_password).await?;
    Ok(books
        .into_iter()
        .map(|b| AddressbookSummary {
            path: b.path,
            name: b.name,
            display_name: b.display_name,
        })
        .collect())
}

// ── CalDAV calendars ────────────────────────────────────────────
//
// Calendar sync mirrors the CardDAV flow: one user-facing entry
// point (`sync_nextcloud_calendars`) walks the user's calendars and
// runs an incremental sync-collection REPORT per calendar, persisting
// each delta transactionally via the store. The UI reads cached data
// via `get_cached_calendars` (list for settings / sidebar header) and
// `get_cached_events` (events in a date window — the sidebar body).
//
// What the UI never sees: hrefs, etags, sync tokens, raw ICS blobs.
// Those all stay behind the store boundary.

/// Thin summary of a calendar — what the Svelte side needs to render
/// a row or colour-chip. Sourced from `CachedCalendar` but omits the
/// sync bookkeeping (tokens, ctag) the UI shouldn't care about.
#[derive(Debug, Clone, Serialize)]
struct CalendarSummary {
    id: String,
    nextcloud_account_id: String,
    display_name: String,
    color: Option<String>,
    last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Layer 1 (Settings). `true` removes the calendar from the sidebar
    /// entirely. Toggled from NextcloudSettings' per-calendar checkboxes.
    #[serde(default)]
    hidden: bool,
    /// Layer 2 (sidebar swatch). `true` keeps the calendar in the sidebar
    /// but stops its events from painting on the agenda grid. Toggled via
    /// the coloured swatch in the CalendarView sidebar.
    #[serde(default)]
    muted: bool,
}

/// Summary returned to the UI after a calendar sync run.
///
/// Per-calendar counts let the UI say "Personal: 4 new, 0 removed"
/// instead of a generic "done". `errors` accumulates per-calendar
/// failures so one broken calendar (commonly a subscribed read-only
/// feed that doesn't support sync-collection) doesn't paint the
/// whole run red.
#[derive(Debug, Clone, Serialize)]
struct SyncCalendarsReport {
    nc_account_id: String,
    calendars_synced: u32,
    upserted: u32,
    deleted: u32,
    errors: Vec<String>,
}

/// Fresh PROPFIND list of the user's calendars on the server.
///
/// Lighter than `sync_nextcloud_calendars` — no per-calendar sync,
/// no cache write. Used in settings UIs where the user just wants
/// to see what calendars exist server-side before toggling sync on.
#[tauri::command]
async fn list_nextcloud_calendars(nc_id: String) -> Result<Vec<CalendarSummary>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let calendars: Vec<CaldavCalendar> =
        caldav_list_calendars(&account.server_url, &account.username, &app_password).await?;
    Ok(calendars
        .into_iter()
        .map(|c| CalendarSummary {
            // Matches the id scheme used by the cache — stable across
            // syncs so the UI can key rows by it whether it's looking
            // at a fresh discovery list or the cached list.
            id: format!("{nc_id}::{}", c.path),
            nextcloud_account_id: nc_id.clone(),
            display_name: c.display_name.unwrap_or(c.name),
            color: c.color,
            // Discovery alone doesn't produce a sync timestamp.
            last_synced_at: None,
            // Raw discovery can't know about local toggles — the
            // cache-backed `get_cached_calendars` path does. This
            // command is only used by the setup probe, so defaulting
            // to fully visible is fine.
            hidden: false,
            muted: false,
        })
        .collect())
}

/// Pull the latest calendars and events from a Nextcloud account.
///
/// Two phases:
///   1. Discovery (PROPFIND) → `upsert_calendars`. This also prunes
///      any calendar that vanished server-side, cascading its events.
///   2. Per-calendar incremental sync. We pass the previous
///      `sync_token` (from the cache) so the server returns only
///      what changed. A failure on calendar N is logged and added
///      to the report; calendar N+1 still runs.
///
/// Each calendar's delta is committed in its own transaction, so
/// a partial run leaves earlier calendars fully up-to-date.
#[tauri::command]
async fn sync_nextcloud_calendars(
    nc_id: String,
    cache: State<'_, Cache>,
) -> Result<SyncCalendarsReport, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    // ── Phase 1: discovery + reconcile the calendar list ────────
    let server_calendars =
        caldav_list_calendars(&account.server_url, &account.username, &app_password).await?;
    tracing::info!(
        "CalDAV: {} calendar(s) discovered for {}",
        server_calendars.len(),
        nc_id
    );

    let rows: Vec<CalendarRow> = server_calendars
        .iter()
        .map(|c| CalendarRow {
            path: c.path.clone(),
            display_name: c.display_name.clone().unwrap_or_else(|| c.name.clone()),
            color: c.color.clone(),
            ctag: c.ctag.clone(),
            // Fresh inserts default to fully visible; the `upsert_calendars`
            // ON CONFLICT clause leaves `hidden` and `muted` untouched on
            // updates so existing local toggles survive re-sync.
            hidden: false,
            muted: false,
        })
        .collect();
    cache.upsert_calendars(&nc_id, &rows)?;

    // ── Phase 2: sync each calendar individually ────────────────
    let mut report = SyncCalendarsReport {
        nc_account_id: nc_id.clone(),
        calendars_synced: 0,
        upserted: 0,
        deleted: 0,
        errors: Vec::new(),
    };

    for cal in server_calendars {
        // id matches the (nc_id, path) scheme `upsert_calendars`
        // just committed, so `get_calendar_sync_state` and
        // `apply_event_delta` will find/target the right row.
        let cal_id = format!("{nc_id}::{}", cal.path);

        let prev_token = cache
            .get_calendar_sync_state(&cal_id)
            .ok()
            .flatten()
            .and_then(|s| s.sync_token);

        let delta = match caldav_sync_calendar(
            &account.server_url,
            &cal.path,
            &account.username,
            &app_password,
            prev_token.as_deref(),
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("CalDAV sync failed for calendar '{}': {e}", cal.path);
                report.errors.push(format!("{}: {e}", cal.path));
                continue;
            }
        };

        // One `RawEvent` can carry several VEVENTs (master + overrides
        // at the same href). Flatten into one store row per VEVENT so
        // the range query sees them individually. `ics_raw` is cloned
        // onto every row from the same href — the raw blob stays
        // identical, and the store is optimised for per-row reads,
        // not per-href grouping.
        let upserts: Vec<CalendarEventRow> =
            delta.upserts.iter().flat_map(raw_event_to_rows).collect();

        if let Err(e) = cache.apply_event_delta(
            &cal_id,
            &upserts,
            &delta.deleted_hrefs,
            delta.new_sync_token.as_deref(),
            cal.ctag.as_deref(),
        ) {
            tracing::warn!("apply_event_delta failed for '{}': {e}", cal.path);
            report.errors.push(format!("{}: {e}", cal.path));
            continue;
        }

        report.calendars_synced += 1;
        report.upserted += upserts.len() as u32;
        report.deleted += delta.deleted_hrefs.len() as u32;
    }

    Ok(report)
}

/// Cache-only list of calendars for a Nextcloud account. Used by the
/// sidebar widget on startup so it can paint before the first sync
/// finishes (or if the user is offline).
#[tauri::command]
fn get_cached_calendars(
    nc_id: String,
    cache: State<'_, Cache>,
) -> Result<Vec<CalendarSummary>, NimbusError> {
    let cached = cache.list_calendars(&nc_id)?;
    Ok(cached
        .into_iter()
        .map(|c| CalendarSummary {
            id: c.id,
            nextcloud_account_id: c.nextcloud_account_id,
            display_name: c.display_name,
            color: c.color,
            last_synced_at: c.last_synced_at,
            hidden: c.hidden,
            muted: c.muted,
        })
        .collect())
}

// ── Calendar management commands (Issue #82) ─────────────────
//
// CalDAV wrappers that add / rename / recolor / delete a calendar
// collection on the server and keep the local cache in step. Each
// mutates exactly one calendar row; the next `sync_nextcloud_
// calendars` run reconciles etag / sync-token / event deltas.
// `set_nextcloud_calendar_hidden` is the only one that doesn't
// talk to the server — hidden is a local-only flag.

/// Create a new calendar on the server and seed a cache row.
///
/// The path segment is a fresh UUID so two concurrent creates can't
/// collide on the wire and so a later rename never rewrites URLs
/// downstream (the slug stays stable regardless of display name).
/// Returns the newly-inserted summary so the UI can add it to the
/// sidebar without a follow-up fetch.
#[tauri::command]
async fn create_nextcloud_calendar(
    nc_id: String,
    display_name: String,
    color: Option<String>,
    cache: State<'_, Cache>,
) -> Result<CalendarSummary, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    let server = account.server_url.trim_end_matches('/');
    let home = format!("{server}/remote.php/dav/calendars/{}/", account.username);
    let slug = uuid::Uuid::new_v4().to_string();

    let url = caldav_create_calendar(
        &home,
        &account.username,
        &app_password,
        &slug,
        &display_name,
        color.as_deref(),
    )
    .await?;

    // Seed the cache so the sidebar paints the new calendar
    // instantly. `ctag` / `sync_token` land on the next full sync —
    // no event rows yet anyway, so the bookkeeping gap is cosmetic.
    let row = CalendarRow {
        path: url.clone(),
        display_name: display_name.clone(),
        color: color.clone(),
        ctag: None,
        hidden: false,
        muted: false,
    };
    let id = cache.insert_calendar(&nc_id, &row)?;

    Ok(CalendarSummary {
        id,
        nextcloud_account_id: nc_id,
        display_name,
        color,
        last_synced_at: None,
        hidden: false,
        muted: false,
    })
}

/// Rename and/or recolor an existing calendar via a single CalDAV
/// `PROPPATCH`. Either argument may be `None` — passing both `None`
/// is a no-op server-side and cache-side.
#[tauri::command]
async fn update_nextcloud_calendar(
    calendar_id: String,
    display_name: Option<String>,
    color: Option<String>,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let (nc_id, path) = cache
        .get_calendar_server_path(&calendar_id)?
        .ok_or_else(|| NimbusError::Other(format!("no cached calendar with id '{calendar_id}'")))?;
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    caldav_update_calendar(
        &path,
        &account.username,
        &app_password,
        display_name.as_deref(),
        color.as_deref(),
    )
    .await?;

    cache.update_calendar_metadata(&calendar_id, display_name.as_deref(), color.as_deref())?;
    Ok(())
}

/// Delete a calendar on the server + drop the cached row (events
/// cascade). The server's DELETE is destructive and irreversible on
/// most Nextcloud setups — callers (i.e. the UI) are expected to
/// confirm with the user before invoking this.
#[tauri::command]
async fn delete_nextcloud_calendar(
    calendar_id: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let (nc_id, path) = cache
        .get_calendar_server_path(&calendar_id)?
        .ok_or_else(|| NimbusError::Other(format!("no cached calendar with id '{calendar_id}'")))?;
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    caldav_delete_calendar(&path, &account.username, &app_password).await?;
    cache.remove_calendar(&calendar_id)?;
    Ok(())
}

/// Layer 1: flip a calendar's sidebar visibility. Purely client-side —
/// no CalDAV traffic. `hidden = true` removes the calendar from the
/// sidebar entirely (controlled from NextcloudSettings).
#[tauri::command]
fn set_nextcloud_calendar_hidden(
    calendar_id: String,
    hidden: bool,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    cache.set_calendar_hidden(&calendar_id, hidden)?;
    Ok(())
}

/// Layer 2: flip a calendar's event-grid visibility. Purely client-side.
/// `muted = true` keeps the calendar in the sidebar but stops its events
/// from painting on the agenda grid (controlled via the sidebar swatch).
#[tauri::command]
fn set_nextcloud_calendar_muted(
    calendar_id: String,
    muted: bool,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    cache.set_calendar_muted(&calendar_id, muted)?;
    Ok(())
}

/// Events in `[range_start, range_end)` across the given calendars,
/// with recurring series already expanded into concrete occurrences.
///
/// `calendar_ids` is the full set the UI wants to display at once —
/// typically every calendar belonging to a Nextcloud account, so one
/// round-trip paints the whole sidebar.
///
/// The expansion pipeline:
/// 1. `cache.list_events_for_expansion` returns three buckets of rows
///    — in-window singletons, all recurring masters, all overrides.
///    Masters and overrides are fetched un-windowed because a series'
///    master may predate the window but still have instances inside
///    it, and an override may have been moved from outside the window
///    to inside it (or vice versa).
/// 2. Overrides are indexed by the `{calendar_id}::{uid}` prefix of
///    their composite id — the very same prefix that a master's id
///    has — so matching an override to its series is O(1).
/// 3. `nimbus_caldav::expand_event` does the RFC 5545 work: RRULE
///    enumeration, EXDATE removal, RDATE insertion, override swap-in.
#[tauri::command]
fn get_cached_events(
    calendar_ids: Vec<String>,
    range_start: chrono::DateTime<chrono::Utc>,
    range_end: chrono::DateTime<chrono::Utc>,
    cache: State<'_, Cache>,
) -> Result<Vec<CalendarEvent>, NimbusError> {
    let input = cache
        .list_events_for_expansion(&calendar_ids, range_start, range_end)
        .map_err(NimbusError::from)?;

    // Index overrides by the master prefix that's baked into their id
    // (`{cal}::{uid}::{epoch}` → `{cal}::{uid}`). Rare uid collisions
    // across different calendars are already ruled out by the
    // `{cal}::` segment.
    let mut overrides_by_master: std::collections::HashMap<&str, Vec<&CalendarEvent>> =
        std::collections::HashMap::new();
    for ov in &input.overrides {
        if let Some(master_id) = ov.id.rsplit_once("::").map(|(prefix, _)| prefix) {
            overrides_by_master.entry(master_id).or_default().push(ov);
        }
    }

    let mut out: Vec<CalendarEvent> = input.singletons;
    for master in &input.masters {
        let ovs = overrides_by_master
            .get(master.id.as_str())
            .cloned()
            .unwrap_or_default();
        out.extend(nimbus_caldav::expand_event(
            master,
            &ovs,
            range_start,
            range_end,
        ));
    }
    // Expansion doesn't guarantee chronological order across the whole
    // set (singletons come first, then per-master occurrences). Sort
    // once at the end so the UI's day-bucket grouping stays coherent.
    out.sort_by_key(|e| e.start);
    Ok(out)
}

/// What the Svelte editor sends for a create or update. Matches the
/// `CalendarEvent` shape the UI already knows but flattens to plain
/// strings / booleans the Tauri IPC layer can serialise without
/// extra adapters. Optional fields stay optional so the form can
/// submit a partial event without leaving phantom values behind.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarEventInput {
    summary: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    location: Option<String>,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    /// True for events the user picked "All day" on. The server stores
    /// these as `VALUE=DATE` ranges; we re-derive that from the start /
    /// end times being a midnight…23:59:59 window.
    #[serde(default)]
    all_day: bool,
    #[serde(default)]
    url: Option<String>,
    /// `OPAQUE` (busy) or `TRANSPARENT` (free). Matches the editor's
    /// "show as" picker. `None` means "leave whatever the server had".
    #[serde(default)]
    transparency: Option<String>,
    #[serde(default)]
    attendees: Vec<EventAttendee>,
    #[serde(default)]
    reminders: Vec<EventReminder>,
}

/// Build a `CalendarEvent` skeleton from form input. Caller fills in
/// `id` (a fresh UID for create, the cached UID for update). Recurrence
/// fields stay empty here — the editor doesn't expose them yet, and
/// any existing recurrence is preserved from the cached event by the
/// update command before this struct is rebuilt.
fn input_to_calendar_event(uid: &str, input: &CalendarEventInput) -> CalendarEvent {
    // For all-day events the editor sends midnight UTC starts; snap
    // the end to 23:59:59 of the last covered day so `build_ics`
    // recognises the all-day shape. For timed events we trust the
    // editor's exact instants.
    let (start, end) = if input.all_day {
        use chrono::TimeZone;
        let start_day = input.start.date_naive();
        let end_day = input.end.date_naive();
        let s = chrono::Utc.from_utc_datetime(&start_day.and_hms_opt(0, 0, 0).unwrap());
        let e = chrono::Utc.from_utc_datetime(&end_day.and_hms_opt(23, 59, 59).unwrap());
        (s, e)
    } else {
        (input.start, input.end)
    };
    CalendarEvent {
        id: uid.to_string(),
        summary: input.summary.clone(),
        description: input.description.clone(),
        start,
        end,
        location: input.location.clone(),
        rrule: None,
        rdate: vec![],
        exdate: vec![],
        recurrence_id: None,
        url: input.url.clone(),
        transparency: input.transparency.clone(),
        attendees: input.attendees.clone(),
        reminders: input.reminders.clone(),
    }
}

/// Convert a `CalendarEvent` (post-write) into the row shape the cache
/// expects. Used by both `create_calendar_event` and
/// `update_calendar_event` so the local cache reflects the new state
/// without waiting for the next sync round.
fn calendar_event_to_row(
    event: &CalendarEvent,
    href: &str,
    etag: &str,
    ics_raw: &str,
) -> CalendarEventRow {
    CalendarEventRow {
        uid: event.id.clone(),
        recurrence_id: event.recurrence_id,
        href: href.to_string(),
        etag: etag.to_string(),
        summary: event.summary.clone(),
        description: event.description.clone(),
        start: event.start,
        end: event.end,
        location: event.location.clone(),
        rrule: event.rrule.clone(),
        rdate: event.rdate.clone(),
        exdate: event.exdate.clone(),
        url: event.url.clone(),
        transparency: event.transparency.clone(),
        attendees: event.attendees.clone(),
        reminders: event.reminders.clone(),
        ics_raw: ics_raw.to_string(),
    }
}

/// Resolve the `(email, display_name)` to write into `ORGANIZER`
/// for an outbound VEVENT.  This drives whether NC's iMIP plugin
/// can route the invite via the user's real Mail-app SMTP (NC 30+
/// Mail Provider): the address must match the user's primary
/// email exactly, otherwise NC falls back to the system mailer
/// with `From: invitations-noreply@…`.
///
/// Strategy:
/// 1. **When attendees are present**, fetch the user's profile
///    from `/ocs/v2.php/cloud/user`.  Its `email` field is what
///    NC's Mail Provider keys against — same source of truth NC
///    uses internally, so we can't get it wrong.
/// 2. **When the OCS lookup fails or returns no email**, fall
///    back to `organizer_local` (username if it parses as an
///    email, else `username@server-host`) so the PUT still
///    succeeds.  The fallback may not match a Mail-app account,
///    in which case NC's system mailer takes over — better than
///    failing the save.
/// 3. **When there are no attendees**, skip the network call
///    entirely and use the local fallback.  NC's scheduling plugin
///    won't fire without attendees, so `ORGANIZER` here is just
///    metadata for the calendar copy.
async fn resolve_organizer(
    account: &NextcloudAccount,
    app_password: &str,
    has_attendees: bool,
) -> (String, Option<String>) {
    if !has_attendees {
        return organizer_local(account);
    }
    match nimbus_nextcloud::user::fetch_current_user(
        &account.server_url,
        &account.username,
        app_password,
    )
    .await
    {
        Ok(profile) => {
            if let Some(email) = profile.email {
                let name = profile
                    .display_name
                    .or_else(|| account.display_name.clone());
                return (email, name);
            }
            tracing::warn!(
                "Nextcloud user has no email set in Personal info — \
                 iMIP will fall back to system mailer"
            );
        }
        Err(e) => tracing::warn!("OCS user lookup failed, using fallback ORGANIZER: {e}"),
    }
    organizer_local(account)
}

/// Local-only fallback when we can't reach OCS.  Same shape we used
/// before: prefer `username` when it's already an email, else
/// synthesise `username@host`.  This is unrouteable on the public
/// internet but satisfies Sabre's "ATTENDEE without ORGANIZER is
/// 403" check so the PUT itself succeeds.
fn organizer_local(account: &NextcloudAccount) -> (String, Option<String>) {
    let email = if account.username.contains('@') {
        account.username.clone()
    } else {
        let host = account
            .server_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .split('/')
            .next()
            .unwrap_or("nextcloud.local");
        format!("{}@{}", account.username, host)
    };
    (email, account.display_name.clone())
}

/// Create a new VEVENT in the given calendar.
///
/// Generates a fresh UUID for the UID so callers don't have to.
/// The PUT uses `If-None-Match: *`, so a UID collision surfaces as
/// a structured error instead of a silent overwrite. On success, the
/// new event is upserted into the local cache so the UI can render it
/// without waiting for the next sync.
#[tauri::command]
async fn create_calendar_event(
    calendar_id: String,
    input: CalendarEventInput,
    cache: State<'_, Cache>,
) -> Result<CalendarEvent, NimbusError> {
    let (nc_id, calendar_path) =
        cache
            .get_calendar_server_path(&calendar_id)?
            .ok_or_else(|| {
                NimbusError::Other(format!(
                    "calendar '{calendar_id}' is not in the local cache — refresh and try again"
                ))
            })?;
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    let uid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let event = input_to_calendar_event(&uid, &input);
    let (organizer_email, organizer_name) =
        resolve_organizer(&account, &app_password, !event.attendees.is_empty()).await;
    let ics = caldav_build_ics(&event, Some(&organizer_email), organizer_name.as_deref());

    // `calendar_path` from the cache is already an absolute URL —
    // `nimbus-caldav::discovery` resolves it via `absolute_url` before
    // storing. Don't re-prefix the server origin or the PUT goes to
    // `https://hosthttps://host/...`.
    let outcome = caldav_create_event(
        &account.server_url,
        &calendar_path,
        &account.username,
        &app_password,
        &uid,
        &ics,
    )
    .await?;

    let row = calendar_event_to_row(&event, &outcome.href, &outcome.etag, &ics);
    cache.upsert_single_event(&calendar_id, &row)?;

    // Re-derive the app-side id the same way `event_row_id` does so the
    // returned event matches what `get_cached_events` will surface.
    let mut out = event;
    out.id = format!("{calendar_id}::{uid}");
    Ok(out)
}

/// Update an existing VEVENT, keyed by its app-side id.
///
/// Preserves the cached UID and href; everything else comes from the
/// editor input. The PUT is gated on the cached etag so a concurrent
/// edit on another device surfaces as a structured error (412 → human-
/// readable string) instead of overwriting the other change silently.
#[tauri::command]
async fn update_calendar_event(
    event_id: String,
    input: CalendarEventInput,
    cache: State<'_, Cache>,
) -> Result<CalendarEvent, NimbusError> {
    let handle = load_event_handle(&cache, &event_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;

    let mut event = input_to_calendar_event(&handle.uid, &input);
    // Preserve recurrence info the editor doesn't surface — losing it
    // would silently demote a recurring series back to a singleton.
    event.recurrence_id = handle.recurrence_id;

    let (organizer_email, organizer_name) =
        resolve_organizer(&account, &app_password, !event.attendees.is_empty()).await;
    let ics = caldav_build_ics(&event, Some(&organizer_email), organizer_name.as_deref());
    // Use the etag-aware retry helper so a concurrent edit on
    // another device (NC web, phone) doesn't surface to the
    // user as "refresh and try again" — it transparently syncs
    // and re-PUTs once.
    let (outcome, handle) = update_event_with_etag_retry(&cache, &event_id, &ics).await?;

    let row = calendar_event_to_row(&event, &outcome.href, &outcome.etag, &ics);
    cache.upsert_single_event(&handle.calendar_id, &row)?;

    let mut out = event;
    out.id = event_id;
    Ok(out)
}

/// Delete an event from the server and the local cache.  Server-side
/// iTIP CANCEL notices to attendees are emitted by Nextcloud's
/// `OCA\DAV\CalDAV\Schedule\IMipPlugin` on the DELETE — no
/// client-side mail involved.
#[tauri::command]
async fn delete_calendar_event(
    event_id: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let handle = load_event_handle(&cache, &event_id)?;
    let nc_account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;

    caldav_delete_event(
        &handle.href,
        &nc_account.username,
        &app_password,
        &handle.etag,
    )
    .await?;
    cache.delete_event_by_id(&event_id)?;
    Ok(())
}

/// Remove a locally-cached event whose iCalendar `UID` matches
/// `uid`.  Surfaced from the inbound CANCEL card in MailView:
/// when an external organiser sends a `METHOD:CANCEL` mail, the
/// user clicks "Remove from my calendar" and we DELETE the
/// CalDAV resource so the cancelled meeting disappears from the
/// grid (and from any other CalDAV client, including their
/// phone).  Idempotent: returns `Ok(())` when no row matches —
/// the user may have already removed the event manually, or the
/// invite never made it into their calendar in the first place.
///
/// Note that we don't fight Sabre's iTIP machinery here.  An
/// attendee-side DELETE of an event whose ORGANIZER is external
/// would normally generate a `METHOD:REPLY;PARTSTAT=DECLINED`
/// from NC's IMipPlugin; that's not what we want when responding
/// to a CANCEL (the organiser already cancelled — a "decline" is
/// noise).  In practice Sabre suppresses REPLY emission when the
/// stored event already carries `STATUS:CANCELLED` or the user's
/// PARTSTAT is unchanged from the previous version, which covers
/// the common case.  Worth flagging explicitly if it turns out
/// to send spurious mail in the wild.
/// True when an event with the given iCalendar UID exists in
/// any of the user's locally-cached calendars.  Used by the
/// CANCEL card to decide whether to expose "Remove from my
/// calendar" — only makes sense when there's actually a local
/// copy to remove.  A miss here is the common case for invites
/// the user never accepted (CANCEL arrives but the event was
/// never imported into a calendar): the card should fall back
/// to a passive "not in your calendar" line instead of the
/// remove button.
#[tauri::command]
fn is_event_in_calendar(uid: String, cache: State<'_, Cache>) -> Result<bool, NimbusError> {
    Ok(cache.find_event_id_by_uid(&uid)?.is_some())
}

/// Record that an iCalendar UID has been cancelled by its
/// organiser.  Called by MailView when it surfaces a
/// `METHOD:CANCEL` mail, so the original REQUEST mail's RSVP
/// card can flip to the cancelled flavour on its next open.
#[tauri::command]
fn record_cancelled_invite(uid: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    cache.mark_invite_cancelled(&uid).map_err(NimbusError::from)
}

/// True when MailView has previously observed a `METHOD:CANCEL`
/// mail for this iCalendar UID.  Used by the RSVP card to
/// flip the original REQUEST mail's flavour to the cancelled
/// banner so the user doesn't unwittingly answer a meeting
/// that's been cancelled.
#[tauri::command]
fn is_invite_cancelled(uid: String, cache: State<'_, Cache>) -> Result<bool, NimbusError> {
    cache.is_invite_cancelled(&uid).map_err(NimbusError::from)
}

#[tauri::command]
async fn dismiss_cancelled_event(uid: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    let Some(event_id) = cache.find_event_id_by_uid(&uid)? else {
        tracing::info!(
            "dismiss_cancelled_event: no cached event with UID {uid}, treating as no-op"
        );
        return Ok(());
    };
    let handle = load_event_handle(&cache, &event_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;
    // Use the silent variant — without `Schedule-Reply: F`,
    // Sabre's attendee-side DELETE handler emits a spurious
    // `METHOD:REPLY;PARTSTAT=DECLINED` to the organiser.  The
    // organiser already sent CANCEL; mailing them a decline
    // back is just noise (and confusing).
    nimbus_caldav::delete_event_silent(
        &handle.href,
        &account.username,
        &app_password,
        &handle.etag,
    )
    .await?;
    cache.delete_event_by_id(&event_id)?;
    Ok(())
}

// ── iTIP / iMIP (#58) ─────────────────────────────────────────────
//
// Outbound: when Compose's "Add Event" flow saves an event, we hand
// the recipient mail clients a `text/calendar; method=REQUEST`
// attachment so any RFC-compliant client can save the invite
// natively (Outlook, Apple Mail, Gmail, Thunderbird).
//
// Inbound: when a received message carries a `text/calendar` part,
// we parse the iCalendar and surface an "invite card" with
// Accept / Decline / Tentative buttons.  Each click silently
// emits a `text/calendar; method=REPLY` email back to the
// organiser — that's the iMIP RSVP loop (RFC 6047).

/// Lightweight iCalendar summary the JS layer renders for an
/// inbound invite (Accept / Decline / Tentative card).  Picks
/// the smallest set of fields the card needs; the full ICS bytes
/// stay on the Rust side and ride through `send_event_rsvp` so
/// the REPLY can carry the same UID and DTSTAMP without the
/// frontend having to round-trip the full event.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InviteSummary {
    /// Calendar-level `METHOD:` value, upper-cased.  iTIP defines
    /// REQUEST (organiser → attendee), REPLY (attendee →
    /// organiser), CANCEL, PUBLISH, REFRESH, COUNTER, DECLINECOUNTER.
    /// `MailView` only shows the RSVP card for REQUEST; the others
    /// (especially REPLY) are typically attendee responses to OUR
    /// invites and don't need a "you can RSVP" card on the
    /// organiser's side.  `None` means no METHOD line was present
    /// (treat as "not an iTIP message" and suppress the card).
    method: Option<String>,
    /// VEVENT UID — the join key between REQUEST + REPLY.
    uid: String,
    /// SUMMARY (title) of the event.
    summary: String,
    /// DTSTART, normalised to UTC (RFC 3339).
    start: chrono::DateTime<chrono::Utc>,
    /// DTEND, normalised to UTC.
    end: chrono::DateTime<chrono::Utc>,
    /// Optional venue / room.
    location: Option<String>,
    /// Optional URL — Talk join links, video conferencing, etc.
    url: Option<String>,
    /// ORGANIZER's email (mailto: URI stripped).  Required by RFC
    /// 5546 whenever any ATTENDEE is present, so we expect it on
    /// real-world invites — but a missing one isn't fatal, the
    /// RSVP just falls back to the message's From: address.
    organizer_email: Option<String>,
    organizer_name: Option<String>,
    /// All ATTENDEEs from the VEVENT.  The card highlights the
    /// row matching the current user's address so they can see
    /// their own NEEDS-ACTION status before clicking.
    attendees: Vec<nimbus_core::models::EventAttendee>,
    /// The full ICS body, used to preserve UID + DTSTAMP +
    /// SEQUENCE on the REPLY without re-fetching.
    raw_ics: String,
}

/// Parse a raw `text/calendar` byte slice into the slim
/// `InviteSummary` the inbound RSVP card consumes.  Looks at the
/// FIRST VEVENT in the file — recurring series and overrides are
/// out of scope for the invite card MVP (the user can still
/// manage them in the Calendar view after accepting).
///
/// `parse_ics` doesn't surface ORGANIZER as a typed field today,
/// so the JS caller is expected to fall back to the message's
/// `From:` header for the recipient of the RSVP REPLY — which is
/// what RFC 5546 says the organiser address tracks anyway.
#[tauri::command]
fn parse_event_invite(bytes: Vec<u8>) -> Result<InviteSummary, NimbusError> {
    let body = String::from_utf8(bytes)
        .map_err(|e| NimbusError::Protocol(format!("invite is not UTF-8: {e}")))?;
    let events = nimbus_caldav::ical::parse_ics(&body)
        .map_err(|e| NimbusError::Protocol(format!("could not parse calendar invite: {e}")))?;
    let event = events
        .into_iter()
        .next()
        .ok_or_else(|| NimbusError::Protocol("invite contains no VEVENT".into()))?;

    let method = extract_calendar_method(&body);

    Ok(InviteSummary {
        method,
        uid: event.id.clone(),
        summary: event.summary.clone(),
        start: event.start,
        end: event.end,
        location: event.location.clone(),
        url: event.url.clone(),
        organizer_email: None,
        organizer_name: None,
        attendees: event.attendees.clone(),
        raw_ics: body,
    })
}

/// Pull the calendar-level `METHOD:` value out of a raw ICS body
/// without round-tripping through a full parser.  iTIP defines
/// the line as a single token after the colon (REQUEST / REPLY /
/// CANCEL / etc.); we just normalise to upper case so JS-side
/// equality checks don't have to be case-insensitive.
fn extract_calendar_method(ics: &str) -> Option<String> {
    for line in ics.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("METHOD:") {
            let m = rest.trim();
            if !m.is_empty() {
                return Some(m.to_uppercase());
            }
        }
    }
    None
}

/// Generate a `METHOD:REPLY` iCalendar body for the user's RSVP
/// response.  Re-renders the original event with PARTSTAT updated
/// for the current user's ATTENDEE row only — every other
/// ATTENDEE keeps whatever the inbound message had so the
/// organiser's mail client doesn't see spurious "everyone
/// changed" diffs.
///
/// Respond to an inbound invite by writing the user's PARTSTAT to
/// CalDAV.  Nextcloud's CalDAV-Schedule plugin (with NC 30+ Mail
/// Provider) generates and SMTPs the iMIP REPLY automatically —
/// the client never touches SMTP for RSVPs.
///
/// Behaviour by partstat:
/// - **ACCEPTED**: PUT into `calendar_id` with PARTSTAT=ACCEPTED,
///   TRANSP=OPAQUE.  The event lands on the user's calendar (and
///   syncs to their phone), and NC mails the organiser.
/// - **TENTATIVE**: PUT with PARTSTAT=TENTATIVE, TRANSP=TRANSPARENT
///   so the calendar can render it visually distinct (striped
///   pattern in CalendarView).
/// - **DECLINED**: PUT with PARTSTAT=DECLINED, then DELETE the
///   resource.  The PUT triggers NC's REPLY (organiser notified);
///   the DELETE removes the entry from the user's calendar so
///   declined meetings don't clutter the grid.
///
/// Resolving the responding attendee's address goes through
/// **every identity Nimbus knows about**, not just one: the NC
/// user-profile email (Sabre's principal CUA), every configured
/// mail-account address, plus an optional `attendee_email`
/// hint from the card (the address the inbound mail was
/// actually sent to).  We intersect that combined set with the
/// inbound ATTENDEE list and use whichever address is *already
/// in the invite* — that's the row Sabre's iTIP broker will
/// match on the user's principal-CUA when generating the
/// REPLY iMIP.
///
/// Why so many sources?  The chain is fragile: NC profile
/// email → Sabre principal CUA → ATTENDEE-row match →
/// IMipPlugin Mail Provider lookup against Mail-app accounts.
/// All four addresses must equal each other for REPLY mail to
/// actually leave NC.  Pinning to a single source means a
/// single misconfiguration (empty NC profile email, mismatched
/// Mail-app primary, etc.) silently breaks REPLY delivery —
/// exactly what was happening before.
// `attendee_hint`: optional hint from the card — the address
// the inbound mail was actually sent to, resolved by the
// frontend from the invite's ATTENDEE list intersected with
// the user's configured mail-account addresses.  Used as the
// highest-priority candidate when picking the row to mutate +
// identify with on Sabre's principal CUA.  May be `None` if
// the card couldn't resolve one.
#[tauri::command]
async fn respond_to_invite(
    calendar_id: String,
    raw_ics: String,
    partstat: String,
    attendee_hint: Option<String>,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    // Resolve the chosen calendar's location on the server.
    let (nc_id, calendar_path) =
        cache
            .get_calendar_server_path(&calendar_id)?
            .ok_or_else(|| {
                NimbusError::Other(format!(
                    "calendar '{calendar_id}' is not in the local cache — refresh and try again"
                ))
            })?;
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;

    // Build the candidate-identity list, in priority order:
    //   1. The card's hint (transport-derived, most likely
    //      verbatim in the invite).
    //   2. NC profile email — Sabre's principal CUA, the
    //      authoritative identity for the iTIP broker.
    //   3. Every configured mail-account address (covers the
    //      "I added a Nimbus mail account whose email differs
    //      from my NC profile" case).
    //   4. The synth `username@server-host` as a last resort.
    // We then take the FIRST candidate that actually appears
    // in the inbound ATTENDEE list — Sabre will match the
    // same row when scanning the body for the principal's CUA.
    // If no candidate matches, we fall back to candidate #2
    // (NC profile email — the address Sabre's broker is most
    // likely to identify as ours) and add a fresh row, so the
    // server-side iTIP can still pair us against the principal.
    let mut candidates: Vec<String> = Vec::new();
    if let Some(hint) = attendee_hint.as_deref() {
        let h = hint.trim();
        if !h.is_empty() {
            candidates.push(h.to_string());
        }
    }
    let nc_profile_email = match nimbus_nextcloud::user::fetch_current_user(
        &account.server_url,
        &account.username,
        &app_password,
    )
    .await
    {
        Ok(p) => p.email,
        Err(e) => {
            tracing::warn!("RSVP: NC user-profile lookup failed ({e})");
            None
        }
    };
    if let Some(e) = nc_profile_email.as_deref() {
        candidates.push(e.to_string());
    }
    if let Ok(mail_accounts) = account_store::load_accounts(&cache) {
        for a in mail_accounts {
            candidates.push(a.email);
        }
    }
    candidates.push(organizer_local(&account).0);
    // Lower-cased, deduplicated, preserving priority order.
    let mut seen = std::collections::HashSet::new();
    let candidates: Vec<String> = candidates
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .filter(|s| seen.insert(s.to_ascii_lowercase()))
        .collect();
    tracing::debug!("RSVP candidate identities: {candidates:?}");

    // Pick the first candidate already present in the inbound
    // ATTENDEE list.  If none match, default to the NC profile
    // email (so Sabre's broker matches the new row we'll add
    // against its principal CUA) — and last-ditch the first
    // non-empty candidate so we always have something.
    let attendee_email = {
        let inbound_attendees: Vec<String> = nimbus_caldav::ical::parse_ics(&raw_ics)
            .ok()
            .and_then(|v| v.into_iter().next())
            .map(|e| e.attendees.into_iter().map(|a| a.email).collect())
            .unwrap_or_default();
        let inbound_set: std::collections::HashSet<String> = inbound_attendees
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        candidates
            .iter()
            .find(|c| inbound_set.contains(&c.to_ascii_lowercase()))
            .cloned()
            .or(nc_profile_email)
            .or_else(|| candidates.into_iter().next())
            .unwrap_or_else(|| organizer_local(&account).0)
    };
    tracing::info!("RSVP: using attendee identity {attendee_email}");

    // Parse the inbound ICS, flip the matching attendee's PARTSTAT,
    // and (for TENTATIVE) override TRANSP so the calendar renders
    // it differently.
    let events = nimbus_caldav::ical::parse_ics(&raw_ics)
        .map_err(|e| NimbusError::Protocol(format!("could not parse invite: {e}")))?;
    let mut event = events
        .into_iter()
        .next()
        .ok_or_else(|| NimbusError::Protocol("invite has no VEVENT".into()))?;

    // Flip the matching ATTENDEE's PARTSTAT.  When no row
    // matches — common for aliases, forwarded invites, or any
    // case where the user's mail-account address differs from
    // what the organiser typed into ATTENDEE — we ADD a fresh
    // row with the user's address instead of failing.  Sabre's
    // iTIP broker keys "is this PUT an RSVP from this user?"
    // off the principal-email match against the ATTENDEE list,
    // and an inserted row satisfies that check exactly the same
    // as a mutated one.  A REPLY then goes out from NC's iMIP
    // plugin with PARTSTAT carrying the user's chosen response.
    let mut matched = false;
    for att in event.attendees.iter_mut() {
        if att.email.eq_ignore_ascii_case(attendee_email.trim()) {
            att.status = Some(partstat.clone());
            // Force iMIP dispatch on the responding row — see
            // EventAttendee::force_send_reply.  Without this,
            // Sabre may process the PARTSTAT change locally
            // but skip the outbound iMIP to the organiser if
            // its "should this notify?" heuristics decline.
            att.force_send_reply = true;
            matched = true;
        }
    }
    if !matched {
        tracing::info!(
            "RSVP for {attendee_email}: address not in original ATTENDEE list, \
             adding a new row with PARTSTAT={partstat}"
        );
        event.attendees.push(EventAttendee {
            email: attendee_email.trim().to_string(),
            common_name: None,
            status: Some(partstat.clone()),
            role: Some("REQ-PARTICIPANT".into()),
            force_send_reply: true,
        });
    }
    if partstat == "TENTATIVE" {
        event.transparency = Some("TRANSPARENT".into());
    } else {
        // ACCEPTED + DECLINED => OPAQUE so the slot blocks (or
        // would block, before the DECLINE-side DELETE wipes it).
        event.transparency = Some("OPAQUE".into());
    }

    // PUT strategy — Sabre's CalDAV-Schedule plugin only fires a
    // REPLY iMIP when it sees a PARTSTAT diff against the
    // previously-stored copy.  A fresh PUT with `If-None-Match: *`
    // creates the resource for the first time and Sabre treats it
    // as the *organiser* writing into their own calendar — no
    // REPLY emerges.  To force the broker to see a real change,
    // first-time PUTs go in two steps:
    //   1. CREATE with the user's row at PARTSTAT=NEEDS-ACTION
    //      (the same state the inbound REQUEST has).  No iTIP
    //      runs here — there's no diff to compare.
    //   2. UPDATE the same href with the user's chosen PARTSTAT.
    //      Sabre sees NEEDS-ACTION → ACCEPTED/TENTATIVE/DECLINED,
    //      generates a METHOD:REPLY iMIP, and IMipPlugin SMTPs it
    //      to ORGANIZER through the system mailer.
    // For events already in the user's cache (re-RSVP / changing
    // your mind), one update_event keyed on the cached etag is
    // enough — Sabre still sees the prior PARTSTAT and emits the
    // REPLY iMIP.

    // The local cache can fall out of sync with the server in
    // ways that matter here: a previous DECLINED RSVP runs PUT
    // followed by DELETE, and Sabre may "soft-delete" by
    // converting the DELETE into a PARTSTAT=DECLINED on the
    // existing resource (so the organiser still sees who
    // declined).  We dropped the local row, but the server still
    // has the resource — so when the user changes their mind,
    // `find_event_id_by_uid` returns None and we'd try to CREATE
    // a fresh resource with the same UID, which the server
    // rejects with 412 ("already exists").  Refresh the cache
    // via a single-calendar CalDAV sync first, so a soft-delete
    // bounces back into the cache and we route through the
    // update path.
    let mut existing_id = cache.find_event_id_by_uid(&event.id)?;
    if existing_id.is_none() {
        if let Err(e) = refresh_calendar_cache(&cache, &nc_id, &calendar_path).await {
            tracing::warn!("RSVP: pre-PUT cache refresh failed (continuing): {e}");
        }
        existing_id = cache.find_event_id_by_uid(&event.id)?;
    }
    // Track the body we actually PUT — used to mirror into the
    // cache afterwards, so the next surgical edit operates on
    // the body that's really on the server (not a regenerated
    // approximation).
    let body_put: String;
    let put_outcome = match existing_id {
        Some(existing_id) => {
            // Surgical-edit path.  Sabre's iTIP broker only
            // dispatches REPLY iMIP when the diff between the
            // stored body and the new PUT is "clean" — just the
            // user's PARTSTAT.  Regenerating the body via
            // `build_ics` drops X-* properties / re-orders /
            // loses params and Sabre then accepts the PARTSTAT
            // change but suppresses the iTIP REPLY (the same
            // restriction NC's web UI works around by editing
            // only the one line).  We do the same here: pull
            // the cached body, surgically replace just the user's
            // ATTENDEE PARTSTAT (and add SCHEDULE-FORCE-SEND=
            // REPLY), preserve everything else byte-for-byte.
            let handle = load_event_handle(&cache, &existing_id)?;
            let surgical = nimbus_caldav::ical::surgical_set_partstat(
                &handle.ics_raw,
                &attendee_email,
                &partstat,
                true,
            );
            let (out, _) = update_event_with_etag_retry(&cache, &existing_id, &surgical).await?;
            body_put = surgical;
            out
        }
        None => {
            // Step 1 with surgical edit on the inbound ICS so
            // the body Sabre stores as the "before" state is a
            // minimal mutation of the original — Sabre's iTIP
            // restrictions accept it cleanly.
            let step1_body = nimbus_caldav::ical::surgical_set_partstat(
                &raw_ics,
                &attendee_email,
                "NEEDS-ACTION",
                false,
            );
            let first = caldav_create_event(
                &account.server_url,
                &calendar_path,
                &account.username,
                &app_password,
                &event.id,
                &step1_body,
            )
            .await?;

            // Step 2 — update keyed on the etag we just got, with
            // the user's chosen PARTSTAT + SCHEDULE-FORCE-SEND.
            // Sabre sees a clean PARTSTAT-only diff against
            // step 1's stored body and dispatches the REPLY iMIP.
            let step2_body = nimbus_caldav::ical::surgical_set_partstat(
                &raw_ics,
                &attendee_email,
                &partstat,
                true,
            );
            let out = caldav_update_event(
                &first.href,
                &account.username,
                &app_password,
                &first.etag,
                &step2_body,
            )
            .await?;
            body_put = step2_body;
            out
        }
    };

    // Mirror the new state into the local cache so CalendarView
    // shows the accepted/tentative event without waiting for the
    // next sync — and so the *next* surgical edit operates on
    // the body that's actually on the server.
    let row = calendar_event_to_row(&event, &put_outcome.href, &put_outcome.etag, &body_put);
    cache.upsert_single_event(&calendar_id, &row)?;

    // DECLINED used to also DELETE the resource here ("no
    // clutter").  That removed user-declined events from the
    // calendar entirely, which made the badge afterwards look
    // like a cancellation (the event wasn't on any calendar but
    // we had a persisted RSVP for it).  Apple Calendar's
    // approach is right: keep the declined event around with
    // PARTSTAT=DECLINED so it stays visible (faded /
    // struck-through in the grid).  CalendarView can render the
    // declined visual state separately; this command just stops
    // deleting the row.

    // Persist the chosen PARTSTAT keyed by UID so the inbox card
    // re-renders the right state on reopen.  This mirrors what's
    // now on the server but avoids a CalDAV round-trip just for
    // UI feedback.
    if let Err(e) = cache.upsert_rsvp_response(&event.id, &partstat) {
        tracing::warn!("failed to persist RSVP response for {}: {e}", event.id);
    }
    Ok(())
}

/// Look up the user's last RSVP answer (ACCEPTED / DECLINED /
/// TENTATIVE) for an iCalendar UID. The invite card calls this on
/// mount so a previously answered invite re-renders in its
/// post-reply state instead of showing fresh Accept/Decline buttons.
#[tauri::command]
async fn get_rsvp_response(
    uid: String,
    cache: State<'_, Cache>,
) -> Result<Option<String>, NimbusError> {
    cache.get_rsvp_response(&uid).map_err(NimbusError::from)
}

/// Read the responding-user's PARTSTAT off the cached calendar
/// event with `uid`, if any.  Source of truth for the inbox
/// RSVP card so it reflects PARTSTAT changes made via NC web
/// UI / the user's phone / any other CalDAV client — not just
/// the changes Nimbus made itself (which is what the local
/// `rsvp_responses` table tracks).
///
/// Runs a **differential CalDAV sync** of the calendar that
/// contains the event before reading, so the card always
/// reflects the latest server state without requiring the user
/// to wait for the background-sync interval.  CalDAV's
/// sync-collection report is incremental (only the deltas since
/// the last sync token), so the round-trip is cheap even on
/// large calendars.
///
/// Identity matching uses the same candidate list
/// `respond_to_invite` builds: the optional `attendee_hint`
/// from the card, the NC profile email, every configured mail
/// account.  Returns `None` when no row matches (or the event
/// isn't in the cache).
#[tauri::command]
async fn get_event_partstat_for_user(
    uid: String,
    attendee_hint: Option<String>,
    cache: State<'_, Cache>,
) -> Result<Option<String>, NimbusError> {
    let Some(event_id) = cache.find_event_id_by_uid(&uid)? else {
        return Ok(None);
    };
    let handle = cache
        .get_event_server_handle(&event_id)?
        .ok_or_else(|| NimbusError::Other("stale calendar cache entry".into()))?;

    // Differential CalDAV sync of the parent calendar — picks
    // up PARTSTAT changes made via NC web UI / phone / any other
    // CalDAV client without waiting for the background-sync
    // interval.  Best-effort: a sync failure leaves the cache
    // as-is and we return the locally-known state.
    if let Some((_, cal_path)) = cache.get_calendar_server_path(&handle.calendar_id)?
        && let Err(e) =
            refresh_calendar_cache(&cache, &handle.nextcloud_account_id, &cal_path).await
    {
        tracing::warn!(
            "RSVP badge: pre-read calendar sync failed (continuing with stale cache): {e}"
        );
    }
    let Some(handle) = cache.get_event_server_handle(&event_id)? else {
        return Ok(None);
    };

    // Build the candidate list — same shape as respond_to_invite.
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;
    let mut candidates: Vec<String> = Vec::new();
    if let Some(h) = attendee_hint.as_deref() {
        let h = h.trim();
        if !h.is_empty() {
            candidates.push(h.to_string());
        }
    }
    if let Ok(profile) = nimbus_nextcloud::user::fetch_current_user(
        &account.server_url,
        &account.username,
        &app_password,
    )
    .await
        && let Some(email) = profile.email
    {
        candidates.push(email);
    }
    if let Ok(mail_accounts) = account_store::load_accounts(&cache) {
        for a in mail_accounts {
            candidates.push(a.email);
        }
    }
    let candidates_lc: Vec<String> = candidates.iter().map(|s| s.to_ascii_lowercase()).collect();

    let events = nimbus_caldav::ical::parse_ics(&handle.ics_raw)
        .map_err(|e| NimbusError::Protocol(format!("parse cached event: {e}")))?;
    let partstat = events.into_iter().next().and_then(|event| {
        event.attendees.into_iter().find_map(|att| {
            if candidates_lc.contains(&att.email.to_ascii_lowercase()) {
                att.status.map(|s| s.to_ascii_uppercase())
            } else {
                None
            }
        })
    });
    Ok(partstat)
}

/// `caldav_update_event` with transparent etag-mismatch
/// recovery.  When the cached etag is stale (another client
/// edited the same event between our last sync and this PUT)
/// we sync the parent calendar to pull the new etag, refetch
/// the server handle, and retry the PUT once.  The user never
/// sees the "refresh and try again" failure mode.
///
/// Caller passes the app-side `event_id` so we can refetch
/// the handle after the sync — `event_row_id` is stable across
/// syncs (`{calendar_id}::{uid}`), so the same id resolves to
/// the freshly-synced row with the new etag.
///
/// Returns the (possibly second-attempt) `WriteOutcome` and
/// the handle it was written against.  A second 412 bubbles
/// up unwrapped — that means something else (not a simple
/// stale-cache race) is in conflict, and the caller should
/// surface it.
async fn update_event_with_etag_retry(
    cache: &Cache,
    event_id: &str,
    ics: &str,
) -> Result<(nimbus_caldav::WriteOutcome, CalendarEventServerHandle), NimbusError> {
    let handle = load_event_handle(cache, event_id)?;
    let account = load_nextcloud_account(&handle.nextcloud_account_id)?;
    let app_password = credentials::get_nextcloud_password(&handle.nextcloud_account_id)?;

    match caldav_update_event(
        &handle.href,
        &account.username,
        &app_password,
        &handle.etag,
        ics,
    )
    .await
    {
        Ok(o) => Ok((o, handle)),
        Err(NimbusError::EtagMismatch(_)) => {
            tracing::info!("stale etag for {event_id}; refreshing calendar cache and retrying");
            let cal_path = cache
                .get_calendar_server_path(&handle.calendar_id)?
                .map(|(_, p)| p)
                .ok_or_else(|| {
                    NimbusError::Other(format!(
                        "calendar '{}' is not in the local cache",
                        handle.calendar_id
                    ))
                })?;
            refresh_calendar_cache(cache, &handle.nextcloud_account_id, &cal_path).await?;
            let fresh = load_event_handle(cache, event_id)?;
            let outcome = caldav_update_event(
                &fresh.href,
                &account.username,
                &app_password,
                &fresh.etag,
                ics,
            )
            .await?;
            Ok((outcome, fresh))
        }
        Err(e) => Err(e),
    }
}

/// Pull the latest events for one calendar via CalDAV
/// sync-collection and apply the delta to the local cache.
/// Same plumbing as `sync_nextcloud_calendars`'s inner loop, but
/// scoped to a single calendar so the inbound-RSVP path can
/// freshen its cache before deciding create-vs-update.  Soft
/// failures (server transient, no auth, anything) bubble back as
/// `Err`; the caller decides whether to fall through.
async fn refresh_calendar_cache(
    cache: &Cache,
    nc_id: &str,
    calendar_path: &str,
) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(nc_id)?;
    let app_password = credentials::get_nextcloud_password(nc_id)?;
    // Look up the local calendar id by path so we can fetch its
    // sync token and apply the delta against it.
    let calendars = cache.list_calendars(nc_id)?;
    let cal = calendars
        .into_iter()
        .find(|c| c.path == calendar_path)
        .ok_or_else(|| {
            NimbusError::Other(format!(
                "calendar '{calendar_path}' is not in the local cache"
            ))
        })?;
    let prev_token = cache
        .get_calendar_sync_state(&cal.id)
        .ok()
        .flatten()
        .and_then(|s| s.sync_token);
    let delta = caldav_sync_calendar(
        &account.server_url,
        &cal.path,
        &account.username,
        &app_password,
        prev_token.as_deref(),
    )
    .await?;
    let upserts: Vec<CalendarEventRow> = delta.upserts.iter().flat_map(raw_event_to_rows).collect();
    cache.apply_event_delta(
        &cal.id,
        &upserts,
        &delta.deleted_hrefs,
        delta.new_sync_token.as_deref(),
        cal.ctag.as_deref(),
    )?;
    Ok(())
}

fn load_event_handle(
    cache: &Cache,
    event_id: &str,
) -> Result<CalendarEventServerHandle, NimbusError> {
    cache
        .get_event_server_handle(event_id)
        .map_err(NimbusError::from)?
        .ok_or_else(|| {
            NimbusError::Other(format!(
                "event '{event_id}' is not in the local cache — refresh and try again"
            ))
        })
}

/// Flatten one CalDAV resource (href-with-ics) into one store row per
/// VEVENT it contains. Master + recurrence-id overrides all share the
/// same `href`, `etag`, and `ics_raw` — `apply_event_delta` keys the
/// wipe-on-upsert by href, so re-syncing an href with fewer overrides
/// correctly removes the ones that vanished server-side.
fn raw_event_to_rows(raw: &RawEvent) -> Vec<CalendarEventRow> {
    raw.events
        .iter()
        .map(|e| CalendarEventRow {
            // The caldav parser stores the VEVENT UID in `id`.
            uid: e.id.clone(),
            recurrence_id: e.recurrence_id,
            href: raw.href.clone(),
            etag: raw.etag.clone(),
            summary: e.summary.clone(),
            description: e.description.clone(),
            start: e.start,
            end: e.end,
            location: e.location.clone(),
            rrule: e.rrule.clone(),
            rdate: e.rdate.clone(),
            exdate: e.exdate.clone(),
            url: e.url.clone(),
            transparency: e.transparency.clone(),
            attendees: e.attendees.clone(),
            reminders: e.reminders.clone(),
            ics_raw: raw.ics_raw.clone(),
        })
        .collect()
}

/// Fold a `ContactInput` into the shape `build_vcard` expects. The
/// UID is pulled from the caller because the two code paths (create
/// vs. update) source it differently — a fresh UUID vs. the cached
/// one.
fn input_to_parsed(uid: &str, input: &ContactInput) -> ParsedVcard {
    ParsedVcard {
        uid: uid.to_string(),
        display_name: input.display_name.clone(),
        emails: input
            .emails
            .iter()
            .map(|e| nimbus_carddav::VcardEmail {
                kind: e.kind.clone(),
                value: e.value.clone(),
            })
            .collect(),
        phones: input
            .phones
            .iter()
            .map(|p| nimbus_carddav::VcardPhone {
                kind: p.kind.clone(),
                value: p.value.clone(),
            })
            .collect(),
        organization: input.organization.clone(),
        photo_mime: input.photo_mime.clone(),
        photo_data: input.photo_data.clone(),
        title: input.title.clone(),
        birthday: input.birthday.clone(),
        note: input.note.clone(),
        addresses: input
            .addresses
            .as_ref()
            .map(|list| {
                list.iter()
                    .map(|a| nimbus_carddav::VcardAddress {
                        kind: a.kind.clone(),
                        street: a.street.clone(),
                        locality: a.locality.clone(),
                        region: a.region.clone(),
                        postal_code: a.postal_code.clone(),
                        country: a.country.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        urls: input.urls.clone().unwrap_or_default(),
        kind: String::new(),
        members: Vec::new(),
        categories: Vec::new(),
    }
}

/// Build a `ContactRow` from a freshly-PUT vCard's outcome. Extracted
/// so create/update both ship the same set of extended fields
/// (addresses, birthday, urls, note, title) into the cache.
fn parsed_to_row(
    href: &str,
    etag: &str,
    uid: &str,
    parsed: &ParsedVcard,
    vcard_raw: String,
) -> ContactRow {
    ContactRow {
        href: href.to_string(),
        etag: etag.to_string(),
        vcard_uid: uid.to_string(),
        display_name: parsed.display_name.clone(),
        emails: parsed
            .emails
            .iter()
            .map(|e| nimbus_core::models::ContactEmail {
                kind: e.kind.clone(),
                value: e.value.clone(),
            })
            .collect(),
        phones: parsed
            .phones
            .iter()
            .map(|p| nimbus_core::models::ContactPhone {
                kind: p.kind.clone(),
                value: p.value.clone(),
            })
            .collect(),
        organization: parsed.organization.clone(),
        photo_mime: parsed.photo_mime.clone(),
        photo_data: parsed.photo_data.clone(),
        title: parsed.title.clone(),
        birthday: parsed.birthday.clone(),
        note: parsed.note.clone(),
        addresses: parsed
            .addresses
            .iter()
            .map(|a| nimbus_core::models::ContactAddress {
                kind: a.kind.clone(),
                street: a.street.clone(),
                locality: a.locality.clone(),
                region: a.region.clone(),
                postal_code: a.postal_code.clone(),
                country: a.country.clone(),
            })
            .collect(),
        urls: parsed.urls.clone(),
        vcard_raw,
        kind: parsed.kind.clone(),
        member_uids: parsed.members.clone(),
        categories: parsed.categories.clone(),
    }
}

/// Hydrate a freshly-written `ContactRow` into a UI-facing
/// `Contact`. The composite id has to match the one the store
/// uses internally (`{nc_account_id}::{vcard_uid}`) so the next
/// `get_contacts` call returns the same record.
fn row_to_contact(nc_account_id: &str, addressbook: &str, row: &ContactRow) -> Contact {
    Contact {
        id: format!("{nc_account_id}::{}", row.vcard_uid),
        nextcloud_account_id: nc_account_id.to_string(),
        addressbook: addressbook.to_string(),
        display_name: row.display_name.clone(),
        email: row.emails.clone(),
        phone: row.phones.clone(),
        organization: row.organization.clone(),
        photo_mime: row.photo_mime.clone(),
        photo_data: row.photo_data.clone(),
        title: row.title.clone(),
        birthday: row.birthday.clone(),
        note: row.note.clone(),
        addresses: row.addresses.clone(),
        urls: row.urls.clone(),
        kind: row.kind.clone(),
        categories: row.categories.clone(),
    }
}

fn load_nextcloud_account(nc_id: &str) -> Result<NextcloudAccount, NimbusError> {
    nextcloud_store::load_accounts()?
        .into_iter()
        .find(|a| a.id == nc_id)
        .ok_or_else(|| NimbusError::Other(format!("no Nextcloud account with id '{nc_id}'")))
}

fn load_contact_handle(
    cache: &Cache,
    contact_id: &str,
) -> Result<ContactServerHandle, NimbusError> {
    cache
        .get_contact_server_handle(contact_id)
        .map_err(NimbusError::from)?
        .ok_or_else(|| {
            NimbusError::Other(format!(
                "contact '{contact_id}' is not in the local cache — refresh and try again"
            ))
        })
}

// ── IMAP commands ───────────────────────────────────────────────
//
// These are the glue between the frontend mail views and the IMAP
// client. Each command performs a full connect → query → logout
// cycle. This is simple but wasteful — every click reconnects.
// A follow-up issue will introduce connection pooling / a persistent
// session so opening an email isn't a full TCP+TLS+LOGIN round-trip.
//
// Every successful network fetch also writes through to the local
// SQLite cache (Issue #4). Today the UI still always hits the
// network; a follow-up PR will flip reads to cache-first with a
// background refresh.

/// Look up an account by ID, or return a helpful error. Takes a
/// `&Cache` because every account row now lives in SQLite (#60) and
/// we want every callsite to be explicit about which DB it's reading
/// from rather than hiding a global behind a free function.
fn load_account(cache: &Cache, id: &str) -> Result<Account, NimbusError> {
    account_store::load_accounts(cache)?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| NimbusError::Other(format!("no account with id '{id}'")))
}

/// Connect to an account's IMAP server using the stored password.
/// Includes any per-account TLS-trusted certs so a self-signed
/// server the user has previously accepted continues to validate.
async fn connect_imap(account: &Account) -> Result<ImapClient, NimbusError> {
    let password = credentials::get_imap_password(&account.id)?;
    ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await
}

/// Connect to an account's JMAP server using the stored password.
async fn connect_jmap(account: &Account) -> Result<JmapClient, NimbusError> {
    let jmap_url = account.jmap_url.as_deref().ok_or_else(|| {
        NimbusError::Other(format!(
            "Account '{}' has use_jmap=true but no jmap_url configured",
            account.id
        ))
    })?;
    let password = credentials::get_imap_password(&account.id)?;
    JmapClient::connect(jmap_url, &account.email, &password).await
}

/// Returns `true` if this account should use JMAP instead of IMAP.
fn uses_jmap(account: &Account) -> bool {
    account.use_jmap && account.jmap_url.is_some()
}

/// Fetch the newest `limit` envelopes from `folder` for the given account.
///
/// Async because the IMAP client is async (tokio task spawned by Tauri).
#[tauri::command]
async fn fetch_envelopes(
    account_id: String,
    folder: String,
    limit: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    match fetch_envelopes_inner(&account_id, &folder, limit, &cache).await {
        Ok(envs) => Ok(envs),
        Err(e) => {
            tracing::error!("fetch_envelopes failed: {e}");
            Err(e)
        }
    }
}

async fn fetch_envelopes_inner(
    account_id: &str,
    folder: &str,
    limit: u32,
    cache: &Cache,
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    let account = load_account(cache, account_id)?;
    let _ = poll_folder(&account, folder, limit, cache).await?;
    // The poll helper already wrote through to the cache and updated
    // the sync bookmark; we return the newest `limit` from the cache
    // rather than just the delta, because the UI expects a full list
    // regardless of whether this was an incremental or full sync.
    cache
        .get_envelopes(account_id, folder, limit)
        .map_err(Into::into)
}

/// Unified-inbox version of `fetch_envelopes`: polls every configured
/// account's `folder` (sequentially — keeps the SMTP/IMAP server load
/// predictable) and then returns the merged newest-`limit` view from
/// the cache. A poll failure on one account is logged and skipped so a
/// single broken account doesn't blank the unified list.
#[tauri::command]
async fn fetch_unified_envelopes(
    folder: String,
    limit: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    let accounts = account_store::load_accounts(&cache).unwrap_or_default();
    for account in &accounts {
        if let Err(e) = poll_folder(account, &folder, limit, &cache).await {
            tracing::warn!("unified poll failed for '{}': {e}", account.id);
        }
    }
    cache
        .get_unified_envelopes(&folder, limit)
        .map_err(Into::into)
}

/// Outcome of polling a single folder — used by both the user-facing
/// `fetch_envelopes` command and the background sync loop.
///
/// Only the "new" subset is returned: the full batch is already
/// reflected in the cache via write-through, and callers that want it
/// simply `cache.get_envelopes(...)` afterwards. On the very first
/// poll (no prior sync state) `new_envelopes` is empty by design — a
/// fresh install shouldn't fire a notification for every pre-existing
/// message.
struct FolderPollOutcome {
    new_envelopes: Vec<EmailEnvelope>,
}

/// Fetch+cache+reconcile for one (account, folder) pair.
///
/// Shared code path for interactive refreshes and background polling.
/// Steps:
/// 1. Consult the cache for prior `SyncState` (UIDVALIDITY + highest UID).
/// 2. JMAP: one-shot fetch; there's no UIDVALIDITY, so "new" is decided
///    purely by comparing UIDs to `prior_highest`.
/// 3. IMAP: incremental fetch via `since_uid`; if UIDVALIDITY rotated,
///    wipe the folder cache and redo in full mode (no notifications on
///    rotation — `new_envelopes` stays empty in that branch).
/// 4. Write envelopes through to the cache.
/// 5. Update the sync bookmark to `max(prior, newest-fetched)` so an
///    empty incremental response can't accidentally rewind it.
async fn poll_folder(
    account: &Account,
    folder: &str,
    limit: u32,
    cache: &Cache,
) -> Result<FolderPollOutcome, NimbusError> {
    let account_id = &account.id;
    let prior = cache.get_sync_state(account_id, folder).ok().flatten();
    let prior_highest = prior.as_ref().and_then(|s| s.highest_uid_seen);

    // ── JMAP path ──────────────────────────────────────────────
    if uses_jmap(account) {
        let client = connect_jmap(account).await?;
        let envelopes = client.fetch_envelopes(folder, limit, None).await?;

        if let Err(e) = cache.upsert_envelopes_for_account(account_id, &envelopes) {
            tracing::warn!("cache.upsert_envelopes (JMAP) failed: {e}");
        }

        let new_envelopes: Vec<EmailEnvelope> = envelopes
            .iter()
            .filter(|e| prior_highest.is_some_and(|p| e.uid > p))
            .cloned()
            .collect();

        // Credit any newly-arrived unread envelopes against the
        // folder's badge so the sidebar count moves immediately on
        // the next read — without waiting for a fresh `STATUS` round
        // trip from `fetch_folders`.
        let new_unread = new_envelopes.iter().filter(|e| !e.is_read).count() as i64;
        if let Err(e) = cache.bump_folder_unread(account_id, folder, new_unread) {
            tracing::warn!("cache.bump_folder_unread (JMAP) failed: {e}");
        }

        // Bookmark UPDATE: JMAP has no UIDVALIDITY; we only track the
        // highest UID so background polls can diff.
        let new_highest = envelopes
            .iter()
            .map(|e| e.uid)
            .max()
            .into_iter()
            .chain(prior_highest)
            .max();
        let state = SyncState {
            uidvalidity: None,
            highest_uid_seen: new_highest,
            last_synced_at: Some(chrono::Utc::now()),
        };
        if let Err(e) = cache.set_sync_state(account_id, folder, &state) {
            tracing::warn!("cache.set_sync_state (JMAP) failed: {e}");
        }

        return Ok(FolderPollOutcome { new_envelopes });
    }

    // ── IMAP path ──────────────────────────────────────────────
    let mut client = connect_imap(account).await?;
    let mut batch = client.fetch_envelopes(folder, limit, prior_highest).await?;

    // UIDVALIDITY check. If the server has rotated it, every cached UID
    // for this folder now points at a different (or deleted) message —
    // wipe the folder and redo the fetch in full mode so the cache
    // reflects reality. We also mark the outcome as rotated so the
    // caller can skip any "new mail" reactions (the UIDs aren't really
    // new — they're the same messages under a new numbering).
    let uidvalidity_rotated = matches!(
        (prior.as_ref().and_then(|s| s.uidvalidity), batch.uidvalidity),
        (Some(old), Some(new)) if old != new,
    );
    if uidvalidity_rotated {
        tracing::warn!(
            "UIDVALIDITY changed for '{account_id}'/'{folder}' \
             (was {:?}, now {:?}) — wiping folder cache",
            prior.as_ref().and_then(|s| s.uidvalidity),
            batch.uidvalidity,
        );
        if let Err(e) = cache.wipe_folder(account_id, folder) {
            tracing::warn!("cache.wipe_folder failed: {e}");
        }
        batch = client.fetch_envelopes(folder, limit, None).await?;
    }

    // Reconcile the cache against the server's live UID set. Without
    // this, any UID expunged between polls (by our own delete/archive
    // paths, by another client, or by the server itself) would linger
    // as a ghost envelope forever — the incremental fetch above only
    // ever pulls UIDs *greater* than the bookmark, it never revisits
    // older ones. Ghosts used to surface as "UID isn't in folder"
    // errors when the user clicked on them from the mail list.
    let server_uids = match client.list_all_uids(folder).await {
        Ok(uids) => uids,
        Err(e) => {
            tracing::warn!(
                "list_all_uids for '{account_id}'/'{folder}' failed (skipping reconcile): {e}"
            );
            Vec::new()
        }
    };

    let _ = client.logout().await;

    if !server_uids.is_empty() || uidvalidity_rotated {
        let server_set: std::collections::HashSet<u32> = server_uids.into_iter().collect();
        match cache.list_envelope_uids(account_id, folder) {
            Ok(cached_uids) => {
                let mut removed = 0u32;
                for uid in cached_uids {
                    if !server_set.contains(&uid) {
                        match cache.remove_envelope(account_id, folder, uid) {
                            Ok(true) => removed += 1,
                            Ok(false) => {}
                            Err(e) => tracing::warn!(
                                "remove_envelope (reconcile) for UID {uid} failed: {e}"
                            ),
                        }
                    }
                }
                if removed > 0 {
                    tracing::info!(
                        "Reconciled '{account_id}'/'{folder}': dropped {removed} ghost UID(s)"
                    );
                }
            }
            Err(e) => tracing::warn!("list_envelope_uids failed: {e}"),
        }
    }

    if let Err(e) = cache.upsert_envelopes_for_account(account_id, &batch.envelopes) {
        tracing::warn!("cache.upsert_envelopes failed: {e}");
    }

    let new_envelopes: Vec<EmailEnvelope> = if uidvalidity_rotated {
        Vec::new()
    } else {
        batch
            .envelopes
            .iter()
            .filter(|e| prior_highest.is_some_and(|p| e.uid > p))
            .cloned()
            .collect()
    };

    // Same idea as the JMAP path — bump the folder badge by the count
    // of newly-arrived unread envelopes so the sidebar reflects new
    // mail without a `STATUS` round trip. After a UIDVALIDITY rotation
    // `new_envelopes` is empty so `delta` is 0 and this is a no-op.
    let new_unread = new_envelopes.iter().filter(|e| !e.is_read).count() as i64;
    if let Err(e) = cache.bump_folder_unread(account_id, folder, new_unread) {
        tracing::warn!("cache.bump_folder_unread failed: {e}");
    }

    let new_highest = batch
        .envelopes
        .iter()
        .map(|e| e.uid)
        .max()
        .into_iter()
        .chain(prior_highest)
        .max();
    let state = SyncState {
        uidvalidity: batch.uidvalidity,
        highest_uid_seen: new_highest,
        last_synced_at: Some(chrono::Utc::now()),
    };
    if let Err(e) = cache.set_sync_state(account_id, folder, &state) {
        tracing::warn!("cache.set_sync_state failed: {e}");
    }

    Ok(FolderPollOutcome { new_envelopes })
}

/// Fetch a full message (headers + body) by folder + UID.
#[tauri::command]
async fn fetch_message(
    account_id: String,
    folder: String,
    uid: u32,
    cache: State<'_, Cache>,
) -> Result<Email, NimbusError> {
    match fetch_message_inner(&account_id, &folder, uid, &cache).await {
        Ok(email) => Ok(email),
        Err(e) => {
            tracing::error!("fetch_message failed: {e}");
            Err(e)
        }
    }
}

async fn fetch_message_inner(
    account_id: &str,
    folder: &str,
    uid: u32,
    cache: &Cache,
) -> Result<Email, NimbusError> {
    let account = load_account(cache, account_id)?;

    let email = if uses_jmap(&account) {
        let client = connect_jmap(&account).await?;
        client.fetch_message(folder, uid, account_id).await?
    } else {
        let mut client = connect_imap(&account).await?;
        let email = client.fetch_message(folder, uid, account_id).await?;
        let _ = client.logout().await;
        email
    };

    // Single transactional write-through: envelope + body together so the
    // two can never drift on a partial failure.
    if let Err(e) = cache.upsert_message(&email) {
        tracing::warn!("cache.upsert_message failed: {e}");
    }

    Ok(email)
}

/// Download the decoded bytes of a single attachment on a message.
///
/// The UI renders attachment metadata from the (cached or freshly
/// fetched) `Email.attachments` list, but the bytes are never shipped
/// inline — a user with a 20 MB PDF on a message would otherwise pay
/// that cost every time they open the mail. Instead the UI calls this
/// command only when the user actually clicks "Download" or
/// "Save to Nextcloud".
///
/// IMAP path: re-FETCHes the raw message body (PEEK, so unread stays
/// unread) and extracts the attachment at `part_id`. JMAP isn't
/// plumbed through yet — callers on JMAP accounts get an explicit
/// `Protocol` error instead of silently returning empty bytes.
#[tauri::command]
async fn download_email_attachment(
    account_id: String,
    folder: String,
    uid: u32,
    part_id: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<u8>, NimbusError> {
    let account = load_account(&cache, &account_id)?;
    if uses_jmap(&account) {
        return Err(NimbusError::Protocol(
            "JMAP attachment download is not implemented yet".into(),
        ));
    }
    let mut client = connect_imap(&account).await?;
    let (_meta, data) = client.fetch_attachment(&folder, uid, part_id).await?;
    let _ = client.logout().await;
    Ok(data)
}

// ── Attachment preview cache (#157) ──────────────────────────
//
// Persists frontend-generated thumbnails alongside the cached
// message body so MailView re-renders without re-fetching the
// full attachment bytes.  See nimbus-store/src/cache/mod.rs
// for the schema and helpers.

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentPreviewView {
    part_id: u32,
    mime: String,
    /// Base64-encoded thumbnail bytes — the frontend pipes these
    /// straight into a `data:` URL without going through a Blob.
    base64: String,
}

/// Record a rendered thumbnail for one attachment.  Frontend
/// calls this once per attachment after AttachmentThumb extracts
/// or downsamples the preview; subsequent opens of the same
/// message read all of them back in a single query via
/// `get_attachment_previews`.
///
/// Bytes arrive base64-encoded — Tauri's default JSON serializer
/// turns a `Vec<u8>` into a `[123, 45, ...]` number array on the
/// wire, which is roughly 3× the raw size.  A base64 string is
/// ≈1.33× and decodes server-side in microseconds.
#[tauri::command]
fn put_attachment_preview(
    account_id: String,
    folder: String,
    uid: u32,
    part_id: u32,
    mime: String,
    base64: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let bytes = STANDARD
        .decode(base64.as_bytes())
        .map_err(|e| NimbusError::Other(format!("attachment preview base64 decode: {e}")))?;
    cache
        .put_attachment_preview(&account_id, &folder, uid, part_id, &mime, &bytes)
        .map_err(NimbusError::from)
}

/// Bulk-fetch every stored thumbnail for a message.  MailView
/// invokes this once when the email mounts and seeds the
/// in-memory thumb cache so no subsequent `<AttachmentThumb>`
/// has to fetch bytes or run extraction.
#[tauri::command]
fn get_attachment_previews(
    account_id: String,
    folder: String,
    uid: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<AttachmentPreviewView>, NimbusError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let rows = cache
        .get_attachment_previews_for_message(&account_id, &folder, uid)
        .map_err(NimbusError::from)?;
    Ok(rows
        .into_iter()
        .map(|r| AttachmentPreviewView {
            part_id: r.part_id,
            mime: r.mime,
            base64: STANDARD.encode(r.bytes),
        })
        .collect())
}

/// Find an iCalendar payload anywhere in the message and return
/// its raw bytes.  Used by MailView as a fallback for invites
/// where the cached `attachments` array doesn't surface the
/// calendar — most commonly the canonical iMIP MIME shape
/// where `text/calendar` is a body alternative inside
/// `multipart/alternative` and mail-parser classifies it as a
/// body part rather than an attachment.  Returns `None` when
/// the message genuinely has no calendar content (caller hides
/// the RSVP card).
#[tauri::command]
async fn download_calendar_from_message(
    account_id: String,
    folder: String,
    uid: u32,
    cache: State<'_, Cache>,
) -> Result<Option<Vec<u8>>, NimbusError> {
    let account = load_account(&cache, &account_id)?;
    if uses_jmap(&account) {
        return Err(NimbusError::Protocol(
            "JMAP calendar extraction is not implemented yet".into(),
        ));
    }
    let mut client = connect_imap(&account).await?;
    let bytes = client.fetch_calendar_payload(&folder, uid).await?;
    let _ = client.logout().await;
    Ok(bytes)
}

/// Mark a message as read on the server and in the local cache.
///
/// Cache first so the UI sees the change immediately; then the network
/// call propagates the `\Seen` flag to the IMAP server. If the server
/// call fails, we surface the error — but the cache is already updated,
/// which is an acceptable divergence (the next sync will reconcile it).
#[tauri::command]
async fn mark_as_read(
    account_id: String,
    folder: String,
    uid: u32,
    cache: State<'_, Cache>,
    app: AppHandle,
) -> Result<(), NimbusError> {
    set_message_read(account_id, folder, uid, true, cache, app).await
}

/// Toggle the read state of a single message. Generalises
/// `mark_as_read` so the UI can also mark messages as *unread*
/// (the explicit "Mark as unread" affordance — toolbar button and
/// MailList right-click menu).
#[tauri::command]
async fn set_message_read(
    account_id: String,
    folder: String,
    uid: u32,
    read: bool,
    cache: State<'_, Cache>,
    app: AppHandle,
) -> Result<(), NimbusError> {
    // Optimistic cache update — instant UI feedback. Both the
    // `mark_envelope_*` helpers also adjust `folders.unread_count`
    // so the sidebar badge moves with the change.
    let cache_result = if read {
        cache.mark_envelope_read(&account_id, &folder, uid)
    } else {
        cache.mark_envelope_unread(&account_id, &folder, uid)
    };
    if let Err(e) = cache_result {
        tracing::warn!("cache flag update failed: {e}");
    }

    // The user's mental model is "I clicked it, the counter moved"
    // — a 5-minute sync wait would feel broken.
    refresh_unread_badge(&app);

    let account = load_account(&cache, &account_id)?;
    if uses_jmap(&account) {
        let client = connect_jmap(&account).await?;
        return if read {
            client.mark_as_read(&folder, uid).await
        } else {
            client.mark_as_unread(&folder, uid).await
        };
    }

    let mut client = connect_imap(&account).await?;
    let result = if read {
        client.mark_as_read(&folder, uid).await
    } else {
        client.mark_as_unread(&folder, uid).await
    };
    let _ = client.logout().await;
    result
}

/// Remove a message from a folder.
///
/// UX shape matches every major mail client: a first "Delete" press
/// moves the message to Trash (reversible), a second press (from
/// Trash itself, or from any folder on accounts without a Trash
/// folder) permanently expunges it.
///
/// Entry points:
///   - MailView "Delete" button → here.
///   - `save_draft` replace flow → bypasses this command and calls
///     the low-level `ImapClient::delete_message` directly, because
///     "replace this draft with a new version" is update-in-place
///     and shouldn't litter Trash with editing history.
#[tauri::command]
async fn delete_message(
    account_id: String,
    folder: String,
    uid: u32,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;

    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Deleting messages via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }

    // Decide move-to-Trash vs permanent. Already-in-Trash comparison
    // is case-insensitive because the folder name the frontend hands
    // us is the server-reported name but mail servers don't
    // guarantee case stability across listings.
    let trash = pick_trash_folder(&account.id, cache.inner());
    let destination = match trash.as_deref() {
        Some(trash) if !folder.eq_ignore_ascii_case(trash) => Some(trash.to_string()),
        _ => None,
    };

    let password = credentials::get_imap_password(&account.id)?;
    let mut client = ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await?;
    let result = match destination.as_deref() {
        Some(trash) => client.move_message(&folder, uid, trash).await,
        None => client.delete_message(&folder, uid).await,
    };
    let _ = client.logout().await;

    // Clear the cache row whether the delete succeeded OR failed with
    // "UID not on the server" — in the success case the cache would
    // otherwise hang onto a ghost row (incremental envelope fetch
    // never re-examines existing UIDs), and in the failure case the
    // reason we hit that error *is* a stale cache row, so dropping it
    // unblocks the user's next refresh.
    if should_clean_cache_for_delete(&result)
        && let Err(e) = cache.remove_envelope(&account_id, &folder, uid)
    {
        tracing::warn!("remove_envelope after delete_message failed: {e}");
    }

    result
}

/// Locate the account's Trash folder via the IMAP `\Trash` special-use
/// attribute or a name-based fallback. Same strategy as the Sent /
/// Drafts / Archive pickers. Returns `None` if nothing matches — the
/// delete path interprets that as "no Trash on this account, fall back
/// to permanent expunge".
fn pick_trash_folder(account_id: &str, cache: &Cache) -> Option<String> {
    let folders = cache.get_folders(account_id).ok()?;

    if let Some(by_attr) = folders.iter().find(|f| {
        f.attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("trash") || a.eq_ignore_ascii_case("\\trash"))
    }) {
        return Some(by_attr.name.clone());
    }

    const NAME_HINTS: &[&str] = &[
        "trash",
        "bin",
        "deleted items",
        "deleted messages",
        "papierkorb",
        "corbeille",
        "[gmail]/trash",
    ];
    folders
        .iter()
        .find(|f| {
            let lower = f.name.to_lowercase();
            NAME_HINTS.iter().any(|h| lower.contains(h))
        })
        .map(|f| f.name.clone())
}

/// Did this delete_message result leave the cache holding a definitely-
/// stale row for the target UID? True when the server confirmed the
/// delete (Ok) *or* reported the UID isn't there (the probe error we
/// added to `delete_message`) — in both cases the cached envelope
/// should come out.
fn should_clean_cache_for_delete(result: &Result<(), NimbusError>) -> bool {
    match result {
        Ok(()) => true,
        Err(NimbusError::Protocol(msg)) => msg.contains("isn't in folder"),
        _ => false,
    }
}

/// Move the message to the account's Archive folder.
///
/// Semantics: single-click "I'm done with this, get it out of my
/// face" — the message is preserved on the server (unlike
/// `delete_message`) but pulled out of the current mailbox so the
/// Inbox stops showing it. If no Archive folder can be located
/// (server doesn't expose one and no common name matches) the
/// caller gets a clear error rather than silently deleting.
#[tauri::command]
async fn archive_message(
    account_id: String,
    folder: String,
    uid: u32,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;

    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Archiving via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }

    let Some(archive) = pick_archive_folder(&account.id, cache.inner()) else {
        return Err(NimbusError::Other(
            "no Archive folder found for this account — create one on the server or tell us which folder to use".into(),
        ));
    };

    if archive.eq_ignore_ascii_case(&folder) {
        // Already sitting in Archive. Silently succeed rather than
        // move-to-self, which some servers reject and others treat
        // as a noop with a surprising UID change.
        return Ok(());
    }

    let password = credentials::get_imap_password(&account.id)?;
    let mut client = ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await?;
    let result = client.move_message(&folder, uid, &archive).await;
    let _ = client.logout().await;

    if result.is_ok() {
        // The envelope row for the source folder needs to go — the
        // next `fetch_envelopes` is an incremental one and won't
        // notice the move by itself.
        if let Err(e) = cache.remove_envelope(&account_id, &folder, uid) {
            tracing::warn!("remove_envelope after archive_message failed: {e}");
        }
    }

    result
}

/// Move a message to an arbitrary user-picked folder (#89).
///
/// Same shape as `archive_message`, but the destination comes
/// straight from the caller — the picker UI in `MailView` and the
/// drag-and-drop handler in the sidebar both feed through here.
/// Move-to-self is a noop because some IMAP servers reject it and
/// others treat it as a UID-changing roundtrip.  JMAP accounts
/// return an error until JMAP MOVE lands.
#[tauri::command]
async fn move_message(
    account_id: String,
    folder: String,
    uid: u32,
    dest_folder: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;

    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Move via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }

    if dest_folder.eq_ignore_ascii_case(&folder) {
        // Move-to-self is a noop.  Don't trip the IMAP server with a
        // request it might reject, and don't bump the UID.
        return Ok(());
    }

    let password = credentials::get_imap_password(&account.id)?;
    let mut client = ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await?;
    let result = client.move_message(&folder, uid, &dest_folder).await;
    let _ = client.logout().await;

    if result.is_ok() {
        // Drop the source-folder envelope row so the next incremental
        // `fetch_envelopes` doesn't have to.  The destination folder
        // will pick up the new envelope on its next sync tick.
        if let Err(e) = cache.remove_envelope(&account_id, &folder, uid) {
            tracing::warn!("remove_envelope after move_message failed: {e}");
        }
    }

    result
}

/// Batch variant of `move_message` (#89): every message in `uids`
/// moves from the same source folder to the same destination on a
/// single IMAP session.  Issues the UID COPY + UID STORE with a
/// comma-joined UID set so the server handles the lot in one
/// round-trip, and EXPUNGEs once at the end.  Per-call
/// connect/login/logout overhead drops from N to 1, and we no
/// longer race per-message connections — the previous "loop in JS
/// + invoke per UID" flow lost the last move on some servers due
/// to rapid connection recycling.
///
/// Returns the list of UIDs the cache + server agree are gone, so
/// the JS caller can fire its post-move callbacks against a
/// definite success set.
#[tauri::command]
async fn move_messages(
    account_id: String,
    folder: String,
    uids: Vec<u32>,
    dest_folder: String,
    cache: State<'_, Cache>,
) -> Result<Vec<u32>, NimbusError> {
    if uids.is_empty() {
        return Ok(vec![]);
    }
    let account = load_account(&cache, &account_id)?;

    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Move via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }

    if dest_folder.eq_ignore_ascii_case(&folder) {
        return Ok(vec![]); // move-to-self noop
    }

    let password = credentials::get_imap_password(&account.id)?;
    let mut client = ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await?;
    let result = client
        .move_messages_batch(&folder, &uids, &dest_folder)
        .await;
    let _ = client.logout().await;

    result?;

    // Drop the source-folder envelope rows for each successful UID so
    // the next incremental `fetch_envelopes` doesn't have to.  The
    // batch IMAP command is all-or-nothing — either every UID moved
    // or the whole call returned an error — so once we get here the
    // entire input set is on the destination side.
    for uid in &uids {
        if let Err(e) = cache.remove_envelope(&account_id, &folder, *uid) {
            tracing::warn!("remove_envelope after move_messages failed: {e}");
        }
    }

    Ok(uids)
}

/// Locate the account's Archive folder via the IMAP `\Archive`
/// special-use attribute or a name-based fallback. Same strategy as
/// `pick_sent_folder` / `pick_drafts_folder`.
fn pick_archive_folder(account_id: &str, cache: &Cache) -> Option<String> {
    let folders = cache.get_folders(account_id).ok()?;

    if let Some(by_attr) = folders.iter().find(|f| {
        f.attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("archive") || a.eq_ignore_ascii_case("\\archive"))
    }) {
        return Some(by_attr.name.clone());
    }

    const NAME_HINTS: &[&str] = &[
        "archive",
        "archiv",
        "archives",
        "archivé",
        "archivés",
        "all mail",
        "[gmail]/all mail",
    ];
    folders
        .iter()
        .find(|f| {
            let lower = f.name.to_lowercase();
            NAME_HINTS.iter().any(|h| lower.contains(h))
        })
        .map(|f| f.name.clone())
}

// ── SMTP commands ───────────────────────────────────────────────

/// Send an email via the account's configured SMTP server.
///
/// The frontend builds an `OutgoingEmail` (recipients, subject, body,
/// attachments) and sends it here. We look up the account to get the
/// SMTP host/port, retrieve the password from the keychain, and connect.
/// The `from` field on `email` is authoritative — the UI sets it from
/// the active account so Compose-from-alias can be added later without
/// backend changes.
///
/// After SMTP delivery, the message is appended to the IMAP Sent folder
/// so the user has a visible record. JMAP handles this server-side.
#[tauri::command]
async fn send_email(
    account_id: String,
    email: OutgoingEmail,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;

    // JMAP handles sending server-side via EmailSubmission and writes
    // a copy to Sent itself — no separate SMTP/APPEND needed.
    if uses_jmap(&account) {
        let client = connect_jmap(&account).await?;
        return client.send_email(&email).await;
    }

    // Build the lettre message once so the same bytes go to both the
    // SMTP recipients and the IMAP `APPEND` to Sent. Avoids the body
    // diverging between the two paths if MIME generation ever becomes
    // non-deterministic.
    let message = build_outgoing_message(&email)?;
    let raw = message.formatted();

    let password = credentials::get_imap_password(&account.id)?;
    let smtp = SmtpClient::connect(
        &account.smtp_host,
        account.smtp_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await?;
    smtp.send(&email).await?;

    // Best-effort APPEND to Sent. SMTP succeeded, so the recipients
    // already have the mail — failing the whole command because we
    // couldn't update the local Sent view would be worse UX than a
    // missing copy. We log and move on; the next folder fetch will
    // catch up if the server still received the SMTP-side delivery.
    //
    // Auto-generated calendar mails (the calendar-grid "send invite"
    // flow + RSVP REPLY) opt out via `skip_sent_copy`: Outlook /
    // Apple Mail / Google Calendar all hide that traffic from the
    // sender's Sent view too — RSVP responses are conceptually
    // meeting machinery, not user-authored mail.
    if !email.skip_sent_copy
        && let Err(e) = append_to_sent(&account, &raw, &cache).await
    {
        tracing::warn!(
            "Sent OK but failed to append a copy to Sent for account '{}': {e}",
            account.id
        );
    }
    Ok(())
}

/// Locate the account's Sent folder (via the IMAP `\Sent` attribute,
/// or a name-based fallback) and `APPEND` the raw RFC 822 bytes there.
/// Marked `\Seen` so it doesn't add to the unread badge.
async fn append_to_sent(account: &Account, raw: &[u8], cache: &Cache) -> Result<(), NimbusError> {
    let sent_folder = pick_sent_folder(&account.id, cache);
    let Some(sent) = sent_folder else {
        return Err(NimbusError::Other(
            "no Sent folder found in cached folder list".into(),
        ));
    };

    let password = credentials::get_imap_password(&account.id)?;
    let mut client = ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await?;
    let result = client.append_message(&sent, raw, &["\\Seen"]).await;
    let _ = client.logout().await;
    result
}

/// Payload for the "this save replaces an existing draft" flow.
/// When Compose opens an existing draft for editing, the frontend
/// hands the source UID + folder back here so `save_draft` can
/// APPEND-then-delete inside the same IMAP session — avoiding the
/// split-connection race where a separate `delete_message` call
/// would run after the APPEND and sometimes leave the original
/// behind.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftReplaceSource {
    folder: String,
    uid: u32,
}

/// Save an in-progress message to the account's IMAP Drafts folder.
///
/// Mirrors `send_email` structurally (same `OutgoingEmail` input, same
/// MIME builder) but skips SMTP entirely — the point is to hand the
/// message to the server so it shows up in the Drafts mailbox across
/// devices and the user can finish / send it later. IMAP-only for now;
/// JMAP accounts get a clear error until the equivalent `Email/set`
/// create-in-Drafts flow is wired up (tracked separately).
///
/// When `replace_source` is set, the save is treated as a
/// continuation of an existing draft the user opened from Drafts:
/// we APPEND the new copy into that *same folder* (not whatever
/// `pick_drafts_folder` thinks Drafts is — the server might have
/// multiple drafts-like folders and we want the edit to land where
/// the user is looking) and then EXPUNGE the source UID in the
/// same session, so from the user's perspective the draft they
/// were editing is updated in place.
#[tauri::command]
async fn save_draft(
    account_id: String,
    email: OutgoingEmail,
    replace_source: Option<DraftReplaceSource>,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;

    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Saving drafts via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }

    let message = build_outgoing_message(&email)?;
    let raw = message.formatted();

    // Prefer the source folder when replacing an existing draft so
    // APPEND and DELETE both target the folder the user actually
    // opened the draft from. Otherwise fall back to the "find the
    // account's Drafts folder" heuristic for brand-new drafts.
    let target_folder = match replace_source.as_ref() {
        Some(src) => src.folder.clone(),
        None => pick_drafts_folder(&account.id, cache.inner()).ok_or_else(|| {
            NimbusError::Other("no Drafts folder found in cached folder list".into())
        })?,
    };

    let password = credentials::get_imap_password(&account.id)?;
    let mut client = ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
        &account.trusted_certs,
    )
    .await?;

    // `\Draft` marks the message as an unfinished draft. `\Seen`
    // keeps it out of the unread badge — there's no point notifying
    // the user about a mail they themselves just composed.
    let append_result = client
        .append_message(&target_folder, &raw, &["\\Draft", "\\Seen"])
        .await;

    // Only attempt the delete if the APPEND actually succeeded —
    // otherwise a flaky APPEND would have us destroy the user's
    // only remaining copy. We also want to clear the cached envelope
    // for the source UID whether the server-side delete hit an
    // existing UID or complained that the UID wasn't there (ghost
    // envelope left over from a previous expunge) — either way the
    // cached row is wrong and hanging onto it just makes the next
    // edit attempt fail the same way.
    let result = if append_result.is_ok() {
        if let Some(src) = replace_source {
            let delete_result = client.delete_message(&src.folder, src.uid).await;
            if should_clean_cache_for_delete(&delete_result)
                && let Err(e) = cache.remove_envelope(&account_id, &src.folder, src.uid)
            {
                tracing::warn!("remove_envelope after save_draft replace failed: {e}");
            }
            match delete_result {
                Ok(()) => Ok(()),
                Err(e) => Err(NimbusError::Other(format!(
                    "Draft saved, but removing the previous copy (UID {}) failed: {e}",
                    src.uid
                ))),
            }
        } else {
            Ok(())
        }
    } else {
        append_result
    };

    let _ = client.logout().await;
    result
}

/// Pick the most likely Drafts folder name from the cached folder list.
/// Same strategy as `pick_sent_folder`: prefer the IMAP `\Drafts`
/// special-use attribute, fall back to common English / German / French
/// names so accounts that haven't been synced yet still land in the
/// right place.
fn pick_drafts_folder(account_id: &str, cache: &Cache) -> Option<String> {
    let folders = cache.get_folders(account_id).ok()?;

    if let Some(by_attr) = folders.iter().find(|f| {
        f.attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("drafts") || a.eq_ignore_ascii_case("\\drafts"))
    }) {
        return Some(by_attr.name.clone());
    }

    const NAME_HINTS: &[&str] = &[
        "drafts",
        "draft",
        "entwürfe",
        "entwurf",
        "brouillons",
        "brouillon",
    ];
    folders
        .iter()
        .find(|f| {
            let lower = f.name.to_lowercase();
            NAME_HINTS.iter().any(|h| lower.contains(h))
        })
        .map(|f| f.name.clone())
}

/// Pick the most likely Sent folder name from the cached folder list.
/// Prefers folders flagged with the IMAP `\Sent` special-use attribute
/// (the canonical, locale-independent answer) and falls back to common
/// English / German / French names so accounts that haven't been
/// re-synced after their first launch still get a copy filed somewhere
/// sensible. Returns `None` if nothing matches — the caller surfaces
/// that as a warning rather than an error.
fn pick_sent_folder(account_id: &str, cache: &Cache) -> Option<String> {
    let folders = cache.get_folders(account_id).ok()?;

    if let Some(by_attr) = folders.iter().find(|f| {
        f.attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("sent") || a.eq_ignore_ascii_case("\\sent"))
    }) {
        return Some(by_attr.name.clone());
    }

    const NAME_HINTS: &[&str] = &[
        "sent",
        "sent items",
        "sent messages",
        "sent mail",
        "gesendet",
        "gesendete elemente",
        "envoyés",
    ];
    folders
        .iter()
        .find(|f| {
            let lower = f.name.to_lowercase();
            NAME_HINTS.iter().any(|h| lower.contains(h))
        })
        .map(|f| f.name.clone())
}

// ── Folder commands ─────────────────────────────────────────────

/// List the account's mailboxes live from the server and write-through
/// into the cache. Called by the Sidebar's refresh path after the
/// cache-first render.
#[tauri::command]
async fn fetch_folders(
    account_id: String,
    cache: State<'_, Cache>,
) -> Result<Vec<Folder>, NimbusError> {
    let account = load_account(&cache, &account_id)?;

    let folders = if uses_jmap(&account) {
        let client = connect_jmap(&account).await?;
        client.list_folders().await?
    } else {
        let mut client = connect_imap(&account).await?;
        let folders = client.list_folders().await?;
        let _ = client.logout().await;
        folders
    };

    // Write-through — cache failures are non-fatal; the live list is
    // still returned so the UI can render something useful.
    if let Err(e) = cache.upsert_folders(&account_id, &folders) {
        tracing::warn!("cache.upsert_folders failed: {e}");
    }
    Ok(folders)
}

#[tauri::command]
fn get_cached_folders(
    account_id: String,
    cache: State<'_, Cache>,
) -> Result<Vec<Folder>, NimbusError> {
    cache.get_folders(&account_id).map_err(Into::into)
}

// ── Folder-management commands ──────────────────────────────────
//
// Thin wrappers around the IMAP CREATE / DELETE / RENAME primitives.
// JMAP-only accounts get a not-yet-implemented error so we're never
// surprised by a silent no-op on those; the JMAP side would use
// `Mailbox/set` and is deferred.

/// Create a new mailbox. Hierarchy is expressed in the `name`
/// argument itself (e.g. `"Projects/2026"` with the server's
/// delimiter) — the caller decides whether this is top-level or a
/// subfolder, we just forward to IMAP. After success the frontend
/// re-runs `fetch_folders` so the new entry shows up.
#[tauri::command]
async fn create_folder(
    account_id: String,
    name: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;
    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Creating folders via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }
    let mut client = connect_imap(&account).await?;
    let result = client.create_folder(&name).await;
    let _ = client.logout().await;
    result
}

/// Delete a mailbox. The IMAP server usually refuses to drop a
/// non-empty folder (errors bubble up unchanged). On success we
/// wipe the folder's cache rows so the sidebar / MailList don't
/// keep showing ghost envelopes until the next reconcile.
#[tauri::command]
async fn delete_folder(
    account_id: String,
    name: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;
    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Deleting folders via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }
    let mut client = connect_imap(&account).await?;
    let result = client.delete_folder(&name).await;
    let _ = client.logout().await;

    if result.is_ok()
        && let Err(e) = cache.wipe_folder(&account_id, &name)
    {
        tracing::warn!("wipe_folder after delete_folder failed: {e}");
    }

    result
}

/// Rename a mailbox. IMAP RENAME preserves UIDs, so we carry every
/// cached envelope / body / sync bookmark over to the new name in
/// one SQL pass via `Cache::rename_folder` — no re-fetching.
#[tauri::command]
async fn rename_folder(
    account_id: String,
    old_name: String,
    new_name: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    let account = load_account(&cache, &account_id)?;
    if uses_jmap(&account) {
        return Err(NimbusError::Other(
            "Renaming folders via JMAP is not yet implemented — this account uses JMAP".into(),
        ));
    }
    let mut client = connect_imap(&account).await?;
    let result = client.rename_folder(&old_name, &new_name).await;
    let _ = client.logout().await;

    if result.is_ok()
        && let Err(e) = cache.rename_folder(&account_id, &old_name, &new_name)
    {
        tracing::warn!("cache.rename_folder failed: {e}");
    }

    result
}

// ── Cache-first read commands ───────────────────────────────────
//
// These return whatever's in the local cache instantly so the UI has
// something to show on launch. The frontend pairs each call with the
// matching network `fetch_*` and replaces the view when fresh data
// lands. Returning `Option`/empty `Vec` (rather than an error) keeps
// the "cache miss is normal" path cheap.

#[tauri::command]
fn get_cached_envelopes(
    account_id: String,
    folder: String,
    limit: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    cache
        .get_envelopes(&account_id, &folder, limit)
        .map_err(Into::into)
}

/// Cache-only sibling of `fetch_unified_envelopes` — returns the merged
/// newest-`limit` envelopes across all accounts without hitting the
/// network. Powers the instant first-paint of the unified inbox.
#[tauri::command]
fn get_unified_cached_envelopes(
    folder: String,
    limit: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    cache
        .get_unified_envelopes(&folder, limit)
        .map_err(Into::into)
}

#[tauri::command]
fn get_cached_message(
    account_id: String,
    folder: String,
    uid: u32,
    cache: State<'_, Cache>,
) -> Result<Option<Email>, NimbusError> {
    cache
        .get_message(&account_id, &folder, uid)
        .map_err(Into::into)
}

// ── JMAP commands ──────────────────────────────────────────────────

/// Test a JMAP connection by performing session discovery.
///
/// Similar to `test_connection` for IMAP — the setup wizard uses this
/// to verify JMAP credentials before saving the account.
#[tauri::command]
async fn test_jmap_connection(
    jmap_url: String,
    username: String,
    password: String,
) -> Result<String, NimbusError> {
    tracing::info!("Testing JMAP connection to {jmap_url} as {username}");
    JmapClient::test(&jmap_url, &username, &password).await
}

/// Probe whether a server supports JMAP by trying `.well-known/jmap`.
///
/// Returns the JMAP base URL if discovered, or `None` if the server
/// doesn't support JMAP. This is a best-effort probe — it's fine to
/// fall back to IMAP if this fails.
#[tauri::command]
async fn detect_jmap(host: String) -> Result<Option<String>, NimbusError> {
    // Try HTTPS first (standard), then HTTP as fallback.
    for scheme in &["https", "http"] {
        let url = format!("{scheme}://{host}/.well-known/jmap");
        tracing::debug!("Probing JMAP at {url}");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| NimbusError::Network(format!("HTTP client error: {e}")))?;

        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 401 => {
                // 200 = JMAP available (open session endpoint).
                // 401 = JMAP available but needs auth (common for production servers).
                let base = format!("{scheme}://{host}");
                tracing::info!("JMAP detected at {base}");
                return Ok(Some(base));
            }
            Ok(resp) => {
                tracing::debug!("JMAP probe got HTTP {} — not available", resp.status());
            }
            Err(e) => {
                tracing::debug!("JMAP probe failed: {e}");
            }
        }
    }

    Ok(None)
}

// ── Custom URI scheme: contact photos ──────────────────────────
//
// Contact avatars are served via a custom `contact-photo://<id>`
// scheme so the webview can request them with a plain `<img src>`
// instead of round-tripping the bytes through the JSON IPC layer.
// JSON serialises a byte as one number (3–4 chars per byte), so
// shipping 200 photos that way turned the contacts list into tens
// of MB of IPC traffic. Going through a URI scheme:
//
// - the body is raw bytes — no encoding bloat
// - the browser caches per-URL, so scrolling a row off and back on
//   doesn't re-fetch
// - `loading="lazy"` on the `<img>` defers fetches for off-screen
//   rows, so opening a 1000-contact addressbook only pays for the
//   ~20 photos actually visible
//
// The path component of the URL is the contact's app-side id,
// percent-encoded by `convertFileSrc` on the JS side.
fn contact_photo_protocol(
    ctx: UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    let id = percent_decode(request.uri().path().trim_start_matches('/'));
    let cache = ctx.app_handle().state::<Cache>();
    match cache.get_contact_photo(&id) {
        Ok(Some((mime, bytes))) => tauri::http::Response::builder()
            .status(200)
            .header("Content-Type", mime)
            // The bytes are immutable per (id, etag) — but we don't
            // know the etag here. A short cache window is enough to
            // dedupe the burst of requests that comes from scrolling.
            .header("Cache-Control", "private, max-age=300")
            .body(std::borrow::Cow::Owned(bytes))
            .expect("build photo response"),
        Ok(None) => tauri::http::Response::builder()
            .status(404)
            .body(std::borrow::Cow::Owned(Vec::new()))
            .expect("build 404"),
        Err(e) => {
            tracing::warn!("contact-photo lookup for '{id}' failed: {e}");
            tauri::http::Response::builder()
                .status(500)
                .body(std::borrow::Cow::Owned(Vec::new()))
                .expect("build 500")
        }
    }
}

/// Minimal RFC 3986 percent-decoder. Avoids pulling in a dep just
/// to undo what `encodeURIComponent` did on the JS side. Unrecognised
/// `%xx` sequences are passed through verbatim.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── Search commands (Issue #15) ────────────────────────────────
//
// Two-tier search, Outlook-style:
//
//   1. `search_emails`  — instant, against the local FTS5 index.
//                         Covers everything in the cache.
//
//   2. `search_imap_server` — explicit fallback that hits IMAP
//                             `UID SEARCH`. Slower, server-dependent,
//                             only run when the user asks for it
//                             ("Search server too" button).
//
// The cache-first path is the default UX. The fallback is a button
// because (a) it's slow and (b) we don't want to spam the server on
// every keystroke.

/// Run a full-text search against the local mail cache.
///
/// The query is parsed as Outlook-style operator syntax (see
/// `nimbus_store::cache::search` for grammar). `scope` and
/// `filters` are optional narrowings from the UI — empty values
/// mean "search everything the cache has".
#[tauri::command]
fn search_emails(
    query: String,
    scope: Option<SearchScope>,
    filters: Option<SearchFilters>,
    cache: State<'_, Cache>,
) -> Result<Vec<SearchHit>, NimbusError> {
    let scope = scope.unwrap_or_default();
    let filters = filters.unwrap_or_default();
    cache
        .search_emails(&query, &scope, &filters)
        .map_err(Into::into)
}

/// Server-side IMAP SEARCH fallback. Only JMAP/IMAP — the JMAP
/// client already pulls everything into the cache lazily, so users
/// pointed at a JMAP server get instant results via the local FTS5
/// index and don't need this path.
///
/// Returns envelopes in the same shape as `fetch_envelopes` so the
/// frontend can feed them into the existing mail-list renderer and
/// also upserts them into the local cache so the next search
/// finds them instantly without another round-trip.
#[tauri::command]
async fn search_imap_server(
    account_id: String,
    folder: String,
    query: String,
    limit: u32,
    cache: State<'_, Cache>,
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    let account = load_account(&cache, &account_id)?;
    if uses_jmap(&account) {
        // JMAP cache-first coverage is comprehensive; no separate
        // server-side search path yet. Return empty so the UI
        // silently no-ops the fallback button for JMAP accounts.
        return Ok(Vec::new());
    }

    let criterion = imap_search_criterion(&query);
    if criterion.is_empty() {
        return Ok(Vec::new());
    }

    let mut client = connect_imap(&account).await?;
    let hits = client.search_envelopes(&folder, &criterion, limit).await?;
    let _ = client.logout().await;

    // Warm the cache so the next query is served locally.
    if !hits.is_empty() {
        cache.upsert_envelopes_for_account(&account_id, &hits)?;
    }
    Ok(hits)
}

/// Translate a user query into an IMAP SEARCH criterion string.
///
/// We keep this much simpler than the FTS parser — IMAP SEARCH
/// doesn't have rich boolean syntax and most servers only support
/// a small subset of RFC 3501's operators. We emit a conjunction
/// (implicit AND) of `TEXT`/`FROM`/`TO`/`SUBJECT` terms.
///
/// The result is a single string like:
///   `SUBJECT "foo" FROM "alice" TEXT "budget"`
fn imap_search_criterion(query: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut free_text: Vec<String> = Vec::new();

    for token in tokenize_imap_query(query) {
        if let Some((op, value)) = token.split_once(':') {
            let value = value.trim_matches('"');
            if value.is_empty() {
                continue;
            }
            let key = match op.to_ascii_lowercase().as_str() {
                "from" => Some("FROM"),
                "to" => Some("TO"),
                "cc" => Some("CC"),
                "subject" | "title" => Some("SUBJECT"),
                "body" => Some("BODY"),
                _ => None,
            };
            if let Some(k) = key {
                parts.push(format!("{k} \"{}\"", imap_quote(value)));
                continue;
            }
        }
        let cleaned = token.trim_matches('"');
        if !cleaned.is_empty() {
            free_text.push(cleaned.to_string());
        }
    }

    for text in free_text {
        parts.push(format!("TEXT \"{}\"", imap_quote(&text)));
    }

    parts.join(" ")
}

/// Split a query into tokens, keeping quoted phrases intact.
fn tokenize_imap_query(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in input.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            w if w.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Escape `"` and `\` inside an IMAP quoted string (RFC 3501 §4.3).
fn imap_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ── Tray, window lifecycle, and background sync (Issue #16) ────
//
// Three concerns wired together here:
//
//   1. **Tray icon + menu** — always present; gives the user a way
//      back into the app when the window is hidden, plus one-click
//      actions (Check Mail, Compose, Quit).
//   2. **Close-to-tray** — if the user's preference is on, clicking
//      the window's close button hides the window instead of
//      quitting. They quit explicitly via the tray menu.
//   3. **Background sync** — a tokio task polls every configured
//      account's INBOX at a user-set interval. New messages trigger
//      a Tauri event that the frontend turns into an OS toast.
//
// The Rust side deliberately does **not** call the notification
// plugin itself. It emits `new-mail` events with
// `{ account_id, folder, uid, from, subject }` payloads and the
// frontend decides whether (and how) to display them. Rationale:
// one permission check path (in JS), one formatting path, and no
// risk of a background tick racing the OS permission prompt.

#[derive(Debug, Clone, Serialize)]
struct NewMailPayload {
    account_id: String,
    folder: String,
    uid: u32,
    from: String,
    subject: String,
}

/// Load the tray icon. Reuses the window icon when present (same PNG
/// we ship with the app) so dev and prod builds paint the same bitmap.
fn load_tray_icon(app: &AppHandle) -> Result<tauri::image::Image<'_>, NimbusError> {
    app.default_window_icon()
        .cloned()
        .ok_or_else(|| NimbusError::Other("no default window icon available for tray".into()))
}

/// Bring the main window to the front. Called from the tray's
/// left-click handler, the tray menu's "Open Nimbus" item, and the
/// `show_main_window` command.
fn show_main_window(app: &AppHandle) -> Result<(), NimbusError> {
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| NimbusError::Other("main window not found".into()))?;
    // show() may be a no-op if the window is already visible, but
    // unminimize() + set_focus() still make sense in that case.
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.set_focus();
    Ok(())
}

/// One poll across every configured account's INBOX. Emits `new-mail`
/// for each envelope whose UID is greater than the previously-seen
/// high-water mark, then emits a single `unread-count-updated` with
/// the fresh total. Used by both the periodic loop and the `Check Mail
/// Now` tray/UI action — same code path so manual and automatic
/// refreshes behave identically.
async fn check_mail_now_inner(app: &AppHandle) -> Result<(), NimbusError> {
    let cache = app.state::<Cache>();
    let accounts = account_store::load_accounts(&cache).unwrap_or_default();

    for account in &accounts {
        match poll_folder(account, "INBOX", 20, &cache).await {
            Ok(outcome) => {
                for env in &outcome.new_envelopes {
                    let payload = NewMailPayload {
                        account_id: account.id.clone(),
                        folder: "INBOX".to_string(),
                        uid: env.uid,
                        from: env.from.clone(),
                        subject: env.subject.clone(),
                    };
                    if let Err(e) = app.emit("new-mail", &payload) {
                        tracing::warn!("failed to emit new-mail event: {e}");
                    }
                }
                if !outcome.new_envelopes.is_empty() {
                    tracing::info!(
                        "{}: {} new message(s) in INBOX",
                        account.id,
                        outcome.new_envelopes.len()
                    );
                }
            }
            Err(e) => {
                // One broken account shouldn't stop us polling the others.
                tracing::warn!("background poll failed for '{}': {e}", account.id);
            }
        }
    }

    // Refresh the tray icon badge, the Windows taskbar overlay, and
    // notify the UI. A failure to read the cache count is non-fatal —
    // the badge stays stale until the next tick.
    refresh_unread_badge(app);

    Ok(())
}

/// Recompute the unread total and apply it everywhere it shows up:
/// the tray icon (badge + tooltip), the Windows taskbar overlay, and
/// the `unread-count-updated` event for the UI.
///
/// Called from three places: the setup hook (paint the initial badge),
/// `check_mail_now_inner` (after polling), and `mark_as_read` (so
/// reading a message visibly drops the count without waiting for the
/// next sync tick).
fn refresh_unread_badge(app: &AppHandle) {
    let total = match app.state::<Cache>().total_unread_count() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("refresh_unread_badge: cache read failed: {e}");
            return;
        }
    };

    if let Some(tray) = app.tray_by_id("nimbus-main") {
        let base = app.state::<TrayBaseIcon>();
        let badged = badge::render_tray_icon(&base.rgba, base.width, base.height, total);
        if let Err(e) = tray.set_icon(Some(badged)) {
            tracing::warn!("failed to update tray icon: {e}");
        }
        let tip = if total == 0 {
            "Nimbus Mail".to_string()
        } else {
            format!("Nimbus Mail — {total} unread")
        };
        let _ = tray.set_tooltip(Some(&tip));
    }

    // Windows-only: the taskbar overlay icon. macOS/Linux have no
    // direct equivalent — `set_overlay_icon` only exists behind
    // `#[cfg(windows)]`. We tried badging the window icon on those
    // platforms via `WebviewWindow::set_icon`, but on Linux that
    // sets the X11 `_NET_WM_ICON` atom — which most WMs (KDE,
    // XFCE, Cinnamon) use for both the taskbar entry AND the
    // title-bar icon. No way through Tauri to update one without
    // the other, and a badged title-bar icon looks out of place
    // sitting next to the window title. So on non-Windows we leave
    // the badge to the system tray icon alone.
    #[cfg(windows)]
    if let Some(win) = app.get_webview_window("main") {
        let overlay = badge::render_taskbar_overlay(total);
        if let Err(e) = win.set_overlay_icon(overlay) {
            tracing::warn!("failed to set taskbar overlay icon: {e}");
        }
    }

    if let Err(e) = app.emit("unread-count-updated", total) {
        tracing::warn!("failed to emit unread-count-updated: {e}");
    }
    // Issue #115: also push the per-account split so the
    // IconRail can paint a red badge on each account's avatar
    // without doing its own poll.  Soft-fails — the global
    // count above is still informative even if this query
    // bombs.
    match app.state::<Cache>().unread_counts_by_account() {
        Ok(by_acc) => {
            if let Err(e) = app.emit("unread-count-by-account-updated", &by_acc) {
                tracing::warn!("failed to emit unread-count-by-account-updated: {e}");
            }
        }
        Err(e) => tracing::warn!("unread_counts_by_account failed: {e}"),
    }
}

/// Per-account unread INBOX count map, keyed by account id.
/// Used by the IconRail on mount to paint per-avatar badges
/// before the next `unread-count-by-account-updated` event
/// fires (those only land on poll completion).
#[tauri::command]
fn get_unread_counts_by_account(
    cache: State<'_, Cache>,
) -> Result<std::collections::HashMap<String, u32>, NimbusError> {
    cache.unread_counts_by_account().map_err(Into::into)
}

// ── Talk-join reminders (issue #123) ──────────────────────────
//
// Goal: fire a desktop notification ahead of any calendar event
// that carries a Nextcloud Talk URL, with the lead time taken
// from the event's own `VALARM` reminders so the user controls
// timing per-event.  Rides the background sync loop's tick, so
// no extra timers; in-memory dedupe keys off `(uid,
// minutes_before)` so a second tick within the firing window
// doesn't double-toast.

/// Lead time in seconds we'll widen the firing window by, on
/// each side of the reminder's exact moment.  Slightly larger
/// than the default 60s tick so a tick that drifts by a few
/// seconds doesn't miss the reminder entirely.
const TALK_REMINDER_FIRE_TOLERANCE_SECS: i64 = 90;

/// In-memory state for the Talk-reminder pipeline.
///
/// `fired`: set of `(uid, minutes_before)` pairs we've already
///   pushed a notification for.  Pruned on each scan to drop
///   entries whose event has already started (the reminder is
///   moot once the meeting is in progress).
/// `dismissed`: UIDs the user explicitly silenced for the rest
///   of the meeting cycle (e.g. after clicking through to join
///   the room — surfaced via the `dismiss_talk_reminder` IPC).
#[derive(Default)]
struct TalkReminderState {
    fired: Mutex<HashSet<(String, i32)>>,
    dismissed: Mutex<HashSet<String>>,
}

/// Pull the first plausible meeting URL out of an event's body
/// text — Nextcloud Talk, Zoom, Teams, Google Meet, Webex, Jitsi,
/// etc.  Any HTTP(S) URL counts; we don't try to be smart about
/// which platform it points at because that ages badly (every
/// quarter brings a new conferencing service).
///
/// Searched fields, in priority order: `URL` (canonical), then
/// `LOCATION` (where Outlook stores the join link), then
/// `DESCRIPTION` (where pasted "click to join" links land).
fn extract_meeting_url(event: &CalendarEvent) -> Option<String> {
    fn extract_from(s: &str) -> Option<String> {
        // Walk word by word so the trailing punctuation in
        // pasted plain-text bodies ("…click here: <url>.")
        // doesn't end up baked into the captured URL.
        for token in s.split_whitespace() {
            let url = token.trim_matches(|c: char| {
                c == '<'
                    || c == '>'
                    || c == '"'
                    || c == '\''
                    || c == ','
                    || c == '.'
                    || c == ';'
                    || c == ')'
                    || c == '('
            });
            if url.starts_with("http://") || url.starts_with("https://") {
                return Some(url.to_string());
            }
        }
        None
    }
    let url_field = event.url.as_deref().unwrap_or("");
    let loc_field = event.location.as_deref().unwrap_or("");
    let desc_field = event.description.as_deref().unwrap_or("");
    extract_from(url_field)
        .or_else(|| extract_from(loc_field))
        .or_else(|| extract_from(desc_field))
}

/// Payload pushed to the frontend on every fired reminder.
/// Mirrors the camelCase shape JS expects.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TalkReminderPayload {
    /// VEVENT UID — the canonical join key the frontend uses to
    /// suppress repeat reminders via `dismiss_talk_reminder`.
    uid: String,
    summary: String,
    /// Event start in UTC RFC 3339 — the JS side localises for
    /// the toast body ("Meeting in 15 min" / "starts at 14:00").
    start: chrono::DateTime<chrono::Utc>,
    talk_url: String,
    /// Lead time the reminder fired at, in minutes.  Lets the
    /// JS side word the toast appropriately ("Now" / "in 5 min"
    /// / "in 1 hour").
    minutes_before: i32,
}

/// Scan upcoming events for ones whose VALARM lead time we've
/// just reached, and emit a `talk-join-reminder` event for any
/// that carry a Talk URL.  Called from the background sync
/// loop; cheap because it reads from the local cache only.
async fn check_talk_reminders_inner(app: &AppHandle) -> Result<(), NimbusError> {
    use chrono::Utc;

    let settings = app.state::<SharedSettings>();
    if !settings.read().await.talk_reminder_enabled {
        return Ok(());
    }

    // Build the list of calendars whose events should trigger a
    // reminder: every non-hidden, non-muted calendar across every
    // connected NC account.  Mirrors the visibility the user
    // already chose for the agenda grid; muting a calendar there
    // also silences its Talk reminders.
    let nc_accounts = nextcloud_store::load_accounts().unwrap_or_default();
    let cache = app.state::<Cache>();
    let mut calendar_ids: Vec<String> = Vec::new();
    for acc in &nc_accounts {
        if let Ok(list) = cache.list_calendars(&acc.id) {
            for c in list {
                if !c.hidden && !c.muted {
                    calendar_ids.push(c.id);
                }
            }
        }
    }
    if calendar_ids.is_empty() {
        return Ok(());
    }

    // Window: from now back ~tolerance (so a tick that just
    // crossed the reminder time still catches it) forward 1 day
    // (covers reminders up to "1 day before", which is the
    // largest preset the editor offers).
    let now = Utc::now();
    let tolerance = chrono::Duration::seconds(TALK_REMINDER_FIRE_TOLERANCE_SECS);
    let range_start = now - tolerance;
    let range_end = now + chrono::Duration::days(1) + tolerance;

    let input = match cache.list_events_for_expansion(&calendar_ids, range_start, range_end) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("talk-reminder scan: list_events_for_expansion failed: {e}");
            return Ok(());
        }
    };

    // Re-run the same RRULE expansion the agenda grid uses so
    // the recurring-event case is handled once, here, instead of
    // duplicated.
    let mut overrides_by_master: std::collections::HashMap<&str, Vec<&CalendarEvent>> =
        std::collections::HashMap::new();
    for ov in &input.overrides {
        if let Some(master_id) = ov.id.rsplit_once("::").map(|(prefix, _)| prefix) {
            overrides_by_master.entry(master_id).or_default().push(ov);
        }
    }
    let mut events: Vec<CalendarEvent> = input.singletons;
    for master in &input.masters {
        let ovs = overrides_by_master
            .get(master.id.as_str())
            .cloned()
            .unwrap_or_default();
        events.extend(nimbus_caldav::expand_event(
            master,
            &ovs,
            range_start,
            range_end,
        ));
    }

    let state = app.state::<TalkReminderState>();
    {
        // Prune `fired` entries whose event has already started —
        // keeps the set bounded in long-running sessions and
        // ensures a meeting that recurs daily fires its reminder
        // again on the next occurrence.
        let mut fired = state.fired.lock().expect("talk-reminder fired mutex");
        let active_uids: HashSet<String> = events
            .iter()
            .filter(|e| e.start > now)
            .map(|e| vevent_uid_from_event_id(&e.id))
            .collect();
        fired.retain(|(uid, _)| active_uids.contains(uid));
    }
    let dismissed_snapshot: HashSet<String> = {
        let d = state
            .dismissed
            .lock()
            .expect("talk-reminder dismissed mutex");
        d.clone()
    };

    for ev in &events {
        // Skip past starts — the reminder is moot once the
        // meeting is in progress.  We still keep them in the
        // window above so the prune step has a current picture.
        if ev.start <= now - chrono::Duration::minutes(1) {
            continue;
        }
        let Some(talk_url) = extract_meeting_url(ev) else {
            continue;
        };
        let uid = vevent_uid_from_event_id(&ev.id);
        if dismissed_snapshot.contains(&uid) {
            continue;
        }
        if ev.reminders.is_empty() {
            // No VALARM on the event → user didn't ask for a
            // reminder; respect that even though we *could* nag
            // them about a Talk meeting.
            continue;
        }

        for reminder in &ev.reminders {
            let minutes = reminder.trigger_minutes_before;
            // Negative `minutes_before` means "after start" — out
            // of scope for a join reminder, skip silently.
            if minutes < 0 {
                continue;
            }
            let fire_at = ev.start - chrono::Duration::minutes(minutes as i64);
            // Fire when `now` is in [fire_at, fire_at + tolerance]:
            // we never look earlier than the requested moment, but
            // do allow a tick's worth of catch-up so a slightly
            // late tick still lands.
            let elapsed = (now - fire_at).num_seconds();
            if elapsed < 0 || elapsed > TALK_REMINDER_FIRE_TOLERANCE_SECS {
                continue;
            }

            let key = (uid.clone(), minutes);
            {
                let mut fired = state.fired.lock().expect("talk-reminder fired mutex");
                if fired.contains(&key) {
                    continue;
                }
                fired.insert(key);
            }

            let payload = TalkReminderPayload {
                uid: uid.clone(),
                summary: ev.summary.clone(),
                start: ev.start,
                talk_url: talk_url.clone(),
                minutes_before: minutes,
            };
            if let Err(e) = app.emit("talk-join-reminder", &payload) {
                tracing::warn!("failed to emit talk-join-reminder: {e}");
            } else {
                tracing::info!(
                    "talk-join-reminder fired: uid={} ({} min before)",
                    uid,
                    minutes
                );
            }
        }
    }

    Ok(())
}

/// Recover the bare VEVENT UID from a composite cached id —
/// `{nc_id}::{cal_path}::{uid}` for masters/singletons or
/// `{nc_id}::{cal_path}::{uid}::occ::{epoch}` for expanded
/// occurrences.  The frontend's `dismiss_talk_reminder` and the
/// dedupe set both key off the bare UID so all occurrences of
/// the same series share a single dismiss / fire entry.
fn vevent_uid_from_event_id(id: &str) -> String {
    let parts: Vec<&str> = id.split("::").collect();
    if parts.len() >= 3 {
        parts[2].to_string()
    } else {
        id.to_string()
    }
}

/// Suppress further Talk-join reminders for the given UID until
/// the user reopens the editor or the in-memory state is reset
/// (process restart).  Called from JS when the user clicks
/// through to join early so we don't pester them mid-meeting.
#[tauri::command]
fn dismiss_talk_reminder(
    uid: String,
    state: State<'_, TalkReminderState>,
) -> Result<(), NimbusError> {
    {
        let mut d = state
            .dismissed
            .lock()
            .expect("talk-reminder dismissed mutex");
        d.insert(uid.clone());
    }
    {
        let mut f = state.fired.lock().expect("talk-reminder fired mutex");
        f.retain(|(u, _)| u != &uid);
    }
    Ok(())
}

/// Periodic poll. Re-reads the settings snapshot each tick so the user
/// can toggle sync on/off or change the interval and have it take
/// effect on the next cycle without restarting the loop.
async fn background_sync_loop(app: AppHandle) {
    tracing::info!("background sync loop started");
    loop {
        let (enabled, interval) = {
            let settings = app.state::<SharedSettings>();
            let s = settings.read().await;
            (
                s.background_sync_enabled,
                Duration::from_secs(s.background_sync_interval_secs.max(MIN_SYNC_INTERVAL_SECS)),
            )
        };

        tokio::time::sleep(interval).await;

        if !enabled {
            continue;
        }
        if let Err(e) = check_mail_now_inner(&app).await {
            tracing::warn!("background check_mail_now_inner failed: {e}");
        }
        // Talk-join reminders ride the same tick — the cache is
        // already warm from the mail poll above and the scan is
        // a couple of SQL queries plus an in-memory loop.
        if let Err(e) = check_talk_reminders_inner(&app).await {
            tracing::warn!("background check_talk_reminders_inner failed: {e}");
        }
    }
}

// ── App-settings commands ──────────────────────────────────────

/// Shared cache for the user's installed font families (#142).
/// Populated once at app startup on a blocking thread so the
/// compose toolbar's font picker reads instantly — re-running
/// font-kit's catalogue walk per dropdown open was visibly
/// laggy on machines with hundreds of fonts.
type SystemFontsCache = Arc<RwLock<Vec<String>>>;

/// Walk the OS font catalogue and return the sorted, de-duped
/// family list.  Pure helper — used by both the startup warmer
/// and a manual refresh path.
fn enumerate_system_fonts() -> Vec<String> {
    let source = font_kit::source::SystemSource::new();
    let families = match source.all_families() {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("font enumeration failed: {e}");
            return Vec::new();
        }
    };
    let mut out: Vec<String> = families
        .into_iter()
        .filter(|f| !f.starts_with('.') && !f.trim().is_empty())
        .collect();
    out.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    out.dedup();
    out
}

// ── On-disk font cache (#142 follow-up) ───────────────────────
//
// Even with the in-memory cache, a cold launch still pays the
// cost of font-kit's catalogue walk — slow on Linux's first-run
// fontconfig and visible enough that the user complained about
// "first compose" lag.  Persist the result to a JSON file in the
// OS cache dir, signed with a cheap fingerprint of the system
// font directories.  Subsequent launches read the JSON in
// microseconds; we only re-run font-kit when the fingerprint
// changes (i.e. the user actually installed or removed a font).
//
// The fingerprint is a SHA-256 of every font-directory mtime
// found by recursive walk.  Adding or removing a file inside any
// directory updates that directory's mtime on every common
// filesystem, so directory mtimes alone catch both additions and
// removals without us needing to stat every individual font file.

#[derive(serde::Serialize, serde::Deserialize)]
struct FontCacheFile {
    fingerprint: String,
    fonts: Vec<String>,
}

fn font_cache_path() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("nimbus-mail").join("system_fonts.json"))
}

/// Standard system font directories per OS.  Used for the
/// fingerprint walk; font-kit itself looks at more places, but
/// these cover where additions / removals actually happen.
fn font_search_dirs() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(w) = std::env::var_os("WINDIR") {
            out.push(std::path::PathBuf::from(w).join("Fonts"));
        }
        if let Some(d) = dirs::data_local_dir() {
            out.push(d.join("Microsoft").join("Windows").join("Fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(std::path::PathBuf::from("/System/Library/Fonts"));
        out.push(std::path::PathBuf::from("/Library/Fonts"));
        if let Some(h) = dirs::home_dir() {
            out.push(h.join("Library").join("Fonts"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        out.push(std::path::PathBuf::from("/usr/share/fonts"));
        out.push(std::path::PathBuf::from("/usr/local/share/fonts"));
        if let Some(h) = dirs::home_dir() {
            out.push(h.join(".fonts"));
            out.push(h.join(".local/share/fonts"));
        }
    }
    out
}

fn collect_dir_mtimes(dir: &std::path::Path, out: &mut Vec<(String, u64)>) {
    let Ok(meta) = std::fs::metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    out.push((dir.to_string_lossy().into_owned(), mtime));
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        if let Ok(m) = entry.metadata()
            && m.is_dir()
        {
            collect_dir_mtimes(&entry.path(), out);
        }
    }
}

fn compute_font_fingerprint() -> String {
    use sha2::{Digest, Sha256};
    let mut pairs: Vec<(String, u64)> = Vec::new();
    for d in font_search_dirs() {
        collect_dir_mtimes(&d, &mut pairs);
    }
    pairs.sort();
    let mut hasher = Sha256::new();
    for (p, m) in &pairs {
        hasher.update(p.as_bytes());
        hasher.update(b"|");
        hasher.update(m.to_string().as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

fn load_font_cache_file() -> Option<FontCacheFile> {
    let path = font_cache_path()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_font_cache_file(file: &FontCacheFile) {
    let Some(path) = font_cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(file) {
        let _ = std::fs::write(&path, bytes);
    }
}

// ── FIDO unlock (#164, Phase 1A) ──────────────────────────────
//
// These commands manage the wraps inside the keychain envelope.
// They don't yet replace the plain-mode startup path — registering
// keys is observable via the Settings UI, and the unlock-at-boot
// flow lands as a separate phase once the wrap/unwrap loop is
// hardware-verified.

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FidoCredentialView {
    /// `"fido_prf"` or `"passphrase"`.
    kind: String,
    credential_id: String,
    label: String,
    salt: String,
    created_at: i64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FidoStatusView {
    /// Always Some in plain / hybrid mode, None once the keychain
    /// is in FIDO-only mode (Phase 1B+).
    has_plain_key: bool,
    /// How many credentials the user has registered.
    credentials: Vec<FidoCredentialView>,
}

/// Snapshot of the keychain envelope.  Used by Settings to render
/// the "Hardware authentication" panel and (later) by the boot
/// path to decide whether to require an unlock before opening the
/// cache.
#[tauri::command]
fn fido_status() -> Result<FidoStatusView, NimbusError> {
    let env = nimbus_store::cache::key::load_envelope()?;
    Ok(FidoStatusView {
        has_plain_key: env.plain_key.is_some(),
        credentials: env
            .wraps
            .into_iter()
            .map(|w| FidoCredentialView {
                kind: match w.kind {
                    nimbus_store::fido::WrapKind::FidoPrf => "fido_prf".to_string(),
                    nimbus_store::fido::WrapKind::Passphrase => "passphrase".to_string(),
                },
                credential_id: w.credential_id,
                label: w.label,
                salt: w.salt,
                created_at: w.created_at,
            })
            .collect(),
    })
}

/// Generate a fresh PRF salt for a new enrollment.  The frontend
/// supplies it as the `prf.eval.first` input to `navigator.
/// credentials.create` so the authenticator returns the matching
/// PRF output.
#[tauri::command]
fn fido_generate_salt() -> Result<String, NimbusError> {
    let salt = nimbus_store::fido::generate_salt()?;
    Ok(nimbus_store::fido::encode_b64(&salt))
}

/// Wrap the current master key under a freshly-registered FIDO
/// credential's PRF output.  Frontend has already called
/// WebAuthn `credentials.create` with the salt from
/// `fido_generate_salt`, received the credential id and the PRF
/// bytes back, and forwards them here for storage.
#[tauri::command]
fn fido_enroll(
    credential_id_b64: String,
    salt_b64: String,
    prf_output_b64: String,
    label: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    use nimbus_store::fido;
    let env = nimbus_store::cache::key::load_envelope()?;
    // Same fallback as `fido_enroll_passphrase`: prefer the
    // envelope's plain key, fall back to the in-memory copy
    // when FIDO-only mode has cleared plain_key.
    let plain_hex = match env.plain_key.as_deref() {
        Some(hex) => hex.to_string(),
        None => cache.master_key_hex().ok_or_else(|| {
            NimbusError::Auth(
                "Cannot enroll a credential while the database is locked — unlock first".into(),
            )
        })?,
    };
    let master_key = hex::decode(&plain_hex)
        .map_err(|e| NimbusError::Storage(format!("master key hex decode: {e}")))?;
    let credential_id = fido::decode_b64(&credential_id_b64)?;
    let salt = fido::decode_b64(&salt_b64)?;
    let prf_output = fido::decode_b64(&prf_output_b64)?;
    let wrap = fido::wrap_master_key(
        fido::WrapKind::FidoPrf,
        &master_key,
        &prf_output,
        &credential_id,
        &salt,
        label,
    )?;
    nimbus_store::cache::key::add_wrap(wrap)?;
    Ok(())
}

/// Wrap the current master key under a passphrase-derived AES key
/// (PBKDF2-HMAC-SHA-256, 720 000 iters).  Doubles as recovery
/// passphrase for Phase 1B and as the test path on platforms
/// where WebAuthn PRF isn't reachable yet (Linux WebKitGTK <
/// 2.46).  Salt + synthetic credential id are server-side
/// generated so the frontend never produces them.
#[tauri::command]
fn fido_enroll_passphrase(
    passphrase: String,
    label: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    use nimbus_store::fido::{self, WrapKind};
    if passphrase.trim().is_empty() {
        return Err(NimbusError::Other("passphrase must not be empty".into()));
    }
    let mut env = nimbus_store::cache::key::load_envelope()?;
    // Prefer the keychain envelope's plain key (pre-FIDO-only),
    // fall back to the in-memory copy that `unlock_with_*` stashes
    // on the Cache.  The fallback is what makes "Change passphrase"
    // work after the user has flipped Key Encryption on — by that
    // point envelope.plain_key is None and we'd otherwise refuse.
    let plain_hex = match env.plain_key.as_deref() {
        Some(hex) => hex.to_string(),
        None => cache.master_key_hex().ok_or_else(|| {
            NimbusError::Auth(
                "Cannot enroll a passphrase while the database is locked — unlock first".into(),
            )
        })?,
    };
    let master_key = hex::decode(&plain_hex)
        .map_err(|e| NimbusError::Storage(format!("master key hex decode: {e}")))?;
    let salt = fido::generate_salt()?;
    let id = fido::generate_passphrase_id()?;
    let aes_key = fido::derive_passphrase_key(&passphrase, &salt)?;
    let wrap = fido::wrap_master_key(
        WrapKind::Passphrase,
        &master_key,
        &aes_key,
        &id,
        &salt,
        label,
    )?;
    // Single-passphrase invariant: the recovery passphrase is a
    // role, not a per-device entry.  Drop any existing passphrase
    // wrap before adding the new one so re-enrolling cleanly
    // replaces the old one (and so add_wrap's credential-id
    // dedup never lets two passphrase wraps coexist with
    // different ids).
    env.wraps.retain(|w| w.kind != WrapKind::Passphrase);
    env.wraps.push(wrap);
    nimbus_store::cache::key::save_envelope(&env)?;
    Ok(())
}

/// Test-only: verify a passphrase wraps unlock the master key.
/// Phase 1B will call this from the lock screen when the user
/// chooses passphrase unlock; today it lets users sanity-check
/// their passphrase entry on Linux without restructuring boot.
/// Returns `true` on success, `false` on a wrong passphrase /
/// no matching wrap, error on storage / crypto failure.
#[tauri::command]
fn fido_verify_passphrase(passphrase: String) -> Result<bool, NimbusError> {
    use nimbus_store::fido::{self, WrapKind};
    let env = nimbus_store::cache::key::load_envelope()?;
    for wrap in &env.wraps {
        if wrap.kind != WrapKind::Passphrase {
            continue;
        }
        let salt = fido::decode_b64(&wrap.salt)?;
        let aes_key = fido::derive_passphrase_key(&passphrase, &salt)?;
        if fido::unwrap_master_key(wrap, &aes_key).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Mirror of `fido_verify_passphrase` for FIDO PRF wraps.  The
/// frontend has already run WebAuthn `credentials.get` against
/// the credential's stored salt and forwards the PRF output
/// here.  Phase 1B's lock screen will use this; today it lets
/// you sanity-check that a registered hardware key still works.
#[tauri::command]
fn fido_verify_prf(
    credential_id_b64: String,
    prf_output_b64: String,
) -> Result<bool, NimbusError> {
    use nimbus_store::fido::{self, WrapKind};
    let env = nimbus_store::cache::key::load_envelope()?;
    let prf = fido::decode_b64(&prf_output_b64)?;
    for wrap in &env.wraps {
        if wrap.kind != WrapKind::FidoPrf {
            continue;
        }
        if wrap.credential_id != credential_id_b64 {
            continue;
        }
        if fido::unwrap_master_key(wrap, &prf).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove a registered credential.  Refuses to drop the last wrap
/// when the keychain is in FIDO-only mode (would orphan the
/// encrypted DB).
#[tauri::command]
fn fido_remove(credential_id_b64: String) -> Result<(), NimbusError> {
    let env = nimbus_store::cache::key::load_envelope()?;
    if env.plain_key.is_none() && env.wraps.len() <= 1 {
        return Err(NimbusError::Other(
            "Cannot remove the last hardware key while FIDO-only mode is active".into(),
        ));
    }
    nimbus_store::cache::key::remove_wrap(&credential_id_b64)?;
    Ok(())
}

// ── Database lock + unlock (#164 Phase 1B) ────────────────────

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DatabaseStatusView {
    /// True when no plain key is in the envelope and the cache
    /// pool isn't open yet — the lock screen should be shown.
    locked: bool,
    /// True when the keychain envelope has zero registered methods
    /// and zero plain key — the user has wiped everything;
    /// app needs to recreate from scratch.
    needs_setup: bool,
    /// One entry per registered unlock method (FIDO PRF or
    /// passphrase), used by the lock screen to render a picker.
    methods: Vec<FidoCredentialView>,
    /// Remaining unlock attempts before wipe-on-failure fires.
    /// `None` when the policy is off or has no limit set —
    /// the lock screen renders "X tries remaining" only when this
    /// is `Some(_)`.
    attempts_remaining: Option<u32>,
}

/// Snapshot used by `App.svelte` on mount to decide whether to
/// route the user to the lock screen or straight into the inbox.
#[tauri::command]
fn database_status(cache: State<'_, Cache>) -> Result<DatabaseStatusView, NimbusError> {
    let env = nimbus_store::cache::key::load_envelope()?;
    let locked = cache.is_locked();
    let attempts_remaining = match (env.wipe_on_failure, env.max_unlock_attempts) {
        (true, Some(max)) if max > 0 => Some(max.saturating_sub(env.failed_attempts)),
        _ => None,
    };
    Ok(DatabaseStatusView {
        locked,
        needs_setup: env.plain_key.is_none() && env.wraps.is_empty(),
        methods: env
            .wraps
            .into_iter()
            .map(|w| FidoCredentialView {
                kind: match w.kind {
                    nimbus_store::fido::WrapKind::FidoPrf => "fido_prf".to_string(),
                    nimbus_store::fido::WrapKind::Passphrase => "passphrase".to_string(),
                },
                credential_id: w.credential_id,
                label: w.label,
                salt: w.salt,
                created_at: w.created_at,
            })
            .collect(),
        attempts_remaining,
    })
}

/// Wipe the cache file and clear the keychain envelope.
/// Triggered when the user exhausts their unlock budget OR
/// when the envelope's integrity MAC fails.
fn perform_wipe(cache: &Cache) {
    if let Err(e) = cache.wipe_on_disk() {
        tracing::error!("wipe_on_disk failed: {e}");
    }
    let cleared = nimbus_store::fido::KeychainEnvelope {
        version: 1,
        plain_key: None,
        wraps: Vec::new(),
        wipe_on_failure: false,
        max_unlock_attempts: None,
        failed_attempts: 0,
        integrity_mac: None,
    };
    if let Err(e) = nimbus_store::cache::key::save_envelope(&cleared) {
        tracing::error!("clearing envelope after wipe failed: {e}");
    }
}

/// Bump the persisted failure counter and, if the user has
/// opted into the wipe-on-failure policy, blow away the cache
/// once the configured retry budget is exhausted.  The counter
/// lives in the keychain envelope (not just process memory) so
/// kill+relaunch can't reset the budget.  An invalid envelope
/// MAC trips the wipe immediately on the next failure regardless
/// of where the persisted counter sat.
fn note_unlock_failure(cache: &Cache, label: &str) -> NimbusError {
    let mut env = match nimbus_store::cache::key::load_envelope() {
        Ok(e) => e,
        Err(e) => return e,
    };
    let tampered = nimbus_store::cache::key::envelope_tampered(&env);
    if tampered {
        tracing::warn!(
            "Keychain envelope MAC mismatch — treating this attempt as terminal."
        );
    }
    env.failed_attempts = env.failed_attempts.saturating_add(1);
    let attempts = env.failed_attempts;
    if let Err(e) = nimbus_store::cache::key::save_envelope(&env) {
        tracing::warn!("could not persist failure counter: {e}");
    }
    if env.wipe_on_failure || tampered {
        let max = env.max_unlock_attempts.unwrap_or(0);
        let trip = tampered || (max > 0 && attempts >= max);
        if trip {
            if tampered {
                tracing::warn!("Wipe fired due to envelope tampering.");
            } else {
                tracing::warn!(
                    "Wipe-on-failure policy fired: {attempts} consecutive failed unlock attempts (limit {max})."
                );
            }
            perform_wipe(cache);
            return NimbusError::Auth(if tampered {
                "Keychain envelope was modified outside Nimbus. The encrypted cache has been wiped.".to_string()
            } else {
                format!(
                    "Too many failed attempts ({attempts}/{max}). The encrypted cache has been wiped."
                )
            });
        }
    }
    NimbusError::Auth(format!("incorrect {label}"))
}

/// Reset the persisted failure counter on a successful unlock.
fn note_unlock_success() {
    let Ok(mut env) = nimbus_store::cache::key::load_envelope() else {
        return;
    };
    if env.failed_attempts == 0 {
        return;
    }
    env.failed_attempts = 0;
    if let Err(e) = nimbus_store::cache::key::save_envelope(&env) {
        tracing::warn!("could not reset failure counter: {e}");
    }
}

/// Unlock the cache from a passphrase.  Tries every passphrase
/// wrap in the envelope, returns the first match.
#[tauri::command]
fn unlock_with_passphrase(
    passphrase: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    use nimbus_store::fido::{self, WrapKind};
    let env = nimbus_store::cache::key::load_envelope()?;
    for wrap in &env.wraps {
        if wrap.kind != WrapKind::Passphrase {
            continue;
        }
        let salt = fido::decode_b64(&wrap.salt)?;
        let aes_key = fido::derive_passphrase_key(&passphrase, &salt)?;
        if let Ok(master) = fido::unwrap_master_key(wrap, &aes_key) {
            let hex = hex::encode(&master);
            cache.unlock_with_master_key(hex).map_err(NimbusError::from)?;
            note_unlock_success();
            return Ok(());
        }
    }
    Err(note_unlock_failure(&cache, "passphrase"))
}

/// Unlock the cache from a FIDO PRF assertion.  Frontend has
/// already run WebAuthn `credentials.get` against the
/// credential's stored salt and forwards the resulting PRF
/// output here.
#[tauri::command]
fn unlock_with_prf(
    credential_id_b64: String,
    prf_output_b64: String,
    cache: State<'_, Cache>,
) -> Result<(), NimbusError> {
    use nimbus_store::fido::{self, WrapKind};
    let env = nimbus_store::cache::key::load_envelope()?;
    let prf = fido::decode_b64(&prf_output_b64)?;
    for wrap in &env.wraps {
        if wrap.kind != WrapKind::FidoPrf || wrap.credential_id != credential_id_b64 {
            continue;
        }
        let master = match fido::unwrap_master_key(wrap, &prf) {
            Ok(m) => m,
            Err(_) => return Err(note_unlock_failure(&cache, "hardware key PRF output")),
        };
        let hex = hex::encode(&master);
        cache.unlock_with_master_key(hex).map_err(NimbusError::from)?;
        note_unlock_success();
        return Ok(());
    }
    Err(NimbusError::Auth(
        "no registered hardware key matches that credential".into(),
    ))
}

/// Switch the cache into FIDO-only mode: drop the plain master
/// key from the keychain envelope so future cold launches MUST
/// authenticate with one of the registered methods.  Refuses
/// unless the user has at least one passphrase OR ≥ 2 hardware
/// keys registered — without a recovery option we'd lock them
/// out permanently the first time a YubiKey gets lost.
#[tauri::command]
fn enable_fido_only_mode() -> Result<(), NimbusError> {
    use nimbus_store::fido::WrapKind;
    let mut env = nimbus_store::cache::key::load_envelope()?;
    if env.plain_key.is_none() {
        return Ok(()); // already FIDO-only — idempotent.
    }
    let passphrase_count = env
        .wraps
        .iter()
        .filter(|w| w.kind == WrapKind::Passphrase)
        .count();
    let fido_count = env
        .wraps
        .iter()
        .filter(|w| w.kind == WrapKind::FidoPrf)
        .count();
    if passphrase_count == 0 && fido_count < 2 {
        return Err(NimbusError::Other(
            "Register at least one passphrase OR two hardware keys before enabling FIDO-only mode \
             — otherwise losing a single key would lock the cache permanently."
                .into(),
        ));
    }
    env.plain_key = None;
    nimbus_store::cache::key::save_envelope(&env)?;
    Ok(())
}

/// Snapshot of the wipe-on-failure policy stored in the
/// keychain envelope.  `enabled = false` means unlimited
/// retries.  `max_attempts = None` means the same — the toggle
/// can be on but with no number set; we treat that as
/// effectively off until a number is provided.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WipePolicyView {
    enabled: bool,
    max_attempts: Option<u32>,
}

#[tauri::command]
fn get_wipe_policy() -> Result<WipePolicyView, NimbusError> {
    let env = nimbus_store::cache::key::load_envelope()?;
    Ok(WipePolicyView {
        enabled: env.wipe_on_failure,
        max_attempts: env.max_unlock_attempts,
    })
}

#[tauri::command]
fn set_wipe_policy(policy: WipePolicyView) -> Result<(), NimbusError> {
    let mut env = nimbus_store::cache::key::load_envelope()?;
    env.wipe_on_failure = policy.enabled;
    env.max_unlock_attempts = if policy.enabled {
        policy.max_attempts.filter(|n| *n > 0)
    } else {
        None
    };
    nimbus_store::cache::key::save_envelope(&env)?;
    Ok(())
}

/// Reverse of `enable_fido_only_mode` — re-store the plain
/// master key in the envelope so the next launch opens the
/// cache without prompting.  Only callable while the cache is
/// already unlocked (we need the in-memory key).
#[tauri::command]
fn disable_fido_only_mode(cache: State<'_, Cache>) -> Result<(), NimbusError> {
    if cache.is_locked() {
        return Err(NimbusError::Auth(
            "Database must be unlocked before FIDO-only mode can be disabled".into(),
        ));
    }
    let key_hex = cache.master_key_hex().ok_or_else(|| {
        NimbusError::Auth(
            "Master key isn't available in memory — unlock the database again before disabling key encryption".into(),
        )
    })?;
    let mut env = nimbus_store::cache::key::load_envelope()?;
    if env.plain_key.is_some() {
        return Ok(()); // already plain — idempotent.
    }
    env.plain_key = Some(key_hex);
    nimbus_store::cache::key::save_envelope(&env)?;
    Ok(())
}

/// Return the cached font list to the frontend.  Reads from
/// the shared `SystemFontsCache` populated at startup; if the
/// cache is somehow empty (startup warmer failed or hasn't run
/// yet), runs the enumeration once on a blocking thread and
/// memoises the result before returning.
#[tauri::command]
async fn list_system_fonts(
    cache: State<'_, SystemFontsCache>,
) -> Result<Vec<String>, NimbusError> {
    {
        let snap = cache.read().await;
        if !snap.is_empty() {
            return Ok(snap.clone());
        }
    }
    // Cold path: warm the cache synchronously this once.
    let fonts = tokio::task::spawn_blocking(enumerate_system_fonts)
        .await
        .map_err(|e| NimbusError::Other(format!("font enumeration join: {e}")))?;
    *cache.write().await = fonts.clone();
    Ok(fonts)
}

#[tauri::command]
async fn get_app_settings(settings: State<'_, SharedSettings>) -> Result<AppSettings, NimbusError> {
    Ok(settings.read().await.clone())
}

#[tauri::command]
async fn update_app_settings(
    new_settings: AppSettings,
    settings: State<'_, SharedSettings>,
) -> Result<(), NimbusError> {
    app_settings::save_settings(&new_settings)?;
    *settings.write().await = new_settings;
    Ok(())
}

#[tauri::command]
async fn check_mail_now(app: AppHandle) -> Result<(), NimbusError> {
    check_mail_now_inner(&app).await
}

// ── Custom themes (#132 tier 2) ────────────────────────────────
//
// User picks a Skeleton-shape CSS file in the Settings → Design
// "Import theme…" flow.  The frontend hands us the file's
// absolute path; we copy the bytes under
// `<config>/nimbus-mail/themes/<id>.css`, parse out the
// `[data-theme="…"]` slug to use as the picker id, and append a
// `CustomTheme` record to AppSettings.
//
// Removal deletes both the on-disk copy and the AppSettings row;
// the frontend's theme picker rebuilds from `get_app_settings`
// after each operation, so no extra plumbing.

/// Resolve the user-themes directory under the app's config root.
/// Created on demand — first import is what creates the folder.
fn custom_themes_dir() -> Result<std::path::PathBuf, NimbusError> {
    let base = dirs::config_dir()
        .ok_or_else(|| NimbusError::Other("cannot resolve user config dir".into()))?;
    let dir = base.join("nimbus-mail").join("themes");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(NimbusError::Other(format!(
            "create themes dir {}: {e}",
            dir.display()
        )));
    }
    Ok(dir)
}

/// Pull the theme slug out of an imported CSS file by scanning
/// for the first `[data-theme="…"]` selector.  Falls back to the
/// file stem when the file doesn't follow Skeleton's convention,
/// so the user still gets *something* in the picker — just won't
/// switch unless they edit the CSS to match the slug.
fn extract_theme_slug(css: &str, fallback: &str) -> String {
    let needle = "[data-theme=";
    if let Some(idx) = css.find(needle) {
        let tail = &css[idx + needle.len()..];
        // Accept both `"foo"` and `'foo'` quoting, tolerate
        // intra-attribute whitespace.
        let trimmed = tail.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix('"')
            .or_else(|| trimmed.strip_prefix('\''))
        {
            if let Some(end) = rest.find(['"', '\'']) {
                let slug = rest[..end].trim();
                if !slug.is_empty() {
                    return slug.to_string();
                }
            }
        }
    }
    fallback.to_string()
}

/// Copy a user-picked CSS file into the app's themes directory
/// and append a `CustomTheme` record to AppSettings.  Returns the
/// freshly-created record so the frontend can register the
/// runtime stylesheet without re-reading settings.
///
/// Soft-fails on a duplicate slug by overwriting the previous
/// import — that's the natural "I edited the same file and want
/// to re-import" flow, and avoids forcing the user to remove the
/// old row first.
#[tauri::command]
async fn import_custom_theme(
    app: AppHandle,
    source_path: String,
    label: Option<String>,
    settings: State<'_, SharedSettings>,
) -> Result<CustomTheme, NimbusError> {
    let src = std::path::PathBuf::from(&source_path);
    if !src.exists() {
        return Err(NimbusError::Other(format!(
            "theme source not found: {source_path}"
        )));
    }
    let css = std::fs::read_to_string(&src)
        .map_err(|e| NimbusError::Other(format!("read theme source: {e}")))?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("custom-theme")
        .to_string();
    let slug = extract_theme_slug(&css, &stem);
    let dir = custom_themes_dir()?;
    let dest = dir.join(format!("{slug}.css"));
    std::fs::write(&dest, &css).map_err(|e| NimbusError::Other(format!("copy theme file: {e}")))?;

    let record = CustomTheme {
        id: slug.clone(),
        label: label.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
            // Title-case the slug so "my_theme" reads "My theme"
            // rather than something the user has to fix manually.
            stem.replace(['_', '-'], " ")
        }),
        description: "Imported theme".to_string(),
        path: dest.to_string_lossy().to_string(),
    };

    {
        let mut s = settings.write().await;
        // Replace any existing row with the same id (re-import).
        s.custom_themes.retain(|t| t.id != record.id);
        s.custom_themes.push(record.clone());
        app_settings::save_settings(&s)?;
    }

    // Tell every window so a second-window picker stays in sync.
    if let Err(e) = app.emit("custom-themes-changed", ()) {
        tracing::warn!("emit custom-themes-changed failed: {e}");
    }
    Ok(record)
}

/// Remove a user-imported theme — drops both the on-disk CSS and
/// the AppSettings row.  No-op when the id isn't found so the UI
/// can fire-and-forget without checking first.
#[tauri::command]
async fn remove_custom_theme(
    app: AppHandle,
    id: String,
    settings: State<'_, SharedSettings>,
) -> Result<(), NimbusError> {
    let path: Option<String> = {
        let mut s = settings.write().await;
        let path = s
            .custom_themes
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.path.clone());
        s.custom_themes.retain(|t| t.id != id);
        // If the removed theme was the active one, drop back to
        // the default so the UI doesn't try to render a missing
        // file on next launch.
        if s.theme_name == id {
            s.theme_name = "cerberus".into();
        }
        app_settings::save_settings(&s)?;
        path
    };
    if let Some(p) = path
        && let Err(e) = std::fs::remove_file(&p)
    {
        tracing::warn!("remove theme file {p}: {e}");
    }
    if let Err(e) = app.emit("custom-themes-changed", ()) {
        tracing::warn!("emit custom-themes-changed failed: {e}");
    }
    Ok(())
}

#[tauri::command]
fn get_total_unread(cache: State<'_, Cache>) -> Result<u32, NimbusError> {
    cache.total_unread_count().map_err(Into::into)
}

#[tauri::command]
fn show_main_window_cmd(app: AppHandle) -> Result<(), NimbusError> {
    show_main_window(&app)
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ── App entry point ─────────────────────────────────────────────

fn main() {
    tracing_subscriber::fmt::init();

    // Open (and migrate) the local mail cache once at startup, then
    // hand it to Tauri as managed state so every command can borrow it.
    // A failure here is fatal: without the cache the write-through path
    // is broken, and the user would silently lose offline capability.
    let cache = Cache::open_default().expect("failed to open local mail cache");

    // Scrub orphan cache rows left behind by removed accounts.
    // `cache.wipe_account(...)` runs on account removal, but if it ever
    // missed (crash, disk error, older build before the wipe landed)
    // the unified inbox would surface envelopes whose owning account
    // no longer exists — every click on one throws "no account with
    // id 'X'". Running the scrub on boot guarantees the shell never
    // paints an orphan past the first frame, regardless of how the
    // cache got into that state.
    match account_store::load_accounts(&cache) {
        Ok(accounts) => {
            let active_ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
            if let Err(e) = cache.prune_orphan_accounts(&active_ids) {
                tracing::warn!("startup orphan-account prune failed: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("skipping startup orphan-account prune — load_accounts failed: {e}")
        }
    }

    // App-wide preferences (Issue #16). A missing file is fine on first
    // run — `load_settings` returns defaults. We wrap in Arc<RwLock<..>>
    // so the background sync loop can re-snapshot per tick while the
    // `update_app_settings` command swaps in a fresh value under the
    // write lock.
    let settings = app_settings::load_settings().unwrap_or_default();
    let shared_settings: SharedSettings = Arc::new(RwLock::new(settings));

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        // #131 follow-up: cross-platform "launch on login".
        // The plugin registers an XDG autostart entry on Linux,
        // a LaunchAgent plist on macOS, and an HKCU\…\Run
        // registry value on Windows.  No launcher args — the
        // binary's default entry point is what we want
        // autostarted.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(cache)
        .manage(shared_settings)
        .manage::<SystemFontsCache>(Arc::new(RwLock::new(Vec::new())))
        .register_uri_scheme_protocol("contact-photo", contact_photo_protocol)
        .setup(|app| {
            // Windows toast attribution.  Without an explicit
            // AppUserModelID the OS falls back to the launching
            // process's AUMID — for `cargo tauri dev` that's the
            // shell (PowerShell, cmd, Git Bash), which is what
            // appears as the toast's source.  Setting our own AUMID
            // here makes notifications attribute to "Nimbus Mail"
            // in both dev and bundled builds.  The display-name +
            // icon come from a Start-Menu shortcut the installer
            // registers with this same AUMID; in dev the toast
            // shows the AUMID itself, which is still better than
            // "PowerShell".
            #[cfg(windows)]
            set_app_user_model_id();

            // Drop the app icon onto disk once and stash its path
            // in managed state so the JS layer can pass it to
            // `sendNotification`.  Without this, libnotify on Linux
            // (and macOS' NSUserNotification) fall back to a
            // generic icon next to the toast in dev builds.
            // Always manage the state, even on failure, so commands
            // taking `State<'_, NotificationIconPath>` always extract
            // (an empty path signals "no icon known").
            let icon_path = install_notification_icon()
                .inspect_err(|e| tracing::warn!("install_notification_icon failed: {e}"))
                .unwrap_or_default();
            app.manage(NotificationIconPath(icon_path));
            // Talk-join reminder state — empty fired/dismissed
            // sets at startup, populated as the background scan
            // discovers upcoming events with VALARM triggers.
            app.manage(TalkReminderState::default());

            // Warm the system-fonts cache off the main thread so
            // the first compose-toolbar font-dropdown open is
            // instant.  Two-tier strategy:
            //
            //   1. Compute a cheap fingerprint of the system font
            //      directories (recursive dir-mtime hash).
            //   2. If a JSON cache exists at the same fingerprint,
            //      load it — saves font-kit's catalogue walk
            //      entirely on every launch where the user hasn't
            //      installed or removed a font.
            //   3. Otherwise run font-kit fresh and persist the
            //      result so the next launch hits the fast path.
            //
            // Runs on a plain OS thread because Tauri's setup
            // callback fires before the async runtime is mounted
            // — calling tokio here would panic with "no reactor
            // running".  We park on the tokio RwLock via
            // `blocking_write`; the lock is uncontended at startup
            // so this is effectively immediate.
            let fonts_cache = app.state::<SystemFontsCache>().inner().clone();
            std::thread::spawn(move || {
                let fingerprint = compute_font_fingerprint();
                if let Some(disk) = load_font_cache_file()
                    && disk.fingerprint == fingerprint
                    && !disk.fonts.is_empty()
                {
                    let count = disk.fonts.len();
                    *fonts_cache.blocking_write() = disk.fonts;
                    tracing::info!("system fonts loaded from disk cache: {count} families");
                    return;
                }
                let list = enumerate_system_fonts();
                let count = list.len();
                save_font_cache_file(&FontCacheFile {
                    fingerprint,
                    fonts: list.clone(),
                });
                *fonts_cache.blocking_write() = list;
                tracing::info!("system fonts enumerated fresh: {count} families");
            });

            // ── Tray menu + icon ────────────────────────────────
            //
            // Built inside `setup` (not a command) so we have `&mut App`
            // and can register the tray lifecycle against the Tauri
            // event loop directly.
            let handle = app.handle().clone();
            let menu = Menu::with_items(
                &handle,
                &[
                    &MenuItem::with_id(&handle, "open", "Open Nimbus", true, None::<&str>)?,
                    &MenuItem::with_id(&handle, "check", "Check Mail Now", true, None::<&str>)?,
                    &MenuItem::with_id(&handle, "compose", "Compose", true, None::<&str>)?,
                    &PredefinedMenuItem::separator(&handle)?,
                    &MenuItem::with_id(&handle, "quit", "Quit Nimbus", true, None::<&str>)?,
                ],
            )?;

            let tray_icon = load_tray_icon(&handle)?;

            // Snapshot the base icon's raw RGBA so the badge renderer
            // can re-composite without re-reading the on-disk PNG on
            // every unread-count change. Stored in managed state.
            let base = TrayBaseIcon {
                rgba: tray_icon.rgba().to_vec(),
                width: tray_icon.width(),
                height: tray_icon.height(),
            };
            app.manage(base);

            let _tray = TrayIconBuilder::with_id("nimbus-main")
                .icon(tray_icon)
                .tooltip("Nimbus Mail")
                .menu(&menu)
                // Windows: without this, left-click auto-pops the menu
                // and our click-handler never fires. We want left-click
                // to show the window, right-click to show the menu.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => {
                        if let Err(e) = show_main_window(app) {
                            tracing::warn!("tray open failed: {e}");
                        }
                    }
                    "check" => {
                        let h = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = check_mail_now_inner(&h).await {
                                tracing::warn!("tray check_mail_now failed: {e}");
                            }
                        });
                    }
                    "compose" => {
                        if let Err(e) = show_main_window(app) {
                            tracing::warn!("tray compose open failed: {e}");
                        }
                        if let Err(e) = app.emit("open-compose", ()) {
                            tracing::warn!("failed to emit open-compose: {e}");
                        }
                    }
                    "quit" => app.exit(0),
                    other => tracing::debug!("unknown tray menu id: {other}"),
                })
                .on_tray_icon_event(|tray, event| {
                    // Single left-click (button up) opens the window.
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                        && let Err(e) = show_main_window(tray.app_handle())
                    {
                        tracing::warn!("tray left-click show failed: {e}");
                    }
                })
                .build(app)?;

            // ── Close-to-tray wiring ────────────────────────────
            //
            // We clone the settings Arc out of managed state so the
            // window-event closure (which is `Fn`, not `FnMut`, and
            // not async) can consult the current preference on every
            // close attempt. `blocking_read` is safe here: the window
            // event thread is already off the async runtime.
            if let Some(main_window) = app.get_webview_window("main") {
                let settings_for_close: SharedSettings =
                    app.state::<SharedSettings>().inner().clone();
                let close_window = main_window.clone();
                main_window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let should_hide = settings_for_close.blocking_read().minimize_to_tray;
                        if should_hide {
                            api.prevent_close();
                            let _ = close_window.hide();
                        }
                    }
                });

                // Honour `start_minimized`: hide the window right away
                // so the app boots straight into the tray.
                let should_hide_on_start = app
                    .state::<SharedSettings>()
                    .inner()
                    .blocking_read()
                    .start_minimized;
                if should_hide_on_start {
                    let _ = main_window.hide();
                }
            } else {
                tracing::warn!("main window not found at setup time");
            }

            // Paint the initial badge from whatever's already in the
            // cache so the tray + taskbar reflect unread count from
            // the moment the app finishes booting (not only after the
            // first sync tick).
            refresh_unread_badge(app.handle());

            // ── Background sync ─────────────────────────────────
            //
            // `tauri::async_runtime::spawn` uses Tauri's managed
            // runtime, which is guaranteed to exist regardless of
            // how the app was started.
            let bg_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                background_sync_loop(bg_handle).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_notification_icon_path,
            send_native_notification,
            get_accounts,
            add_account,
            remove_account,
            update_account,
            set_account_password,
            set_folder_icon,
            discover_account_settings,
            probe_server_certificate,
            test_connection,
            fetch_envelopes,
            fetch_unified_envelopes,
            fetch_message,
            download_email_attachment,
            put_attachment_preview,
            get_attachment_previews,
            download_calendar_from_message,
            fetch_folders,
            create_folder,
            delete_folder,
            rename_folder,
            mark_as_read,
            set_message_read,
            send_email,
            save_draft,
            delete_message,
            archive_message,
            move_message,
            move_messages,
            get_cached_envelopes,
            get_unified_cached_envelopes,
            get_cached_message,
            get_cached_folders,
            test_jmap_connection,
            detect_jmap,
            search_emails,
            search_imap_server,
            start_nextcloud_login,
            poll_nextcloud_login,
            get_nextcloud_accounts,
            refresh_nextcloud_capabilities,
            get_nextcloud_user_email,
            remove_nextcloud_account,
            open_url,
            list_nextcloud_files,
            download_nextcloud_file,
            nextcloud_file_preview,
            create_nextcloud_share,
            update_nextcloud_share_label,
            create_nextcloud_directory,
            list_talk_rooms,
            create_talk_room,
            set_talk_room_public,
            find_nextcloud_user_by_email,
            add_talk_participants,
            delete_talk_room,
            add_talk_participant,
            rename_talk_room,
            list_nextcloud_notes,
            get_nextcloud_note,
            create_nextcloud_note,
            update_nextcloud_note,
            delete_nextcloud_note,
            upload_to_nextcloud,
            office_open_attachment,
            office_close_attachment,
            office_sweep_temp,
            pdf_open_attachment,
            pdf_close_attachment,
            print_attachment,
            save_bytes_to_path,
            sync_nextcloud_contacts,
            get_contacts_sync_status,
            get_calendars_sync_status,
            get_contacts,
            search_contacts,
            get_contact_photo,
            create_contact,
            update_contact,
            delete_contact,
            list_contact_groups,
            create_contact_group,
            update_contact_group,
            delete_contact_group,
            set_contact_group_hidden,
            set_contact_group_emoji,
            list_nextcloud_groups,
            list_contact_categories,
            set_category_use_as_mailing_list,
            add_contact_to_category,
            remove_contact_from_category,
            rename_contact_category,
            delete_contact_category,
            list_mailing_lists,
            set_mailing_list_hidden,
            set_mailing_list_emoji,
            rename_mailing_list,
            list_nextcloud_addressbooks,
            list_nextcloud_calendars,
            sync_nextcloud_calendars,
            get_cached_calendars,
            create_nextcloud_calendar,
            update_nextcloud_calendar,
            delete_nextcloud_calendar,
            set_nextcloud_calendar_hidden,
            set_nextcloud_calendar_muted,
            get_cached_events,
            create_calendar_event,
            parse_event_invite,
            respond_to_invite,
            rsvp_existing_event,
            get_rsvp_response,
            get_event_partstat_for_user,
            update_calendar_event,
            delete_calendar_event,
            dismiss_cancelled_event,
            is_event_in_calendar,
            record_cancelled_invite,
            is_invite_cancelled,
            // Issue #16: tray + notifications + preferences
            get_app_settings,
            list_system_fonts,
            fido_status,
            fido_generate_salt,
            fido_enroll,
            fido_enroll_passphrase,
            fido_verify_passphrase,
            fido_verify_prf,
            fido_remove,
            database_status,
            unlock_with_passphrase,
            unlock_with_prf,
            enable_fido_only_mode,
            disable_fido_only_mode,
            get_wipe_policy,
            set_wipe_policy,
            update_app_settings,
            import_custom_theme,
            remove_custom_theme,
            check_mail_now,
            dismiss_talk_reminder,
            get_total_unread,
            get_unread_counts_by_account,
            show_main_window_cmd,
            quit_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nimbus");
}
