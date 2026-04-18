//! Nimbus — a modern mail client with Nextcloud integration.
//!
//! This is the Tauri application entry point. It registers Tauri
//! commands (the IPC bridge between Rust and Svelte) and launches
//! the native window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use nimbus_carddav::{
    Addressbook, ParsedVcard, RawContact, build_vcard, create_contact as carddav_create_contact,
    delete_contact as carddav_delete_contact, list_addressbooks, sync_addressbook,
    update_contact as carddav_update_contact,
};
use nimbus_core::NimbusError;
use nimbus_core::models::{
    Account, Contact, Email, EmailEnvelope, Folder, NextcloudAccount, OutgoingEmail,
};
use nimbus_imap::ImapClient;
use nimbus_jmap::JmapClient;
use nimbus_nextcloud::{
    LoginFlowInit, LoginFlowResult, fetch_capabilities, poll_login, start_login,
};
use nimbus_smtp::SmtpClient;
use nimbus_store::cache::{
    ContactRow, ContactServerHandle, SearchFilters, SearchHit, SearchScope, SyncState,
};
use nimbus_store::{Cache, account_store, credentials, nextcloud_store};
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri::State;
use tauri::UriSchemeContext;

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
/// Also drops cached contacts and CardDAV sync state for this account;
/// a best-effort failure there is logged but doesn't block removal.
#[tauri::command]
fn remove_nextcloud_account(id: String, cache: State<'_, Cache>) -> Result<(), NimbusError> {
    credentials::delete_nextcloud_password(&id)?;
    if let Err(e) = cache.wipe_nextcloud_contacts(&id) {
        tracing::warn!("failed to wipe contacts for NC account '{id}': {e}");
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

    // ── JMAP path ──────────────────────────────────────────────
    // JMAP handles pagination server-side (no UIDVALIDITY, no incremental
    // sync bookmarks). We fetch, cache, and return.
    if uses_jmap(&account) {
        let client = connect_jmap(&account).await?;
        let envelopes = client.fetch_envelopes(folder, limit, None).await?;

        if let Err(e) = cache.upsert_envelopes_for_account(account_id, &envelopes) {
            tracing::warn!("cache.upsert_envelopes (JMAP) failed: {e}");
        }
        return Ok(envelopes);
    }

    // ── IMAP path (unchanged) ──────────────────────────────────
    let mut client = connect_imap(&account).await?;

    // Consult the cache so we know whether to do an incremental or full sync.
    // An error reading sync state is not fatal — we just fall back to full.
    let prior = cache.get_sync_state(account_id, folder).ok().flatten();
    let since_uid = prior.as_ref().and_then(|s| s.highest_uid_seen);

    let mut batch = client.fetch_envelopes(folder, limit, since_uid).await?;

    // UIDVALIDITY check. If the server has rotated it, every cached UID
    // for this folder now points at a different (or deleted) message —
    // wipe the folder and redo the fetch in full mode so the cache
    // reflects reality.
    let uidvalidity_changed = matches!(
        (prior.as_ref().and_then(|s| s.uidvalidity), batch.uidvalidity),
        (Some(old), Some(new)) if old != new,
    );
    if uidvalidity_changed {
        tracing::warn!(
            "UIDVALIDITY changed for '{account_id}'/'{folder}' \
             (was {:?}, now {:?}) — wiping folder cache",
            prior.as_ref().and_then(|s| s.uidvalidity),
            batch.uidvalidity,
        );
        if let Err(e) = cache.wipe_folder(account_id, folder) {
            tracing::warn!("cache.wipe_folder failed: {e}");
        }
        // Redo the fetch with no `since_uid` so we get the newest `limit`
        // messages under the new UID space.
        batch = client.fetch_envelopes(folder, limit, None).await?;
    }

    let _ = client.logout().await;

    // Write-through. Cache failures are logged but don't block the return:
    // the cache is an optimisation, not a correctness requirement.
    if let Err(e) = cache.upsert_envelopes_for_account(account_id, &batch.envelopes) {
        tracing::warn!("cache.upsert_envelopes failed: {e}");
    }

    // Update sync bookmarks. `highest_uid_seen` is max(prior, newly-fetched)
    // so an empty incremental response doesn't accidentally rewind it.
    let new_highest = batch
        .envelopes
        .iter()
        .map(|e| e.uid)
        .max()
        .into_iter()
        .chain(prior.as_ref().and_then(|s| s.highest_uid_seen))
        .max();
    let state = SyncState {
        uidvalidity: batch.uidvalidity,
        highest_uid_seen: new_highest,
        last_synced_at: Some(chrono::Utc::now()),
    };
    if let Err(e) = cache.set_sync_state(account_id, folder, &state) {
        tracing::warn!("cache.set_sync_state failed: {e}");
    }

    // Return the newest `limit` from the cache, not just what the server
    // sent back — an incremental sync only ships new messages, but the
    // UI expects a full list. Cache read is cheap (indexed by
    // `(account_id, folder, internal_date DESC)`).
    cache
        .get_envelopes(account_id, folder, limit)
        .map_err(Into::into)
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
) -> Result<(), NimbusError> {
    // Optimistic cache update — instant UI feedback.
    if let Err(e) = cache.mark_envelope_read(&account_id, &folder, uid) {
        tracing::warn!("cache.mark_envelope_read failed: {e}");
    }

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

// ── App entry point ─────────────────────────────────────────────

fn main() {
    tracing_subscriber::fmt::init();

    // Open (and migrate) the local mail cache once at startup, then
    // hand it to Tauri as managed state so every command can borrow it.
    // A failure here is fatal: without the cache the write-through path
    // is broken, and the user would silently lose offline capability.
    let cache = Cache::open_default().expect("failed to open local mail cache");

    tauri::Builder::default()
        .manage(cache)
        .register_uri_scheme_protocol("contact-photo", contact_photo_protocol)
        // Register all our commands so the frontend can call them
        .invoke_handler(tauri::generate_handler![
            get_accounts,
            add_account,
            remove_account,
            update_account,
            test_connection,
            fetch_envelopes,
            fetch_message,
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
            sync_nextcloud_contacts,
            get_contacts,
            search_contacts,
            get_contact_photo,
            create_contact,
            update_contact,
            delete_contact,
            list_nextcloud_addressbooks,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nimbus");
}
