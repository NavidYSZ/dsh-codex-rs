//! Request clock readings (dsh `dsh-time-context`): eligible steps append a
//! source-attributed time reading as the last message of the entering batch,
//! at most once per refresh interval.
use std::sync::Arc;

use async_trait::async_trait;
use dshx_agent::{PreStep, PreStepHook};
use dshx_core::{ContentBlock, Message, MessageSource, Role, SessionEvent, kinds};
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::service::Clock;
use crate::time::{Zone, canonical_zone, iana_shape, system_zone};

const NAME: &str = "time-context";

/// The browser zone facts of one open turn.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowserZone {
    Resolved(String),
    Mixed(Vec<String>),
    Missing,
}

/// dsh `formatDuration`: compact whole-second units.
pub fn format_duration(elapsed_ms: i64) -> String {
    let mut s = elapsed_ms.max(0) / 1000;
    let days = s / 86_400;
    s %= 86_400;
    let hours = s / 3600;
    s %= 3600;
    let minutes = s / 60;
    s %= 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{s}s"));
    parts.join(" ")
}

/// dsh `formatTimestamp`: `YYYY-MM-DDTHH:mm:ss±HH:MM[Zone]` (no milliseconds).
pub fn format_timestamp(now_ms: i64, zone: &Zone) -> String {
    let ts = Timestamp::from_millisecond(now_ms).unwrap_or(Timestamp::UNIX_EPOCH);
    let offset = zone.tz.to_offset(ts);
    let dt = offset.to_datetime(ts);
    let total = offset.seconds();
    let sign = if total < 0 { '-' } else { '+' };
    let abs = total.unsigned_abs();
    let (h, m, sec) = (abs / 3600, abs % 3600 / 60, abs % 60);
    let offset_text = if sec == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{sec:02}")
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{offset_text}[{}]",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        zone.name
    )
}

/// dsh `browserTimeZone`: a host-canonicalized zone on a user-rpc message.
fn browser_zone(message: &Value) -> Option<String> {
    let s = &message["source"];
    if s["kind"] != "user" || !s["rpcId"].is_string() {
        return None;
    }
    let zone = s["clientTimeZone"].as_str()?;
    (zone == "UTC" || iana_shape(zone))
        .then(|| canonical_zone(zone).ok().map(|z| z.name))
        .flatten()
}

fn derive_browser(messages: &[Value]) -> BrowserZone {
    let mut zones: Vec<String> = messages.iter().filter_map(browser_zone).collect();
    zones.sort();
    zones.dedup();
    match zones.len() {
        0 => BrowserZone::Missing,
        1 => BrowserZone::Resolved(zones.remove(0)),
        _ => BrowserZone::Mixed(zones),
    }
}

fn render_browser(b: &BrowserZone) -> String {
    match b {
        BrowserZone::Resolved(z) => format!(
            "Browser time zone for this request: {z}. Interpret otherwise-unqualified dates and times in this zone."
        ),
        BrowserZone::Mixed(z) => format!(
            "Browser time zone for this request: mixed {}. Ask the user to clarify otherwise-unqualified dates and times.",
            serde_json::to_string(z).unwrap_or_default()
        ),
        BrowserZone::Missing => "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.".into(),
    }
}

/// The `timeContext` projection over the committed events.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Projection {
    last_message: Option<u64>,
    last_injection: Option<u64>,
    last_turn_injection: Option<u64>,
}

fn project(events: &[SessionEvent]) -> Projection {
    let mut p = Projection::default();
    for e in events {
        match e.kind.as_str() {
            kinds::TURN_START | kinds::TURN_END => p.last_turn_injection = None,
            kinds::USER_MESSAGE => {
                p.last_message = Some(e.time);
                if e.data["source"]["kind"] == NAME {
                    p.last_injection = Some(e.time);
                    p.last_turn_injection = Some(e.time);
                }
            }
            kinds::ASSISTANT_MESSAGE | kinds::TOOL_RESULT => p.last_message = Some(e.time),
            _ => {}
        }
    }
    p
}

/// The pre-step hook. Register it after every other hook so the reading is
/// the last entering message (dsh registers it with `prepend: true`).
pub struct TimeContextHook {
    zone: Zone,
    refresh_ms: u64,
    clock: Clock,
}

impl TimeContextHook {
    /// `time_zone`: an IANA zone or `None` for the system zone.
    pub fn new(
        time_zone: Option<&str>,
        refresh_ms: u64,
        clock: Option<Clock>,
    ) -> anyhow::Result<Arc<Self>> {
        let zone = match time_zone {
            Some(z) => canonical_zone(z).map_err(|_| {
                anyhow::anyhow!(
                    "time-context: invalid IANA timeZone {}",
                    serde_json::to_string(z).unwrap_or_default()
                )
            })?,
            None => system_zone(),
        };
        Ok(Arc::new(TimeContextHook {
            zone,
            refresh_ms,
            clock: clock.unwrap_or_else(|| Arc::new(|| dshx_core::now_ms() as i64)),
        }))
    }

    /// The reading for one step (dsh `renderText`).
    pub fn render(
        &self,
        now: i64,
        turn: u64,
        step: u64,
        previous: Option<i64>,
        zone: &Zone,
        browser_line: &str,
    ) -> String {
        let elapsed =
            previous.map_or_else(|| "unavailable".to_owned(), |p| format_duration(now - p));
        let baseline = if step == 1 {
            "model-visible message"
        } else {
            "step context"
        };
        format!(
            "Time sampled while preparing turn {turn}, step {step}: {}\n{browser_line}\nElapsed since the preceding {baseline}: {elapsed}.",
            format_timestamp(now, zone)
        )
    }
}

#[async_trait]
impl PreStepHook for TimeContextHook {
    async fn pre_step(
        &self,
        step: &PreStep<'_>,
        entering: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
        if step.rejected() {
            return Ok(());
        }
        let now = (self.clock)();
        let (projection, entered) = step.read_history(|events, _| {
            let start = events
                .iter()
                .rposition(|e| e.kind == kinds::TURN_START && e.data["turn"] == json!(step.turn));
            let entered: Vec<Value> = start
                .map(|s| {
                    events[s..]
                        .iter()
                        .filter(|e| e.kind == kinds::USER_MESSAGE)
                        .map(|e| e.data.clone())
                        .collect()
                })
                .unwrap_or_default();
            (project(events), entered)
        });
        if self.refresh_ms > 0
            && let Some(last) = projection.last_injection
            && now >= last as i64
            && ((now - last as i64) as u64) < self.refresh_ms
        {
            return Ok(());
        }
        let previous = if step.step == 1 {
            projection.last_message
        } else {
            projection.last_turn_injection
        };
        let mut messages = entered;
        messages.extend(entering.iter().filter_map(|m| serde_json::to_value(m).ok()));
        let browser = derive_browser(&messages);
        let zone = match &browser {
            BrowserZone::Resolved(z) => canonical_zone(z).unwrap_or_else(|_| self.zone.clone()),
            _ => self.zone.clone(),
        };
        let text = self.render(
            now,
            step.turn,
            step.step,
            previous.map(|p| p as i64),
            &zone,
            &render_browser(&browser),
        );
        entering.push(Message {
            id: dshx_core::new_id(),
            role: Role::User,
            content: vec![ContentBlock::text(text.clone())],
            source: MessageSource::Other(json!({
                "kind": NAME,
                "form": "snapshot",
                "sections": [{"name": NAME, "text": text}],
            })),
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_match_dsh() {
        let berlin = canonical_zone("Europe/Berlin").unwrap();
        // The reading dsh 0.2.0-rc.2 logged in a captured session.
        assert_eq!(
            format_timestamp(1_792_617_122_412, &berlin),
            "2026-10-21T23:12:02+02:00[Europe/Berlin]"
        );
        let utc = canonical_zone("UTC").unwrap();
        assert_eq!(format_timestamp(0, &utc), "1970-01-01T00:00:00+00:00[UTC]");
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(90_061_500), "1d 1h 1m 1s");
        assert_eq!(format_duration(3_600_000), "1h 0s");
        let hook = TimeContextHook::new(Some("Europe/Berlin"), 600_000, None).unwrap();
        assert_eq!(
            hook.render(
                1_792_617_122_412,
                1,
                1,
                None,
                &berlin,
                &render_browser(&BrowserZone::Missing)
            ),
            "Time sampled while preparing turn 1, step 1: 2026-10-21T23:12:02+02:00[Europe/Berlin]\nBrowser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.\nElapsed since the preceding model-visible message: unavailable."
        );
        assert!(TimeContextHook::new(Some("Nowhere"), 0, None).is_err());
    }

    #[test]
    fn browser_zones() {
        let msg = |z: &str| json!({"source": {"kind": "user", "rpcId": "r", "clientTimeZone": z}});
        assert_eq!(
            derive_browser(&[msg("Europe/Berlin")]),
            BrowserZone::Resolved("Europe/Berlin".into())
        );
        assert_eq!(
            derive_browser(&[msg("UTC"), msg("Europe/Berlin")]),
            BrowserZone::Mixed(vec!["Europe/Berlin".into(), "UTC".into()])
        );
        assert_eq!(
            derive_browser(&[json!({"source": {"kind": "user"}})]),
            BrowserZone::Missing
        );
    }
}
