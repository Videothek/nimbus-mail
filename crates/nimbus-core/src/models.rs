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
    pub use_jmap: bool,
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
    #[serde(skip)]
    pub attachments: Vec<Attachment>,
}

/// A file attachment for an outgoing email.
///
/// The raw bytes are held in memory. For large files, consider
/// streaming from disk in the future.
#[derive(Debug, Clone)]
pub struct Attachment {
    /// Display filename (e.g. "report.pdf")
    pub filename: String,
    /// MIME type (e.g. "application/pdf")
    pub content_type: String,
    /// Raw file contents
    pub data: Vec<u8>,
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
