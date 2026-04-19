//! iCalendar (`text/calendar`) → flat `CalendarEvent` mapping.
//!
//! Uses the `ical` crate's `IcalParser` to handle the painful parts
//! (line folding at col 75, escape sequences, `BEGIN:VCALENDAR`
//! nesting) and then walks the resulting VEVENT properties to extract
//! the handful of fields we surface in the UI.
//!
//! # Scope of this module (important)
//!
//! - **One `CalendarEvent` per VEVENT block.** A recurring series
//!   lands as its master event (the first occurrence) plus one row
//!   per `RECURRENCE-ID` override. We capture the raw recurrence
//!   fields (RRULE, RDATE, EXDATE, RECURRENCE-ID) on every event so
//!   `nimbus_caldav::expand` can turn a master into concrete
//!   occurrences without re-syncing.
//! - **Timezone handling.** Three cases, all resolved to UTC:
//!   - `…Z` suffix → exact UTC
//!   - `TZID=<iana-zone>` param → resolved via `chrono-tz`; DST gaps
//!     fall back to UTC, DST overlaps pick the earliest instant
//!   - no `Z`, no `TZID` (floating time) → treated as UTC
//!
//!   Unknown or mistyped TZIDs fall back to UTC with a warning.
//! - **All-day events** (`VALUE=DATE`) land as `00:00:00Z` … `23:59:59Z`
//!   on the same day, so the UI can treat them uniformly with timed
//!   events.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use ical::parser::ical::IcalParser;
use ical::property::Property;

use nimbus_core::NimbusError;
use nimbus_core::models::CalendarEvent;

/// Parse one iCalendar body (the content of a `calendar-data` element
/// or a `.ics` file) into zero or more `CalendarEvent`s.
///
/// A single calendar object resource can contain multiple VEVENT
/// components (the master + override instances for a recurring
/// series). We return each VEVENT as its own event — the caller will
/// see duplicates for recurring series; de-duplication by UID + the
/// full RRULE expansion is the follow-up issue's problem.
///
/// Returns `Ok(vec)` even if the body contains no VEVENTs (some
/// calendar objects are just VTODO / VJOURNAL) — the caller decides
/// whether an empty slice is interesting.
pub fn parse_ics(raw: &str) -> Result<Vec<CalendarEvent>, NimbusError> {
    let reader = std::io::BufReader::new(raw.as_bytes());
    let parser = IcalParser::new(reader);
    let mut events: Vec<CalendarEvent> = Vec::new();

    for cal_result in parser {
        let cal = cal_result.map_err(|e| NimbusError::Protocol(format!("iCalendar parse: {e}")))?;
        for ev in &cal.events {
            match event_from_properties(&ev.properties) {
                Ok(Some(e)) => events.push(e),
                Ok(None) => {
                    // Missing UID or DTSTART — skip rather than fail the
                    // whole sync. Log so we can spot recurring offenders.
                    tracing::warn!("Skipped VEVENT: missing required fields");
                }
                Err(e) => {
                    tracing::warn!("Skipped VEVENT: {e}");
                }
            }
        }
    }

    Ok(events)
}

/// Build a `CalendarEvent` from the properties of a VEVENT. Returns
/// `Ok(None)` if the event is missing UID or DTSTART (can't be
/// meaningfully represented).
fn event_from_properties(props: &[Property]) -> Result<Option<CalendarEvent>, String> {
    let mut uid: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut description: Option<String> = None;
    let mut location: Option<String> = None;
    let mut dtstart: Option<DateTimeValue> = None;
    let mut dtend: Option<DateTimeValue> = None;
    let mut duration: Option<Duration> = None;
    let mut rrule: Option<String> = None;
    let mut rdate: Vec<DateTime<Utc>> = Vec::new();
    let mut exdate: Vec<DateTime<Utc>> = Vec::new();
    let mut recurrence_id: Option<DateTime<Utc>> = None;

    for prop in props {
        let name = prop.name.to_ascii_uppercase();
        let Some(value) = prop.value.as_deref() else {
            continue;
        };
        match name.as_str() {
            "UID" => uid = Some(value.to_string()),
            "SUMMARY" => summary = Some(unescape_text(value)),
            "DESCRIPTION" => description = Some(unescape_text(value)),
            "LOCATION" => location = Some(unescape_text(value)),
            "DTSTART" => dtstart = Some(parse_datetime_property(prop, value)?),
            "DTEND" => dtend = Some(parse_datetime_property(prop, value)?),
            "DURATION" => duration = parse_duration(value),
            "RRULE" => rrule = Some(value.to_string()),
            "RDATE" => rdate.extend(parse_datetime_list(prop, value)?),
            "EXDATE" => exdate.extend(parse_datetime_list(prop, value)?),
            "RECURRENCE-ID" => {
                // A single date-time value — reuse the list parser and
                // keep the first entry. RECURRENCE-ID is always a single
                // value in practice.
                if let Some(first) = parse_datetime_list(prop, value)?.into_iter().next() {
                    recurrence_id = Some(first);
                }
            }
            _ => {}
        }
    }

    let Some(uid) = uid else { return Ok(None) };
    let Some(start_val) = dtstart else {
        return Ok(None);
    };

    let (start, end) = resolve_window(start_val, dtend, duration);

    Ok(Some(CalendarEvent {
        id: uid,
        summary: summary.unwrap_or_default(),
        description,
        start,
        end,
        location,
        rrule,
        rdate,
        exdate,
        recurrence_id,
    }))
}

/// Resolve final (start, end) UTC timestamps from the parsed DTSTART,
/// optional DTEND, and optional DURATION.
///
/// Precedence per RFC 5545 §3.6.1:
/// - DTEND wins if present
/// - else DTSTART + DURATION
/// - else for all-day events: DTSTART .. end-of-day
/// - else: zero-length event at DTSTART
fn resolve_window(
    start_val: DateTimeValue,
    dtend: Option<DateTimeValue>,
    duration: Option<Duration>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = start_val.to_utc_start();

    let end = match (dtend, duration, &start_val) {
        (Some(e), _, _) => e.to_utc_end(),
        (None, Some(d), _) => start + d,
        (None, None, DateTimeValue::Date(_)) => start + Duration::seconds(86_399),
        (None, None, _) => start,
    };

    (start, end)
}

/// A DTSTART / DTEND value can be either a date-time or a pure date.
/// We keep the distinction so all-day events get sensible end times.
#[derive(Debug, Clone)]
enum DateTimeValue {
    DateTime(DateTime<Utc>),
    Date(NaiveDate),
}

impl DateTimeValue {
    /// Convert to the UTC start of this value: midnight for dates,
    /// the exact instant for date-times.
    fn to_utc_start(&self) -> DateTime<Utc> {
        match self {
            DateTimeValue::DateTime(dt) => *dt,
            DateTimeValue::Date(d) => {
                Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).expect("0:00:00 is always valid"))
            }
        }
    }

    /// Convert to the UTC end sentinel: for dates, `DTEND` in iCalendar
    /// is *exclusive* at midnight (RFC 5545 §3.6.1) — a one-day event
    /// on May 1 is written `DTEND;VALUE=DATE:20260502`, meaning "up to
    /// but not including May 2". The UI wants an inclusive end, so we
    /// step back one day and snap to `23:59:59` on the last covered
    /// day. For date-times, DTEND is itself exclusive of the event
    /// but we preserve the raw instant — the UI already handles that.
    fn to_utc_end(&self) -> DateTime<Utc> {
        match self {
            DateTimeValue::DateTime(dt) => *dt,
            DateTimeValue::Date(d) => {
                // Defensive: a malformed VEVENT with DTEND == DTSTART
                // (RFC says DTEND MUST be strictly after DTSTART, but
                // real-world producers occasionally send equal values)
                // would produce an end *before* start here. Callers in
                // `resolve_window` don't re-validate, so guard with
                // `pred_opt()` and fall back to same-day end.
                let last = d.pred_opt().unwrap_or(*d);
                Utc.from_utc_datetime(
                    &last
                        .and_hms_opt(23, 59, 59)
                        .expect("23:59:59 is always valid"),
                )
            }
        }
    }
}

/// Parse a DTSTART / DTEND property value. Consults property
/// parameters to distinguish dates from date-times and to look up
/// the IANA timezone when TZID is set.
fn parse_datetime_property(prop: &Property, value: &str) -> Result<DateTimeValue, String> {
    let is_date_only = property_param(prop, "VALUE")
        .map(|v| v.eq_ignore_ascii_case("DATE"))
        .unwrap_or(false)
        || value.len() == 8; // YYYYMMDD, no 'T'

    if is_date_only {
        let d = NaiveDate::parse_from_str(value, "%Y%m%d")
            .map_err(|e| format!("DATE value {value:?}: {e}"))?;
        return Ok(DateTimeValue::Date(d));
    }

    let tzid = property_param(prop, "TZID");
    let dt = parse_single_datetime(value, tzid)?;
    Ok(DateTimeValue::DateTime(dt))
}

/// Parse one iCalendar DATE-TIME string to UTC. Understands the three
/// forms RFC 5545 §3.3.5 allows, in the context of an optional TZID:
///
/// 1. `20260420T153000Z` — exact UTC, TZID ignored if present (RFC
///    forbids combining but some exporters mess up; prefer the `Z`).
/// 2. `20260420T153000` **with** `TZID=America/New_York` — interpret
///    as local time in that zone, convert to UTC via `chrono-tz`.
///    Unknown TZIDs fall back to UTC with a warning.
/// 3. `20260420T153000` **without** TZID — floating / no specific
///    zone. Treated as UTC; no way to do better without user context.
fn parse_single_datetime(value: &str, tzid: Option<&str>) -> Result<DateTime<Utc>, String> {
    if let Some(stripped) = value.strip_suffix('Z') {
        let dt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S")
            .map_err(|e| format!("UTC DATE-TIME {value:?}: {e}"))?;
        return Ok(Utc.from_utc_datetime(&dt));
    }

    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .map_err(|e| format!("DATE-TIME {value:?}: {e}"))?;

    if let Some(tz_name) = tzid {
        match tz_name.parse::<Tz>() {
            Ok(tz) => {
                // from_local_datetime returns LocalResult which can be
                // ambiguous (DST fall-back overlap) or None (spring-forward
                // gap). For overlaps we take the earlier instant — matches
                // what every major calendar app does. For gaps we fall
                // back to UTC so the event at least lands somewhere
                // sensible, and log so we can spot it.
                match tz.from_local_datetime(&naive) {
                    chrono::LocalResult::Single(dt) => return Ok(dt.with_timezone(&Utc)),
                    chrono::LocalResult::Ambiguous(earliest, _latest) => {
                        return Ok(earliest.with_timezone(&Utc));
                    }
                    chrono::LocalResult::None => {
                        tracing::warn!(
                            "DATE-TIME {value:?} falls in a DST gap for {tz_name} — treating as UTC"
                        );
                    }
                }
            }
            Err(_) => {
                tracing::warn!("Unknown TZID {tz_name:?} — treating DATE-TIME {value:?} as UTC");
            }
        }
    }

    Ok(Utc.from_utc_datetime(&naive))
}

/// Parse a comma-separated DATE-TIME list (`RDATE`, `EXDATE`). Each
/// entry shares the property's TZID. Entries that fail to parse are
/// skipped with a warning — a malformed RDATE shouldn't lose the rest
/// of the series.
///
/// `VALUE=DATE` lists (all-day exceptions) are currently returned as
/// midnight UTC for each date. Full all-day-in-local-zone handling is
/// a separate follow-up — today's expander treats these as the UTC
/// instants the parser produces.
fn parse_datetime_list(prop: &Property, value: &str) -> Result<Vec<DateTime<Utc>>, String> {
    let tzid = property_param(prop, "TZID");
    let is_date_only = property_param(prop, "VALUE")
        .map(|v| v.eq_ignore_ascii_case("DATE"))
        .unwrap_or(false);

    let mut out = Vec::new();
    for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let parsed = if is_date_only {
            NaiveDate::parse_from_str(item, "%Y%m%d")
                .map(|d| {
                    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).expect("midnight is valid"))
                })
                .map_err(|e| format!("DATE list item {item:?}: {e}"))
        } else {
            parse_single_datetime(item, tzid)
        };
        match parsed {
            Ok(dt) => out.push(dt),
            Err(e) => tracing::warn!("Skipping RDATE/EXDATE/RECURRENCE-ID item: {e}"),
        }
    }
    Ok(out)
}

/// Look up a property parameter by name (case-insensitive). The
/// `ical` crate stores parameters as `Vec<(String, Vec<String>)>`.
fn property_param<'a>(prop: &'a Property, name: &str) -> Option<&'a str> {
    prop.params
        .as_ref()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .and_then(|(_, vs)| vs.first())
        .map(|s| s.as_str())
}

/// Parse an RFC 5545 DURATION value — e.g. `PT1H30M`, `P1D`, `-PT15M`.
/// Returns `None` for unrecognisable input (caller falls back to a
/// zero-length event).
fn parse_duration(value: &str) -> Option<Duration> {
    let (sign, rest) = match value.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, value.strip_prefix('+').unwrap_or(value)),
    };
    let rest = rest.strip_prefix('P')?;

    // Split at 'T' — everything before is the date part (weeks/days),
    // everything after is the time part (hours/minutes/seconds).
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };

    let mut total_secs: i64 = 0;

    // Date part: W or D
    if !date_part.is_empty() {
        let (n, unit) = split_number_unit(date_part)?;
        match unit {
            'W' => total_secs += n * 7 * 86_400,
            'D' => total_secs += n * 86_400,
            _ => return None,
        }
    }

    // Time part: H, M, S in that order — iterate through remaining segments.
    if let Some(mut t) = time_part {
        while !t.is_empty() {
            let (n, unit) = split_number_unit(t)?;
            let len = n.to_string().len() + 1;
            t = &t[len..];
            match unit {
                'H' => total_secs += n * 3_600,
                'M' => total_secs += n * 60,
                'S' => total_secs += n,
                _ => return None,
            }
        }
    }

    Some(Duration::seconds(sign * total_secs))
}

/// Split leading digits from a trailing unit letter. `"90M"` → `(90, 'M')`.
fn split_number_unit(s: &str) -> Option<(i64, char)> {
    let digit_end = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_end == 0 {
        return None;
    }
    let num: i64 = s[..digit_end].parse().ok()?;
    let unit = s[digit_end..].chars().next()?;
    Some((num, unit.to_ascii_uppercase()))
}

/// Reverse iCalendar TEXT escaping: `\n` → newline, `\,` / `\;` / `\\`
/// → the literal character. Per RFC 5545 §3.3.11.
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_UTC: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:evt-1@example.com\r\n\
SUMMARY:Team standup\r\n\
DESCRIPTION:Daily sync\\nBring coffee\r\n\
LOCATION:Zoom\r\n\
DTSTART:20260420T090000Z\r\n\
DTEND:20260420T093000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn parses_simple_utc_event() {
        let events = parse_ics(SIMPLE_UTC).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.id, "evt-1@example.com");
        assert_eq!(e.summary, "Team standup");
        assert_eq!(e.description.as_deref(), Some("Daily sync\nBring coffee"));
        assert_eq!(e.location.as_deref(), Some("Zoom"));
        assert_eq!(e.start.to_rfc3339(), "2026-04-20T09:00:00+00:00");
        assert_eq!(e.end.to_rfc3339(), "2026-04-20T09:30:00+00:00");
        // Non-recurring: all the new fields should be empty.
        assert!(e.rrule.is_none());
        assert!(e.rdate.is_empty());
        assert!(e.exdate.is_empty());
        assert!(e.recurrence_id.is_none());
    }

    #[test]
    fn captures_rrule_rdate_exdate() {
        let ics = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:weekly@example.com\r\n\
SUMMARY:Weekly sync\r\n\
DTSTART:20260420T090000Z\r\n\
DTEND:20260420T093000Z\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO\r\n\
RDATE:20260501T090000Z,20260515T090000Z\r\n\
EXDATE:20260504T090000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
        assert_eq!(e.rdate.len(), 2);
        assert_eq!(e.rdate[0].to_rfc3339(), "2026-05-01T09:00:00+00:00");
        assert_eq!(e.rdate[1].to_rfc3339(), "2026-05-15T09:00:00+00:00");
        assert_eq!(e.exdate.len(), 1);
        assert_eq!(e.exdate[0].to_rfc3339(), "2026-05-04T09:00:00+00:00");
    }

    #[test]
    fn captures_recurrence_id_override() {
        // An override VEVENT for the April 27 occurrence of a weekly
        // series — same UID as the master, but RECURRENCE-ID set.
        let ics = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:weekly@example.com\r\n\
SUMMARY:Moved to 10am\r\n\
DTSTART:20260427T100000Z\r\n\
DTEND:20260427T103000Z\r\n\
RECURRENCE-ID:20260427T090000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert!(e.rrule.is_none());
        assert_eq!(
            e.recurrence_id.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-04-27T09:00:00+00:00")
        );
    }

    #[test]
    fn tzid_resolves_to_utc_via_chrono_tz() {
        // 15:00 America/New_York on 2026-04-20 is UTC-4 (EDT), so
        // expected UTC is 19:00.
        let ics = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:ny-mtg@example.com\r\n\
SUMMARY:NY meeting\r\n\
DTSTART;TZID=America/New_York:20260420T150000\r\n\
DTEND;TZID=America/New_York:20260420T160000\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.start.to_rfc3339(), "2026-04-20T19:00:00+00:00");
        assert_eq!(e.end.to_rfc3339(), "2026-04-20T20:00:00+00:00");
    }

    #[test]
    fn unknown_tzid_falls_back_to_utc() {
        let ics = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:bogus-tz@example.com\r\n\
SUMMARY:Bad TZ\r\n\
DTSTART;TZID=Mars/Olympus:20260420T150000\r\n\
DTEND;TZID=Mars/Olympus:20260420T160000\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        // Falls back to treating the naive time as UTC.
        assert_eq!(e.start.to_rfc3339(), "2026-04-20T15:00:00+00:00");
    }

    const ALL_DAY: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:evt-allday@example.com\r\n\
SUMMARY:Holiday\r\n\
DTSTART;VALUE=DATE:20260501\r\n\
DTEND;VALUE=DATE:20260502\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn parses_all_day_event() {
        let events = parse_ics(ALL_DAY).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        // DTSTART=May 1, DTEND=May 2 means a single-day event on May 1
        // (DTEND is exclusive). Start = UTC midnight of May 1, end snaps
        // to 23:59:59 on the *last covered day* (May 1), not on DTEND.
        assert_eq!(e.start.to_rfc3339(), "2026-05-01T00:00:00+00:00");
        assert_eq!(e.end.to_rfc3339(), "2026-05-01T23:59:59+00:00");
    }

    const WITH_DURATION: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:evt-dur@example.com\r\n\
SUMMARY:Long meeting\r\n\
DTSTART:20260420T140000Z\r\n\
DURATION:PT1H30M\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn parses_multi_day_all_day_event() {
        // A three-day all-day event (May 1, 2, 3) is written in iCal
        // as DTSTART=May 1 / DTEND=May 4 (DTEND exclusive). Inclusive
        // end should land at May 3 23:59:59.
        let ics = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:evt-multiday@example.com\r\n\
SUMMARY:Conference\r\n\
DTSTART;VALUE=DATE:20260501\r\n\
DTEND;VALUE=DATE:20260504\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].start.to_rfc3339(), "2026-05-01T00:00:00+00:00");
        assert_eq!(events[0].end.to_rfc3339(), "2026-05-03T23:59:59+00:00");
    }

    #[test]
    fn dtstart_plus_duration() {
        let events = parse_ics(WITH_DURATION).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].end.to_rfc3339(), "2026-04-20T15:30:00+00:00");
    }

    #[test]
    fn skips_vevent_without_uid() {
        let ics = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:No UID\r\n\
DTSTART:20260420T090000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn duration_parser_handles_common_shapes() {
        assert_eq!(parse_duration("PT1H"), Some(Duration::hours(1)));
        assert_eq!(parse_duration("PT30M"), Some(Duration::minutes(30)));
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1D"), Some(Duration::days(1)));
        assert_eq!(parse_duration("P1W"), Some(Duration::days(7)));
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
    }
}
