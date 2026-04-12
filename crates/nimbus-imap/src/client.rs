//! IMAP client — connects to a mail server via TLS and provides
//! methods to interact with mailboxes.

use async_imap::Session;
use async_native_tls::TlsStream;
use async_std::net::TcpStream;
use futures::TryStreamExt;
use nimbus_core::error::NimbusError;
use nimbus_core::models::Folder;
use tracing::{debug, info};

/// An authenticated IMAP session, ready to interact with mailboxes.
///
/// # Usage
/// ```ignore
/// let client = ImapClient::connect("imap.example.com", 993, "user@example.com", "password").await?;
/// let folders = client.list_folders().await?;
/// client.logout().await?;
/// ```
pub struct ImapClient {
    /// The underlying async-imap session, wrapped in TLS.
    /// `Option` so we can take it out during logout.
    session: Option<Session<TlsStream<TcpStream>>>,
}

impl ImapClient {
    /// Connect to an IMAP server over TLS and log in.
    ///
    /// This does three things in order:
    /// 1. Opens a TCP connection to host:port
    /// 2. Wraps it in TLS (so all data is encrypted)
    /// 3. Sends LOGIN with your credentials
    ///
    /// Returns an authenticated `ImapClient` ready for use.
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
    ) -> Result<Self, NimbusError> {
        info!(host, port, username, "Connecting to IMAP server");

        // Step 1: TCP connection
        let addr = format!("{host}:{port}");
        let tcp = TcpStream::connect(&addr)
            .await
            .map_err(|e| NimbusError::Network(format!("Failed to connect to {addr}: {e}")))?;

        debug!("TCP connection established");

        // Step 2: TLS handshake — this encrypts the connection.
        // We use the hostname for certificate verification.
        let tls_connector = async_native_tls::TlsConnector::new();
        let tls_stream = tls_connector
            .connect(host, tcp)
            .await
            .map_err(|e| NimbusError::Network(format!("TLS handshake failed with {host}: {e}")))?;

        debug!("TLS handshake completed");

        // Step 3: Create the IMAP client on top of the TLS stream
        // and log in with credentials.
        let imap_client = async_imap::Client::new(tls_stream);
        let session = imap_client.login(username, password).await.map_err(|e| {
            // login() returns (error, client) on failure — we only need the error
            NimbusError::Auth(format!("IMAP login failed: {}", e.0))
        })?;

        info!("Successfully logged in as {username}");

        Ok(Self {
            session: Some(session),
        })
    }

    /// List all folders (mailboxes) on the server.
    ///
    /// Uses the IMAP `LIST` command with a wildcard to get everything.
    /// Each folder comes back with a name, hierarchy delimiter, and attributes
    /// (like \Sent, \Trash, etc.) that tell us what the folder is for.
    pub async fn list_folders(&mut self) -> Result<Vec<Folder>, NimbusError> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| NimbusError::Protocol("Session is closed".into()))?;

        // LIST "" "*" means: starting from root (""), list all folders ("*")
        // This returns an async Stream, so we collect all results with try_collect().
        let mailboxes: Vec<_> = session
            .list(Some(""), Some("*"))
            .await
            .map_err(|e| NimbusError::Protocol(format!("Failed to list folders: {e}")))?
            .try_collect()
            .await
            .map_err(|e| NimbusError::Protocol(format!("Failed to read folder list: {e}")))?;

        let folders: Vec<Folder> = mailboxes
            .iter()
            .map(|mailbox| {
                let attributes = mailbox
                    .attributes()
                    .iter()
                    .map(|attr| format!("{attr:?}"))
                    .collect();

                Folder {
                    name: mailbox.name().to_string(),
                    delimiter: mailbox.delimiter().map(|d| d.to_string()),
                    attributes,
                }
            })
            .collect();

        info!("Found {} folders", folders.len());
        for folder in &folders {
            debug!("  Folder: {} (attrs: {:?})", folder.name, folder.attributes);
        }

        Ok(folders)
    }

    /// Log out from the IMAP server and close the connection cleanly.
    ///
    /// Always call this when you're done — it sends the LOGOUT command
    /// so the server knows we're leaving properly.
    pub async fn logout(mut self) -> Result<(), NimbusError> {
        if let Some(mut session) = self.session.take() {
            session
                .logout()
                .await
                .map_err(|e| NimbusError::Protocol(format!("IMAP logout failed: {e}")))?;
            info!("Logged out from IMAP server");
        }
        Ok(())
    }
}
