//! Nimbus Nextcloud — integration with Nextcloud APIs.
//!
//! # What this crate does
//!
//! - **Login Flow v2** (`auth`) — the modern, browser-based auth flow
//!   used by the official Nextcloud desktop client. No passwords ever
//!   touch the app: the user authorises in their browser and the server
//!   hands us back a revocable *app password*.
//! - **Capability detection** (`capabilities`) — asks the server which
//!   apps (Talk, Files, CalDAV, CardDAV) are installed so the UI can
//!   show or hide features accordingly.
//! - **Talk / Files** (stubs for now) — filled in as their own issues.

pub mod auth;
pub mod capabilities;
pub mod client;
pub mod files;
pub mod talk;

pub use auth::{LoginFlowInit, LoginFlowResult, poll_login, start_login};
pub use capabilities::fetch_capabilities;
