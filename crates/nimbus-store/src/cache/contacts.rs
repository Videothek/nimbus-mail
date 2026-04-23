//! Contacts cache and sync-state persistence.
//!
//! The shapes here mirror the CardDAV layer's outputs, but kept in
//! their own struct (`ContactRow`) so the store crate doesn't have to
//! depend on `nimbus-carddav`. The Tauri layer converts between the
//! two — a tiny field-for-field map.
//!
//! # Why store the raw vCard
//!
//! Two reasons:
//!
//! 1. **Forward-compat**: when we later want to expose more vCard
//!    fields (birthday, addresses, categories), we can re-extract
//!    them from the cached row without re-syncing every contact from
//!    the server. Important on big address books.
//! 2. **Round-trip safety**: if we ever add edit support, we need the
//!    exact vCard text we last sent so we can produce a sensible diff
//!    instead of regenerating from a lossy projection.

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{OptionalExtension, params};

use nimbus_core::models::{Contact, ContactAddress, ContactPhone};

use crate::cache::{Cache, CacheError};

/// One contact ready for upsert. Mirrors `nimbus_carddav::sync::RawContact`
/// without the dependency.
#[derive(Debug, Clone)]
pub struct ContactRow {
    pub href: String,
    pub etag: String,
    pub vcard_uid: String,
    pub display_name: String,
    pub emails: Vec<String>,
    /// Phone numbers paired with the vCard `TEL;TYPE=…` kind hint.
    /// Stored as JSON in `phones_json`; reads tolerate the legacy
    /// `Vec<String>` shape so existing rows keep working until the
    /// next sync rewrites them in the new shape.
    pub phones: Vec<ContactPhone>,
    pub organization: Option<String>,
    pub photo_mime: Option<String>,
    pub photo_data: Option<Vec<u8>>,
    /// Job title (vCard `TITLE`).
    pub title: Option<String>,
    /// Birthday (vCard `BDAY`) as the literal vCard string —
    /// formats vary, the UI renders verbatim.
    pub birthday: Option<String>,
    /// Free-form note (vCard `NOTE`).
    pub note: Option<String>,
    pub addresses: Vec<ContactAddress>,
    pub urls: Vec<String>,
    pub vcard_raw: String,
}

/// Sync bookmark for one addressbook.
#[derive(Debug, Clone)]
pub struct AddressbookSyncState {
    pub display_name: Option<String>,
    pub sync_token: Option<String>,
    pub ctag: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
}

/// Server-side bookkeeping for one cached contact, returned from
/// `get_contact_server_handle`. The Tauri layer needs these fields
/// to do a PUT or DELETE — the user-facing `Contact` struct hides
/// them deliberately since the UI shouldn't touch hrefs and etags.
#[derive(Debug, Clone)]
pub struct ContactServerHandle {
    pub nextcloud_account_id: String,
    pub addressbook: String,
    pub vcard_uid: String,
    pub href: String,
    pub etag: String,
    pub vcard_raw: String,
}

impl Cache {
    // ── Contacts ────────────────────────────────────────────────

    /// Apply one CardDAV sync delta in a single transaction.
    ///
    /// `upserts` are added or changed contacts; `deleted_hrefs` are
    /// resources the server reported as gone (404 in the sync-collection
    /// response). The new sync token, if any, is persisted in the
    /// `addressbook_sync_state` row alongside.
    ///
    /// All-or-nothing: a failure inside the transaction leaves the
    /// previous cache state intact, so we never half-apply a delta.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_contact_delta(
        &self,
        nc_account_id: &str,
        addressbook: &str,
        addressbook_display_name: Option<&str>,
        upserts: &[ContactRow],
        deleted_hrefs: &[String],
        new_sync_token: Option<&str>,
        new_ctag: Option<&str>,
    ) -> Result<(), CacheError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;
        let now = Utc::now().timestamp();

        // Deletes by href — match within the addressbook to avoid
        // ever accidentally clobbering a contact in another book that
        // shares the same vcard UID (rare but theoretically possible).
        if !deleted_hrefs.is_empty() {
            let mut stmt = tx.prepare(
                "DELETE FROM contacts
                 WHERE nextcloud_account_id = ?1
                   AND addressbook = ?2
                   AND href = ?3",
            )?;
            for href in deleted_hrefs {
                stmt.execute(params![nc_account_id, addressbook, href])?;
            }
        }

        if !upserts.is_empty() {
            let mut stmt = tx.prepare(
                "INSERT INTO contacts
                    (id, nextcloud_account_id, addressbook, vcard_uid, href, etag,
                     display_name, emails_json, phones_json, organization,
                     photo_mime, photo_data, vcard_raw, cached_at,
                     title, birthday, note, addresses_json, urls_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT (nextcloud_account_id, addressbook, vcard_uid) DO UPDATE SET
                    href           = excluded.href,
                    etag           = excluded.etag,
                    display_name   = excluded.display_name,
                    emails_json    = excluded.emails_json,
                    phones_json    = excluded.phones_json,
                    organization   = excluded.organization,
                    photo_mime     = excluded.photo_mime,
                    photo_data     = excluded.photo_data,
                    vcard_raw      = excluded.vcard_raw,
                    cached_at      = excluded.cached_at,
                    title          = excluded.title,
                    birthday       = excluded.birthday,
                    note           = excluded.note,
                    addresses_json = excluded.addresses_json,
                    urls_json      = excluded.urls_json",
            )?;
            for c in upserts {
                let id = format!("{nc_account_id}::{}", c.vcard_uid);
                let emails = serde_json::to_string(&c.emails).unwrap_or_else(|_| "[]".into());
                let phones = serde_json::to_string(&c.phones).unwrap_or_else(|_| "[]".into());
                let addresses =
                    serde_json::to_string(&c.addresses).unwrap_or_else(|_| "[]".into());
                let urls = serde_json::to_string(&c.urls).unwrap_or_else(|_| "[]".into());
                stmt.execute(params![
                    id,
                    nc_account_id,
                    addressbook,
                    c.vcard_uid,
                    c.href,
                    c.etag,
                    c.display_name,
                    emails,
                    phones,
                    c.organization,
                    c.photo_mime,
                    c.photo_data,
                    c.vcard_raw,
                    now,
                    c.title,
                    c.birthday,
                    c.note,
                    addresses,
                    urls,
                ])?;
            }
        }

        // Sync state — upsert the bookmark even when the delta itself
        // was empty, so an empty incremental run still bumps
        // last_synced_at.
        tx.execute(
            "INSERT INTO addressbook_sync_state
                (nextcloud_account_id, addressbook, display_name, sync_token, ctag, last_synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (nextcloud_account_id, addressbook) DO UPDATE SET
                display_name   = COALESCE(excluded.display_name, addressbook_sync_state.display_name),
                sync_token     = COALESCE(excluded.sync_token, addressbook_sync_state.sync_token),
                ctag           = COALESCE(excluded.ctag, addressbook_sync_state.ctag),
                last_synced_at = excluded.last_synced_at",
            params![
                nc_account_id,
                addressbook,
                addressbook_display_name,
                new_sync_token,
                new_ctag,
                now,
            ],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Most-recent `last_synced_at` across every addressbook for the
    /// given Nextcloud account, in UTC. `Ok(None)` means we've never
    /// completed a sync for this account — the settings UI uses that
    /// to show "Never synced" rather than a misleading "0s ago".
    pub fn latest_addressbook_sync_at(
        &self,
        nc_account_id: &str,
    ) -> Result<Option<DateTime<Utc>>, CacheError> {
        let conn = self.pool.get()?;
        let ts: Option<i64> = conn
            .query_row(
                "SELECT MAX(last_synced_at)
                 FROM addressbook_sync_state
                 WHERE nextcloud_account_id = ?1",
                params![nc_account_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(ts.and_then(|t| Utc.timestamp_opt(t, 0).single()))
    }

    /// Read the addressbook sync bookmark, if any.
    pub fn get_addressbook_sync_state(
        &self,
        nc_account_id: &str,
        addressbook: &str,
    ) -> Result<Option<AddressbookSyncState>, CacheError> {
        let conn = self.pool.get()?;
        let row = conn
            .query_row(
                "SELECT display_name, sync_token, ctag, last_synced_at
                 FROM addressbook_sync_state
                 WHERE nextcloud_account_id = ?1 AND addressbook = ?2",
                params![nc_account_id, addressbook],
                |r| {
                    let ts: Option<i64> = r.get(3)?;
                    Ok(AddressbookSyncState {
                        display_name: r.get(0)?,
                        sync_token: r.get(1)?,
                        ctag: r.get(2)?,
                        last_synced_at: ts.and_then(|t| Utc.timestamp_opt(t, 0).single()),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// All contacts, alphabetised, optionally scoped to a single
    /// Nextcloud account. Powers the contacts list view.
    ///
    /// **Deliberately omits photo bytes** — `photo_data` is always
    /// returned as `None`. Photos can be 50–500 KB each and Tauri
    /// serialises them as JSON number arrays (3–4× bloat), so
    /// shipping them in the list payload turns a 200-contact
    /// addressbook into 30+ MB of IPC traffic. `photo_mime` is kept
    /// as a presence flag; the UI uses `get_contact_photo` to fetch
    /// the bytes on demand for whichever rows it actually paints.
    pub fn list_contacts(&self, nc_account_id: Option<&str>) -> Result<Vec<Contact>, CacheError> {
        let conn = self.pool.get()?;
        let mut stmt;
        let rows = match nc_account_id {
            Some(nc) => {
                stmt = conn.prepare(
                    "SELECT id, nextcloud_account_id, display_name, emails_json,
                            phones_json, organization, photo_mime,
                            title, birthday, note, addresses_json, urls_json
                     FROM contacts
                     WHERE nextcloud_account_id = ?1
                     ORDER BY display_name COLLATE NOCASE",
                )?;
                stmt.query_map(params![nc], row_to_contact_no_photo)?
            }
            None => {
                stmt = conn.prepare(
                    "SELECT id, nextcloud_account_id, display_name, emails_json,
                            phones_json, organization, photo_mime,
                            title, birthday, note, addresses_json, urls_json
                     FROM contacts
                     ORDER BY display_name COLLATE NOCASE",
                )?;
                stmt.query_map([], row_to_contact_no_photo)?
            }
        };
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch one contact's photo bytes by app-side id. Returns
    /// `Ok(None)` when the contact has no photo (or doesn't exist),
    /// so the UI can render its initial-letter placeholder without a
    /// distinct error path.
    pub fn get_contact_photo(
        &self,
        contact_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, CacheError> {
        let conn = self.pool.get()?;
        let row: Option<(Option<String>, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT photo_mime, photo_data FROM contacts WHERE id = ?1",
                params![contact_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(mime, bytes)| match (mime, bytes) {
            (Some(m), Some(b)) if !b.is_empty() => Some((m, b)),
            _ => None,
        }))
    }

    /// Substring search over name + email for autocomplete.
    ///
    /// Matches `display_name` OR any email containing `query` (case
    /// insensitive). Excludes rows with no email addresses — the
    /// compose autocomplete needs *something* to fill into the field,
    /// so a phone-only contact is just noise here. Caps results at
    /// `limit` so a typo that matches half the address book doesn't
    /// tank the UI.
    pub fn search_contacts(&self, query: &str, limit: u32) -> Result<Vec<Contact>, CacheError> {
        let conn = self.pool.get()?;
        let needle = format!("%{}%", query.replace('%', r"\%").replace('_', r"\_"));
        // emails_json is the stringified JSON array. "[]" is the
        // canonical empty form (see apply_contact_delta), so excluding
        // it filters phone/photo-only rows reliably.
        //
        // Same photo-omission story as `list_contacts` — autocomplete
        // doesn't render avatars, so shipping bytes is pure waste.
        let mut stmt = conn.prepare(
            "SELECT id, nextcloud_account_id, display_name, emails_json,
                    phones_json, organization, photo_mime,
                    title, birthday, note, addresses_json, urls_json
             FROM contacts
             WHERE emails_json != '[]'
               AND (display_name LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                    OR emails_json  LIKE ?1 ESCAPE '\\' COLLATE NOCASE)
             ORDER BY display_name COLLATE NOCASE
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![needle, limit as i64], row_to_contact_no_photo)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Number of contacts cached for a Nextcloud account — cheap to
    /// surface in the Settings UI alongside "Sync now".
    pub fn count_contacts(&self, nc_account_id: &str) -> Result<u32, CacheError> {
        let conn = self.pool.get()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM contacts WHERE nextcloud_account_id = ?1",
            params![nc_account_id],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Look up the server-side handle (href + etag + addressbook + raw
    /// vCard) for a single cached contact by its app-side id.
    ///
    /// Returns `Ok(None)` if the row isn't cached — the caller treats
    /// that as "stale UI; trigger a refresh and try again".
    pub fn get_contact_server_handle(
        &self,
        contact_id: &str,
    ) -> Result<Option<ContactServerHandle>, CacheError> {
        let conn = self.pool.get()?;
        let row = conn
            .query_row(
                "SELECT nextcloud_account_id, addressbook, vcard_uid, href, etag, vcard_raw
                 FROM contacts
                 WHERE id = ?1",
                params![contact_id],
                |r| {
                    Ok(ContactServerHandle {
                        nextcloud_account_id: r.get(0)?,
                        addressbook: r.get(1)?,
                        vcard_uid: r.get(2)?,
                        href: r.get(3)?,
                        etag: r.get(4)?,
                        vcard_raw: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Insert (or replace) a single contact row outside the
    /// sync-collection delta path. Used by the create/update Tauri
    /// commands after a successful PUT to Nextcloud — we already
    /// have the new etag and don't want to wait for the next sync
    /// to see our own write.
    ///
    /// Does not touch `addressbook_sync_state`; the next regular
    /// sync will move the token forward and will simply find no
    /// changes for the row we just wrote (or report it as our own
    /// edit, also fine).
    pub fn upsert_single_contact(
        &self,
        nc_account_id: &str,
        addressbook: &str,
        row: &ContactRow,
    ) -> Result<(), CacheError> {
        let conn = self.pool.get()?;
        let id = format!("{nc_account_id}::{}", row.vcard_uid);
        let emails = serde_json::to_string(&row.emails).unwrap_or_else(|_| "[]".into());
        let phones = serde_json::to_string(&row.phones).unwrap_or_else(|_| "[]".into());
        let addresses = serde_json::to_string(&row.addresses).unwrap_or_else(|_| "[]".into());
        let urls = serde_json::to_string(&row.urls).unwrap_or_else(|_| "[]".into());
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO contacts
                (id, nextcloud_account_id, addressbook, vcard_uid, href, etag,
                 display_name, emails_json, phones_json, organization,
                 photo_mime, photo_data, vcard_raw, cached_at,
                 title, birthday, note, addresses_json, urls_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT (nextcloud_account_id, addressbook, vcard_uid) DO UPDATE SET
                href           = excluded.href,
                etag           = excluded.etag,
                display_name   = excluded.display_name,
                emails_json    = excluded.emails_json,
                phones_json    = excluded.phones_json,
                organization   = excluded.organization,
                photo_mime     = excluded.photo_mime,
                photo_data     = excluded.photo_data,
                vcard_raw      = excluded.vcard_raw,
                cached_at      = excluded.cached_at,
                title          = excluded.title,
                birthday       = excluded.birthday,
                note           = excluded.note,
                addresses_json = excluded.addresses_json,
                urls_json      = excluded.urls_json",
            params![
                id,
                nc_account_id,
                addressbook,
                row.vcard_uid,
                row.href,
                row.etag,
                row.display_name,
                emails,
                phones,
                row.organization,
                row.photo_mime,
                row.photo_data,
                row.vcard_raw,
                now,
                row.title,
                row.birthday,
                row.note,
                addresses,
                urls,
            ],
        )?;
        Ok(())
    }

    /// Remove one contact by its app-side id.
    pub fn delete_contact_by_id(&self, contact_id: &str) -> Result<(), CacheError> {
        let conn = self.pool.get()?;
        conn.execute("DELETE FROM contacts WHERE id = ?1", params![contact_id])?;
        Ok(())
    }

    /// Drop all contacts and sync state for a Nextcloud account —
    /// called when the user disconnects that account.
    pub fn wipe_nextcloud_contacts(&self, nc_account_id: &str) -> Result<(), CacheError> {
        let conn = self.pool.get()?;
        conn.execute(
            "DELETE FROM contacts WHERE nextcloud_account_id = ?1",
            params![nc_account_id],
        )?;
        conn.execute(
            "DELETE FROM addressbook_sync_state WHERE nextcloud_account_id = ?1",
            params![nc_account_id],
        )?;
        Ok(())
    }
}

/// Map a row that excludes the `photo_data` column. `photo_mime` is
/// kept (column index 6) so the UI knows whether a photo exists
/// without having to ship the bytes; `photo_data` is forced to
/// `None`. Pair with the SELECT lists in `list_contacts` and
/// `search_contacts`.
fn row_to_contact_no_photo(r: &rusqlite::Row<'_>) -> rusqlite::Result<Contact> {
    let emails_json: String = r.get(3)?;
    let phones_json: String = r.get(4)?;
    let addresses_json: String = r.get(10)?;
    let urls_json: String = r.get(11)?;
    Ok(Contact {
        id: r.get(0)?,
        nextcloud_account_id: r.get(1)?,
        display_name: r.get(2)?,
        email: serde_json::from_str(&emails_json).unwrap_or_default(),
        phone: decode_phones(&phones_json),
        organization: r.get(5)?,
        photo_mime: r.get(6)?,
        photo_data: None,
        title: r.get(7)?,
        birthday: r.get(8)?,
        note: r.get(9)?,
        addresses: serde_json::from_str(&addresses_json).unwrap_or_default(),
        urls: serde_json::from_str(&urls_json).unwrap_or_default(),
    })
}

/// Read `phones_json` tolerantly. The new shape is `[{kind, value}]`
/// (vCard `TEL;TYPE=…`); rows written before this column was typed
/// have the old `[String]` shape, which we lift to a typed array
/// with `kind = "other"` so no number ever vanishes from the UI.
/// On the next sync, the rewrite from CardDAV puts the proper kind
/// in place.
fn decode_phones(json: &str) -> Vec<ContactPhone> {
    if let Ok(typed) = serde_json::from_str::<Vec<ContactPhone>>(json) {
        return typed;
    }
    if let Ok(plain) = serde_json::from_str::<Vec<String>>(json) {
        return plain
            .into_iter()
            .map(|v| ContactPhone {
                kind: "other".to_string(),
                value: v,
            })
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{pool, schema};

    fn open_test_cache() -> Cache {
        let pool = pool::open_memory_pool().expect("open memory pool");
        let mut conn = pool.get().expect("checkout");
        schema::run_migrations(&mut conn).expect("migrate");
        drop(conn);
        Cache { pool }
    }

    fn row(uid: &str, name: &str, email: &str) -> ContactRow {
        ContactRow {
            href: format!("/dav/{uid}.vcf"),
            etag: format!("etag-{uid}"),
            vcard_uid: uid.into(),
            display_name: name.into(),
            emails: vec![email.into()],
            phones: vec![],
            organization: None,
            photo_mime: None,
            photo_data: None,
            title: None,
            birthday: None,
            note: None,
            addresses: Vec::new(),
            urls: Vec::new(),
            vcard_raw: format!("BEGIN:VCARD\r\nUID:{uid}\r\nEND:VCARD\r\n"),
        }
    }

    #[test]
    fn search_excludes_contacts_without_email() {
        let cache = open_test_cache();
        let phone_only = ContactRow {
            emails: vec![],
            phones: vec![ContactPhone {
                kind: "cell".into(),
                value: "+1 555 1234".into(),
            }],
            ..row("u9", "Phone Only", "")
        };
        cache
            .apply_contact_delta("nc1", "contacts", None, &[phone_only], &[], None, None)
            .unwrap();
        // Substring of the display name still finds nothing because the
        // row has no emails to autocomplete.
        assert!(cache.search_contacts("phone", 10).unwrap().is_empty());
    }

    #[test]
    fn upsert_then_search_finds_by_name_and_email() {
        let cache = open_test_cache();
        let upserts = vec![
            row("u1", "Alice Wonder", "alice@example.com"),
            row("u2", "Bob Marley", "bob@reggae.com"),
        ];
        cache
            .apply_contact_delta(
                "nc1",
                "contacts",
                Some("Contacts"),
                &upserts,
                &[],
                Some("tok-1"),
                Some("c1"),
            )
            .unwrap();

        // Hit by name
        let r = cache.search_contacts("alice", 10).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].display_name, "Alice Wonder");

        // Hit by email
        let r = cache.search_contacts("reggae", 10).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].email, vec!["bob@reggae.com"]);

        // count_contacts
        assert_eq!(cache.count_contacts("nc1").unwrap(), 2);

        // sync state stuck around
        let s = cache
            .get_addressbook_sync_state("nc1", "contacts")
            .unwrap()
            .unwrap();
        assert_eq!(s.sync_token.as_deref(), Some("tok-1"));
        assert_eq!(s.ctag.as_deref(), Some("c1"));
        assert_eq!(s.display_name.as_deref(), Some("Contacts"));
    }

    #[test]
    fn delta_applies_deletes_and_updates() {
        let cache = open_test_cache();
        cache
            .apply_contact_delta(
                "nc1",
                "contacts",
                None,
                &[row("u1", "Alice", "a@x.com")],
                &[],
                Some("t1"),
                None,
            )
            .unwrap();

        // Update Alice and delete u1's href in the same delta — no, that
        // contradicts; do them separately. Update first.
        cache
            .apply_contact_delta(
                "nc1",
                "contacts",
                None,
                &[ContactRow {
                    display_name: "Alice Updated".into(),
                    ..row("u1", "Alice", "a@x.com")
                }],
                &[],
                Some("t2"),
                None,
            )
            .unwrap();

        let after_update = cache.list_contacts(Some("nc1")).unwrap();
        assert_eq!(after_update[0].display_name, "Alice Updated");

        // Now delete by the href the row was stored at.
        cache
            .apply_contact_delta(
                "nc1",
                "contacts",
                None,
                &[],
                &["/dav/u1.vcf".into()],
                Some("t3"),
                None,
            )
            .unwrap();

        assert_eq!(cache.count_contacts("nc1").unwrap(), 0);
    }

    #[test]
    fn wipe_clears_sync_state_too() {
        let cache = open_test_cache();
        cache
            .apply_contact_delta(
                "nc1",
                "contacts",
                None,
                &[row("u1", "x", "x@x.com")],
                &[],
                Some("t"),
                None,
            )
            .unwrap();
        cache.wipe_nextcloud_contacts("nc1").unwrap();
        assert_eq!(cache.count_contacts("nc1").unwrap(), 0);
        assert!(
            cache
                .get_addressbook_sync_state("nc1", "contacts")
                .unwrap()
                .is_none()
        );
    }
}
