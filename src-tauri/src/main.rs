//! Nimbus — a modern mail client with Nextcloud integration.
//!
//! This is the Tauri application entry point. It registers Tauri
//! commands (the IPC bridge between Rust and Svelte) and launches
//! the native window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use nimbus_core::NimbusError;
use nimbus_core::models::{Account, Email, EmailEnvelope};
use nimbus_imap::ImapClient;
use nimbus_store::cache::SyncState;
use nimbus_store::{Cache, account_store, credentials};
use tauri::State;

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

/// Stub: "test" a connection to the given server.
///
/// Real IMAP/SMTP connection testing will come when we wire this up to
/// the new `ImapClient::connect` — for now this just lets the setup UI
/// render a success message.
#[tauri::command]
fn test_connection(host: String, port: u16) -> Result<String, NimbusError> {
    tracing::info!("Test connection to {host}:{port} (stub — always succeeds)");
    Ok(format!("Connection to {host}:{port} OK (stub)"))
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
    let mut client = connect_imap(&account).await?;
    let email = client.fetch_message(folder, uid, account_id).await?;
    let _ = client.logout().await;

    // Single transactional write-through: envelope + body together so the
    // two can never drift on a partial failure.
    if let Err(e) = cache.upsert_message(&email) {
        tracing::warn!("cache.upsert_message failed: {e}");
    }

    Ok(email)
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
        // Register all our commands so the frontend can call them
        .invoke_handler(tauri::generate_handler![
            get_accounts,
            add_account,
            remove_account,
            update_account,
            test_connection,
            fetch_envelopes,
            fetch_message,
            get_cached_envelopes,
            get_cached_message,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nimbus");
}
