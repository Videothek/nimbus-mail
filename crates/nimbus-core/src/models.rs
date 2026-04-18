//! Core domain models shared across all Nimbus crates.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Represents an email account configured by the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// Whether to prefer JMAP over IMAP when available.
    #[serde(default)]
    pub use_jmap: bool,
    /// Base URL of the JMAP server (e.g. `https://mail.example.com`).
    /// Only used when `use_jmap` is true. Discovered automatically
    /// during account setup if the server supports `.well-known/jmap`.
    #[serde(default)]
    pub jmap_url: Option<String>,
}

/// Lightweight email metadata for list views.
///
/// This is what we fetch when populating the mail list sidebar — just
/// enough to render a row. Full body / HTML / attachments come from
/// a separate `fetch_message` call when the user clicks an email.
///
/// `uid` is the IMAP UID within the folder and uniquely identifies the
/// message across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailEnvelope {
    pub uid: u32,
    pub folder: String,
    pub from: String,
    pub subject: String,
    pub date: DateTime<Utc>,
    pub is_read: bool,
    pub is_starred: bool,
}

/// Represents an email message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Email {
    pub id: String,
    pub account_id: String,
    pub folder: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub date: DateTime<Utc>,
    pub is_read: bool,
    pub is_starred: bool,
    pub has_attachments: bool,
}

/// Represents an IMAP mailbox folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    /// Full folder name (e.g. "INBOX", "INBOX/Work")
    pub name: String,
    /// Hierarchy delimiter used by the server (e.g. "/" or ".")
    pub delimiter: Option<String>,
    /// IMAP folder attributes (e.g. \Sent, \Trash, \Drafts)
    pub attributes: Vec<String>,
    /// Number of unseen (unread) messages in this folder.
    /// `None` if the server didn't respond to the STATUS query.
    pub unread_count: Option<u32>,
}

/// Represents an email to be composed and sent via SMTP.
///
/// Unlike `Email` (which models a received message), this struct
/// carries only the fields needed for *sending*: recipients, subject,
/// body (plain text and/or HTML), and optional attachments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingEmail {
    /// Sender address (e.g. "alice@example.com")
    pub from: String,
    /// Primary recipients
    pub to: Vec<String>,
    /// Carbon-copy recipients
    pub cc: Vec<String>,
    /// Blind carbon-copy recipients
    pub bcc: Vec<String>,
    /// Optional Reply-To address (if different from `from`)
    pub reply_to: Option<String>,
    /// Subject line
    pub subject: String,
    /// Plain-text body (at least one of body_text / body_html should be set)
    pub body_text: Option<String>,
    /// HTML body
    pub body_html: Option<String>,
    /// File attachments
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

/// A file attachment for an outgoing email.
///
/// The raw bytes are held in memory. For large files, consider
/// streaming from disk in the future.
///
/// `data` is serialised as a JSON array of bytes — the Svelte frontend
/// reads the picked file with `FileReader.readAsArrayBuffer` and sends
/// `Array.from(new Uint8Array(buffer))`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Display filename (e.g. "report.pdf")
    pub filename: String,
    /// MIME type (e.g. "application/pdf")
    pub content_type: String,
    /// Raw file contents
    pub data: Vec<u8>,
}

/// A persistent Nextcloud connection.
///
/// One `NextcloudAccount` can be shared across multiple mail accounts —
/// users often have several email identities but a single Nextcloud
/// instance that backs attachments, Talk rooms, contacts and calendars.
/// That's why this lives as its own top-level record (separate from
/// `Account`) and is not keyed by email.
///
/// The `app_password` itself is **never** stored here — it lives in the
/// OS keychain under service `nimbus-mail-nextcloud` keyed by `id`.
/// `capabilities` is cached at connect time so the UI can show which
/// Nextcloud apps are available without a round-trip on every render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextcloudAccount {
    /// Stable UUID; used as the keychain account key for the app password.
    pub id: String,
    /// Base URL of the Nextcloud server, e.g. `https://cloud.example.com`.
    /// Stored without trailing slash.
    pub server_url: String,
    /// Nextcloud login name returned by Login Flow v2. Often differs from
    /// the user's email — it's whatever NC uses to identify the user.
    pub username: String,
    /// Optional pretty name shown in the UI — pulled from
    /// `/ocs/v2.php/cloud/user` after login when available.
    pub display_name: Option<String>,
    /// What the server supports, snapshotted at connect time.
    pub capabilities: Option<NextcloudCapabilities>,
}

/// Boolean flags for which Nextcloud apps the connected server offers.
///
/// Nextcloud's capabilities endpoint returns a deep, provider-specific
/// JSON tree; we reduce it to the handful of bits the UI actually
/// branches on. Refetched (via `fetch_capabilities`) when the user
/// explicitly asks to refresh the connection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NextcloudCapabilities {
    /// Nextcloud server version (e.g. "28.0.4"). Useful for feature gates.
    pub version: Option<String>,
    /// Nextcloud Talk (spreed) is installed and enabled.
    pub talk: bool,
    /// Files app — attachments, file sharing. Effectively always true on
    /// a working NC install, but we still check to be defensive.
    pub files: bool,
    /// CalDAV calendar endpoint is available.
    pub caldav: bool,
    /// CardDAV contact endpoint is available.
    pub carddav: bool,
}

/// Represents a contact from CardDAV / Nextcloud.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub display_name: String,
    pub email: Vec<String>,
    pub phone: Vec<String>,
    pub organization: Option<String>,
}

/// Represents a calendar event from CalDAV / Nextcloud.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub summary: String,
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub location: Option<String>,
}
