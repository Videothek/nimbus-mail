//! Shared error types for Nimbus.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NimbusError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Nextcloud API error: {0}")]
    Nextcloud(String),

    #[error("{0}")]
    Other(String),
}
