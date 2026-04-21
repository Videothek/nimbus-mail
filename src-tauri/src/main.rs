//! Nimbus — a modern mail client with Nextcloud integration.
//!
//! This is the Tauri application entry point. It registers Tauri
//! commands (the IPC bridge between Rust and Svelte) and launches
//! the native window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod badge;

use nimbus_caldav::{
    Calendar as CaldavCalendar, RawEvent, list_calendars as caldav_list_calendars,
    sync_calendar as caldav_sync_calendar,
};
use nimbus_carddav::{
    Addressbook, ParsedVcard, RawContact, build_vcard, create_contact as carddav_create_contact,
    delete_contact as carddav_delete_contact, list_addressbooks, sync_addressbook,
    update_contact as carddav_update_contact,
};
use nimbus_core::NimbusError;
use nimbus_core::models::{
    Account, AppSettings, CalendarEvent, Contact, Email, EmailEnvelope, Folder, NextcloudAccount,
    OutgoingEmail,
};
use nimbus_imap::ImapClient;
use nimbus_jmap::JmapClient;
use nimbus_nextcloud::{
    FileEntry, LoginFlowInit, LoginFlowResult, fetch_capabilities, poll_login, start_login,
};
use nimbus_smtp::SmtpClient;
use nimbus_store::cache::{
    CalendarEventRow, CalendarRow, ContactRow, ContactServerHandle, SearchFilters, SearchHit,
    SearchScope, SyncState,
};
use nimbus_store::{Cache, account_store, app_settings, credentials, nextcloud_store};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
fn get_accounts() -> Result<Vec<Account>, NimbusError> {
    account_store::load_accounts()
}

/// Add a new email account and store its password in the OS keychain.
///
/// The frontend sends an `Account` object plus a `password`. The account
/// metadata goes to `accounts.json`; the password goes to the OS keychain.
/// Separating them means the JSON file never contains secrets and can be
/// safely inspected or backed up.
#[tauri::command]
fn add_account(account: Account, password: String) -> Result<(), NimbusError> {
    credentials::store_imap_password(&account.id, &password)?;
    account_store::add_account(account)
}

/// Remove an account and its stored password.
///
/// Order matters: keychain → cache → account record. If any step fails,
/// the remaining state is still consistent with the account being present
/// (the user can retry). The last step (removing from accounts.json) is
/// the visible source of truth, so we do it last.
#[tauri::command]
fn remove_account(id: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    credentials::delete_imap_password(&id)?;
    // Best-effort: a failure here leaves orphaned cache rows but doesn't
    // block account removal. Log and continue.
    if let Err(e) = cache.wipe_account(&id) {
        tracing::warn!("failed to wipe cache for account '{id}': {e}");
    }
    account_store::remove_account(&id)
}

/// Update an existing account's settings.
#[tauri::command]
fn update_account(account: Account) -> Result<(), NimbusError> {
    account_store::update_account(account)
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
) -> Result<String, NimbusError> {
    tracing::info!("Testing IMAP connection to {host}:{port} as {username}");
    let client = ImapClient::connect(&host, port, &username, &password).await?;
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
async fn list_nextcloud_files(
    nc_id: String,
    path: String,
) -> Result<Vec<FileEntry>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::list_directory(
        &account.server_url,
        &account.username,
        &app_password,
        &path,
    )
    .await
}

/// Download a single file from Nextcloud.
///
/// Returns the raw bytes for the UI to stuff into a compose attachment
/// (or save wherever the caller needs). Large files are held in memory
/// for now — matches how locally-picked attachments work. A streaming
/// path is a separate future issue once compose itself streams.
#[tauri::command]
async fn download_nextcloud_file(
    nc_id: String,
    path: String,
) -> Result<Vec<u8>, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::download_file(
        &account.server_url,
        &account.username,
        &app_password,
        &path,
    )
    .await
}

/// Create a public share link for a Nextcloud file and return the URL.
///
/// The compose UI uses this to insert a "click here to download" link
/// into the email body — a lighter alternative to attaching the bytes
/// for big files or files the recipient might want to re-download.
///
/// The share is read-only with no password and no expiry; per-share
/// options can be added as a separate command (`update_nextcloud_share`)
/// once the UI grows controls for them.
#[tauri::command]
async fn create_nextcloud_share(
    nc_id: String,
    path: String,
) -> Result<String, NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    let share = nimbus_nextcloud::create_public_share(
        &account.server_url,
        &account.username,
        &app_password,
        &path,
    )
    .await?;
    Ok(share.url)
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
async fn create_nextcloud_directory(
    nc_id: String,
    path: String,
) -> Result<(), NimbusError> {
    let account = load_nextcloud_account(&nc_id)?;
    let app_password = credentials::get_nextcloud_password(&nc_id)?;
    nimbus_nextcloud::create_directory(
        &account.server_url,
        &account.username,
        &app_password,
        &path,
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
        emails: c.emails.clone(),
        phones: c.phones.clone(),
        organization: c.organization.clone(),
        photo_mime: c.photo_mime.clone(),
        photo_data: c.photo_data.clone(),
        vcard_raw: c.vcard_raw.clone(),
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
#[derive(Debug, Clone, Deserialize)]
struct ContactInput {
    display_name: String,
    emails: Vec<String>,
    phones: Vec<String>,
    organization: Option<String>,
    photo_mime: Option<String>,
    photo_data: Option<Vec<u8>>,
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

    let row = ContactRow {
        href: outcome.href.clone(),
        etag: outcome.etag.clone(),
        vcard_uid: uid.clone(),
        display_name: parsed.display_name.clone(),
        emails: parsed.emails.clone(),
        phones: parsed.phones.clone(),
        organization: parsed.organization.clone(),
        photo_mime: parsed.photo_mime.clone(),
        photo_data: parsed.photo_data.clone(),
        vcard_raw: vcard,
    };
    cache
        .upsert_single_contact(&nc_id, &addressbook_name, &row)
        .map_err(NimbusError::from)?;

    Ok(row_to_contact(&nc_id, &row))
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

    let parsed = input_to_parsed(&handle.vcard_uid, &input);
    let vcard = build_vcard(&parsed);

    let outcome = carddav_update_contact(
        &handle.href,
        &account.username,
        &app_password,
        &handle.etag,
        &vcard,
    )
    .await?;

    let row = ContactRow {
        href: outcome.href.clone(),
        etag: outcome.etag.clone(),
        vcard_uid: handle.vcard_uid.clone(),
        display_name: parsed.display_name.clone(),
        emails: parsed.emails.clone(),
        phones: parsed.phones.clone(),
        organization: parsed.organization.clone(),
        photo_mime: parsed.photo_mime.clone(),
        photo_data: parsed.photo_data.clone(),
        vcard_raw: vcard,
    };
    cache
        .upsert_single_contact(&handle.nextcloud_account_id, &handle.addressbook, &row)
        .map_err(NimbusError::from)?;

    Ok(row_to_contact(&handle.nextcloud_account_id, &row))
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
async fn list_nextcloud_calendars(
    nc_id: String,
) -> Result<Vec<CalendarSummary>, NimbusError> {
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
        let upserts: Vec<CalendarEventRow> = delta
            .upserts
            .iter()
            .flat_map(raw_event_to_rows)
            .collect();

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
        })
        .collect())
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
        out.extend(nimbus_caldav::expand_event(master, &ovs, range_start, range_end));
    }
    // Expansion doesn't guarantee chronological order across the whole
    // set (singletons come first, then per-master occurrences). Sort
    // once at the end so the UI's day-bucket grouping stays coherent.
    out.sort_by_key(|e| e.start);
    Ok(out)
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
        emails: input.emails.clone(),
        phones: input.phones.clone(),
        organization: input.organization.clone(),
        photo_mime: input.photo_mime.clone(),
        photo_data: input.photo_data.clone(),
    }
}

/// Hydrate a freshly-written `ContactRow` into a UI-facing
/// `Contact`. The composite id has to match the one the store
/// uses internally (`{nc_account_id}::{vcard_uid}`) so the next
/// `get_contacts` call returns the same record.
fn row_to_contact(nc_account_id: &str, row: &ContactRow) -> Contact {
    Contact {
        id: format!("{nc_account_id}::{}", row.vcard_uid),
        nextcloud_account_id: nc_account_id.to_string(),
        display_name: row.display_name.clone(),
        email: row.emails.clone(),
        phone: row.phones.clone(),
        organization: row.organization.clone(),
        photo_mime: row.photo_mime.clone(),
        photo_data: row.photo_data.clone(),
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

/// Look up an account by ID, or return a helpful error.
fn load_account(id: &str) -> Result<Account, NimbusError> {
    account_store::load_accounts()?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| NimbusError::Other(format!("no account with id '{id}'")))
}

/// Connect to an account's IMAP server using the stored password.
async fn connect_imap(account: &Account) -> Result<ImapClient, NimbusError> {
    let password = credentials::get_imap_password(&account.id)?;
    ImapClient::connect(
        &account.imap_host,
        account.imap_port,
        &account.email,
        &password,
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
    let account = load_account(account_id)?;
    let _ = poll_folder(&account, folder, limit, cache).await?;
    // The poll helper already wrote through to the cache and updated
    // the sync bookmark; we return the newest `limit` from the cache
    // rather than just the delta, because the UI expects a full list
    // regardless of whether this was an incremental or full sync.
    cache
        .get_envelopes(account_id, folder, limit)
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

    let _ = client.logout().await;

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
    let account = load_account(account_id)?;

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
) -> Result<Vec<u8>, NimbusError> {
    let account = load_account(&account_id)?;
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
    // Optimistic cache update — instant UI feedback.
    if let Err(e) = cache.mark_envelope_read(&account_id, &folder, uid) {
        tracing::warn!("cache.mark_envelope_read failed: {e}");
    }

    // Reading a message should immediately drop the tray/taskbar
    // badge — the user's mental model is "I read it, the counter
    // dropped" and a 5-minute sync wait would feel broken.
    refresh_unread_badge(&app);

    let account = load_account(&account_id)?;
    if uses_jmap(&account) {
        let client = connect_jmap(&account).await?;
        return client.mark_as_read(&folder, uid).await;
    }

    let mut client = connect_imap(&account).await?;
    let result = client.mark_as_read(&folder, uid).await;
    let _ = client.logout().await;
    result
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
#[tauri::command]
async fn send_email(account_id: String, email: OutgoingEmail) -> Result<(), NimbusError> {
    let account = load_account(&account_id)?;

    // JMAP handles sending server-side via EmailSubmission — no separate
    // SMTP connection needed.
    if uses_jmap(&account) {
        let client = connect_jmap(&account).await?;
        return client.send_email(&email).await;
    }

    let password = credentials::get_imap_password(&account.id)?;
    let client = SmtpClient::connect(
        &account.smtp_host,
        account.smtp_port,
        &account.email,
        &password,
    )
    .await?;
    client.send(&email).await
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
    let account = load_account(&account_id)?;

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
    let account = load_account(&account_id)?;
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
    let accounts = account_store::load_accounts().unwrap_or_default();
    let cache = app.state::<Cache>();

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
    // direct equivalent, and Tauri only exposes `set_overlay_icon`
    // behind `#[cfg(windows)]`.
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
    }
}

// ── App-settings commands ──────────────────────────────────────

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
        .manage(cache)
        .manage(shared_settings)
        .register_uri_scheme_protocol("contact-photo", contact_photo_protocol)
        .setup(|app| {
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
            get_accounts,
            add_account,
            remove_account,
            update_account,
            test_connection,
            fetch_envelopes,
            fetch_message,
            download_email_attachment,
            fetch_folders,
            mark_as_read,
            send_email,
            get_cached_envelopes,
            get_cached_message,
            get_cached_folders,
            test_jmap_connection,
            detect_jmap,
            search_emails,
            search_imap_server,
            start_nextcloud_login,
            poll_nextcloud_login,
            get_nextcloud_accounts,
            remove_nextcloud_account,
            open_url,
            list_nextcloud_files,
            download_nextcloud_file,
            create_nextcloud_share,
            create_nextcloud_directory,
            upload_to_nextcloud,
            save_bytes_to_path,
            sync_nextcloud_contacts,
            get_contacts,
            search_contacts,
            get_contact_photo,
            create_contact,
            update_contact,
            delete_contact,
            list_nextcloud_addressbooks,
            list_nextcloud_calendars,
            sync_nextcloud_calendars,
            get_cached_calendars,
            get_cached_events,
            // Issue #16: tray + notifications + preferences
            get_app_settings,
            update_app_settings,
            check_mail_now,
            get_total_unread,
            show_main_window_cmd,
            quit_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nimbus");
}
