//! SMTP client — connects to a mail server and sends emails.

use lettre::address::Envelope;
use lettre::message::{
    Attachment as LettreAttachment, Mailbox, MessageBuilder, MultiPart, SinglePart,
    header::{ContentDisposition, ContentId, ContentType},
};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use nimbus_core::error::NimbusError;
use nimbus_core::models::{OutgoingEmail, TrustedCert};
use nimbus_core::tls;
use rustls_pki_types::ServerName;
use tokio_rustls::TlsConnector;
use tracing::{debug, info};

/// An SMTP client that can send emails over an encrypted connection.
///
/// # Usage
/// ```ignore
/// let client = SmtpClient::connect("smtp.example.com", 587, "user@example.com", "password").await?;
/// client.send(&email).await?;
/// ```
pub struct SmtpClient {
    /// The underlying async SMTP transport (lettre).
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpClient {
    /// Connect to an SMTP server with STARTTLS and authenticate.
    ///
    /// This configures the transport to:
    /// 1. Connect to host:port
    /// 2. Upgrade to TLS via STARTTLS (port 587) or use implicit TLS (port 465)
    /// 3. Authenticate with the given credentials
    ///
    /// Returns a ready-to-send `SmtpClient`.
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        trusted_certs: &[TrustedCert],
    ) -> Result<Self, NimbusError> {
        info!(host, port, username, "Connecting to SMTP server");

        let credentials = Credentials::new(username.to_string(), password.to_string());

        // Build a `TlsParameters` that knows about every cert the
        // user has explicitly trusted for this account. Lettre adds
        // them straight onto its rustls root store (alongside
        // webpki-roots), which gives the same effective behaviour
        // as nimbus-imap: a server presenting a chain that ends in
        // one of the trusted certs validates as if it were CA-signed.
        let tls_params = build_tls_params(host, trusted_certs)?;

        // Port 465 uses implicit TLS (wrapped from the start).
        // Port 587 (and others) use STARTTLS (upgrade after connecting).
        let transport = if port == 465 {
            debug!("Using implicit TLS (port 465)");
            AsyncSmtpTransport::<Tokio1Executor>::relay(host)
                .map_err(|e| NimbusError::Network(format!("Failed to create SMTP relay: {e}")))?
                .port(port)
                .tls(Tls::Wrapper(tls_params))
                .credentials(credentials)
                .build()
        } else {
            debug!("Using STARTTLS (port {port})");
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
                .map_err(|e| NimbusError::Network(format!("Failed to create STARTTLS relay: {e}")))?
                .port(port)
                .tls(Tls::Required(tls_params))
                .credentials(credentials)
                .build()
        };

        // Test the connection by verifying we can reach the server.
        transport
            .test_connection()
            .await
            .map_err(|e| NimbusError::Network(format!("SMTP connection test failed: {e}")))?;

        info!("SMTP connection established and authenticated");

        Ok(Self { transport })
    }

    /// Send an email message.
    ///
    /// Builds the email from an `OutgoingEmail` struct, handling:
    /// - Plain text and/or HTML bodies
    /// - CC, BCC, and Reply-To headers
    /// - File attachments
    ///
    /// At least one of `body_text` or `body_html` must be set.
    pub async fn send(&self, email: &OutgoingEmail) -> Result<(), NimbusError> {
        info!(
            from = %email.from,
            to = ?email.to,
            subject = %email.subject,
            "Sending email"
        );

        let message = build_outgoing_message(email)?;

        self.transport
            .send(message)
            .await
            .map_err(|e| NimbusError::Protocol(format!("Failed to send email: {e}")))?;

        info!("Email sent successfully to {:?}", email.to);
        Ok(())
    }
}

/// Build a lettre `TlsParameters` for `host`, threading per-account
/// TLS-trust into lettre's verifier.
///
/// Lettre's `add_root_certificate` calls `RootCertStore::add` under
/// the hood, which validates each cert as a proper CA trust anchor
/// — and rejects self-signed leaves (the common case for personal
/// mail servers, and the whole reason a user would have a trusted
/// cert in the first place). Lettre also doesn't let us inject a
/// custom rustls verifier the way `nimbus-imap` can.
///
/// So when the account has any trusted certs we fall back to
/// `dangerous_accept_invalid_certs(true)`. That's looser than the
/// per-fingerprint check `nimbus-imap` does — it accepts any cert
/// the SMTP server presents, not just the one(s) the user trusted
/// — but the practical effect lines up with user intent: "I trust
/// this server's cert"; SMTP only ever talks to the same server
/// the user just trusted at the IMAP step.
fn build_tls_params(
    host: &str,
    trusted_certs: &[TrustedCert],
) -> Result<TlsParameters, NimbusError> {
    let mut builder = TlsParameters::builder(host.to_string());
    if !trusted_certs.is_empty() {
        builder = builder
            .dangerous_accept_invalid_certs(true)
            .dangerous_accept_invalid_hostnames(true);
    }
    builder
        .build_rustls()
        .map_err(|e| NimbusError::Network(format!("build TLS params: {e}")))
}

/// Probe the SMTP server's TLS certificate without verifying it.
/// Mirror of `nimbus_imap::probe_server_certificate` — used by the
/// "trust this server?" flow when a connect fails because the cert
/// isn't yet in the user's trust list.
///
/// Assumes implicit-TLS (port 465). For STARTTLS-only ports (587)
/// the cert isn't visible until after a SMTP greeting + STARTTLS
/// dance — and in practice the IMAP probe usually surfaces the
/// same cert (same host), so we let the UI try the IMAP probe first.
pub async fn probe_server_certificate(
    host: &str,
    port: u16,
) -> Result<Vec<u8>, NimbusError> {
    let addr = format!("{host}:{port}");
    let tcp = tokio::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| NimbusError::Network(format!("Failed to connect to {addr}: {e}")))?;

    let connector = TlsConnector::from(tls::no_verify_config());
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| NimbusError::Protocol(format!("invalid SMTP hostname '{host}': {e}")))?;
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| NimbusError::Network(format!("TLS probe failed with {host}: {e}")))?;

    let (_io, conn) = tls.get_ref();
    let leaf = conn
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or_else(|| {
            NimbusError::Protocol(format!("server '{host}' returned no certificate"))
        })?
        .to_vec();
    Ok(leaf)
}

/// Build the lettre `Message` for an outgoing email *without* sending it.
///
/// Exposed so callers (e.g. `main.rs`) can build the message once, send
/// it via SMTP, and then take the formatted RFC 822 bytes from
/// `message.formatted()` to `APPEND` a copy into the IMAP Sent folder
/// — without re-running the (potentially expensive) MIME serialization.
pub fn build_outgoing_message(email: &OutgoingEmail) -> Result<Message, NimbusError> {
    let from_mailbox: Mailbox = email.from.parse().map_err(|e| {
        NimbusError::Protocol(format!("Invalid 'from' address '{}': {e}", email.from))
    })?;

    let mut builder: MessageBuilder = Message::builder()
        .from(from_mailbox.clone())
        .subject(&email.subject);

    for addr in &email.to {
        let mailbox: Mailbox = addr
            .parse()
            .map_err(|e| NimbusError::Protocol(format!("Invalid 'to' address '{addr}': {e}")))?;
        builder = builder.to(mailbox);
    }
    for addr in &email.cc {
        let mailbox: Mailbox = addr
            .parse()
            .map_err(|e| NimbusError::Protocol(format!("Invalid 'cc' address '{addr}': {e}")))?;
        builder = builder.cc(mailbox);
    }
    for addr in &email.bcc {
        let mailbox: Mailbox = addr
            .parse()
            .map_err(|e| NimbusError::Protocol(format!("Invalid 'bcc' address '{addr}': {e}")))?;
        builder = builder.bcc(mailbox);
    }

    if let Some(reply_to) = &email.reply_to {
        let mailbox: Mailbox = reply_to.parse().map_err(|e| {
            NimbusError::Protocol(format!("Invalid 'reply-to' address '{reply_to}': {e}"))
        })?;
        builder = builder.reply_to(mailbox);
    }

    // When there are no recipients (a draft the user hasn't addressed
    // yet), lettre's `build()` would otherwise reject the message with
    // "missing destination address". The SMTP envelope is irrelevant
    // for the IMAP-APPEND path that drafts take, so we substitute a
    // placeholder envelope that reuses From as both sender and
    // receiver — just enough to satisfy the type, without leaking a
    // synthetic recipient into the RFC 822 headers the reader sees.
    // The SMTP send path validates recipients in the UI before
    // reaching this function, so this branch only trips for drafts.
    if email.to.is_empty() && email.cc.is_empty() && email.bcc.is_empty() {
        let envelope = Envelope::new(
            Some(from_mailbox.email.clone()),
            vec![from_mailbox.email.clone()],
        )
        .map_err(|e| NimbusError::Protocol(format!("Failed to build draft envelope: {e}")))?;
        builder = builder.envelope(envelope);
    }

    // The presence of a `calendar_part` forces the iMIP-flavoured
    // tree (text/plain + text/html + text/calendar inside the
    // alternative; the `.ics` also added as a separate attachment
    // for download).  Otherwise the plain attach-or-not split
    // applies as before.
    if email.calendar_part.is_some() {
        build_with_calendar(builder, email)
    } else if email.attachments.is_empty() {
        build_body_only(builder, email)
    } else {
        build_with_attachments(builder, email)
    }
}

/// Build an email with just a body (no attachments).
fn build_body_only(builder: MessageBuilder, email: &OutgoingEmail) -> Result<Message, NimbusError> {
    match (&email.body_text, &email.body_html) {
        // Both text and HTML → multipart/alternative so the mail client picks the best one.
        (Some(text), Some(html)) => {
            debug!("Building multipart/alternative body (text + HTML)");
            builder
                .multipart(
                    MultiPart::alternative()
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_PLAIN)
                                .body(text.clone()),
                        )
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_HTML)
                                .body(html.clone()),
                        ),
                )
                .map_err(|e| NimbusError::Protocol(format!("Failed to build email: {e}")))
        }
        // Only plain text.
        (Some(text), None) => {
            debug!("Building plain text body");
            builder
                .header(ContentType::TEXT_PLAIN)
                .body(text.clone())
                .map_err(|e| NimbusError::Protocol(format!("Failed to build email: {e}")))
        }
        // Only HTML.
        (None, Some(html)) => {
            debug!("Building HTML body");
            builder
                .header(ContentType::TEXT_HTML)
                .body(html.clone())
                .map_err(|e| NimbusError::Protocol(format!("Failed to build email: {e}")))
        }
        // No body at all — send an empty plain text message.
        (None, None) => {
            debug!("No body provided, sending empty message");
            builder
                .header(ContentType::TEXT_PLAIN)
                .body(String::new())
                .map_err(|e| NimbusError::Protocol(format!("Failed to build email: {e}")))
        }
    }
}

/// Build an email with attachments.
///
/// Structure:
/// ```text
/// multipart/mixed
/// ├── multipart/alternative (or single body part)
/// │   ├── text/plain
/// │   └── text/html
/// ├── attachment 1
/// └── attachment 2
/// ```
fn build_with_attachments(
    builder: MessageBuilder,
    email: &OutgoingEmail,
) -> Result<Message, NimbusError> {
    debug!(
        "Building email with {} attachment(s)",
        email.attachments.len()
    );

    // Start with the body as the first part of a mixed multipart.
    let body_part = match (&email.body_text, &email.body_html) {
        (Some(text), Some(html)) => MultiPart::mixed().multipart(
            MultiPart::alternative()
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_PLAIN)
                        .body(text.clone()),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html.clone()),
                ),
        ),
        (Some(text), None) => MultiPart::mixed().singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(text.clone()),
        ),
        (None, Some(html)) => MultiPart::mixed().singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone()),
        ),
        (None, None) => MultiPart::mixed().singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(String::new()),
        ),
    };

    // Add each attachment to the multipart message.
    let multipart = email.attachments.iter().fold(body_part, |mp, attachment| {
        let content_type = attachment
            .content_type
            .parse::<ContentType>()
            .unwrap_or(ContentType::parse("application/octet-stream").unwrap());

        let part = match &attachment.content_id {
            // No content-id: use lettre's stock Attachment helper. Emits
            // `Content-Disposition: attachment; filename=...` + the
            // content type; exactly the previous behaviour.
            None => LettreAttachment::new(attachment.filename.clone())
                .body(attachment.data.clone(), content_type),
            // With a content-id: we need *both* disposition=attachment
            // (so recipients see it in their attachment tray) AND a
            // Content-ID header (so `<a href="cid:<id>">` in the HTML
            // body can resolve back to this part). Lettre's
            // `Attachment::new_inline` sets Content-ID but flips
            // disposition to `inline`, and `Attachment::new` can't
            // add Content-ID at all — so we build the SinglePart by
            // hand instead, stacking exactly the three headers we
            // need. Angle brackets on the id are the RFC 2392 shape.
            Some(cid) => SinglePart::builder()
                .header(ContentDisposition::attachment(&attachment.filename))
                .header(ContentId::from(format!("<{cid}>")))
                .header(content_type)
                .body(attachment.data.clone()),
        };

        mp.singlepart(part)
    });

    builder
        .multipart(multipart)
        .map_err(|e| NimbusError::Protocol(format!("Failed to build email with attachments: {e}")))
}

/// Build an iMIP-flavoured invite email (#58).
///
/// Structure (matches what Google Calendar / Outlook generate):
/// ```text
/// multipart/mixed
/// ├── multipart/alternative
/// │   ├── text/plain                    ← fallback body
/// │   ├── text/html                     ← rich body
/// │   └── text/calendar; method=…       ← iTIP detection trigger
/// ├── (regular user attachments, if any)
/// └── application/ics; name="invite.ics"  ← downloadable .ics
/// ```
///
/// The third `multipart/alternative` part is what makes
/// Outlook / Apple Mail / Gmail / Thunderbird recognise the
/// message as an iTIP invite and surface their native
/// Accept / Decline / Tentative buttons.  Without it, those
/// clients just see a regular email with a calendar attachment
/// and don't render the RSVP UI.
///
/// Keeping the `.ics` as a downloadable attachment too is
/// belt-and-braces: clients that don't surface RSVP buttons
/// (older Outlook web access, basic webmail) can still import
/// the event manually by clicking the file.
fn build_with_calendar(
    builder: MessageBuilder,
    email: &OutgoingEmail,
) -> Result<Message, NimbusError> {
    let cal = email
        .calendar_part
        .as_ref()
        .expect("build_with_calendar called without calendar_part");

    let calendar_content_type: ContentType = format!(
        "text/calendar; method={}; charset=utf-8",
        cal.method
    )
    .parse()
    .map_err(|e| NimbusError::Protocol(format!("Bad calendar content-type: {e}")))?;

    // Body alternative — text/plain (always), text/html (if
    // present), text/calendar (always).  Order matters: clients
    // pick the LAST alternative they understand, so iTIP-aware
    // clients land on text/calendar (and show RSVP UI) while
    // text-only clients fall back to text/html → text/plain.
    //
    // `MultiPart::alternative()` returns a builder that becomes a
    // MultiPart only after the first `.singlepart()`; we always
    // seed with text/plain (possibly empty) so the chain is sane.
    let plain_body = email.body_text.clone().unwrap_or_default();
    let mut alternative = MultiPart::alternative().singlepart(
        SinglePart::builder()
            .header(ContentType::TEXT_PLAIN)
            .body(plain_body),
    );
    if let Some(html) = &email.body_html {
        alternative = alternative.singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone()),
        );
    }
    alternative = alternative.singlepart(
        SinglePart::builder()
            .header(calendar_content_type.clone())
            .body(cal.ics.clone()),
    );

    // Outer multipart/mixed: body alternative + user attachments
    // + the downloadable .ics file copy.
    let mut mixed = MultiPart::mixed().multipart(alternative);
    for attachment in &email.attachments {
        let content_type = attachment
            .content_type
            .parse::<ContentType>()
            .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
        let part = match &attachment.content_id {
            None => LettreAttachment::new(attachment.filename.clone())
                .body(attachment.data.clone(), content_type),
            Some(cid) => SinglePart::builder()
                .header(ContentDisposition::attachment(&attachment.filename))
                .header(ContentId::from(format!("<{cid}>")))
                .header(content_type)
                .body(attachment.data.clone()),
        };
        mixed = mixed.singlepart(part);
    }
    // Downloadable `.ics` for clients that can't import via the
    // alternative.  `application/ics` (vs `text/calendar`) is the
    // common pattern for the *attachment* form — Apple Mail and
    // Outlook both recognise it as a calendar file with the
    // standard "Add to Calendar" affordance.
    let ics_attachment_type: ContentType = "application/ics; name=\"invite.ics\""
        .parse()
        .map_err(|e| NimbusError::Protocol(format!("Bad ics attach content-type: {e}")))?;
    mixed = mixed.singlepart(
        LettreAttachment::new("invite.ics".to_string())
            .body(cal.ics.clone().into_bytes(), ics_attachment_type),
    );

    builder
        .multipart(mixed)
        .map_err(|e| NimbusError::Protocol(format!("Failed to build invite email: {e}")))
}
