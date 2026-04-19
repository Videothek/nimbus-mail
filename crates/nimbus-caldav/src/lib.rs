//! Nimbus CalDAV — calendar sync via the CalDAV protocol.
//!
//! Provides calendar event retrieval and sync with CalDAV servers,
//! including Nextcloud Calendar. Mirrors the architecture of
//! `nimbus-carddav` (see its top-level comment): hand-rolled DAV
//! requests with `reqwest` + `quick-xml`, RFC 6578 `sync-collection`
//! for incremental sync, and a pure-parser `ical` module that turns
//! `text/calendar` bodies into flat `CalendarEvent`s.
//!
//! # Scope
//!
//! - Discovery + sync-collection + calendar-multiget (read-only).
//! - One `CalendarEvent` row per VEVENT in the cache — masters and
//!   `RECURRENCE-ID` overrides land as separate rows sharing a UID.
//!   The [`expand`] module then turns a master + overrides + a
//!   date window into concrete occurrences for the UI.
//! - UTC, all-day, and named-TZID events (via `chrono-tz`) all
//!   resolve accurately. Only unknown TZIDs and DST-gap edge cases
//!   fall back to UTC (logged at `warn`).
//! - No write path yet — `VEVENT` create / update / delete will be
//!   added alongside a calendar UI.

pub mod client;
pub mod discovery;
pub mod expand;
pub mod ical;
pub mod sync;
mod xml_util;

pub use discovery::{Calendar, list_calendars};
pub use expand::expand_event;
pub use ical::parse_ics;
pub use sync::{CalendarSyncDelta, RawEvent, sync_calendar};
