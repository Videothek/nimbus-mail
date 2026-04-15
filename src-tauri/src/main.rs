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
    let envelopes = client.fetch_envelopes(folder, limit).await?;
    let _ = client.logout().await;

    // Write-through. If the cache write fails we still return the live
    // data — the cache is an optimisation, not a correctness requirement,
    // so it should never block a successful response.
    if let Err(e) = cache.upsert_envelopes_for_account(account_id, &envelopes) {
        tracing::warn!("cache.upsert_envelopes failed: {e}");
    }
    // Track the highest UID we just saw so a future incremental sync can
    // start from there. `uidvalidity` is still unknown (IMAP client does
    // not yet expose it); leave it None until #4 Part B.
    if let Some(highest) = envelopes.iter().map(|e| e.uid).max() {
        let state = SyncState {
            uidvalidity: None,
            highest_uid_seen: Some(highest),
            last_synced_at: Some(chrono::Utc::now()),
        };
        if let Err(e) = cache.set_sync_state(account_id, folder, &state) {
            tracing::warn!("cache.set_sync_state failed: {e}");
        }
    }

    Ok(envelopes)
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

    // Cache the body alongside an envelope row — the envelope side of the
    // cache may not have seen this UID yet (user clicked a message we
    // didn't preload), so we upsert both.
    let envelope = EmailEnvelope {
        uid,
        folder: folder.to_string(),
        from: email.from.clone(),
        subject: email.subject.clone(),
        date: email.date,
        is_read: email.is_read,
        is_starred: email.is_starred,
    };
    if let Err(e) = cache.upsert_envelopes_for_account(account_id, &[envelope]) {
        tracing::warn!("cache.upsert_envelopes (from message) failed: {e}");
    }
    if let Err(e) = cache.upsert_body(
        account_id,
        folder,
        uid,
        email.body_text.as_deref(),
        email.body_html.as_deref(),
        email.has_attachments,
        None,
    ) {
        tracing::warn!("cache.upsert_body failed: {e}");
    }

    Ok(email)
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nimbus");
}
