//! Token prices and usage statistics: the single source of every cost the UI
//! shows (`GET /api/sessions/{id}/stats`, `GET /api/usage`). Costs are USD.
//!
//! A request costs `(input × miss + cache_read × hit + cache_write × write +
//! output × out) / 1e6` at the price that applied when it was made (the time
//! of its `assistant/message` or `compaction/summary` event). The counters are
//! disjoint like [`TokenUsage`]: `input` is uncached input only.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dshx_agent::TurnEndCompaction;
use dshx_core::{ContentBlock, Message, Role, SessionEvent, TokenUsage};
use dshx_session::{SessionStore, SessionSummary};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CURRENCY: &str = "USD";

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

// ---- prices ------------------------------------------------------------------

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    /// Uncached input (DeepSeek's "cache miss").
    pub input: f64,
    pub output: f64,
    /// Input served from the prompt cache ("cache hit").
    pub cache_read: f64,
    /// Input written to the prompt cache.
    pub cache_write: f64,
}

impl Price {
    fn scaled(self, factor: f64) -> Price {
        Price {
            input: self.input * factor,
            output: self.output * factor,
            cache_read: self.cache_read * factor,
            cache_write: self.cache_write * factor,
        }
    }
}

/// One model's price; `off_peak` applies outside DeepSeek's peak hours.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    pub price: Price,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_peak: Option<Price>,
}

// DeepSeek list prices from https://api-docs.deepseek.com/quick_start/pricing
// (checked 22.10.2026). Off-peak rates are half the peak rates. DeepSeek bills
// no surcharge for filling its prompt cache: input that is not a cache hit is a
// cache miss. Its Anthropic endpoint reports such input as `input_tokens` (and,
// if ever, `cache_creation_input_tokens`, our disjoint `cacheWriteTokens`), so
// a cache write is priced like a miss.
const DEEPSEEK_FLASH: Price = Price {
    input: 0.30,
    output: 1.20,
    cache_read: 0.006,
    cache_write: 0.30,
};
const DEEPSEEK_V4_PRO: Price = Price {
    input: 1.32,
    output: 3.96,
    cache_read: 0.044,
    cache_write: 1.32,
};

/// Prices per model. Built in: DeepSeek's official models on DeepSeek's clock.
/// Other providers add flat prices with [`PriceBook::with_override`] (the
/// `cost` of a `[providers.*]` model); a model without a price counts tokens only.
#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    /// Exact `(provider, model)` prices; they win over the built-in ones.
    overrides: BTreeMap<(String, String), ModelPrice>,
    /// Built-in prices by model id, for DeepSeek's own routes.
    builtin: BTreeMap<String, ModelPrice>,
    /// Flat prices of configured models over time ([`price_book_at`]): a
    /// request keeps the price of its time, also after its provider changed
    /// or was removed.
    history: BTreeMap<(String, String), Vec<PricePoint>>,
}

/// A flat price from `since` (ms) on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PricePoint {
    pub since: u64,
    pub price: Price,
}

/// The point that applied at `time`; before the first one, the first (the
/// price was set before dsh-codex started recording).
fn price_at(points: &[PricePoint], time: u64) -> Option<Price> {
    points
        .iter()
        .rev()
        .find(|p| p.since <= time)
        .or(points.first())
        .map(|p| p.price)
}

/// The price of one request and the clock it was read from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quote {
    pub price: Price,
    pub off_peak: bool,
    /// No holiday table for the request's year: peak was assumed.
    pub approximate: bool,
}

impl PriceBook {
    /// The built-in DeepSeek prices.
    pub fn deepseek() -> Self {
        let mut builtin = BTreeMap::new();
        for (id, price) in [
            ("deepseek-flash", DEEPSEEK_FLASH),
            ("deepseek-v4-pro", DEEPSEEK_V4_PRO),
        ] {
            builtin.insert(
                id.to_owned(),
                ModelPrice {
                    price,
                    off_peak: Some(price.scaled(0.5)),
                },
            );
        }
        PriceBook {
            overrides: BTreeMap::new(),
            builtin,
            history: BTreeMap::new(),
        }
    }

    /// A flat price for `model` of `provider`, at every hour.
    #[allow(dead_code)] // for the `cost` of configured providers
    pub fn with_override(mut self, provider: &str, model: &str, price: Price) -> Self {
        self.overrides.insert(
            (provider.to_owned(), model.to_owned()),
            ModelPrice {
                price,
                off_peak: None,
            },
        );
        self
    }

    pub fn lookup(&self, provider: &str, model: &str) -> Option<ModelPrice> {
        let key = (provider.to_owned(), model.to_owned());
        if let Some(p) = self.overrides.get(&key) {
            return Some(*p);
        }
        if let Some(last) = self.history.get(&key).and_then(|points| points.last()) {
            return Some(ModelPrice {
                price: last.price,
                off_peak: None,
            });
        }
        // Old or foreign logs may lack the provider; DeepSeek's routes are `deepseek-*`.
        (provider.is_empty() || provider.starts_with("deepseek"))
            .then(|| self.builtin.get(model).copied())
            .flatten()
    }

    /// The price of a request to `model` made at `time` (ms since the epoch).
    pub fn quote(&self, provider: &str, model: &str, time: u64) -> Option<Quote> {
        if let Some(price) = self
            .history
            .get(&(provider.to_owned(), model.to_owned()))
            .and_then(|points| price_at(points, time))
        {
            return Some(Quote {
                price,
                off_peak: false,
                approximate: false,
            });
        }
        let p = self.lookup(provider, model)?;
        let Some(off_peak) = p.off_peak else {
            return Some(Quote {
                price: p.price,
                off_peak: false,
                approximate: false,
            });
        };
        let (peak, approximate) = deepseek_peak(time);
        Some(Quote {
            price: if peak { p.price } else { off_peak },
            off_peak: !peak,
            approximate,
        })
    }

    /// Changes whenever a price does (cached costs are recomputed then).
    pub fn fingerprint(&self) -> String {
        let overrides: Vec<_> = self.overrides.iter().collect();
        let history: Vec<_> = self.history.iter().collect();
        serde_json::json!({ "builtin": self.builtin, "overrides": overrides, "history": history })
            .to_string()
    }
}

/// `$DSHX_HOME/prices.json`: every flat price a configured model had, with
/// the time it was first seen, keyed `provider/model`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PriceHistoryFile {
    #[serde(default)]
    models: BTreeMap<String, Vec<PricePoint>>,
}

/// The prices for the current settings plus the recorded history: a price
/// seen for the first time is recorded from `now` on, so costs already
/// shown keep their price when a provider's prices change or the provider
/// is removed.
pub fn price_book_at(home: &Path, config: &dshx_core::config::Config, now: u64) -> PriceBook {
    let mut book = price_book(config);
    let path = home.join("prices.json");
    let mut file: PriceHistoryFile = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut changed = false;
    for ((provider, model), current) in &book.overrides {
        let points = file
            .models
            .entry(format!("{provider}/{model}"))
            .or_default();
        if points.last().map(|p| p.price) != Some(current.price) {
            points.push(PricePoint {
                since: now,
                price: current.price,
            });
            changed = true;
        }
    }
    if changed {
        let tmp = path.with_extension("json.tmp");
        let written = serde_json::to_vec_pretty(&file)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            tracing::warn!("{}: {e}", path.display());
        }
    }
    for (key, points) in file.models {
        // Provider ids have no `/`; model ids may.
        if let Some((provider, model)) = key.split_once('/') {
            book.history
                .insert((provider.to_owned(), model.to_owned()), points);
        }
    }
    book
}

/// The prices for the current settings: DeepSeek's built-in ones plus the
/// flat `cost` of every model of the configured providers (catalog prices
/// for catalog providers such as `anthropic`).
pub fn price_book(config: &dshx_core::config::Config) -> PriceBook {
    let mut book = PriceBook::deepseek();
    for (id, provider) in &config.providers {
        // A broken table is reported where it is used; it simply has no prices.
        let Ok(spec) = dshx_llm::provider::ProviderSpec::resolve(id, provider) else {
            continue;
        };
        for model in &spec.models {
            if let Some(cost) = model.cost {
                book = book.with_override(
                    &spec.id,
                    &model.id,
                    Price {
                        input: cost.input,
                        output: cost.output,
                        cache_read: cost.cache_read_price(),
                        cache_write: cost.cache_write_price(),
                    },
                );
            }
        }
    }
    book
}

/// Inclusive `((month, day), (month, day))` range of one year.
type DayRange = ((u32, u32), (u32, u32));

/// Chinese public holidays 2026 (State Council notice of 4.11.2025), as
/// inclusive `(month, day)` ranges. Weekend make-up workdays stay off-peak:
/// DeepSeek's peak hours fall on Monday to Friday only.
const CN_HOLIDAYS_2026: [DayRange; 7] = [
    ((1, 1), (1, 3)),
    ((2, 15), (2, 23)),
    ((4, 4), (4, 6)),
    ((5, 1), (5, 5)),
    ((6, 19), (6, 21)),
    ((9, 25), (9, 27)),
    ((10, 1), (10, 7)),
];

fn cn_holidays(year: i64) -> Option<&'static [DayRange]> {
    match year {
        2026 => Some(&CN_HOLIDAYS_2026),
        _ => None,
    }
}

/// DeepSeek's clock: peak is 01:00–04:00 and 06:00–10:00 UTC, Monday to
/// Friday, except Chinese public holidays; everything else is off-peak.
/// Returns `(peak, approximate)`; a weekday in a peak window of a year without
/// a holiday table counts as peak and is approximate. Both windows lie within
/// the same calendar day in UTC and in Beijing, so the UTC date decides.
pub fn deepseek_peak(time: u64) -> (bool, bool) {
    let ms = time as i64;
    let days = ms.div_euclid(DAY_MS);
    // 1970-01-01 was a Thursday; 0 = Monday.
    let weekday = (days + 3).rem_euclid(7);
    let hour = ms.rem_euclid(DAY_MS) / HOUR_MS as i64;
    if weekday >= 5 || !((1..4).contains(&hour) || (6..10).contains(&hour)) {
        return (false, false);
    }
    let (year, month, day) = civil(days);
    match cn_holidays(year) {
        Some(ranges) => {
            let holiday = ranges
                .iter()
                .any(|&(from, to)| (month, day) >= from && (month, day) <= to);
            (!holiday, false)
        }
        None => (true, true),
    }
}

/// Days since 1970-01-01 → `(year, month, day)` (proleptic Gregorian).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn day_label(days: i64) -> String {
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

// ---- tallies -----------------------------------------------------------------

/// USD per kind of token.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostSplit {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

/// Tokens and costs of one or more requests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Tally {
    pub requests: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Includes the reasoning tokens.
    pub output_tokens: u64,
    /// Only where the provider reports them.
    pub reasoning_tokens: u64,
    /// Of the priced requests.
    pub cost_usd: f64,
    pub cost: CostSplit,
    /// What the cache hits would have cost as uncached input.
    pub saved_usd: f64,
    /// Requests billed at DeepSeek's off-peak price.
    pub off_peak_requests: u64,
    /// Requests of a model without a price (tokens only).
    pub unpriced_requests: u64,
    /// Peak was assumed for some request (no holiday table for its year).
    pub approximate: bool,
}

impl Tally {
    pub fn add(&mut self, usage: &TokenUsage, quote: Option<Quote>) {
        let read = usage.cache_read_tokens.unwrap_or(0);
        let write = usage.cache_write_tokens.unwrap_or(0);
        self.requests += 1;
        self.input_tokens += usage.input_tokens;
        self.cache_read_tokens += read;
        self.cache_write_tokens += write;
        self.output_tokens += usage.output_tokens;
        self.reasoning_tokens += usage.reasoning_tokens.unwrap_or(0);
        let Some(q) = quote else {
            self.unpriced_requests += 1;
            return;
        };
        let per = |tokens: u64, usd: f64| tokens as f64 * usd / 1e6;
        let split = CostSplit {
            input: per(usage.input_tokens, q.price.input),
            cache_read: per(read, q.price.cache_read),
            cache_write: per(write, q.price.cache_write),
            output: per(usage.output_tokens, q.price.output),
        };
        self.cost.input += split.input;
        self.cost.cache_read += split.cache_read;
        self.cost.cache_write += split.cache_write;
        self.cost.output += split.output;
        self.cost_usd += split.input + split.cache_read + split.cache_write + split.output;
        self.saved_usd += per(read, q.price.input - q.price.cache_read);
        self.off_peak_requests += u64::from(q.off_peak);
        self.approximate |= q.approximate;
    }

    pub fn merge(&mut self, other: &Tally) {
        self.requests += other.requests;
        self.input_tokens += other.input_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cost_usd += other.cost_usd;
        self.cost.input += other.cost.input;
        self.cost.cache_read += other.cost.cache_read;
        self.cost.cache_write += other.cost.cache_write;
        self.cost.output += other.cost.output;
        self.saved_usd += other.saved_usd;
        self.off_peak_requests += other.off_peak_requests;
        self.unpriced_requests += other.unpriced_requests;
        self.approximate |= other.approximate;
    }

    fn tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens + self.output_tokens
    }
}

/// A billed model request in the log: `(time, provider, model, usage)` of an
/// `assistant/message` or `compaction/summary` event that reports usage.
fn request_of(
    e: &SessionEvent,
    provider: &str,
    model: &str,
) -> Option<(String, String, TokenUsage)> {
    let usage: TokenUsage = serde_json::from_value(e.data.get("usage")?.clone()).ok()?;
    let (p, m) = match e.kind.as_str() {
        "assistant/message" => {
            let source = e.data.pointer("/message/source");
            (
                source.and_then(|s| s.get("provider")),
                source.and_then(|s| s.get("model")),
            )
        }
        "compaction/summary" => (e.data.get("provider"), e.data.get("model")),
        _ => return None,
    };
    let p = p.and_then(Value::as_str).unwrap_or(provider).to_owned();
    let m = m.and_then(Value::as_str).unwrap_or(model).to_owned();
    Some((p, m, usage))
}

// ---- one session -------------------------------------------------------------

/// Wall times, folded like dsh `sessionStats`: model time is `step/start` →
/// `assistant/message`, the first token is the first non-empty delta, decode
/// time runs from it to the message on steps that report output tokens, and
/// tool time pairs `tool/call` → `tool/result`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    pub steps: u64,
    pub llm_ms: u64,
    pub tool_ms: u64,
    pub ttft_ms: u64,
    pub ttft_steps: u64,
    pub decode_ms: u64,
    pub decode_tokens: u64,
    pub avg_ttft_ms: Option<f64>,
    pub tokens_per_second: Option<f64>,
}

impl Timings {
    fn finish(&mut self) {
        self.avg_ttft_ms =
            (self.ttft_steps > 0).then(|| self.ttft_ms as f64 / self.ttft_steps as f64);
        // A stream that arrived at once (a cache, a mock) has no meaningful speed.
        self.tokens_per_second = (self.decode_ms >= MIN_DECODE_MS)
            .then(|| self.decode_tokens as f64 / (self.decode_ms as f64 / 1000.0));
    }
}

/// Decode time below which tokens per second are not shown.
const MIN_DECODE_MS: u64 = 200;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestStats {
    pub seq: u64,
    pub time: u64,
    pub turn: Option<u64>,
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub usage: Tally,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStats {
    pub turn: u64,
    pub start: Option<u64>,
    pub end: Option<u64>,
    /// `turn/end` reason kind; `None` while the turn runs.
    pub status: Option<String>,
    #[serde(flatten)]
    pub usage: Tally,
    /// `provider/model` of its requests, in order of first use.
    pub routes: Vec<String>,
    pub timings: Timings,
}

/// Heuristic split (four characters per token, like dsh's token meter).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakdown {
    pub system_tokens: u64,
    pub tools_tokens: u64,
    pub message_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStats {
    pub auto: bool,
    /// Estimated history size at which the agent compacts before the next step.
    pub threshold_tokens: u64,
    pub threshold_percent: f64,
    /// Compaction after a completed turn: `threshold`, `every-turn` or `off`.
    pub mode: &'static str,
    /// The context size that triggers it in `threshold` mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_end_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextStats {
    pub context_window: Option<u64>,
    /// What the next request starts from: input and output of the last one.
    pub used_tokens: u64,
    /// `used_tokens` is the heuristic estimate (after a compaction).
    pub estimated: bool,
    pub percent: Option<f64>,
    pub breakdown: Breakdown,
    pub compaction: Option<CompactionStats>,
}

/// The agent's pressure-compaction rule (dsh `compaction-basic`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionRule {
    pub auto: bool,
    pub threshold_ratio: f64,
    pub headroom_tokens: u64,
    /// Output cap of the route when the request header names none.
    pub default_max_tokens: u64,
    /// dsh-codex's compaction after a completed turn.
    pub turn_end: TurnEndCompaction,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub currency: &'static str,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub turn_count: u64,
    pub totals: Tally,
    pub timings: Timings,
    pub last_request: Option<RequestStats>,
    pub turns: Vec<TurnStats>,
    pub context: ContextStats,
}

struct OpenStep {
    turn: u64,
    step: u64,
    start: u64,
    first_token: Option<u64>,
}

/// dsh `assistantStreamFirstTokenTime`: the first non-empty delta of a
/// compact stream record list (a named tool call counts at its start).
fn first_token_time(stream: &Value) -> Option<u64> {
    for rec in stream.as_array()? {
        let time = match rec.get("type").and_then(Value::as_str) {
            Some("chunk") => {
                let c = &rec["chunk"];
                let token = match c.get("type").and_then(Value::as_str) {
                    Some("text-delta" | "reasoning-delta") => {
                        c["text"].as_str().is_some_and(|t| !t.is_empty())
                    }
                    Some("tool-call-delta") => {
                        c["argumentsDelta"].as_str().is_some_and(|t| !t.is_empty())
                            || c.get("name").is_some_and(|n| !n.is_null())
                    }
                    _ => false,
                };
                if token { rec["time"].as_u64() } else { None }
            }
            Some("tool-call-chunks") if rec.get("name").is_some_and(|n| !n.is_null()) => {
                rec["time0"].as_u64()
            }
            Some("text-chunks" | "reasoning-chunks" | "tool-call-chunks") => {
                let key = if rec["type"] == "tool-call-chunks" {
                    "args"
                } else {
                    "texts"
                };
                let parts = rec[key].as_array().map(Vec::as_slice).unwrap_or_default();
                let dt = rec["dt"].as_array().map(Vec::as_slice).unwrap_or_default();
                let mut time = rec["time0"].as_i64().unwrap_or(0);
                let mut found = None;
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        time += dt.get(i - 1).and_then(Value::as_i64).unwrap_or(0);
                    }
                    if part.as_str().is_some_and(|p| !p.is_empty()) {
                        found = u64::try_from(time).ok();
                        break;
                    }
                }
                found
            }
            _ => None,
        };
        if time.is_some() {
            return time;
        }
    }
    None
}

/// JS `string.length`, which dsh's estimates count.
fn js_len(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

fn estimate_message(m: &Message) -> u64 {
    let quarter = |n: u64| n.div_ceil(4);
    let blocks: u64 = m
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                quarter(js_len(text)) + 4
            }
            ContentBlock::ToolCall {
                name, arguments, ..
            } => quarter(js_len(name)) + quarter(js_len(arguments)) + 4,
            other => quarter(js_len(&serde_json::to_string(other).unwrap_or_default())) + 4,
        })
        .sum();
    blocks + 4
}

fn tools_estimate(header: Option<&Value>) -> u64 {
    match header
        .and_then(|h| h.get("tools"))
        .and_then(Value::as_array)
    {
        Some(tools) if !tools.is_empty() => {
            js_len(&Value::Array(tools.clone()).to_string()).div_ceil(4) + 4
        }
        _ => 0,
    }
}

/// Tokens, costs and timings of one session log. `from_turn` limits the turn
/// list (the totals always cover the whole log).
pub fn session_stats(
    events: &[SessionEvent],
    prices: &PriceBook,
    rule: Option<CompactionRule>,
    default_window: Option<u64>,
    from_turn: Option<u64>,
) -> SessionStats {
    let mut turns: Vec<TurnStats> = Vec::new();
    let mut index: HashMap<u64, usize> = HashMap::new();
    let mut totals = Tally::default();
    let mut timings = Timings::default();
    let mut open: Option<OpenStep> = None;
    let mut pending: HashMap<String, (u64, Option<u64>)> = HashMap::new();
    let mut last: Option<RequestStats> = None;
    let mut current_turn: Option<u64> = None;
    let mut provider = String::new();
    let mut model = String::new();
    let mut window: Option<u64> = None;
    let mut header: Option<Value> = None;
    let mut compacted = false;

    fn turn_mut<'a>(
        turns: &'a mut Vec<TurnStats>,
        index: &mut HashMap<u64, usize>,
        n: u64,
    ) -> &'a mut TurnStats {
        let i = *index.entry(n).or_insert_with(|| {
            turns.push(TurnStats {
                turn: n,
                ..TurnStats::default()
            });
            turns.len() - 1
        });
        &mut turns[i]
    }

    for e in events {
        let d = &e.data;
        let turn_no = d.get("turn").and_then(Value::as_u64);
        match e.kind.as_str() {
            "turn/start" => {
                if let Some(n) = turn_no {
                    turn_mut(&mut turns, &mut index, n).start = Some(e.time);
                    current_turn = Some(n);
                }
            }
            "turn/end" => {
                if let Some(n) = turn_no.or(current_turn) {
                    let t = turn_mut(&mut turns, &mut index, n);
                    t.end = Some(e.time);
                    t.status = Some(
                        d.pointer("/reason/kind")
                            .and_then(Value::as_str)
                            .unwrap_or("completed")
                            .to_owned(),
                    );
                }
                current_turn = None;
                pending.clear();
            }
            "step/start" => {
                if let (Some(turn), Some(step)) = (turn_no, d.get("step").and_then(Value::as_u64)) {
                    open = Some(OpenStep {
                        turn,
                        step,
                        start: e.time,
                        first_token: None,
                    });
                }
            }
            "step/end" => {
                timings.steps += 1;
                if let Some(n) = turn_no {
                    turn_mut(&mut turns, &mut index, n).timings.steps += 1;
                }
                open = None;
            }
            "assistant/attempt" => {
                if let Some(o) = open.as_mut().filter(|o| {
                    Some(o.turn) == turn_no && Some(o.step) == d.get("step").and_then(Value::as_u64)
                }) && o.first_token.is_none()
                {
                    o.first_token = first_token_time(&d["stream"]);
                }
            }
            "assistant/message" | "compaction/summary" => {
                let turn = turn_no.or(current_turn);
                if let Some((p, m, usage)) = request_of(e, &provider, &model) {
                    let quote = prices.quote(&p, &m, e.time);
                    let mut one = Tally::default();
                    one.add(&usage, quote);
                    totals.merge(&one);
                    if let Some(n) = turn {
                        let t = turn_mut(&mut turns, &mut index, n);
                        t.usage.merge(&one);
                        let route = format!("{p}/{m}");
                        if !t.routes.contains(&route) {
                            t.routes.push(route);
                        }
                    }
                    last = Some(RequestStats {
                        seq: e.seq,
                        time: e.time,
                        turn,
                        provider: p.clone(),
                        model: m.clone(),
                        usage: one,
                    });
                    compacted = false;
                    if e.kind == "assistant/message" {
                        provider = p;
                        model = m;
                    }
                }
                if e.kind != "assistant/message" {
                    continue;
                }
                let step = d.get("step").and_then(Value::as_u64);
                let Some(o) = open.take_if(|o| Some(o.turn) == turn_no && Some(o.step) == step)
                else {
                    continue;
                };
                let add = |t: &mut Timings| {
                    t.llm_ms += e.time.saturating_sub(o.start);
                    if let Some(first) = o.first_token.or_else(|| first_token_time(&d["stream"])) {
                        t.ttft_ms += first.saturating_sub(o.start);
                        t.ttft_steps += 1;
                        if let Some(out) = d.pointer("/usage/outputTokens").and_then(Value::as_u64)
                        {
                            t.decode_ms += e.time.saturating_sub(first);
                            t.decode_tokens += out;
                        }
                    }
                };
                add(&mut timings);
                add(&mut turn_mut(&mut turns, &mut index, o.turn).timings);
            }
            "tool/call" => {
                if let Some(id) = d.get("callId").and_then(Value::as_str) {
                    pending.insert(id.to_owned(), (e.time, turn_no.or(current_turn)));
                }
            }
            "tool/result" => {
                let id = d
                    .pointer("/message/source/callId")
                    .or_else(|| d.pointer("/message/toolCallId"))
                    .and_then(Value::as_str);
                if let Some((start, turn)) = id.and_then(|id| pending.remove(id)) {
                    let ms = e.time.saturating_sub(start);
                    timings.tool_ms += ms;
                    if let Some(n) = turn {
                        turn_mut(&mut turns, &mut index, n).timings.tool_ms += ms;
                    }
                }
            }
            "compaction/end" => compacted = last.is_some(),
            "request/context" => {
                if let Some(w) = d.get("contextWindow").and_then(Value::as_u64) {
                    window = Some(w);
                }
                if let Some(m) = d.get("model").and_then(Value::as_str) {
                    model = m.to_owned();
                }
                if let Some(p) = d.get("provider").and_then(Value::as_str) {
                    provider = p.to_owned();
                }
            }
            "request/header" => header = d.get("header").cloned(),
            _ => {}
        }
    }

    // What the next request will carry, by kind (heuristic, like dsh's meter).
    let mut breakdown = Breakdown {
        tools_tokens: tools_estimate(header.as_ref()),
        ..Breakdown::default()
    };
    for m in dshx_session::derive_messages(events) {
        let tokens = estimate_message(&m);
        if m.role == Role::System {
            breakdown.system_tokens += tokens;
        } else {
            breakdown.message_tokens += tokens;
        }
    }
    let estimate = breakdown.system_tokens + breakdown.tools_tokens + breakdown.message_tokens;
    let (used_tokens, estimated) = match &last {
        Some(r) if !compacted => (r.usage.tokens(), false),
        _ => (estimate, true),
    };
    let window = window.or(default_window).filter(|w| *w > 0);
    let percent = window.map(|w| (used_tokens as f64 / w as f64 * 100.0).min(100.0));
    let compaction = rule.zip(window).map(|(r, w)| {
        let reserved = header
            .as_ref()
            .and_then(|h| h.pointer("/config/maxTokens"))
            .and_then(Value::as_u64)
            .unwrap_or(r.default_max_tokens);
        let pressure = w.saturating_sub(reserved).saturating_sub(r.headroom_tokens);
        let threshold = (w as f64 * r.threshold_ratio).min(pressure as f64).floor() as u64;
        let (mode, turn_end_tokens) = match r.turn_end {
            _ if !r.auto => ("off", None),
            TurnEndCompaction::Threshold(tokens) => ("threshold", Some(tokens)),
            TurnEndCompaction::EveryTurn => ("every-turn", None),
            TurnEndCompaction::Off => ("off", None),
        };
        CompactionStats {
            auto: r.auto,
            threshold_tokens: threshold,
            threshold_percent: threshold as f64 / w as f64 * 100.0,
            mode,
            turn_end_tokens,
        }
    });

    timings.finish();
    for t in &mut turns {
        t.timings.finish();
    }
    let turn_count = turns.len() as u64;
    if let Some(from) = from_turn {
        turns.retain(|t| t.turn >= from);
    }
    SessionStats {
        currency: CURRENCY,
        provider: (!provider.is_empty()).then_some(provider),
        model: (!model.is_empty()).then_some(model),
        turn_count,
        totals,
        timings,
        last_request: last,
        turns,
        context: ContextStats {
            context_window: window,
            used_tokens,
            estimated,
            percent,
            breakdown,
            compaction,
        },
    }
}

// ---- all sessions ------------------------------------------------------------

/// Usage of one model in one hour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bucket {
    hour: u64,
    provider: String,
    model: String,
    #[serde(flatten)]
    usage: Tally,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexedSession {
    /// `[updatedAt, sizeBytes, eventCount]` of the listing the buckets came from.
    marker: (u64, u64, u64),
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    updated_at: u64,
    #[serde(default)]
    parent: Option<String>,
    buckets: Vec<Bucket>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    version: u32,
    /// [`PriceBook::fingerprint`] the costs were computed with.
    #[serde(default)]
    prices: String,
    #[serde(default)]
    sessions: BTreeMap<String, IndexedSession>,
}

/// 2: requests inherited by a fork (branch or forked subagent) count only
/// in the session that made them.
const INDEX_VERSION: u32 = 2;

fn marker(row: &SessionSummary) -> (u64, u64, u64) {
    (row.updated_at, row.size_bytes, row.event_count)
}

fn buckets_of(events: &[SessionEvent], prices: &PriceBook) -> Vec<Bucket> {
    let mut by_key: BTreeMap<(u64, String, String), Tally> = BTreeMap::new();
    let (mut provider, mut model) = (String::new(), String::new());
    let inherited = events
        .iter()
        .rposition(|e| {
            e.kind == "session/end-seed" && e.data.get("inherited") == Some(&Value::Bool(true))
        })
        .map_or(0, |cut| cut + 1);
    for (i, e) in events.iter().enumerate() {
        if e.kind == "request/context" {
            if let Some(p) = e.data.get("provider").and_then(Value::as_str) {
                provider = p.to_owned();
            }
            if let Some(m) = e.data.get("model").and_then(Value::as_str) {
                model = m.to_owned();
            }
            continue;
        }
        let Some((p, m, usage)) = request_of(e, &provider, &model) else {
            continue;
        };
        if i < inherited {
            continue;
        }
        let quote = prices.quote(&p, &m, e.time);
        by_key
            .entry((e.time - e.time % HOUR_MS, p, m))
            .or_default()
            .add(&usage, quote);
    }
    by_key
        .into_iter()
        .map(|((hour, provider, model), usage)| Bucket {
            hour,
            provider,
            model,
            usage,
        })
        .collect()
}

/// Usage per `(provider, model)`.
type ByModel = BTreeMap<(String, String), Tally>;

/// `GET /api/usage` parameters.
#[derive(Debug, Clone, Copy)]
pub struct UsageQuery {
    pub since: u64,
    pub until: u64,
    /// The reader's UTC offset in minutes (days are local days).
    pub offset_minutes: i64,
    /// How many chats `topChats` lists.
    pub top: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub priced: bool,
    #[serde(flatten)]
    pub usage: Tally,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayUsage {
    /// Local `YYYY-MM-DD`.
    pub day: String,
    #[serde(flatten)]
    pub usage: Tally,
    pub models: Vec<ModelUsage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUsage {
    pub id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub updated_at: u64,
    /// Subagent sessions whose usage is included.
    pub subagents: u64,
    #[serde(flatten)]
    pub usage: Tally,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub currency: &'static str,
    pub since: u64,
    pub until: u64,
    pub totals: Tally,
    pub days: Vec<DayUsage>,
    pub models: Vec<ModelUsage>,
    pub top_chats: Vec<ChatUsage>,
    /// Sessions with requests in the range.
    pub sessions: u64,
}

/// Hourly usage per model of every session, kept in
/// `$DSHX_HOME/usage-index.json`: a listing marker per session decides whether
/// its log must be read again, so a report reads only logs that changed.
pub struct UsageIndex {
    path: PathBuf,
    file: Mutex<IndexFile>,
}

impl UsageIndex {
    pub fn load(home: &Path) -> Self {
        let path = home.join("usage-index.json");
        let file = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<IndexFile>(&b).ok())
            .filter(|f| f.version == INDEX_VERSION)
            .unwrap_or_default();
        UsageIndex {
            path,
            file: Mutex::new(file),
        }
    }

    /// Brings the sessions of `rows` up to date, reading the logs that changed.
    /// With `complete`, `rows` is the whole listing and the rest is dropped.
    pub fn refresh(
        &self,
        store: &SessionStore,
        rows: &[SessionSummary],
        prices: &PriceBook,
        complete: bool,
    ) -> anyhow::Result<()> {
        let mut file = self.file.lock().unwrap();
        let fingerprint = prices.fingerprint();
        let mut dirty = false;
        if file.prices != fingerprint || file.version != INDEX_VERSION {
            *file = IndexFile {
                version: INDEX_VERSION,
                prices: fingerprint,
                sessions: BTreeMap::new(),
            };
            dirty = true;
        }
        for row in rows {
            if file
                .sessions
                .get(&row.id)
                .is_some_and(|s| s.marker == marker(row))
            {
                continue;
            }
            // A log that cannot be read right now is tried again next time.
            let Ok(stored) = store.read(&row.id) else {
                continue;
            };
            // Branches name their source too, but only delegated children
            // count toward its subagents.
            let subagent =
                stored.header.extra.get("origin").and_then(Value::as_str) == Some("subagent");
            let parent = subagent
                .then(|| {
                    row.parent_session.clone().or_else(|| {
                        stored
                            .header
                            .extra
                            .get("parentSession")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                })
                .flatten();
            file.sessions.insert(
                row.id.clone(),
                IndexedSession {
                    marker: marker(row),
                    title: row.title.clone(),
                    cwd: row.cwd.clone(),
                    updated_at: row.updated_at,
                    parent,
                    buckets: buckets_of(&stored.events, prices),
                },
            );
            dirty = true;
        }
        if complete {
            let listed: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
            let before = file.sessions.len();
            file.sessions.retain(|id, _| listed.contains(id.as_str()));
            dirty |= file.sessions.len() != before;
        }
        if dirty {
            self.save(&file)?;
        }
        Ok(())
    }

    fn save(&self, file: &IndexFile) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// The session a (subagent) session ultimately belongs to.
    fn root<'a>(file: &'a IndexFile, mut id: &'a str) -> &'a str {
        for _ in 0..32 {
            match file.sessions.get(id).and_then(|s| s.parent.as_deref()) {
                Some(parent) if file.sessions.contains_key(parent) => id = parent,
                _ => break,
            }
        }
        id
    }

    /// All usage of the subagent sessions under `id`: `(sessions, usage)`.
    pub fn descendants(&self, id: &str) -> (u64, Tally) {
        let file = self.file.lock().unwrap();
        let mut count = 0;
        let mut usage = Tally::default();
        for (sid, s) in &file.sessions {
            if sid == id || !Self::descends(&file, sid, id) {
                continue;
            }
            count += 1;
            for b in &s.buckets {
                usage.merge(&b.usage);
            }
        }
        (count, usage)
    }

    fn descends<'a>(file: &'a IndexFile, mut id: &'a str, ancestor: &str) -> bool {
        for _ in 0..32 {
            match file.sessions.get(id).and_then(|s| s.parent.as_deref()) {
                Some(p) if p == ancestor => return true,
                Some(p) => id = p,
                None => return false,
            }
        }
        false
    }

    pub fn report(&self, q: &UsageQuery, prices: &PriceBook) -> UsageReport {
        let file = self.file.lock().unwrap();
        let offset = q.offset_minutes * 60_000;
        let local_day = |ms: u64| (ms as i64 + offset).div_euclid(DAY_MS);
        // Every day of the range gets a row; a long range only its last 400.
        let last = local_day(q.until.max(q.since));
        let first = local_day(q.since).max(last - 400);
        let mut totals = Tally::default();
        let mut days: BTreeMap<i64, (Tally, ByModel)> =
            (first..=last).map(|d| (d, Default::default())).collect();
        let mut models: BTreeMap<(String, String), Tally> = BTreeMap::new();
        let mut chats: HashMap<&str, (Tally, HashSet<&str>)> = HashMap::new();
        let mut sessions = 0;
        for (id, s) in &file.sessions {
            let mut used = false;
            for b in s
                .buckets
                .iter()
                .filter(|b| b.hour + HOUR_MS > q.since && b.hour <= q.until)
            {
                used = true;
                let key = (b.provider.clone(), b.model.clone());
                totals.merge(&b.usage);
                models.entry(key.clone()).or_default().merge(&b.usage);
                let day = days.entry(local_day(b.hour.max(q.since))).or_default();
                day.0.merge(&b.usage);
                day.1.entry(key).or_default().merge(&b.usage);
                let root = Self::root(&file, id);
                let chat = chats.entry(root).or_default();
                chat.0.merge(&b.usage);
                if root != id {
                    chat.1.insert(id.as_str());
                }
            }
            sessions += u64::from(used);
        }
        let model_rows = |m: BTreeMap<(String, String), Tally>| -> Vec<ModelUsage> {
            let mut rows: Vec<ModelUsage> = m
                .into_iter()
                .map(|((provider, model), usage)| ModelUsage {
                    priced: prices.lookup(&provider, &model).is_some(),
                    provider,
                    model,
                    usage,
                })
                .collect();
            rows.sort_by(|a, b| {
                b.usage
                    .cost_usd
                    .total_cmp(&a.usage.cost_usd)
                    .then(b.usage.tokens().cmp(&a.usage.tokens()))
            });
            rows
        };
        let mut top: Vec<ChatUsage> = chats
            .into_iter()
            .map(|(id, (usage, children))| {
                let s = file.sessions.get(id);
                ChatUsage {
                    id: id.to_owned(),
                    title: s.and_then(|s| s.title.clone()),
                    cwd: s.map(|s| s.cwd.clone()).unwrap_or_default(),
                    updated_at: s.map_or(0, |s| s.updated_at),
                    subagents: children.len() as u64,
                    usage,
                }
            })
            .collect();
        top.sort_by(|a, b| {
            b.usage
                .cost_usd
                .total_cmp(&a.usage.cost_usd)
                .then(b.usage.tokens().cmp(&a.usage.tokens()))
                .then(a.id.cmp(&b.id))
        });
        top.truncate(q.top);
        UsageReport {
            currency: CURRENCY,
            since: q.since,
            until: q.until,
            totals,
            days: days
                .into_iter()
                .map(|(d, (usage, m))| DayUsage {
                    day: day_label(d),
                    usage,
                    models: model_rows(m),
                })
                .collect(),
            models: model_rows(models),
            top_chats: top,
            sessions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshx_core::{NewEvent, SessionLog as _};
    use serde_json::json;

    /// Milliseconds of a UTC date and time.
    fn at(y: i64, m: u32, d: u32, h: u32, min: u32) -> u64 {
        // Inverse of `civil`, by search: fine for tests.
        let mut days = (y - 1970) * 365 + (y - 1969) / 4 - 2;
        while civil(days) < (y, m, d) {
            days += 1;
        }
        while civil(days) > (y, m, d) {
            days -= 1;
        }
        (days * DAY_MS) as u64 + u64::from(h) * HOUR_MS + u64::from(min) * 60_000
    }

    fn usage(input: u64, read: u64, write: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: None,
            cache_read_tokens: Some(read),
            cache_write_tokens: Some(write),
            reasoning_tokens: None,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(day_label(20_748), "2026-10-22");
        assert_eq!(
            civil((at(2028, 2, 29, 0, 0) as i64) / DAY_MS),
            (2028, 2, 29)
        );
    }

    #[test]
    fn peak_windows_on_weekdays() {
        // Monday 19.10.2026.
        assert_eq!(
            deepseek_peak(at(2026, 10, 19, 0, 59) + 59_999),
            (false, false)
        );
        assert_eq!(deepseek_peak(at(2026, 10, 19, 1, 0)), (true, false));
        assert_eq!(
            deepseek_peak(at(2026, 10, 19, 3, 59) + 59_999),
            (true, false)
        );
        assert_eq!(deepseek_peak(at(2026, 10, 19, 4, 0)), (false, false));
        assert_eq!(deepseek_peak(at(2026, 10, 19, 5, 59)), (false, false));
        assert_eq!(deepseek_peak(at(2026, 10, 19, 6, 0)), (true, false));
        assert_eq!(deepseek_peak(at(2026, 10, 19, 9, 59)), (true, false));
        assert_eq!(deepseek_peak(at(2026, 10, 19, 10, 0)), (false, false));
        // Friday on, weekend off.
        assert_eq!(deepseek_peak(at(2026, 10, 23, 2, 0)), (true, false));
        assert_eq!(deepseek_peak(at(2026, 10, 24, 2, 0)), (false, false));
        assert_eq!(deepseek_peak(at(2026, 10, 25, 7, 0)), (false, false));
    }

    #[test]
    fn chinese_holidays_are_off_peak() {
        // National Day (Thursday 1.10.) and the day after it.
        assert_eq!(deepseek_peak(at(2026, 10, 1, 2, 0)), (false, false));
        assert_eq!(deepseek_peak(at(2026, 10, 7, 8, 0)), (false, false));
        assert_eq!(deepseek_peak(at(2026, 10, 8, 2, 0)), (true, false));
        // A Saturday make-up workday stays off-peak (peak is Monday–Friday).
        assert_eq!(deepseek_peak(at(2026, 10, 10, 2, 0)), (false, false));
        // Spring Festival week.
        assert_eq!(deepseek_peak(at(2026, 2, 18, 7, 0)), (false, false));
    }

    #[test]
    fn unknown_years_assume_peak_and_say_so() {
        // Wednesday 3.3.2027: no table yet.
        assert_eq!(deepseek_peak(at(2027, 3, 3, 2, 0)), (true, true));
        // Off-peak by the clock alone needs no table.
        assert_eq!(deepseek_peak(at(2027, 3, 3, 12, 0)), (false, false));
        assert_eq!(deepseek_peak(at(2027, 3, 6, 2, 0)), (false, false));
    }

    #[test]
    fn request_cost_follows_the_clock() {
        let book = PriceBook::deepseek();
        let u = usage(1_000_000, 2_000_000, 500_000, 100_000);
        let mut peak = Tally::default();
        peak.add(
            &u,
            book.quote(
                "deepseek-official",
                "deepseek-flash",
                at(2026, 10, 19, 2, 0),
            ),
        );
        // 0.30 + 2 × 0.006 + 0.5 × 0.30 + 0.1 × 1.20
        assert!(close(peak.cost_usd, 0.582), "{}", peak.cost_usd);
        assert!(close(peak.cost.cache_read, 0.012));
        assert!(close(peak.saved_usd, 2.0 * (0.30 - 0.006)));
        assert_eq!(peak.off_peak_requests, 0);

        let mut off = Tally::default();
        off.add(
            &u,
            book.quote(
                "deepseek-official",
                "deepseek-flash",
                at(2026, 10, 19, 12, 0),
            ),
        );
        assert!(close(off.cost_usd, 0.291), "{}", off.cost_usd);
        assert_eq!(off.off_peak_requests, 1);

        let mut pro = Tally::default();
        pro.add(
            &usage(1_000_000, 1_000_000, 0, 1_000_000),
            book.quote(
                "deepseek-official",
                "deepseek-v4-pro",
                at(2026, 10, 19, 2, 0),
            ),
        );
        assert!(close(pro.cost_usd, 1.32 + 0.044 + 3.96));

        let mut approx = Tally::default();
        approx.add(
            &u,
            book.quote("deepseek-official", "deepseek-flash", at(2027, 3, 3, 2, 0)),
        );
        assert!(approx.approximate);
        assert!(close(approx.cost_usd, 0.582));
    }

    #[test]
    fn unknown_models_count_tokens_only() {
        let book = PriceBook::deepseek();
        assert!(book.quote("anthropic", "claude-sonnet-5-5", 0).is_none());
        assert!(
            book.quote("openrouter", "deepseek-flash", 0).is_none(),
            "built-in prices are DeepSeek's own"
        );
        let mut t = Tally::default();
        t.add(&usage(10, 20, 0, 5), None);
        assert_eq!((t.requests, t.unpriced_requests, t.tokens()), (1, 1, 35));
        assert_eq!(t.cost_usd, 0.0);
    }

    #[test]
    fn configured_providers_bring_their_prices() {
        let mut config = dshx_core::config::Config::default();
        config
            .providers
            .insert("anthropic".into(), Default::default());
        let mut custom = dshx_core::config::ProviderConfig {
            api: Some("openai-completions".into()),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            ..Default::default()
        };
        custom.models.push(dshx_core::config::ProviderModelConfig {
            id: "local-model".into(),
            cost: Some(dshx_core::config::ModelCost {
                input: 1.0,
                output: 2.0,
                cache_read: Some(0.5),
                cache_write: None,
            }),
            ..Default::default()
        });
        config.providers.insert("lab".into(), custom);
        let book = price_book(&config);
        let when = at(2026, 10, 19, 12, 0);
        assert!(book.quote("anthropic", "claude-sonnet-5-5", when).is_some());
        let lab = book.quote("lab", "local-model", when).unwrap();
        assert_eq!((lab.price.input, lab.price.output), (1.0, 2.0));
        // An unknown cache price counts like uncached input, never as free.
        assert_eq!((lab.price.cache_read, lab.price.cache_write), (0.5, 1.0));
        assert!(
            book.quote("deepseek-official", "deepseek-flash", when)
                .is_some()
        );
    }

    #[test]
    fn costs_keep_the_price_of_their_time() {
        let home = tempfile::tempdir().unwrap();
        let mut config = dshx_core::config::Config::default();
        let model = |input: f64| dshx_core::config::ProviderModelConfig {
            id: "m".into(),
            cost: Some(dshx_core::config::ModelCost {
                input,
                output: 2.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut lab = dshx_core::config::ProviderConfig {
            api: Some("openai-completions".into()),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            ..Default::default()
        };
        lab.models.push(model(1.0));
        config.providers.insert("lab".into(), lab.clone());
        let early = 1_000_000;
        let _ = price_book_at(home.path(), &config, early);
        // The price changes later: earlier requests keep theirs.
        lab.models[0] = model(4.0);
        config.providers.insert("lab".into(), lab);
        let book = price_book_at(home.path(), &config, early + 10_000);
        assert_eq!(book.quote("lab", "m", early + 5).unwrap().price.input, 1.0);
        assert_eq!(
            book.quote("lab", "m", early + 20_000).unwrap().price.input,
            4.0
        );
        // Removing the provider keeps the history (and the costs) priced.
        config.providers.clear();
        let gone = price_book_at(home.path(), &config, early + 30_000);
        assert_eq!(gone.quote("lab", "m", early + 5).unwrap().price.input, 1.0);
        assert!(gone.lookup("lab", "m").is_some());
    }

    #[test]
    fn overrides_are_flat_and_win() {
        let price = Price {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        };
        let book = PriceBook::deepseek()
            .with_override("anthropic", "claude-sonnet-5-5", price)
            .with_override("deepseek-official", "deepseek-flash", price);
        for hour in [2, 12] {
            let q = book
                .quote("anthropic", "claude-sonnet-5-5", at(2026, 10, 19, hour, 0))
                .unwrap();
            assert_eq!((q.price, q.off_peak), (price, false));
        }
        assert_eq!(
            book.quote(
                "deepseek-official",
                "deepseek-flash",
                at(2026, 10, 19, 12, 0)
            )
            .unwrap()
            .price,
            price
        );
        assert_ne!(book.fingerprint(), PriceBook::deepseek().fingerprint());
    }

    fn ev(seq: u64, time: u64, kind: &str, data: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time,
            data,
            extra: Default::default(),
        }
    }

    fn message(turn: u64, step: u64, content: Value, usage: Value, stream: Value) -> Value {
        json!({
            "turn": turn, "step": step,
            "message": {"role": "assistant", "id": format!("a{turn}{step}"), "content": content,
                "source": {"kind": "model", "provider": "deepseek-official", "model": "deepseek-flash"}},
            "usage": usage, "stream": stream,
        })
    }

    /// One turn, two steps (a tool call, then the answer), at peak time.
    fn log(base: u64) -> Vec<SessionEvent> {
        let t = |ms: u64| base + ms;
        vec![
            ev(0, t(0), "turn/start", json!({"turn": 1})),
            ev(1, t(10), "step/start", json!({"turn": 1, "step": 1})),
            ev(
                2,
                t(11),
                "user/message",
                json!({"role": "user", "id": "u1", "source": {"kind": "user"},
                "content": [{"type": "text", "text": "List the files"}]}),
            ),
            ev(
                3,
                t(12),
                "request/header",
                json!({"header": {"config": {"provider": "deepseek-official", "model": "deepseek-flash", "maxTokens": 256_000},
                "tools": [{"name": "bash", "description": "Run a command", "parameters": {"type": "object"}}]}}),
            ),
            ev(
                4,
                t(13),
                "request/context",
                json!({"provider": "deepseek-official", "model": "deepseek-flash", "contextWindow": 1_000_000}),
            ),
            ev(
                5,
                t(1_010),
                "assistant/message",
                message(
                    1,
                    1,
                    json!([{"type": "tool-call", "id": "c1", "name": "bash", "arguments": "{\"command\":\"ls\"}"}]),
                    json!({"inputTokens": 1000, "cacheReadTokens": 9000, "outputTokens": 50}),
                    json!([{"type": "tool-call-chunks", "time0": t(410), "index": 0, "dt": [], "id": "c1", "name": "bash", "args": [""]}]),
                ),
            ),
            ev(
                6,
                t(1_020),
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "{\"command\":\"ls\"}"}),
            ),
            ev(
                7,
                t(1_520),
                "tool/result",
                json!({"turn": 1, "step": 1, "message": {"role": "tool", "toolCallId": "c1",
                "source": {"kind": "tool", "callId": "c1"}, "content": [{"type": "text", "text": "a.txt"}]}}),
            ),
            ev(8, t(1_530), "step/end", json!({"turn": 1, "step": 1})),
            ev(9, t(1_600), "step/start", json!({"turn": 1, "step": 2})),
            ev(
                10,
                t(3_600),
                "assistant/message",
                message(
                    1,
                    2,
                    json!([{"type": "text", "text": "One file: a.txt"}]),
                    json!({"inputTokens": 200, "cacheReadTokens": 10_000, "outputTokens": 100}),
                    json!([{"type": "text-chunks", "time0": t(2_000), "index": 0, "dt": [100], "texts": ["", "One file"]}]),
                ),
            ),
            ev(11, t(3_610), "step/end", json!({"turn": 1, "step": 2})),
            ev(
                12,
                t(3_620),
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ]
    }

    #[test]
    fn session_stats_fold_costs_timings_and_context() {
        let base = at(2026, 10, 19, 2, 0);
        let rule = CompactionRule {
            auto: true,
            threshold_ratio: 0.8,
            headroom_tokens: 65_536,
            default_max_tokens: 256_000,
            turn_end: TurnEndCompaction::Threshold(200_000),
        };
        let s = session_stats(&log(base), &PriceBook::deepseek(), Some(rule), None, None);
        assert_eq!(s.turn_count, 1);
        assert_eq!(s.totals.requests, 2);
        assert_eq!(
            (
                s.totals.input_tokens,
                s.totals.cache_read_tokens,
                s.totals.output_tokens
            ),
            (1200, 19_000, 150)
        );
        let expected = (1200.0 * 0.30 + 19_000.0 * 0.006 + 150.0 * 1.20) / 1e6;
        assert!(close(s.totals.cost_usd, expected));
        let turn = &s.turns[0];
        assert!(close(turn.usage.cost_usd, expected));
        assert_eq!(turn.routes, ["deepseek-official/deepseek-flash"]);
        assert_eq!(
            (turn.start, turn.end, turn.status.as_deref()),
            (Some(base), Some(base + 3_620), Some("completed"))
        );
        // Model time: 1000 + 2000 ms; first tokens after 400 and 500 ms (the
        // empty fragment does not count); tool 500 ms.
        assert_eq!(s.timings.llm_ms, 3_000);
        assert_eq!((s.timings.ttft_ms, s.timings.ttft_steps), (400 + 500, 2));
        assert_eq!(
            (s.timings.decode_ms, s.timings.decode_tokens),
            (600 + 1_500, 150)
        );
        assert_eq!(s.timings.tool_ms, 500);
        assert_eq!(s.timings.steps, 2);
        assert_eq!(turn.timings.tool_ms, 500);
        let last = s.last_request.as_ref().unwrap();
        assert_eq!((last.seq, last.usage.requests), (10, 1));
        assert_eq!(s.context.used_tokens, 10_300);
        assert!(!s.context.estimated);
        assert!(close(s.context.percent.unwrap(), 1.03));
        assert!(s.context.breakdown.tools_tokens > 0 && s.context.breakdown.message_tokens > 0);
        // min(0.8 × 1M, 1M − 256k − 64k)
        let c = s.context.compaction.as_ref().unwrap();
        assert_eq!(c.threshold_tokens, 678_464);
        assert!(close(c.threshold_percent, 67.8464));
        assert_eq!((c.mode, c.turn_end_tokens), ("threshold", Some(200_000)));
        let off = CompactionRule {
            auto: false,
            ..rule
        };
        let s_off = session_stats(&log(base), &PriceBook::deepseek(), Some(off), None, None);
        let c = s_off.context.compaction.as_ref().unwrap();
        assert_eq!((c.mode, c.turn_end_tokens), ("off", None));
        assert_eq!(s.model.as_deref(), Some("deepseek-flash"));

        let later = session_stats(&log(base), &PriceBook::deepseek(), None, None, Some(2));
        assert!(later.turns.is_empty());
        assert_eq!(later.turn_count, 1);
    }

    #[test]
    fn inherited_requests_count_only_where_they_were_made() {
        let mut events = log(at(2026, 10, 19, 12, 0));
        let requests = |events: &[SessionEvent]| {
            buckets_of(events, &PriceBook::deepseek())
                .iter()
                .map(|b| b.usage.requests)
                .sum::<u64>()
        };
        assert_eq!(requests(&events), 2);
        let last = events.last().unwrap().clone();
        events.push(SessionEvent {
            kind: "session/end-seed".into(),
            seq: last.seq + 1,
            time: last.time,
            data: json!({ "inherited": true }),
            extra: Default::default(),
        });
        assert_eq!(
            requests(&events),
            0,
            "a branch inherits the requests, not their cost"
        );
    }

    #[test]
    fn compaction_estimates_the_context_until_the_next_request() {
        let mut events = log(at(2026, 10, 19, 12, 0));
        events.push(ev(
            13,
            events[12].time + 10,
            "compaction/summary",
            json!({
                "provider": "deepseek-official", "model": "deepseek-flash",
                "usage": {"inputTokens": 500, "outputTokens": 80},
            }),
        ));
        events.push(ev(14, events[12].time + 20, "compaction/end", json!({})));
        let s = session_stats(&events, &PriceBook::deepseek(), None, Some(1_000_000), None);
        assert_eq!(s.totals.requests, 3, "the summary request is billed");
        assert!(s.context.estimated);
        assert_eq!(s.totals.off_peak_requests, 3);
    }

    /// Surface events need their `surfaceOp` to be written.
    fn new_event(kind: &str, data: Value) -> NewEvent {
        let mut e = NewEvent::new(kind, data);
        if matches!(
            kind,
            "user/message" | "assistant/message" | "tool/result" | "system/message"
        ) {
            e.extra.insert("surfaceOp".into(), json!("append"));
        }
        e
    }

    fn write_session(
        store: &SessionStore,
        cwd: &Path,
        events: &[SessionEvent],
        parent: Option<&str>,
    ) -> String {
        let mut header = SessionStore::new_header(&cwd.display().to_string());
        if let Some(p) = parent {
            header.extra.insert("parentSession".into(), json!(p));
            header.extra.insert("origin".into(), json!("subagent"));
        }
        let log = store.create_with_header(header).unwrap();
        log.append_now(
            events
                .iter()
                .map(|e| new_event(&e.kind, e.data.clone()))
                .collect(),
        )
        .unwrap();
        log.flush_now().unwrap();
        log.id().to_owned()
    }

    #[test]
    fn index_reports_days_models_and_chats_and_rereads_only_changes() {
        let home = tempfile::tempdir().unwrap();
        let store = SessionStore::new(home.path().join("sessions"));
        let cwd = home.path();
        // Log times are the write times; both sessions land "now".
        let parent = write_session(&store, cwd, &log(0), None);
        let child = write_session(&store, cwd, &log(0)[..8], Some(&parent));
        let prices = PriceBook::deepseek();
        let index = UsageIndex::load(home.path());
        let rows = store.list().unwrap();
        index.refresh(&store, &rows, &prices, true).unwrap();

        let now = dshx_core::now_ms();
        let q = UsageQuery {
            since: now - 2 * DAY_MS as u64,
            until: now + 1000,
            offset_minutes: 120,
            top: 5,
        };
        let r = index.report(&q, &prices);
        assert_eq!(r.sessions, 2);
        assert_eq!(r.totals.requests, 3);
        assert_eq!(r.days.len(), 3, "gap-filled local days");
        assert_eq!(r.days.iter().map(|d| d.usage.requests).sum::<u64>(), 3);
        assert_eq!(r.models.len(), 1);
        assert!(r.models[0].priced);
        assert_eq!(r.top_chats.len(), 1, "the subagent counts for its parent");
        assert_eq!(
            (r.top_chats[0].id.as_str(), r.top_chats[0].subagents),
            (parent.as_str(), 1)
        );
        let (n, usage) = index.descendants(&parent);
        assert_eq!((n, usage.requests), (1, 1));

        // The persisted index answers without the logs; a changed log is re-read.
        let reloaded = UsageIndex::load(home.path());
        assert_eq!(reloaded.report(&q, &prices).totals.requests, 3);
        let log = store.open(&child).unwrap();
        log.append_now(vec![new_event("assistant/message", log_message())])
            .unwrap();
        log.flush_now().unwrap();
        drop(log);
        reloaded
            .refresh(&store, &store.list().unwrap(), &prices, true)
            .unwrap();
        assert_eq!(reloaded.report(&q, &prices).totals.requests, 4);
        assert_eq!(reloaded.descendants(&parent).1.requests, 2);
    }

    fn log_message() -> Value {
        message(
            2,
            1,
            json!([{"type": "text", "text": "hi"}]),
            json!({"inputTokens": 1, "outputTokens": 1}),
            json!([]),
        )
    }
}
