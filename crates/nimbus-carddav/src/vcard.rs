//! vCard → flat struct mapping.
//!
//! We use the `ical` crate's vCard parser to handle the painful parts
//! (line folding, encoded values, escape sequences) and walk the
//! resulting properties to extract the handful of fields we care
//! about.
//!
//! # Field selection
//!
//! Outlook-style autocomplete needs name + email + photo at minimum;
//! we also keep phone numbers and the organisation since they're
//! cheap to grab and useful for the contact card. Birthday, address,
//! categories etc. live in `vcard_raw` for now — the row stays
//! re-extractable when we build a richer contact view later.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ical::parser::vcard::VcardParser;
use ical::property::Property;

use nimbus_core::NimbusError;

/// The fields we lift out of a vCard. `uid` is required by RFC 6350,
/// so a missing UID makes the vCard unusable for sync (we have no
/// stable identifier) and we surface that as an error rather than
/// fabricating one — the caller will skip and warn.
#[derive(Debug, Clone)]
pub struct ParsedVcard {
    pub uid: String,
    pub display_name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub organization: Option<String>,
    pub photo_mime: Option<String>,
    pub photo_data: Option<Vec<u8>>,
}

/// Parse a single vCard string. The input is the raw `BEGIN:VCARD … END:VCARD`
/// block; the `ical` parser returns at most one card from it.
pub fn parse_vcard(raw: &str) -> Result<ParsedVcard, NimbusError> {
    let reader = std::io::BufReader::new(raw.as_bytes());
    let mut parser = VcardParser::new(reader);

    let card = parser
        .next()
        .ok_or_else(|| NimbusError::Protocol("empty vCard".to_string()))?
        .map_err(|e| NimbusError::Protocol(format!("vCard parse: {e}")))?;

    let mut uid: Option<String> = None;
    let mut formatted_name = String::new();
    let mut structured_name = String::new();
    let mut emails: Vec<String> = Vec::new();
    let mut phones: Vec<String> = Vec::new();
    let mut organization: Option<String> = None;
    let mut photo_mime: Option<String> = None;
    let mut photo_data: Option<Vec<u8>> = None;

    for prop in &card.properties {
        let name = prop.name.to_ascii_uppercase();
        let Some(value) = &prop.value else { continue };
        match name.as_str() {
            "UID" => uid = Some(value.clone()),
            "FN" => formatted_name = value.clone(),
            "N" => {
                // N is Family;Given;Additional;Prefix;Suffix
                let parts: Vec<&str> = value.split(';').collect();
                let given = parts.get(1).copied().unwrap_or("").trim();
                let family = parts.first().copied().unwrap_or("").trim();
                structured_name = format!("{given} {family}").trim().to_string();
            }
            "EMAIL" => {
                let v = value.trim().to_string();
                if !v.is_empty() && !emails.contains(&v) {
                    emails.push(v);
                }
            }
            "TEL" => {
                let v = value.trim().to_string();
                if !v.is_empty() && !phones.contains(&v) {
                    phones.push(v);
                }
            }
            "ORG" => {
                // ORG is Company;Department;... — first segment is the
                // organisation proper.
                let first = value.split(';').next().unwrap_or("").trim().to_string();
                if !first.is_empty() {
                    organization = Some(first);
                }
            }
            "PHOTO" => {
                if let Some((mime, bytes)) = decode_photo(prop, value) {
                    photo_mime = Some(mime);
                    photo_data = Some(bytes);
                }
            }
            _ => {}
        }
    }

    // Prefer FN (formatted name) — it's what RFC 6350 says clients
    // should display. Fall back to N → first email → "(unnamed)".
    let display_name = if !formatted_name.is_empty() {
        formatted_name
    } else if !structured_name.is_empty() {
        structured_name
    } else if let Some(first) = emails.first() {
        first.clone()
    } else {
        "(unnamed)".to_string()
    };

    let uid = uid.ok_or_else(|| NimbusError::Protocol("vCard missing UID".to_string()))?;

    Ok(ParsedVcard {
        uid,
        display_name,
        emails,
        phones,
        organization,
        photo_mime,
        photo_data,
    })
}

/// Decode a PHOTO property into `(mime, bytes)`.
///
/// Two shapes show up in the wild:
///
/// - **vCard 3 inline:** `PHOTO;ENCODING=b;TYPE=JPEG:<base64>` — the
///   value is base64 text, the type comes from a TYPE param.
/// - **vCard 4 data URI:** `PHOTO:data:image/jpeg;base64,<base64>` —
///   the value embeds both mime and bytes.
///
/// External URLs (`PHOTO:https://…`) are skipped — we don't fetch
/// them in this pass.
fn decode_photo(prop: &Property, value: &str) -> Option<(String, Vec<u8>)> {
    // vCard 4 data URI form.
    if let Some(rest) = value.strip_prefix("data:") {
        let (meta, b64) = rest.split_once(',')?;
        let mime = meta
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = BASE64.decode(b64).ok()?;
        return Some((mime, bytes));
    }
    // vCard 3 inline form — value is bare base64, type/encoding in params.
    let mut is_base64 = false;
    let mut mime = "image/jpeg".to_string(); // safe default for NC
    if let Some(params) = &prop.params {
        for (key, vals) in params {
            let upper = key.to_ascii_uppercase();
            if upper == "ENCODING" {
                if vals
                    .iter()
                    .any(|v| matches!(v.to_ascii_lowercase().as_str(), "b" | "base64"))
                {
                    is_base64 = true;
                }
            } else if upper == "TYPE"
                && let Some(t) = vals.first()
            {
                let t = t.to_ascii_lowercase();
                if !t.is_empty() {
                    mime = if t.starts_with("image/") {
                        t
                    } else {
                        format!("image/{t}")
                    };
                }
            }
        }
    }
    if !is_base64 {
        return None;
    }
    let cleaned: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = BASE64.decode(cleaned).ok()?;
    Some((mime, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_vcard() {
        let raw = "BEGIN:VCARD\r\n\
                   VERSION:3.0\r\n\
                   UID:abc-123\r\n\
                   FN:Alice Example\r\n\
                   EMAIL;TYPE=INTERNET:alice@example.com\r\n\
                   TEL;TYPE=CELL:+1 555 0100\r\n\
                   ORG:Example Corp;Engineering\r\n\
                   END:VCARD\r\n";
        let p = parse_vcard(raw).unwrap();
        assert_eq!(p.uid, "abc-123");
        assert_eq!(p.display_name, "Alice Example");
        assert_eq!(p.emails, vec!["alice@example.com"]);
        assert_eq!(p.phones, vec!["+1 555 0100"]);
        assert_eq!(p.organization.as_deref(), Some("Example Corp"));
        assert!(p.photo_data.is_none());
    }

    #[test]
    fn falls_back_to_n_when_fn_absent() {
        let raw = "BEGIN:VCARD\r\n\
                   VERSION:3.0\r\n\
                   UID:nofn\r\n\
                   N:Smith;Bob;;;\r\n\
                   END:VCARD\r\n";
        let p = parse_vcard(raw).unwrap();
        assert_eq!(p.display_name, "Bob Smith");
    }

    #[test]
    fn missing_uid_is_an_error() {
        let raw = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nEND:VCARD\r\n";
        assert!(parse_vcard(raw).is_err());
    }

    #[test]
    fn decodes_data_uri_photo() {
        // 1x1 GIF, base64.
        let raw = "BEGIN:VCARD\r\n\
                   VERSION:4.0\r\n\
                   UID:p1\r\n\
                   FN:With Photo\r\n\
                   PHOTO:data:image/gif;base64,R0lGODlhAQABAAAAACw=\r\n\
                   END:VCARD\r\n";
        let p = parse_vcard(raw).unwrap();
        assert_eq!(p.photo_mime.as_deref(), Some("image/gif"));
        assert!(!p.photo_data.unwrap().is_empty());
    }
}
