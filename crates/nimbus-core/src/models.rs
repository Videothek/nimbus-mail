//! Core domain models shared across all Nimbus crates.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// App-wide user preferences (not per-account).
///
/// Persisted to a single JSON file (`app_settings.json`) alongside
/// `accounts.json`. The struct carries `#[serde(default)]` at the
/// top level so adding a new field in a future version silently
/// slots in its default value instead of failing to parse an
/// existing user's settings file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Close button hides the window instead of quitting the app.
    /// Users quit explicitly via the tray menu.
    pub minimize_to_tray: bool,
    /// Whether the background sync loop polls INBOX across all accounts.
    pub background_sync_enabled: bool,
    /// How often (seconds) to poll. Clamped to a 30s floor at runtime
    /// so a misconfigured file can't DOS the user's mail server.
    pub background_sync_interval_secs: u64,
    /// Whether to show OS-native toasts when new mail arrives.
    pub notifications_enabled: bool,
    /// Launch hidden to tray on app start.
    pub start_minimized: bool,
    /// Skeleton UI theme name (e.g. `"cerberus"`, `"modern"`,
    /// `"pine"`). The frontend keeps the canonical list of themes
    /// it knows how to render; this value is the user's selection
    /// and is set on `<html data-theme="…">` at startup.
    pub theme_name: String,
    /// Whether the UI follows the OS light/dark preference, or
    /// is pinned to one. Applied via `<html data-mode="…">`.
    pub theme_mode: ThemeMode,
    /// Render HTML email bodies on a forced white canvas.
    ///
    /// HTML emails almost always set inline text colours assuming a
    /// light page background — the dark text becomes unreadable in
    /// dark mode if we let it render against the app's surface
    /// colour. With this on (default), the email body wrapper is
    /// painted white regardless of the app theme. Turn off to let
    /// the email render against the app's background — useful for
    /// dark-themed emails or when a sender provides a proper
    /// dark-mode design.
    pub mail_html_white_background: bool,
}

/// Light/dark mode selection. `System` follows the OS preference
/// (`prefers-color-scheme`) and reacts live when the user changes
/// their OS theme; `Light` / `Dark` pin the mode regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}


impl Default for AppSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray: true,
            background_sync_enabled: true,
            // Tightened from 5 minutes to 60s as part of the icon-
            // rail shell pass: the manual "Refresh" button is gone
            // from the sidebar, so the background loop is the only
            // thing keeping the inbox fresh between the on-view-
            // switch poll and the user's next interaction. 60s is
            // the modern-client floor; users who care about server
            // load can bump it in Settings (30s hard floor still
            // enforced at runtime).
            background_sync_interval_secs: 60,
            notifications_enabled: true,
            start_minimized: false,
            theme_name: "cerberus".to_string(),
            theme_mode: ThemeMode::System,
            mail_html_white_background: true,
        }
    }
}

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
    /// Optional plain-text signature appended below new messages
    /// composed from this account. Empty/None = no signature. The
    /// frontend renders the standard `-- ` separator before it.
    #[serde(default)]
    pub signature: Option<String>,
    /// User-defined "folder name contains X → use icon Y" rules.
    /// The Sidebar applies these *before* its built-in icon
    /// heuristics so a user can theme their own folders ("Bank",
    /// "Amazon", a project name, …) without having to wait for the
    /// app to ship a hard-coded mapping. Per-account so users with
    /// different filing schemes on different mail accounts don't
    /// have to share one global list.
    #[serde(default)]
    pub folder_icons: Vec<FolderIconRule>,
    /// Per-folder icon overrides keyed by the full folder path. This
    /// is the "I right-clicked → Change icon" entry point — beats
    /// every other icon source (including special-use attributes)
    /// so if the user pins 📮 on their Inbox they actually get 📮,
    /// not whatever our default would be. Keyed by full path so
    /// `INBOX/Projects/2026` and `Projects/2026` can each carry
    /// their own choice without one matching the other.
    #[serde(default)]
    pub folder_icon_overrides: std::collections::HashMap<String, String>,
    /// TLS certificates the user has explicitly trusted for this
    /// account — typically self-signed certs on a personal mail
    /// server that webpki-roots wouldn't normally accept. Each
    /// entry is added to the rustls config's root store, so a
    /// matching cert chain validates as if it were CA-signed.
    /// Per-account so trust granted to one mail server can't
    /// silently apply to another.
    #[serde(default)]
    pub trusted_certs: Vec<TrustedCert>,
}

/// One TLS leaf certificate the user has chosen to trust for an
/// account. We keep the raw DER bytes so the cert can be plugged
/// straight into `rustls::RootCertStore` (and into lettre's
/// `add_root_certificate`) on every connect, plus the SHA-256
/// fingerprint for display in settings ("you trust 4 certificates
/// for this account: aa:bb:cc:…") and a human-readable host /
/// added-on date so the user can audit the list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedCert {
    /// Raw DER-encoded certificate bytes. Stored as a `Vec<u8>` so
    /// the JSON representation is a base64 string under the hood
    /// (serde-bytes if we needed it; serde's default for `Vec<u8>`
    /// is an array of integers — works fine for a few-hundred-byte
    /// cert and stays human-debuggable).
    pub der: Vec<u8>,
    /// SHA-256 of the DER bytes, lowercase hex with `:` separators
    /// every two characters (`aa:bb:cc:…`). This is what the user
    /// compared against their server when they trusted it; we
    /// surface it in settings so they can confirm what's stored.
    pub sha256: String,
    /// Hostname this cert was trusted *for*. Just informational —
    /// rustls handles the actual hostname matching during the
    /// handshake, so a cert valid for `mail.example.com` won't
    /// silently extend trust to `other.example.com`.
    pub host: String,
    /// Unix epoch seconds when the cert was added. Lets the
    /// settings UI render "trusted on Jan 5" so a stale entry is
    /// recognisable.
    pub added_at: i64,
}

/// One "folder name contains keyword → show icon" rule. `keyword`
/// is matched case-insensitively against the folder's name (and the
/// last hierarchy segment, so `INBOX/Bank` and `Bank` both match
/// `bank`). `icon` is whatever the user typed — a single emoji is
/// the expected case but we don't enforce it; the sidebar just
/// drops the string into the icon slot verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderIconRule {
    pub keyword: String,
    pub icon: String,
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
    /// Owning account id. Populated when envelopes are read out of the
    /// cache (where `account_id` is a column on every row) so the UI
    /// can render an account label in unified-inbox mode and route the
    /// "open message" click to the right account. IMAP/JMAP clients
    /// don't know their own account id, so they leave this empty —
    /// the cache write-through stamps it from the call site, and the
    /// cache read paths fill it back in. `#[serde(default)]` keeps
    /// older cached payloads parsing cleanly.
    #[serde(default)]
    pub account_id: String,
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
    /// Metadata for each attachment on the message — no bytes. Kept
    /// empty when `has_attachments` is false. The bytes are fetched on
    /// demand via a separate command so opening a message with a 50 MB
    /// attachment is still snappy.
    ///
    /// `#[serde(default)]` keeps older cached payloads (written before
    /// this field existed) deserialising cleanly — they come back as an
    /// empty list, which lines up with `has_attachments=false` for
    /// messages from before the attachment metadata landed.
    #[serde(default)]
    pub attachments: Vec<EmailAttachment>,
}

/// Metadata for one attachment on a received email.
///
/// The bytes are NOT carried here — they can be many megabytes and
/// would make every message fetch/cache hit that size. Instead we
/// expose enough to render an attachment chip and to later request the
/// bytes via `download_email_attachment` using `(folder, uid, part_id)`.
///
/// `part_id` is the index of this attachment among the message's
/// attachments (0, 1, 2, …) as `mail-parser` orders them. It's stable
/// for a given raw message — we re-parse on download and pick the same
/// index. Storing an opaque index rather than a MIME part path keeps
/// this JSON-friendly and avoids leaking parser internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAttachment {
    /// Display filename (from `Content-Disposition: filename` or
    /// `Content-Type: name`). Defaults to `"attachment"` if the server
    /// sent neither — we'd rather show a label than hide the file.
    pub filename: String,
    /// MIME type, e.g. `"application/pdf"`. Defaults to
    /// `"application/octet-stream"` when missing.
    pub content_type: String,
    /// Decoded size in bytes. `None` if the parser couldn't determine
    /// it (rare — most attachments are base64/quoted-printable with a
    /// deterministic decoded length).
    pub size: Option<u64>,
    /// Zero-based index into the parsed message's attachment list.
    /// Used as a stable handle for re-fetching the bytes on demand.
    pub part_id: u32,
    /// RFC 2392 Content-ID, when the MIME part carried one. Lifted
    /// from the message's `Content-ID` header by `mail-parser` —
    /// without the surrounding angle brackets — so a body anchor
    /// `<a href="cid:abc-123">` can resolve to this attachment by
    /// `cid_str.eq_ignore_ascii_case(att.content_id.as_deref()?)`.
    /// `None` when the attachment isn't referenced inline.
    /// `#[serde(default)]` keeps cached payloads from before this
    /// field existed deserialising cleanly as `None`.
    #[serde(default)]
    pub content_id: Option<String>,
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
    /// RFC 2392 Content-ID, used when the body HTML contains a
    /// `<a href="cid:…">` reference to this attachment (the `/`
    /// attachment-picker shortcut in Compose). Optional because
    /// legacy attachment payloads predate the field — we treat an
    /// absent `content_id` the same as "no inline reference".
    #[serde(default)]
    pub content_id: Option<String>,
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
    /// Nextcloud Office / Collabora (the `richdocuments` app id) is
    /// installed and enabled. When true, the attachment-click flow
    /// can open `.docx` / `.odt` / `.xlsx` etc. in an embedded
    /// editor; when false the UI falls back to plain download.
    /// `#[serde(default)]` so capability snapshots cached before
    /// this field existed deserialise as `false`.
    #[serde(default)]
    pub office: bool,
}

/// Represents a contact from CardDAV / Nextcloud.
///
/// `id` is a stable app-side UUID we generate the first time we see a
/// vCard — handy as a single string the UI can use as a key. The
/// CardDAV side is identified by the triple
/// `(nextcloud_account_id, addressbook, vcard_uid)`; that triple lives
/// only in the cache, the UI never deals with it.
///
/// `photo_data` is the decoded image bytes (vCard PHOTO is base64 in
/// the wire format, we decode once on import). Kept on the contact row
/// so the autocomplete dropdown can render thumbnails without a
/// separate fetch — Outlook does this; we should too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    /// Which Nextcloud account this contact came from. Lets the UI
    /// group contacts by source if a user has more than one NC server.
    pub nextcloud_account_id: String,
    pub display_name: String,
    /// Email addresses paired with a kind hint (vCard `EMAIL;TYPE=…`).
    /// Same shape pattern as `phone` and `addresses` so the UI can
    /// group "home / work / other" the way Nextcloud Contacts does.
    pub email: Vec<ContactEmail>,
    /// Phone numbers paired with a kind hint (vCard `TEL;TYPE=…`).
    /// Same shape pattern as `addresses` so the UI can group "home /
    /// work / mobile / fax / other" the way Nextcloud Contacts does.
    pub phone: Vec<ContactPhone>,
    pub organization: Option<String>,
    /// MIME type of `photo_data` (e.g. "image/jpeg"); `None` if no photo.
    pub photo_mime: Option<String>,
    /// Raw decoded photo bytes. Serialised as a JSON byte array so the
    /// frontend can wrap it in a `Blob` URL for `<img src>`.
    pub photo_data: Option<Vec<u8>>,
    /// Job title (vCard `TITLE`) — separate from `organization`'s
    /// company name. Often paired with org in the contact card UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Postal addresses (vCard `ADR`). Multiple allowed; each carries
    /// a kind hint (`home` / `work` / `other`) so the UI can group
    /// them like Nextcloud Contacts does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<ContactAddress>,
    /// Birthday (vCard `BDAY`). Stored as the raw vCard text — date
    /// formats vary (`19851031`, `1985-10-31`, `--10-31` for missing
    /// year) and parsing here would lose information the UI can still
    /// render verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birthday: Option<String>,
    /// Personal/work URLs (vCard `URL`). Multiple allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// Free-form note (vCard `NOTE`). Single multi-line string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One postal address from a vCard `ADR` property.
///
/// vCard 4 splits the address into seven fields (PO box, extended,
/// street, locality, region, postal code, country). We omit the
/// PO-box and extended slots — Nextcloud Contacts doesn't surface
/// them either — and keep the rest as plain strings the UI renders
/// in standard "street, city region postal, country" order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactAddress {
    /// "home", "work", or "other". Lower-cased, and `"other"` is the
    /// fallback when the vCard `TYPE` parameter is absent or
    /// unrecognised.
    pub kind: String,
    pub street: String,
    pub locality: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

/// One phone number from a vCard `TEL` property paired with a kind
/// hint pulled from its `TYPE=` parameter. vCard 4 lets `TYPE` carry
/// a comma-separated list — we pick the first recognised value
/// (`home` / `work` / `cell` / `fax`) and fall back to `"other"` so
/// no entry ever loses its value just because we couldn't classify
/// it. Mirrors the `ContactAddress` pattern so the UI grouping
/// works the same way Nextcloud Contacts does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactPhone {
    pub kind: String,
    pub value: String,
}

/// One email address from a vCard `EMAIL` property paired with a
/// kind hint pulled from its `TYPE=` parameter. Recognises `home`
/// and `work`; `INTERNET` (a vCard-3 legacy meaning "this is an
/// email address") is treated as no information and falls back to
/// `"other"`. Same shape as `ContactPhone`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactEmail {
    pub kind: String,
    pub value: String,
}

/// Represents a calendar event from CalDAV / Nextcloud.
///
/// The recurrence fields below (`rrule`, `rdate`, `exdate`,
/// `recurrence_id`) are **captured during sync but not yet expanded**.
/// The struct always describes one concrete instance — the master for
/// non-recurring events, or the first occurrence of a recurring
/// series. See issue #47 for the expansion work that turns these
/// fields into visible additional occurrences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub summary: String,
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub location: Option<String>,
    /// Raw RRULE value, e.g. `FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20270101T000000Z`.
    /// Stored as-is so the eventual expander doesn't re-parse from the
    /// iCalendar source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    /// Extra occurrence dates added to the series (`RDATE`). Mostly
    /// empty in practice — many calendar UIs don't expose it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rdate: Vec<DateTime<Utc>>,
    /// Cancelled occurrences (`EXDATE`). Present on cancelled
    /// instances of an otherwise-recurring series.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exdate: Vec<DateTime<Utc>>,
    /// If this VEVENT is an override for a specific occurrence of a
    /// recurring series, this holds the original start time of that
    /// occurrence (the `RECURRENCE-ID`). `None` for masters and for
    /// non-recurring events. The shared UID between master and
    /// override is in `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<DateTime<Utc>>,
    /// `URL` property — a link associated with the event (meeting URL,
    /// agenda doc, etc.). Free-form, the editor doesn't validate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `TRANSP` — `OPAQUE` (default — busy time) or `TRANSPARENT`
    /// (free time). The editor surfaces this as a "show as
    /// busy / free" picker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency: Option<String>,
    /// `ATTENDEE` properties. Empty for events with no participants.
    /// We store name + email + the participant status the server last
    /// reported; the UI only edits the email list today.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<EventAttendee>,
    /// `VALARM` blocks. The editor exposes a single "remind me X
    /// before" picker, but the model carries a list so existing
    /// events with several alarms round-trip without losing data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminders: Vec<EventReminder>,
}

/// A single ATTENDEE property on a VEVENT.
///
/// Only the most-used fields are surfaced — CN (display name), the
/// `mailto:` email, and PARTSTAT (acceptance status). The full set
/// (ROLE, RSVP, CUTYPE, …) is preserved opaquely in `ics_raw` until
/// a follow-up issue surfaces them in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventAttendee {
    /// The email after `mailto:`. Required by the iCalendar spec.
    pub email: String,
    /// `CN=` parameter (e.g. `"Jane Doe"`). Optional — many invites
    /// only carry the email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common_name: Option<String>,
    /// `PARTSTAT=` parameter — `NEEDS-ACTION` / `ACCEPTED` /
    /// `DECLINED` / `TENTATIVE`. `None` falls back to NEEDS-ACTION
    /// when written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// A single VALARM block.
///
/// We model the most common reminder shape — a relative offset before
/// the event start — directly. The trigger is stored as **minutes
/// before** the event (positive = before, negative = after).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventReminder {
    /// Minutes before the event start. `15` means "fire 15 minutes
    /// before". Negative values fire after the start.
    pub trigger_minutes_before: i32,
    /// `ACTION` — `DISPLAY` (popup) or `EMAIL`. Defaults to `DISPLAY`
    /// when written if `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}
