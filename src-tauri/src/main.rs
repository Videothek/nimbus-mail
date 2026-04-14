//! Nimbus — a modern mail client with Nextcloud integration.
//!
//! This is the Tauri application entry point. It registers Tauri
//! commands (the IPC bridge between Rust and Svelte) and launches
//! the native window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use nimbus_core::NimbusError;
use nimbus_core::models::{Account, Email, EmailEnvelope};
use nimbus_imap::ImapClient;
use nimbus_store::{account_store, credentials};

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
/// We delete the password *before* the account record so that if the
/// keychain call fails, the account stays listed (and the user can retry).
/// If the password delete succeeds but the file write fails, we'd leak a
/// keychain entry with no account — acceptable trade-off.
#[tauri::command]
fn remove_account(id: String) -> Result<(), NimbusError> {
    credentials::delete_imap_password(&id)?;
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
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    match fetch_envelopes_inner(&account_id, &folder, limit).await {
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
) -> Result<Vec<EmailEnvelope>, NimbusError> {
    let account = load_account(account_id)?;
    let mut client = connect_imap(&account).await?;
    let envelopes = client.fetch_envelopes(folder, limit).await?;
    let _ = client.logout().await;
    Ok(envelopes)
}

/// Fetch a full message (headers + body) by folder + UID.
#[tauri::command]
async fn fetch_message(
    account_id: String,
    folder: String,
    uid: u32,
) -> Result<Email, NimbusError> {
    match fetch_message_inner(&account_id, &folder, uid).await {
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
) -> Result<Email, NimbusError> {
    let account = load_account(account_id)?;
    let mut client = connect_imap(&account).await?;
    let email = client.fetch_message(folder, uid, account_id).await?;
    let _ = client.logout().await;
    Ok(email)
}

// ── App entry point ─────────────────────────────────────────────

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
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
