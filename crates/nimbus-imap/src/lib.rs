//! Nimbus IMAP — handles mail retrieval over IMAP.
//!
//! This crate provides async IMAP connectivity for fetching,
//! syncing, and managing mailboxes.

mod client;

pub use client::ImapClient;
