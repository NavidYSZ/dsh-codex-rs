//! Schedule records (dsh `dsh-schedule` domain): strict decoding of stored
//! rules, creation from tool selectors with one acceptance clock sample,
//! latest-missed-occurrence resolution and the model-facing framing.
use jiff::civil::{Date, DateTime, Time};
use serde_json::{Map, Value, json};

use crate::InputError;
use crate::cron::{self, Cron};
use crate::time::{
    self, MAX_MS, MAX_SAFE, MIN_MS, canonical_zone, iso, local_date, local_instant, parse_clock,
    parse_iso, time_string,
};

/// Lower bound of a fixed-rate reminder.
pub const MIN_EVERY_INTERVAL_SECONDS: i64 = 60;
/// Upper bound of a task title (UTF-16 units).
pub const MAX_TITLE_LENGTH: usize = 120;
const REQUIRED_TITLE: &str = "title is required and must be non-empty after trimming.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    After {
        after_seconds: i64,
    },
    At,
    Every {
        every_seconds: i64,
    },
    Daily {
        time: String,
        time_zone: String,
    },
    Weekly {
        time: String,
        time_zone: String,
        weekdays: Vec<u8>,
    },
    Cron {
        expression: String,
        time_zone: String,
    },
}

/// One stored reminder (dsh `ScheduleRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub rule: Rule,
    /// The earliest unaccepted occurrence, a canonical UTC instant.
    pub scheduled_at: String,
}

impl Record {
    pub fn kind(&self) -> &'static str {
        match self.rule {
            Rule::After { .. } => "after",
            Rule::At => "at",
            Rule::Every { .. } => "every",
            Rule::Daily { .. } => "daily",
            Rule::Weekly { .. } => "weekly",
            Rule::Cron { .. } => "cron",
        }
    }

    /// `every`, `daily`, `weekly` and `cron` repeat.
    pub fn is_recurring(&self) -> bool {
        !matches!(self.rule, Rule::After { .. } | Rule::At)
    }

    pub fn scheduled_ms(&self) -> i64 {
        parse_iso(&self.scheduled_at).unwrap_or(MAX_MS)
    }

    /// The record in dsh's key order.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), self.id.clone().into());
        m.insert("kind".into(), self.kind().into());
        m.insert("title".into(), self.title.clone().into());
        m.insert("prompt".into(), self.prompt.clone().into());
        match &self.rule {
            Rule::After { after_seconds } => {
                m.insert("afterSeconds".into(), (*after_seconds).into());
            }
            Rule::At => {}
            Rule::Every { every_seconds } => {
                m.insert("everySeconds".into(), (*every_seconds).into());
            }
            Rule::Daily { time, time_zone } => {
                m.insert("time".into(), time.clone().into());
                m.insert("timeZone".into(), time_zone.clone().into());
            }
            Rule::Weekly {
                time,
                time_zone,
                weekdays,
            } => {
                m.insert("time".into(), time.clone().into());
                m.insert("timeZone".into(), time_zone.clone().into());
                m.insert("weekdays".into(), json!(weekdays));
            }
            Rule::Cron {
                expression,
                time_zone,
            } => {
                m.insert("expression".into(), expression.clone().into());
                m.insert("timeZone".into(), time_zone.clone().into());
            }
        }
        m.insert("scheduledAt".into(), self.scheduled_at.clone().into());
        Value::Object(m)
    }

    /// dsh `scheduleView`: the record plus its timing state.
    pub fn view(&self, now: i64) -> Value {
        let mut v = self.to_json();
        let state = if now >= self.scheduled_ms() {
            "overdue"
        } else {
            "scheduled"
        };
        v["state"] = state.into();
        v["deliveryMode"] = "host".into();
        v
    }

    /// dsh `decodeScheduleRecord`: a current stored record (title required).
    pub fn from_json(v: &Value) -> Result<Record, String> {
        let m = v.as_object().ok_or("schedule record must be an object")?;
        let kind = m.get("kind").and_then(Value::as_str).unwrap_or_default();
        let (expected, message): (&[&str], &str) = match kind {
            "after" => (
                &[
                    "id",
                    "kind",
                    "title",
                    "prompt",
                    "afterSeconds",
                    "scheduledAt",
                ],
                "after schedule must contain exactly id, kind, title, prompt, afterSeconds, and scheduledAt",
            ),
            "at" => (
                &["id", "kind", "title", "prompt", "scheduledAt"],
                "at schedule must contain exactly id, kind, title, prompt, and scheduledAt",
            ),
            "every" => (
                &[
                    "id",
                    "kind",
                    "title",
                    "prompt",
                    "everySeconds",
                    "scheduledAt",
                ],
                "every schedule must contain exactly id, kind, title, prompt, everySeconds, and scheduledAt",
            ),
            "daily" => (
                &[
                    "id",
                    "kind",
                    "title",
                    "prompt",
                    "time",
                    "timeZone",
                    "scheduledAt",
                ],
                "daily schedule must contain exactly id, kind, title, prompt, time, timeZone, and scheduledAt",
            ),
            "weekly" => (
                &[
                    "id",
                    "kind",
                    "title",
                    "prompt",
                    "time",
                    "timeZone",
                    "weekdays",
                    "scheduledAt",
                ],
                "weekly schedule must contain exactly id, kind, title, prompt, time, timeZone, weekdays, and scheduledAt",
            ),
            "cron" => (
                &[
                    "id",
                    "kind",
                    "title",
                    "prompt",
                    "expression",
                    "timeZone",
                    "scheduledAt",
                ],
                "cron schedule must contain exactly id, kind, title, prompt, expression, timeZone, and scheduledAt",
            ),
            _ => return Err("v1 schedule kind must be \"after\", \"at\", or \"every\"".into()),
        };
        let title = m.get("title").and_then(Value::as_str);
        let title = match title {
            Some(t) if !t.trim().is_empty() => t,
            _ => return Err(REQUIRED_TITLE.trim_end_matches('.').to_owned() + "."),
        };
        if utf16_len(title) > MAX_TITLE_LENGTH {
            return Err("title must be at most 120 characters".into());
        }
        if title.trim() != title {
            return Err("title must be a trimmed string".into());
        }
        if m.len() != expected.len() || !expected.iter().all(|k| m.contains_key(*k)) {
            return Err(message.into());
        }
        let prompt = m["prompt"]
            .as_str()
            .filter(|p| !p.is_empty() && p.trim() == *p)
            .ok_or_else(|| format!("{kind} prompt must be non-empty and already trimmed"))?;
        let id = m["id"]
            .as_str()
            .filter(|i| !i.is_empty() && i.trim() == *i)
            .ok_or("schedule id must be a non-empty string without surrounding whitespace")?;
        let scheduled_at = m["scheduledAt"].as_str().unwrap_or_default();
        if parse_iso(scheduled_at).is_none() {
            return Err(
                "scheduledAt must be a canonical four-digit-year RFC 3339 UTC instant".into(),
            );
        }
        let zone = |field: &str| -> Result<String, String> {
            let z = m[field]
                .as_str()
                .ok_or_else(|| format!("{kind} time and timeZone must be strings"))?;
            canonical_zone(z).map_err(|e| e.message)?;
            Ok(z.to_owned())
        };
        let clock = || -> Result<String, String> {
            let t = m["time"]
                .as_str()
                .ok_or_else(|| format!("{kind} time and timeZone must be strings"))?;
            let parsed = parse_clock(t).ok_or_else(|| clock_message(kind))?;
            if time_string(parsed) != t {
                return Err(format!("{kind} time must be normalized to HH:mm:ss.SSS"));
            }
            Ok(t.to_owned())
        };
        let rule = match kind {
            "after" => {
                let s = time::safe(&m["afterSeconds"])
                    .filter(|s| *s > 0)
                    .ok_or("afterSeconds must be a positive safe integer")?;
                Rule::After { after_seconds: s }
            }
            "at" => Rule::At,
            "every" => {
                let s = time::safe(&m["everySeconds"])
                    .filter(|s| {
                        *s >= MIN_EVERY_INTERVAL_SECONDS
                            && s.checked_mul(1000).is_some_and(|ms| ms <= MAX_SAFE)
                    })
                    .ok_or("everySeconds must be a safe integer of at least 60")?;
                Rule::Every { every_seconds: s }
            }
            "daily" => Rule::Daily {
                time: clock()?,
                time_zone: zone("timeZone")?,
            },
            "weekly" => {
                let time_zone = zone("timeZone")?;
                let time = clock()?;
                let stored = &m["weekdays"];
                let weekdays = normalize_weekdays(stored).map_err(|e| e.message)?;
                if stored != &json!(weekdays) {
                    return Err("weekly weekdays must be normalized to unique ascending ISO weekday numbers".into());
                }
                Rule::Weekly {
                    time,
                    time_zone,
                    weekdays,
                }
            }
            _ => {
                let time_zone = zone("timeZone")?;
                let expression = m["expression"]
                    .as_str()
                    .ok_or("cron expression and timeZone must be strings")?;
                let canonical = cron::parse(expression).map_err(|e| e.message)?.expression;
                if canonical != expression {
                    return Err("cron expression must be canonical".into());
                }
                Rule::Cron {
                    expression: expression.to_owned(),
                    time_zone,
                }
            }
        };
        Ok(Record {
            id: id.to_owned(),
            title: title.to_owned(),
            prompt: prompt.to_owned(),
            rule,
            scheduled_at: scheduled_at.to_owned(),
        })
    }
}

fn clock_message(selector: &str) -> String {
    format!(
        "{selector}.time must use HH:mm:ss with optional 1-3 fractional digits, without leap seconds or 24:00."
    )
}

pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// dsh `scheduleTitle`.
pub fn schedule_title(title: &str) -> Result<String, InputError> {
    let t = title.trim();
    if t.is_empty() {
        return Err(InputError::new("invalid_prompt", REQUIRED_TITLE));
    }
    if utf16_len(t) > MAX_TITLE_LENGTH {
        return Err(InputError::new(
            "invalid_prompt",
            "title must be at most 120 characters.",
        ));
    }
    Ok(t.to_owned())
}

fn prompt_text(prompt: &str) -> Result<String, InputError> {
    let p = prompt.trim();
    if p.is_empty() {
        return Err(InputError::new(
            "invalid_prompt",
            "prompt must be non-empty after trimming.",
        ));
    }
    Ok(p.to_owned())
}

/// dsh `futureInstant`.
fn future_instant(epoch: i64, now: i64) -> Result<String, InputError> {
    if !(MIN_MS..=MAX_MS).contains(&epoch) {
        return Err(InputError::new(
            "time_out_of_range",
            "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.",
        ));
    }
    if epoch <= now {
        return Err(InputError::new(
            "not_future",
            "The scheduled time must be strictly in the future.",
        ));
    }
    Ok(iso(epoch))
}

fn representable_now(now: i64, selector: &str) -> Result<(), InputError> {
    if (MIN_MS..=MAX_MS).contains(&now) {
        Ok(())
    } else {
        Err(InputError::new(
            "time_out_of_range",
            format!(
                "{selector} creation time must be a representable four-digit-year UTC instant."
            ),
        ))
    }
}

/// dsh `normalizeWeekdays`.
pub fn normalize_weekdays(v: &Value) -> Result<Vec<u8>, InputError> {
    let rule = |m: String| InputError::new("invalid_rule", m);
    let Some(list) = v.as_array().filter(|a| !a.is_empty()) else {
        return Err(rule(
            "weekly.weekdays must be a non-empty array of ISO weekday numbers.".into(),
        ));
    };
    let mut seen = Vec::new();
    for w in list {
        let n = w.as_f64().filter(|f| f.fract() == 0.0 && (1.0..=7.0).contains(f)).ok_or_else(|| {
            rule("Each weekly.weekdays entry must be an integer from 1 (Monday) through 7 (Sunday).".into())
        })? as u8;
        if seen.contains(&n) {
            return Err(rule(format!(
                "weekly.weekdays must not repeat weekday {n}."
            )));
        }
        seen.push(n);
    }
    seen.sort_unstable();
    Ok(seen)
}

fn record_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Map<String, Value>> {
    v.as_object()
        .filter(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
}

/// dsh `parseOffsetInstant`: an RFC 3339 date-time with `Z` or a numeric offset.
fn parse_offset_instant(value: &str) -> Result<i64, InputError> {
    let shape = || {
        InputError::new(
            "invalid_rule",
            "at must use YYYY-MM-DDTHH:mm:ss with optional 1-3 digit fractional seconds and an explicit Z or numeric offset.",
        )
    };
    let real = || {
        InputError::new(
            "invalid_rule",
            "The at value must be a real ISO calendar date and time.",
        )
    };
    let b = value.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(shape());
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = value.get(r)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) = (
        num(0..4),
        num(5..7),
        num(8..10),
        num(11..13),
        num(14..16),
        num(17..19),
    ) else {
        return Err(shape());
    };
    let mut rest = &value[19..];
    let mut millis = 0;
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.bytes().take_while(u8::is_ascii_digit).count();
        if !(1..=3).contains(&n) {
            return Err(shape());
        }
        millis = format!("{:0<3}", &r[..n])
            .parse::<i64>()
            .map_err(|_| shape())?;
        rest = &r[n..];
    }
    let offset_minutes = if rest == "Z" {
        0
    } else {
        let rb = rest.as_bytes();
        if rb.len() != 6 || !(rb[0] == b'+' || rb[0] == b'-') || rb[3] != b':' {
            return Err(shape());
        }
        let oh = rest[1..3]
            .parse::<i64>()
            .ok()
            .filter(|_| rest[1..3].bytes().all(|c| c.is_ascii_digit()));
        let om = rest[4..6]
            .parse::<i64>()
            .ok()
            .filter(|_| rest[4..6].bytes().all(|c| c.is_ascii_digit()));
        let (Some(oh), Some(om)) = (oh, om) else {
            return Err(shape());
        };
        if y == 0 || h > 23 || mi > 59 || s > 59 {
            return Err(real());
        }
        if oh > 23 || om > 59 || (rb[0] == b'-' && oh == 0 && om == 0) {
            return Err(InputError::new(
                "invalid_rule",
                "The at numeric offset is invalid.",
            ));
        }
        let sign = if rb[0] == b'+' { 1 } else { -1 };
        sign * (oh * 60 + om)
    };
    if y == 0 || h > 23 || mi > 59 || s > 59 {
        return Err(real());
    }
    let dt = DateTime::new(
        y as i16,
        mo as i8,
        d as i8,
        h as i8,
        mi as i8,
        s as i8,
        (millis * 1_000_000) as i32,
    )
    .map_err(|_| real())?;
    let local = jiff::tz::TimeZone::UTC
        .to_ambiguous_timestamp(dt)
        .compatible()
        .map_err(|_| real())?
        .as_millisecond();
    Ok(local - offset_minutes * 60_000)
}

/// dsh `parseAtInput`: an explicit-offset string or `{date, time, time_zone}`.
pub fn parse_at(at: &Value) -> Result<i64, InputError> {
    if let Some(s) = at.as_str() {
        return parse_offset_instant(s);
    }
    if !at.is_object() {
        return Err(InputError::new(
            "invalid_rule",
            "at must be an explicit-offset string or local calendar object.",
        ));
    }
    let Some(m) = record_of(at, &["date", "time", "time_zone"]) else {
        return Err(InputError::new(
            "invalid_rule",
            "Local at must contain exactly date, time, and time_zone.",
        ));
    };
    let (Some(date), Some(t)) = (m["date"].as_str(), m["time"].as_str()) else {
        return Err(InputError::new(
            "invalid_rule",
            "Local at date and time must be strings.",
        ));
    };
    let Some(zone) = m["time_zone"].as_str() else {
        return Err(InputError::new(
            "invalid_time_zone",
            "time_zone must be a string.",
        ));
    };
    let shape = || {
        InputError::new(
            "invalid_rule",
            "Local at requires date YYYY-MM-DD and time HH:mm:ss with optional one-to-three digit milliseconds.",
        )
    };
    let db = date.as_bytes();
    if db.len() != 10
        || db[4] != b'-'
        || db[7] != b'-'
        || !date
            .bytes()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return Err(shape());
    }
    let (y, mo, d): (i64, i64, i64) = (
        date[..4].parse().unwrap_or(0),
        date[5..7].parse().unwrap_or(0),
        date[8..].parse().unwrap_or(0),
    );
    let tb = t.as_bytes();
    if tb.len() < 8
        || tb[2] != b':'
        || tb[5] != b':'
        || !t[..8]
            .bytes()
            .enumerate()
            .all(|(i, c)| i == 2 || i == 5 || c.is_ascii_digit())
    {
        return Err(shape());
    }
    let (h, mi, s): (i64, i64, i64) = (
        t[..2].parse().unwrap_or(0),
        t[3..5].parse().unwrap_or(0),
        t[6..8].parse().unwrap_or(0),
    );
    let millis = match &t[8..] {
        "" => 0,
        f if f.starts_with('.')
            && (2..=4).contains(&f.len())
            && f[1..].bytes().all(|c| c.is_ascii_digit()) =>
        {
            format!("{:0<3}", &f[1..]).parse::<i64>().unwrap_or(0)
        }
        _ => return Err(shape()),
    };
    if y == 0 || h > 23 || mi > 59 || s > 59 {
        return Err(InputError::new(
            "invalid_rule",
            "The local at value must be a real ISO calendar date and time.",
        ));
    }
    let real = || {
        InputError::new(
            "invalid_rule",
            "The at value must be a real ISO calendar date and time.",
        )
    };
    let dt = DateTime::new(
        y as i16,
        mo as i8,
        d as i8,
        h as i8,
        mi as i8,
        s as i8,
        (millis * 1_000_000) as i32,
    )
    .map_err(|_| real())?;
    let zone = canonical_zone(zone)?;
    local_instant(dt, &zone.tz).ok_or_else(|| {
        InputError::new(
            "invalid_rule",
            "The local at time does not exist in the selected time zone.",
        )
    })
}

/// dsh `parseDailyInput`: normalized `HH:mm:ss.SSS` and zone.
pub fn parse_daily(v: &Value) -> Result<(String, crate::time::Zone), InputError> {
    let Some(m) = record_of(v, &["time", "time_zone"]).filter(|m| m["time"].is_string()) else {
        return Err(InputError::new(
            "invalid_rule",
            "daily must contain exactly time and time_zone, with time HH:mm:ss and optional 1-3 fractional digits.",
        ));
    };
    let Some(zone) = m["time_zone"].as_str() else {
        return Err(InputError::new(
            "invalid_time_zone",
            "time_zone must be a string.",
        ));
    };
    let t = parse_clock(m["time"].as_str().unwrap_or_default())
        .ok_or_else(|| InputError::new("invalid_rule", clock_message("daily")))?;
    Ok((time_string(t), canonical_zone(zone)?))
}

/// dsh `parseWeeklyInput`.
pub fn parse_weekly(v: &Value) -> Result<(String, crate::time::Zone, Vec<u8>), InputError> {
    let Some(m) =
        record_of(v, &["time", "time_zone", "weekdays"]).filter(|m| m["time"].is_string())
    else {
        return Err(InputError::new(
            "invalid_rule",
            "weekly must contain exactly time, time_zone, and weekdays, with time HH:mm:ss and optional 1-3 fractional digits.",
        ));
    };
    let Some(zone) = m["time_zone"].as_str() else {
        return Err(InputError::new(
            "invalid_time_zone",
            "time_zone must be a string.",
        ));
    };
    let t = parse_clock(m["time"].as_str().unwrap_or_default())
        .ok_or_else(|| InputError::new("invalid_rule", clock_message("weekly")))?;
    let zone = canonical_zone(zone)?;
    let weekdays = normalize_weekdays(&m["weekdays"])?;
    Ok((time_string(t), zone, weekdays))
}

/// dsh `parseCronInput`.
pub fn parse_cron_input(v: &Value) -> Result<(String, crate::time::Zone), InputError> {
    let Some(m) =
        record_of(v, &["expression", "time_zone"]).filter(|m| m["expression"].is_string())
    else {
        return Err(InputError::new(
            "invalid_rule",
            "cron must contain exactly expression and time_zone, with a five-field cron expression.",
        ));
    };
    let Some(zone) = m["time_zone"].as_str() else {
        return Err(InputError::new(
            "invalid_time_zone",
            "time_zone must be a string.",
        ));
    };
    let expression = cron::parse(m["expression"].as_str().unwrap_or_default())?.expression;
    Ok((expression, canonical_zone(zone)?))
}

/// Selector-specific creation (dsh `create*ScheduleRecord`), one clock sample `now`.
pub enum Selector<'a> {
    After(&'a Value),
    At(&'a Value),
    Every(&'a Value),
    Daily(&'a Value),
    Weekly(&'a Value),
    Cron(&'a Value),
}

pub fn create_record(
    id: &str,
    prompt: &str,
    selector: Selector<'_>,
    now: i64,
    title: &str,
) -> Result<Record, InputError> {
    let prompt = prompt_text(prompt)?;
    let (rule, scheduled_at) = match selector {
        Selector::After(v) => {
            let s = time::safe(v).filter(|s| *s > 0).ok_or_else(|| {
                InputError::new(
                    "invalid_rule",
                    "after_seconds must be a positive safe integer.",
                )
            })?;
            let target = now.saturating_add(s.saturating_mul(1000));
            (
                Rule::After { after_seconds: s },
                future_instant(target, now)?,
            )
        }
        Selector::At(v) => (Rule::At, future_instant(parse_at(v)?, now)?),
        Selector::Every(v) => {
            let s = time::safe(v).ok_or_else(|| {
                InputError::new("invalid_rule", "every_seconds must be a safe integer.")
            })?;
            if s < MIN_EVERY_INTERVAL_SECONDS {
                return Err(InputError::new(
                    "frequency_too_high",
                    "every_seconds must be at least 60.",
                ));
            }
            let target = now.saturating_add(s.saturating_mul(1000));
            (
                Rule::Every { every_seconds: s },
                future_instant(target, now)?,
            )
        }
        Selector::Daily(v) => {
            let (t, zone) = parse_daily(v)?;
            representable_now(now, "Daily")?;
            let at = next_matching(
                parse_clock(&t).unwrap_or(Time::midnight()),
                &zone.tz,
                now,
                &|_| true,
                None,
            )
            .ok_or_else(|| {
                InputError::new(
                    "time_out_of_range",
                    "No future daily occurrence is representable as a four-digit-year UTC instant.",
                )
            })?;
            (
                Rule::Daily {
                    time: t,
                    time_zone: zone.name,
                },
                at,
            )
        }
        Selector::Weekly(v) => {
            let (t, zone, weekdays) = parse_weekly(v)?;
            representable_now(now, "Weekly")?;
            let days = weekdays.clone();
            let at = next_matching(parse_clock(&t).unwrap_or(Time::midnight()), &zone.tz, now, &|d| on_weekday(&days, d), None)
                .ok_or_else(|| InputError::new("time_out_of_range", "No future weekly occurrence is representable as a four-digit-year UTC instant."))?;
            (
                Rule::Weekly {
                    time: t,
                    time_zone: zone.name,
                    weekdays,
                },
                at,
            )
        }
        Selector::Cron(v) => {
            let (expression, zone) = parse_cron_input(v)?;
            representable_now(now, "Cron")?;
            let parsed = cron::parse(&expression)?;
            let at = next_cron(&parsed, &zone.tz, now).ok_or_else(|| {
                InputError::new(
                    "time_out_of_range",
                    "No future cron occurrence is representable as a four-digit-year UTC instant.",
                )
            })?;
            (
                Rule::Cron {
                    expression,
                    time_zone: zone.name,
                },
                at,
            )
        }
    };
    Ok(Record {
        id: id.to_owned(),
        title: schedule_title(title)?,
        prompt,
        rule,
        scheduled_at,
    })
}

fn on_weekday(days: &[u8], d: Date) -> bool {
    days.contains(&(d.weekday().to_monday_one_offset() as u8))
}

/// dsh `nextMatchingTarget`: the first actual occurrence after `now` (and
/// after the delivered local date `after`).
fn next_matching(
    t: Time,
    tz: &jiff::tz::TimeZone,
    now: i64,
    matches: &dyn Fn(Date) -> bool,
    after: Option<Date>,
) -> Option<String> {
    let mut date = local_date(now, tz);
    if let Some(a) = after
        && date <= a
    {
        date = time::next_day(a)?;
    }
    loop {
        if matches(date)
            && let Some(target) = local_instant(date.to_datetime(t), tz)
        {
            if target > MAX_MS {
                return None;
            }
            if target >= MIN_MS && target > now {
                return Some(iso(target));
            }
        }
        date = time::next_day(date)?;
    }
}

/// dsh `latestDueOccurrence`.
fn latest_due(
    t: Time,
    tz: &jiff::tz::TimeZone,
    matches: &dyn Fn(Date) -> bool,
    accepted: i64,
    saved: i64,
) -> i64 {
    let Some(mut date) = time::next_day(local_date(accepted, &jiff::tz::TimeZone::UTC)) else {
        return saved;
    };
    loop {
        if matches(date)
            && let Some(c) = local_instant(date.to_datetime(t), tz)
            && c <= accepted
        {
            return c.max(saved);
        }
        match time::prev_day(date) {
            Some(d) => date = d,
            None => return saved,
        }
    }
}

fn first_of_next_month(d: Date) -> Option<Date> {
    let (y, m) = if d.month() == 12 {
        (d.year().checked_add(1)?, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    Date::new(y, m, 1).ok()
}

/// dsh `nextCronTarget` (400-year search horizon).
fn next_cron(c: &Cron, tz: &jiff::tz::TimeZone, floor: i64) -> Option<String> {
    let times = c.times();
    let first = local_date(floor, tz);
    let floor_time = time::local_time(floor, tz);
    let ceiling = Date::new(9999, 12, 31).ok()?;
    let horizon = first
        .checked_add(jiff::Span::new().years(400))
        .unwrap_or(ceiling);
    let last = horizon.min(ceiling);
    let mut date = first;
    while date <= last {
        if !c.month_matches(date) {
            date = first_of_next_month(date)?;
            continue;
        }
        if c.date_matches(date) {
            for t in &times {
                if date == first && *t < floor_time {
                    continue;
                }
                let Some(target) = local_instant(date.to_datetime(*t), tz) else {
                    continue;
                };
                if target > MAX_MS {
                    return None;
                }
                if target >= MIN_MS && target > floor {
                    return Some(iso(target));
                }
            }
        }
        date = time::next_day(date)?;
    }
    None
}

/// dsh `latestCronOccurrence`.
fn latest_cron(c: &Cron, tz: &jiff::tz::TimeZone, decision: i64, saved: i64) -> i64 {
    let times = c.times();
    let Some(mut date) = time::next_day(local_date(decision, &jiff::tz::TimeZone::UTC)) else {
        return saved;
    };
    let floor = local_date(MIN_MS, tz);
    let horizon = date
        .checked_sub(jiff::Span::new().years(400))
        .unwrap_or(floor);
    let first = horizon.max(floor);
    while date >= first {
        if !c.month_matches(date) {
            match Date::new(date.year(), date.month(), 1)
                .ok()
                .and_then(time::prev_day)
            {
                Some(d) => date = d,
                None => return saved,
            }
            continue;
        }
        if c.date_matches(date) {
            let earliest = times
                .iter()
                .find_map(|t| local_instant(date.to_datetime(*t), tz));
            if earliest.is_none_or(|e| e <= decision) {
                for t in times.iter().rev() {
                    if let Some(candidate) = local_instant(date.to_datetime(*t), tz)
                        && candidate <= decision
                        && candidate >= MIN_MS
                    {
                        return candidate.max(saved);
                    }
                }
            }
        }
        match time::prev_day(date) {
            Some(d) => date = d,
            None => return saved,
        }
    }
    saved
}

/// One accepted occurrence of a recurring rule and its next target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub occurrence_at: String,
    pub next_scheduled_at: Option<String>,
}

/// dsh `resolveRecurringOccurrence`: the latest due occurrence at
/// `accepted` (missed earlier ones are skipped) and the first future one.
pub fn resolve_occurrence(record: &Record, accepted: i64) -> Result<Occurrence, String> {
    let saved = record.scheduled_ms();
    if !(MIN_MS..=MAX_MS).contains(&accepted) {
        return Err(format!(
            "{} acceptedAt must be a representable four-digit-year instant",
            record.kind()
        ));
    }
    if accepted < saved {
        return Err(format!(
            "{} dispatch cannot precede the active scheduledAt",
            record.kind()
        ));
    }
    let zone_of = |z: &str| canonical_zone(z).map_err(|e| e.message);
    match &record.rule {
        Rule::Every { every_seconds } => {
            let interval = every_seconds * 1000;
            let occurrence = saved + (accepted - saved) / interval * interval;
            let next = occurrence + interval;
            Ok(Occurrence {
                occurrence_at: iso(occurrence),
                next_scheduled_at: (next <= MAX_MS).then(|| iso(next)),
            })
        }
        Rule::Daily { time, time_zone }
        | Rule::Weekly {
            time, time_zone, ..
        } => {
            let zone = zone_of(time_zone)?;
            let t = parse_clock(time).ok_or("invalid stored time")?;
            let days = match &record.rule {
                Rule::Weekly { weekdays, .. } => Some(weekdays.clone()),
                _ => None,
            };
            let matches = |d: Date| days.as_ref().is_none_or(|w| on_weekday(w, d));
            let occurrence = latest_due(t, &zone.tz, &matches, accepted, saved);
            let next = next_matching(
                t,
                &zone.tz,
                accepted,
                &matches,
                Some(local_date(occurrence, &zone.tz)),
            );
            Ok(Occurrence {
                occurrence_at: iso(occurrence),
                next_scheduled_at: next,
            })
        }
        Rule::Cron {
            expression,
            time_zone,
        } => {
            let zone = zone_of(time_zone)?;
            let parsed = cron::parse(expression).map_err(|e| e.message)?;
            let occurrence = latest_cron(&parsed, &zone.tz, accepted, saved);
            Ok(Occurrence {
                occurrence_at: iso(occurrence),
                next_scheduled_at: next_cron(&parsed, &zone.tz, accepted),
            })
        }
        Rule::After { .. } | Rule::At => {
            Err("one-shot reminders have no recurring occurrence".into())
        }
    }
}

/// Fixed origin line of every reminder delivery.
const FRAMING: &str = "This is a scheduled message from the user";

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// dsh `renderReminderFraming` (one-shot reminders).
pub fn reminder_text(record: &Record) -> String {
    [
        "[SCHEDULE REMINDER]".to_owned(),
        FRAMING.to_owned(),
        format!("schedule_id_json: {}", js(&record.id)),
        format!("occurrence_at: {}", record.scheduled_at),
        format!("reminder_prompt_json: {}", js(&record.prompt)),
    ]
    .join("\n")
}

/// dsh `renderRecurringReminderBatchFraming`.
pub fn batch_text(reminders: &[(&Record, &str)]) -> String {
    let payload: Vec<Value> = reminders
        .iter()
        .map(|(r, at)| json!({"schedule_id": r.id, "occurrence_at": at, "reminder_prompt": r.prompt}))
        .collect();
    [
        "[SCHEDULE REMINDER BATCH]".to_owned(),
        FRAMING.to_owned(),
        format!(
            "reminders_json: {}",
            serde_json::to_string(&payload).unwrap_or_default()
        ),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_617_122_000; // 2026-10-21T21:12:02Z

    fn mk(sel: Selector<'_>) -> Result<Record, InputError> {
        create_record("schedule-1", " Check CI ", sel, NOW, " CI ")
    }

    #[test]
    fn creates_each_selector() {
        let after = mk(Selector::After(&json!(90))).unwrap();
        assert_eq!(
            after.to_json(),
            json!({"id": "schedule-1", "kind": "after", "title": "CI", "prompt": "Check CI", "afterSeconds": 90, "scheduledAt": "2026-10-21T21:13:32.000Z"})
        );
        let at = mk(Selector::At(
            &json!({"date": "2026-10-22", "time": "09:00:00", "time_zone": "Europe/Berlin"}),
        ))
        .unwrap();
        assert_eq!(at.scheduled_at, "2026-10-22T07:00:00.000Z");
        let at2 = mk(Selector::At(&json!("2026-10-22T09:00:00+02:00"))).unwrap();
        assert_eq!(at2.scheduled_at, "2026-10-22T07:00:00.000Z");
        let daily = mk(Selector::Daily(
            &json!({"time": "23:00:00", "time_zone": "Europe/Berlin"}),
        ))
        .unwrap();
        // 23:00 in Berlin already passed today (23:12 local).
        assert_eq!(daily.scheduled_at, "2026-10-22T21:00:00.000Z");
        let weekly = mk(Selector::Weekly(
            &json!({"time": "09:00:00", "time_zone": "UTC", "weekdays": [5, 1]}),
        ))
        .unwrap();
        assert_eq!(weekly.scheduled_at, "2026-10-23T09:00:00.000Z");
        assert_eq!(weekly.to_json()["weekdays"], json!([1, 5]));
        let cron = mk(Selector::Cron(
            &json!({"expression": "0 9 * * 1-5", "time_zone": "UTC"}),
        ))
        .unwrap();
        assert_eq!(cron.scheduled_at, "2026-10-22T09:00:00.000Z");
        let every = mk(Selector::Every(&json!(3600))).unwrap();
        assert_eq!(every.scheduled_at, "2026-10-21T22:12:02.000Z");
        for r in [after, at, every, daily, weekly, cron] {
            assert_eq!(Record::from_json(&r.to_json()).unwrap(), r);
        }
    }

    #[test]
    fn input_errors_match_dsh() {
        fn code(sel: Selector<'_>) -> &'static str {
            mk(sel).unwrap_err().code
        }
        assert_eq!(code(Selector::Every(&json!(30))), "frequency_too_high");
        assert_eq!(
            code(Selector::At(&json!("2020-01-01T00:00:00Z"))),
            "not_future"
        );
        assert_eq!(
            code(Selector::At(&json!("2026-13-01T00:00:00Z"))),
            "invalid_rule"
        );
        assert_eq!(
            code(Selector::Daily(
                &json!({"time": "23:00", "time_zone": "UTC"})
            )),
            "invalid_rule"
        );
        assert_eq!(
            code(Selector::Daily(
                &json!({"time": "23:00:00", "time_zone": "Mars/Base"})
            )),
            "invalid_time_zone"
        );
        assert_eq!(
            code(Selector::At(
                &json!({"date": "2027-03-28", "time": "02:30:00", "time_zone": "Europe/Berlin"})
            )),
            "invalid_rule"
        );
        let e = create_record("s", "  ", Selector::After(&json!(5)), NOW, "t").unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            ("invalid_prompt", "prompt must be non-empty after trimming.")
        );
    }

    #[test]
    fn missed_occurrences_resolve_to_the_latest() {
        let daily = Record {
            id: "d".into(),
            title: "t".into(),
            prompt: "p".into(),
            rule: Rule::Daily {
                time: "09:00:00.000".into(),
                time_zone: "UTC".into(),
            },
            scheduled_at: "2026-10-18T09:00:00.000Z".into(),
        };
        // Down for three days: only 2026-10-21 09:00 is delivered, next is tomorrow.
        let o = resolve_occurrence(&daily, NOW).unwrap();
        assert_eq!(o.occurrence_at, "2026-10-21T09:00:00.000Z");
        assert_eq!(
            o.next_scheduled_at.as_deref(),
            Some("2026-10-22T09:00:00.000Z")
        );
        let every = Record {
            rule: Rule::Every {
                every_seconds: 3600,
            },
            scheduled_at: "2026-10-21T18:00:00.000Z".into(),
            ..daily.clone()
        };
        let o = resolve_occurrence(&every, NOW).unwrap();
        assert_eq!(o.occurrence_at, "2026-10-21T21:00:00.000Z");
        assert_eq!(
            o.next_scheduled_at.as_deref(),
            Some("2026-10-21T22:00:00.000Z")
        );
        let cron = Record {
            rule: Rule::Cron {
                expression: "*/15 9-17 * * 1-5".into(),
                time_zone: "UTC".into(),
            },
            scheduled_at: "2026-10-16T09:00:00.000Z".into(),
            ..daily
        };
        let o = resolve_occurrence(&cron, NOW).unwrap();
        assert_eq!(o.occurrence_at, "2026-10-21T17:45:00.000Z");
        assert_eq!(
            o.next_scheduled_at.as_deref(),
            Some("2026-10-22T09:00:00.000Z")
        );
    }

    #[test]
    fn framing_matches_dsh() {
        let r = mk(Selector::After(&json!(90))).unwrap();
        assert_eq!(
            reminder_text(&r),
            "[SCHEDULE REMINDER]\nThis is a scheduled message from the user\nschedule_id_json: \"schedule-1\"\noccurrence_at: 2026-10-21T21:13:32.000Z\nreminder_prompt_json: \"Check CI\""
        );
        assert_eq!(
            batch_text(&[(&r, "2026-10-21T21:00:00.000Z")]),
            "[SCHEDULE REMINDER BATCH]\nThis is a scheduled message from the user\nreminders_json: [{\"schedule_id\":\"schedule-1\",\"occurrence_at\":\"2026-10-21T21:00:00.000Z\",\"reminder_prompt\":\"Check CI\"}]"
        );
    }
}
