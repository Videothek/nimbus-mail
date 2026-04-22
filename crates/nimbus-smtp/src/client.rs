//! SMTP client — connects to a mail server and sends emails.

use lettre::message::{
    Attachment as LettreAttachment, Mailbox, MessageBuilder, MultiPart, SinglePart,
    header::ContentType,
};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use nimbus_core::error::NimbusError;
use nimbus_core::models::OutgoingEmail;
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
    ) -> Result<Self, NimbusError> {
        info!(host, port, username, "Connecting to SMTP server");

        let credentials = Credentials::new(username.to_string(), password.to_string());

        // Port 465 uses implicit TLS (wrapped from the start).
        // Port 587 (and others) use STARTTLS (upgrade after connecting).
        let transport = if port == 465 {
            debug!("Using implicit TLS (port 465)");
            AsyncSmtpTransport::<Tokio1Executor>::relay(host)
                .map_err(|e| NimbusError::Network(format!("Failed to create SMTP relay: {e}")))?
                .port(port)
                .credentials(credentials)
                .build()
        } else {
            debug!("Using STARTTLS (port {port})");
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
                .map_err(|e| NimbusError::Network(format!("Failed to create STARTTLS relay: {e}")))?
                .port(port)
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
        .from(from_mailbox)
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

    if email.attachments.is_empty() {
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

        let lettre_attachment = LettreAttachment::new(attachment.filename.clone())
            .body(attachment.data.clone(), content_type);

        mp.singlepart(lettre_attachment)
    });

    builder
        .multipart(multipart)
        .map_err(|e| NimbusError::Protocol(format!("Failed to build email with attachments: {e}")))
}
