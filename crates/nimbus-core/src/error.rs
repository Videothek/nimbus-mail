//! Shared error types for Nimbus.

use serde::Serialize;
use thiserror::Error;

/// The central error type used throughout all Nimbus crates.
///
/// It derives `Serialize` so that Tauri can send errors across
/// the IPC boundary to the frontend as JSON.
#[derive(Debug, Error, Serialize)]
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
