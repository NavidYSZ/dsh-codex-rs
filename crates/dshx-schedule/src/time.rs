//! Instants, zones and local wall-clock resolution with the semantics of
//! dsh's Temporal code: four-digit-year UTC instants in milliseconds, the
//! earlier instant of a repeated local time, and no instant for a local time
//! that falls into a gap.
use jiff::Timestamp;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};

use crate::InputError;

/// `Date.parse("0001-01-01T00:00:00.000Z")`.
pub const MIN_MS: i64 = -62_135_596_800_000;
/// `Date.parse("9999-12-31T23:59:59.999Z")`.
pub const MAX_MS: i64 = 253_402_300_799_999;
/// `Number.MAX_SAFE_INTEGER`.
pub const MAX_SAFE: i64 = 9_007_199_254_740_991;

/// A JS safe integer (`Number.isSafeInteger`).
pub fn safe(v: &serde_json::Value) -> Option<i64> {
    let f = v.as_f64()?;
    (f.fract() == 0.0 && f.abs() <= MAX_SAFE as f64).then_some(f as i64)
}

/// A validated zone with the name dsh stores.
#[derive(Debug, Clone)]
pub struct Zone {
    pub name: String,
    pub tz: TimeZone,
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count (Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `new Date(ms).toISOString()` for a four-digit-year instant.
pub fn iso(ms: i64) -> String {
    let days = ms.div_euclid(DAY);
    let rest = ms.rem_euclid(DAY);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

const DAY: i64 = 86_400_000;

/// The canonical UTC profile `YYYY-MM-DDTHH:mm:ss.sssZ` (year ≠ 0000), as epoch ms.
pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 24 || s.get(..4)? == "0000" {
        return None;
    }
    if b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, mo, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (h, mi, sec, ms) = (
        digits(11..13)?,
        digits(14..16)?,
        digits(17..19)?,
        digits(20..23)?,
    );
    if !(1..=12).contains(&mo) || d < 1 || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let out = days_from_civil(y, mo, d) * DAY + h * 3_600_000 + mi * 60_000 + sec * 1000 + ms;
    (iso(out) == s).then_some(out)
}

/// dsh `canonicalizeTimeZone`: `UTC` or an IANA `Area/Location` name known
/// to the zone database.
pub fn canonical_zone(value: &str) -> Result<Zone, InputError> {
    let invalid = || {
        InputError::new(
            "invalid_time_zone",
            "time_zone must be UTC or a valid IANA Area/Location name.",
        )
    };
    if value.is_empty() || value.trim() != value || (value != "UTC" && !iana_shape(value)) {
        return Err(invalid());
    }
    let tz = TimeZone::get(value).map_err(|_| invalid())?;
    Ok(Zone {
        name: value.to_owned(),
        tz,
    })
}

/// `^[A-Za-z][A-Za-z0-9_+.-]*(?:\/[A-Za-z0-9_+.-]+)+$`
pub fn iana_shape(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(first) = parts.next() else {
        return false;
    };
    let tail_ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '.' | '-'))
    };
    let mut rest = 0;
    let all_tails = parts.all(|p| {
        rest += 1;
        tail_ok(p)
    });
    first
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && tail_ok(first)
        && all_tails
        && rest >= 1
}

/// dsh `localInstant`: the earlier instant of a local date-time, `None`
/// when the local time does not exist in the zone (a gap).
pub fn local_instant(dt: DateTime, tz: &TimeZone) -> Option<i64> {
    let offset = match tz.to_ambiguous_timestamp(dt).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset,
        AmbiguousOffset::Fold { before, .. } => before,
        AmbiguousOffset::Gap { .. } => return None,
    };
    offset.to_timestamp(dt).ok().map(|t| t.as_millisecond())
}

/// The calendar date of an instant in a zone.
pub fn local_date(ms: i64, tz: &TimeZone) -> Date {
    let ts = Timestamp::from_millisecond(ms).unwrap_or(Timestamp::UNIX_EPOCH);
    tz.to_datetime(ts).date()
}

/// The wall-clock time of an instant in a zone.
pub fn local_time(ms: i64, tz: &TimeZone) -> Time {
    let ts = Timestamp::from_millisecond(ms).unwrap_or(Timestamp::UNIX_EPOCH);
    tz.to_datetime(ts).time()
}

/// `Temporal.PlainTime.toString({fractionalSecondDigits: 3})`.
pub fn time_string(t: Time) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        t.hour(),
        t.minute(),
        t.second(),
        t.subsec_nanosecond() / 1_000_000
    )
}

/// `HH:mm:ss` with optional 1-3 fractional digits (no leap seconds, no 24:00).
pub fn parse_clock(value: &str) -> Option<Time> {
    let (hms, frac) = match value.split_once('.') {
        Some((a, f)) => (a, Some(f)),
        None => (value, None),
    };
    let b = hms.as_bytes();
    if b.len() != 8 || b[2] != b':' || b[5] != b':' {
        return None;
    }
    let num = |s: &str| -> Option<i8> {
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (h, m, s) = (num(&hms[..2])?, num(&hms[3..5])?, num(&hms[6..8])?);
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    let ms = match frac {
        None => 0,
        Some(f) if (1..=3).contains(&f.len()) && f.bytes().all(|c| c.is_ascii_digit()) => {
            format!("{f:0<3}").parse::<i32>().ok()?
        }
        Some(_) => return None,
    };
    Time::new(h, m, s, ms * 1_000_000).ok()
}

/// The next calendar day, `None` past 9999-12-31.
pub fn next_day(d: Date) -> Option<Date> {
    d.tomorrow().ok()
}

pub fn prev_day(d: Date) -> Option<Date> {
    d.yesterday().ok()
}

/// The zone name time-context and the system fallback report.
pub fn system_zone() -> Zone {
    let tz = TimeZone::system();
    let name = tz.iana_name().unwrap_or("UTC").to_owned();
    Zone { name, tz }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(MAX_MS), "9999-12-31T23:59:59.999Z");
        assert_eq!(
            parse_iso("2026-10-21T21:12:02.123Z"),
            Some(1_792_617_122_123)
        );
        assert_eq!(parse_iso("2026-02-30T00:00:00.000Z"), None);
        assert_eq!(parse_iso("0000-01-01T00:00:00.000Z"), None);
        assert_eq!(parse_iso("2026-10-21T21:12:02Z"), None);
    }

    #[test]
    fn gaps_and_folds_like_temporal() {
        let berlin = canonical_zone("Europe/Berlin").unwrap();
        // 2026-03-29 02:30 does not exist in Berlin.
        let gap = DateTime::new(2026, 3, 29, 2, 30, 0, 0).unwrap();
        assert_eq!(local_instant(gap, &berlin.tz), None);
        // 2026-10-25 02:30 happens twice; the earlier one is +02:00.
        let fold = DateTime::new(2026, 10, 25, 2, 30, 0, 0).unwrap();
        assert_eq!(
            iso(local_instant(fold, &berlin.tz).unwrap()),
            "2026-10-25T00:30:00.000Z"
        );
        assert!(canonical_zone("Mars/Base").is_err());
        assert!(canonical_zone("Berlin").is_err());
        assert!(canonical_zone("UTC").is_ok());
    }

    #[test]
    fn clock_times() {
        assert_eq!(
            time_string(parse_clock("23:00:00").unwrap()),
            "23:00:00.000"
        );
        assert_eq!(
            time_string(parse_clock("09:05:07.5").unwrap()),
            "09:05:07.500"
        );
        assert!(parse_clock("24:00:00").is_none());
        assert!(parse_clock("9:00:00").is_none());
        assert!(parse_clock("09:00:60").is_none());
        assert!(parse_clock("09:00:00.1234").is_none());
    }
}
