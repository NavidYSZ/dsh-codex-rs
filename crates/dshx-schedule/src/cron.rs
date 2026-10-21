//! Strict five-field Vixie cron (dsh `parseCronExpression`): matched value
//! sets, the canonical spelling dsh stores, and the day-of-month /
//! day-of-week star flags that select AND or OR matching.
use jiff::civil::{Date, Time};

use crate::InputError;

struct Spec {
    name: &'static str,
    min: u32,
    max: u32,
    canonical_max: u32,
}

const FIELDS: [Spec; 5] = [
    Spec {
        name: "minute",
        min: 0,
        max: 59,
        canonical_max: 59,
    },
    Spec {
        name: "hour",
        min: 0,
        max: 23,
        canonical_max: 23,
    },
    Spec {
        name: "day-of-month",
        min: 1,
        max: 31,
        canonical_max: 31,
    },
    Spec {
        name: "month",
        min: 1,
        max: 12,
        canonical_max: 12,
    },
    Spec {
        name: "day-of-week",
        min: 0,
        max: 7,
        canonical_max: 6,
    },
];

/// One parsed expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    /// The canonical expression.
    pub expression: String,
    pub minutes: Vec<u32>,
    pub hours: Vec<u32>,
    pub days_of_month: Vec<u32>,
    pub months: Vec<u32>,
    pub days_of_week: Vec<u32>,
    pub day_of_month_star: bool,
    pub day_of_week_star: bool,
}

fn rule(message: String) -> InputError {
    InputError::new("invalid_rule", message)
}

fn invalid_element(spec: &Spec, element: &str) -> InputError {
    rule(format!(
        "cron.expression {} field element {} must be *, a value, a-b, */n, a-b/n, or a comma-separated list of those.",
        spec.name,
        serde_json::to_string(element).unwrap_or_default()
    ))
}

/// `Number(text)` of a digit string, `None` when not a safe integer.
fn number(text: &str) -> Option<u64> {
    let n: u128 = text.parse().ok()?;
    (n <= 9_007_199_254_740_991).then_some(n as u64)
}

fn value(text: &str, spec: &Spec) -> Result<u32, InputError> {
    match number(text) {
        Some(v) if v >= u64::from(spec.min) && v <= u64::from(spec.max) => Ok(v as u32),
        _ => Err(rule(format!(
            "cron.expression {} field value {text} is outside {}-{}.",
            spec.name, spec.min, spec.max
        ))),
    }
}

fn step(text: &str, spec: &Spec) -> Result<u32, InputError> {
    match number(text) {
        Some(s) if s >= 1 => Ok(s.min(u64::from(u32::MAX)) as u32),
        _ => Err(rule(format!(
            "cron.expression {} field step must be a positive integer.",
            spec.name
        ))),
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

fn fold(spec: &Spec, v: u32) -> u32 {
    if spec.canonical_max != spec.max && v == spec.max {
        spec.min
    } else {
        v
    }
}

/// dsh `cronFieldValues`.
fn field_values(raw: &str, spec: &Spec) -> Result<Vec<u32>, InputError> {
    let mut matched = std::collections::BTreeSet::new();
    let walk = |set: &mut std::collections::BTreeSet<u32>, from: u32, to: u32, by: u32| {
        let mut v = from;
        while v <= to {
            set.insert(v);
            v = match v.checked_add(by) {
                Some(n) => n,
                None => break,
            };
        }
    };
    for element in raw.split(',') {
        if element.is_empty() {
            return Err(rule(format!(
                "cron.expression {} field must not contain an empty list element.",
                spec.name
            )));
        }
        if element == "*" {
            walk(&mut matched, spec.min, spec.max, 1);
            continue;
        }
        if let Some(rest) = element.strip_prefix('*') {
            let Some(text) = rest.strip_prefix('/').filter(|t| digits(t)) else {
                return Err(invalid_element(spec, element));
            };
            let by = step(text, spec)?;
            walk(&mut matched, spec.min, spec.max, by);
            continue;
        }
        let (range, step_text) = match element.split_once('/') {
            Some((r, s)) => (r, Some(s)),
            None => (element, None),
        };
        let (start_text, end_text) = match range.split_once('-') {
            Some((a, b)) => (a, Some(b)),
            None => (range, None),
        };
        if !digits(start_text)
            || end_text.is_some_and(|e| !digits(e))
            || step_text.is_some_and(|s| !digits(s))
        {
            return Err(invalid_element(spec, element));
        }
        let start = value(start_text, spec)?;
        let Some(end_text) = end_text else {
            if step_text.is_some() {
                return Err(invalid_element(spec, element));
            }
            matched.insert(start);
            continue;
        };
        let last = value(end_text, spec)?;
        if start > last {
            return Err(rule(format!(
                "cron.expression {} field range {start}-{last} is inverted.",
                spec.name
            )));
        }
        let by = match step_text {
            Some(s) => step(s, spec)?,
            None => 1,
        };
        walk(&mut matched, start, last, by);
    }
    let values: std::collections::BTreeSet<u32> =
        matched.into_iter().map(|v| fold(spec, v)).collect();
    Ok(values.into_iter().collect())
}

/// dsh `encodeCronField`: the shortest comma list (never a leading `*`).
fn encode(values: &[u32]) -> String {
    let first = values[0];
    if values.len() == 1 {
        return first.to_string();
    }
    let last = values[values.len() - 1];
    let by = values[1] - first;
    if values.windows(2).all(|w| w[1] - w[0] == by) {
        return if by == 1 {
            format!("{first}-{last}")
        } else {
            format!("{first}-{last}/{by}")
        };
    }
    let mut parts = Vec::new();
    let mut run = first;
    for w in values.windows(2) {
        if w[1] == w[0] + 1 {
            continue;
        }
        parts.push(if run == w[0] {
            run.to_string()
        } else {
            format!("{run}-{}", w[0])
        });
        run = w[1];
    }
    parts.push(if run == last {
        run.to_string()
    } else {
        format!("{run}-{last}")
    });
    parts.join(",")
}

/// dsh `starWalk`.
fn star_walk(spec: &Spec, by: u32) -> Vec<u32> {
    let mut set = std::collections::BTreeSet::new();
    let mut v = spec.min;
    while v <= spec.max {
        set.insert(fold(spec, v));
        v += by;
    }
    set.into_iter().collect()
}

/// dsh `encodeStarCronField`: a `*`-led spelling that keeps the star flag.
fn encode_star(values: &[u32], spec: &Spec) -> String {
    if values.len() as u32 == spec.canonical_max - spec.min + 1 {
        return "*".into();
    }
    let present: std::collections::BTreeSet<u32> = values.iter().copied().collect();
    let mut best: Option<(u32, Vec<u32>)> = None;
    for by in 1..=spec.max - spec.min + 1 {
        let walk = star_walk(spec, by);
        let longer = best.as_ref().is_none_or(|(_, w)| walk.len() > w.len());
        if longer && walk.iter().all(|v| present.contains(v)) {
            best = Some((by, walk));
        }
    }
    let (by, walk) = best.unwrap_or((spec.max - spec.min + 1, vec![spec.min]));
    let remaining: Vec<u32> = values
        .iter()
        .copied()
        .filter(|v| !walk.contains(v))
        .collect();
    if remaining.is_empty() {
        format!("*/{by}")
    } else {
        format!("*/{by},{}", encode(&remaining))
    }
}

/// dsh `parseCronExpression`.
pub fn parse(expression: &str) -> Result<Cron, InputError> {
    if expression.is_empty() || expression.trim() != expression {
        return Err(rule(
            "cron.expression must be a non-empty trimmed string.".into(),
        ));
    }
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(rule("cron.expression must contain exactly five whitespace-separated fields: minute hour day-of-month month day-of-week.".into()));
    }
    let mut values = Vec::new();
    let mut canonical = Vec::new();
    let mut stars = Vec::new();
    for (raw, spec) in fields.iter().zip(FIELDS.iter()) {
        let v = field_values(raw, spec)?;
        let star = raw.starts_with('*');
        canonical.push(if star {
            encode_star(&v, spec)
        } else {
            encode(&v)
        });
        stars.push(star);
        values.push(v);
    }
    let mut values = values.into_iter();
    Ok(Cron {
        expression: canonical.join(" "),
        minutes: values.next().unwrap_or_default(),
        hours: values.next().unwrap_or_default(),
        days_of_month: values.next().unwrap_or_default(),
        months: values.next().unwrap_or_default(),
        days_of_week: values.next().unwrap_or_default(),
        day_of_month_star: stars[2],
        day_of_week_star: stars[4],
    })
}

impl Cron {
    /// dsh `cronDateMatch` (Vixie `DOM_STAR`/`DOW_STAR`).
    pub fn date_matches(&self, date: Date) -> bool {
        if !self.months.contains(&(date.month() as u32)) {
            return false;
        }
        let dom = self.days_of_month.contains(&(date.day() as u32));
        let dow = self
            .days_of_week
            .contains(&(date.weekday().to_sunday_zero_offset() as u32));
        if !self.day_of_month_star && !self.day_of_week_star {
            dom || dow
        } else {
            dom && dow
        }
    }

    /// The rule's local times of day in ascending order.
    pub fn times(&self) -> Vec<Time> {
        let mut out = Vec::new();
        for h in &self.hours {
            for m in &self.minutes {
                if let Ok(t) = Time::new(*h as i8, *m as i8, 0, 0) {
                    out.push(t);
                }
            }
        }
        out
    }

    pub fn month_matches(&self, date: Date) -> bool {
        self.months.contains(&(date.month() as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(e: &str) -> String {
        parse(e).map(|c| c.expression).unwrap_or_else(|e| e.message)
    }

    #[test]
    fn canonical_spellings() {
        assert_eq!(canon("*/15 9-17 * * 1-5"), "*/15 9-17 * * 1-5");
        assert_eq!(canon("0,15,30,45 * * * *"), "0-45/15 * * * *");
        assert_eq!(canon("0 0 1 1 7"), "0 0 1 1 0");
        assert_eq!(canon("5,6,7,9 0 * * 0-7"), "5-7,9 0 * * 0-6");
        assert_eq!(canon("* * * * *"), "* * * * *");
        assert_eq!(canon("*/2,1 * * * *"), "*/2,1 * * * *");
        assert_eq!(canon("0 */5 * * *"), "0 */5 * * *");
    }

    #[test]
    fn errors_match_dsh() {
        assert_eq!(
            canon("* * * *"),
            "cron.expression must contain exactly five whitespace-separated fields: minute hour day-of-month month day-of-week."
        );
        assert_eq!(
            canon("60 * * * *"),
            "cron.expression minute field value 60 is outside 0-59."
        );
        assert_eq!(
            canon("5-1 * * * *"),
            "cron.expression minute field range 5-1 is inverted."
        );
        assert_eq!(
            canon("1/2 * * * *"),
            "cron.expression minute field element \"1/2\" must be *, a value, a-b, */n, a-b/n, or a comma-separated list of those."
        );
        assert_eq!(
            canon("*/0 * * * *"),
            "cron.expression minute field step must be a positive integer."
        );
        assert_eq!(
            canon("1,,2 * * * *"),
            "cron.expression minute field must not contain an empty list element."
        );
        assert_eq!(
            canon(" * * * * *"),
            "cron.expression must be a non-empty trimmed string."
        );
    }

    #[test]
    fn star_flags_select_and_or() {
        let both = parse("0 0 13 * 5").unwrap(); // the 13th OR a Friday
        let fri = Date::new(2026, 10, 23).unwrap();
        let the13 = Date::new(2026, 10, 13).unwrap();
        let mon = Date::new(2026, 10, 19).unwrap();
        assert!(both.date_matches(fri) && both.date_matches(the13) && !both.date_matches(mon));
        let stepped = parse("0 0 */2 * 5").unwrap(); // odd days AND Friday
        assert!(stepped.date_matches(fri) && !stepped.date_matches(mon));
    }
}
