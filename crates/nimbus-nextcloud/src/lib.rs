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
//! - **Files** (`files`) — WebDAV browse / download for "attach from
//!   Nextcloud". Public shares (`shares`) for "send as link".
//! - **Talk** (`talk`) — list, create, and add participants to Talk
//!   rooms. The "create Talk room from email thread" flow lives here.

pub mod auth;
pub mod capabilities;
pub mod client;
pub mod files;
pub mod shares;
pub mod talk;

pub use auth::{LoginFlowInit, LoginFlowResult, poll_login, start_login};
pub use capabilities::fetch_capabilities;
pub use files::{FileEntry, create_directory, download_file, list_directory, upload_file};
pub use shares::{PublicShare, create_public_share};
pub use talk::{ParticipantSource, RoomType, TalkRoom, add_participant, create_room, list_rooms};
