//! Background jobs (dsh `jobs-local` + `tool-jobs`): a registry of running
//! work with a per-job output ring, bounded waits, kills, and the
//! `job_list` / `job_output` / `job_kill` tools.
//!
//! Every bash call is a job while it runs (as in dsh): the registry reports
//! `job/registered` and `job/settled` through [`JobRegistry::subscribe`], and the
//! agent appends them to the session log and delivers completion notices.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{NewEvent, Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::args::{self, fail};
use crate::schemas::upstream_schema;
use crate::state::ToolState;
use crate::util::{js_number, now_ms, random_hex, utf8_suffix};

const DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER: usize = 10;
const DEFAULT_RETAIN_BYTES: usize = 256 * 1024;
const DEFAULT_SETTLED_RETAIN_BYTES: usize = 16 * 1024;
/// `job_output` wait without `timeout_ms` (dsh `waitTimeoutMs`).
pub const WAIT_DEFAULT_MS: f64 = 30_000.0;
/// Hard cap on any `job_output` wait (dsh `maxWaitTimeoutMs`).
pub const WAIT_MAX_MS: f64 = 600_000.0;
/// dsh `CONTEXT_SUMMARY_MAX_CHARS`.
const CONTEXT_SUMMARY_MAX_CHARS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Stopping,
    Completed,
    Killed,
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Stopping => "stopping",
            JobStatus::Completed => "completed",
            JobStatus::Killed => "killed",
            JobStatus::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Completed | JobStatus::Killed | JobStatus::Failed
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobChannel {
    Stdout,
    Stderr,
    /// Producer narration for observers; never shown to the model.
    Log,
}

/// Why a job settled (dsh `JobSettleCause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleCause {
    Producer,
    Kill,
    Teardown,
}

impl SettleCause {
    pub fn as_str(self) -> &'static str {
        match self {
            SettleCause::Producer => "producer",
            SettleCause::Kill => "kill",
            SettleCause::Teardown => "teardown",
        }
    }
}

/// Terminal outcome a producer reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    /// `Completed`, `Killed` or `Failed`.
    pub status: JobStatus,
    pub detail: Option<String>,
    /// Final result text for final-output jobs (none for bash).
    pub result: Option<String>,
}

/// The tool call that started a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobToolCall {
    pub call_id: String,
    pub root_call_id: String,
}

/// Snapshot of one job (dsh `JobView`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub owner: Option<String>,
    pub tool_call: Option<JobToolCall>,
    pub status: JobStatus,
    pub progress: Option<String>,
    pub detail: Option<String>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub output_total: u64,
    pub output_earliest: u64,
    pub spill_paths: Vec<String>,
}

impl JobView {
    /// Live progress while running, the terminal reason once settled.
    pub fn public_detail(&self) -> Option<&str> {
        self.progress.as_deref().or(self.detail.as_deref())
    }

    /// `[status: completed, exit code: 0]`
    pub fn status_line(&self) -> String {
        match self.public_detail() {
            Some(detail) => format!("[status: {}, {detail}]", self.status.as_str()),
            None => format!("[status: {}]", self.status.as_str()),
        }
    }

    /// dsh `PublicJobSnapshot` as JSON.
    pub fn public_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".into(), json!(self.id));
        map.insert("kind".into(), json!(self.kind));
        map.insert("label".into(), json!(self.label));
        map.insert("status".into(), json!(self.status.as_str()));
        if let Some(detail) = self.public_detail() {
            map.insert("detail".into(), json!(detail));
        }
        map.insert("startedAt".into(), json!(self.started_at));
        if let Some(finished) = self.finished_at {
            map.insert("finishedAt".into(), json!(finished));
        }
        Value::Object(map)
    }

    fn registered_data(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("jobId".into(), json!(self.id));
        map.insert("owner".into(), json!(self.owner));
        map.insert("kind".into(), json!(self.kind));
        map.insert("label".into(), json!(self.label));
        map.insert("startedAt".into(), json!(self.started_at));
        if let Some(call) = &self.tool_call {
            map.insert(
                "toolCall".into(),
                json!({"callId": call.call_id, "rootCallId": call.root_call_id}),
            );
        }
        map
    }
}

/// Lifecycle notifications for the agent (see [`JobRegistry::subscribe`]).
#[derive(Debug, Clone)]
pub enum JobEvent {
    Registered {
        job: JobView,
    },
    Settled {
        job: JobView,
        cause: SettleCause,
        /// A live `job_output`/bash wait received this settlement.
        awaited: bool,
        /// The model itself requested the kill with `job_kill`.
        killed_by_model: bool,
        producer_reported: bool,
    },
    Removed {
        job: JobView,
    },
}

impl JobEvent {
    /// The session event dsh logs for this transition (`job/registered`,
    /// `job/settled`), for owned jobs only.
    pub fn session_event(&self) -> Option<NewEvent> {
        match self {
            JobEvent::Registered { job } if job.owner.is_some() => Some(NewEvent::new(
                "job/registered",
                Value::Object(job.registered_data()),
            )),
            JobEvent::Settled {
                job,
                cause,
                producer_reported,
                ..
            } if job.owner.is_some() => {
                let mut data = job.registered_data();
                data.insert(
                    "finishedAt".into(),
                    json!(job.finished_at.unwrap_or_else(now_ms)),
                );
                data.insert("status".into(), json!(job.status.as_str()));
                data.insert("cause".into(), json!(cause.as_str()));
                data.insert("producerReported".into(), json!(producer_reported));
                if let Some(detail) = &job.detail {
                    data.insert("detail".into(), json!(detail));
                }
                Some(NewEvent::new("job/settled", Value::Object(data)))
            }
            _ => None,
        }
    }

    /// The completion notice the owner should receive (dsh `tool-jobs`
    /// delivery): `None` when the model already has the outcome (a wait got it,
    /// it killed the job itself) or the session is being torn down. Deliver it
    /// as a user message with source `{kind: "tool-jobs", form: "notice",
    /// summary}` — injected into a busy agent's next step, or opening a turn on
    /// an idle one.
    pub fn completion_notice(&self) -> Option<CompletionNotice> {
        let JobEvent::Settled {
            job,
            cause,
            awaited,
            killed_by_model,
            ..
        } = self
        else {
            return None;
        };
        if *awaited || *killed_by_model || *cause == SettleCause::Teardown || job.owner.is_none() {
            return None;
        }
        let status = job.status_line();
        let summary = format!("{} {} {status}", job.kind, job.label);
        let summary = if summary.chars().count() <= CONTEXT_SUMMARY_MAX_CHARS {
            summary
        } else {
            let cut: String = summary
                .chars()
                .take(CONTEXT_SUMMARY_MAX_CHARS - 1)
                .collect();
            format!("{cut}…")
        };
        Some(CompletionNotice {
            owner: job.owner.clone().unwrap_or_default(),
            text: format!(
                "background job {} ({}: {}) finished {status}. Read its output with job_output.",
                job.id, job.kind, job.label
            ),
            summary,
        })
    }
}

/// A completion message for the job's owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionNotice {
    pub owner: String,
    pub text: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct JobChunk {
    pub at: u64,
    pub text: String,
    pub channel: Option<JobChannel>,
    pub gap_before: bool,
}

/// One consuming model read.
#[derive(Debug, Clone)]
pub struct JobRead {
    pub chunks: Vec<JobChunk>,
    pub lossy: bool,
    pub result: Option<String>,
    pub job: JobView,
}

/// Bounded output ring in whole-stream byte coordinates (dsh `OutputRing`).
#[derive(Debug, Default)]
struct OutputRing {
    chunks: VecDeque<JobChunk>,
    retained: usize,
    total: u64,
    earliest: u64,
}

impl OutputRing {
    fn append(
        &mut self,
        text: &str,
        channel: Option<JobChannel>,
        gap_before: bool,
        cap: usize,
    ) -> bool {
        if text.is_empty() {
            return false;
        }
        self.chunks.push_back(JobChunk {
            at: self.total,
            text: text.to_owned(),
            channel,
            gap_before,
        });
        self.total += text.len() as u64;
        self.retained += text.len();
        self.trim(cap);
        true
    }

    fn trim(&mut self, cap: usize) {
        while self.retained > cap && self.chunks.len() > 1 {
            if let Some(dropped) = self.chunks.pop_front() {
                self.retained -= dropped.text.len();
            }
        }
        if self.chunks.len() == 1
            && let Some(single) = self.chunks.front_mut()
            && single.text.len() > cap
        {
            let tail = utf8_suffix(&single.text, cap).to_owned();
            single.at += (single.text.len() - tail.len()) as u64;
            single.text = tail;
            single.gap_before = true;
            self.retained = single.text.len();
        }
        self.earliest = self.chunks.front().map_or(self.total, |c| c.at);
    }

    fn read_from(&self, from: u64) -> (Vec<JobChunk>, bool) {
        let chunks = self
            .chunks
            .iter()
            .filter(|c| c.at + c.text.len() as u64 > from)
            .cloned()
            .collect();
        (chunks, from < self.earliest)
    }
}

type CancelFn = Arc<dyn Fn(Option<&str>) + Send + Sync>;
type SpillFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

struct Job {
    id: String,
    kind: String,
    label: String,
    owner: Option<String>,
    tool_call: Option<JobToolCall>,
    status: JobStatus,
    ring: OutputRing,
    model_cursor: u64,
    result: Option<String>,
    result_delivered: bool,
    progress: Option<String>,
    detail: Option<String>,
    started_at: u64,
    finished_at: Option<u64>,
    kill_reason: Option<String>,
    settle_cause: Option<SettleCause>,
    cancel: Option<CancelFn>,
    pending_cancel: Option<Option<String>>,
    spill: Option<SpillFn>,
    settled: watch::Sender<bool>,
    waiters: usize,
    killed_by_model: bool,
}

impl Job {
    fn view(&self) -> JobView {
        let mut spill_paths = self.spill.as_ref().map(|f| f()).unwrap_or_default();
        spill_paths.dedup();
        JobView {
            id: self.id.clone(),
            kind: self.kind.clone(),
            label: self.label.clone(),
            owner: self.owner.clone(),
            tool_call: self.tool_call.clone(),
            status: self.status,
            progress: self.progress.clone(),
            detail: self.detail.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            output_total: self.ring.total,
            output_earliest: self.ring.earliest,
            spill_paths,
        }
    }
}

/// Registry limits (dsh `jobs-local` config).
#[derive(Debug, Clone)]
pub struct JobConfig {
    pub max_concurrent_jobs_per_owner: usize,
    pub retain_bytes: usize,
    pub settled_retain_bytes: usize,
}

impl Default for JobConfig {
    fn default() -> Self {
        JobConfig {
            max_concurrent_jobs_per_owner: DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER,
            retain_bytes: DEFAULT_RETAIN_BYTES,
            settled_retain_bytes: DEFAULT_SETTLED_RETAIN_BYTES,
        }
    }
}

struct Shared {
    jobs: Mutex<Vec<Job>>,
    config: JobConfig,
    sinks: Mutex<Vec<mpsc::UnboundedSender<JobEvent>>>,
}

/// What a producer registers.
pub struct JobSpec {
    pub kind: String,
    pub label: String,
    /// Owning session id; owned jobs are invisible to other sessions.
    pub owner: Option<String>,
    pub tool_call: Option<JobToolCall>,
}

/// Outcome of a kill request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillResult {
    Requested,
    AlreadyFinished,
}

/// The job registry. Cheap to clone; clones share all jobs.
#[derive(Clone)]
pub struct JobRegistry {
    shared: Arc<Shared>,
}

impl Default for JobRegistry {
    fn default() -> Self {
        JobRegistry::new(JobConfig::default())
    }
}

/// The producer side of one job.
#[derive(Clone)]
pub struct JobSink {
    registry: JobRegistry,
    id: String,
}

impl JobSink {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Appends producer output (dropped once the job settled).
    pub fn append(&self, channel: JobChannel, text: &str) {
        self.registry.append(&self.id, channel, text);
    }

    /// Installs the stop hook a kill calls; a kill that arrived earlier runs now.
    pub fn bind_cancel(&self, cancel: impl Fn(Option<&str>) + Send + Sync + 'static) {
        let cancel: CancelFn = Arc::new(cancel);
        let pending = {
            let mut jobs = self.registry.shared.jobs.lock().unwrap();
            let Some(job) = jobs.iter_mut().find(|j| j.id == self.id) else {
                return;
            };
            job.cancel = Some(cancel.clone());
            job.pending_cancel.take()
        };
        if let Some(reason) = pending {
            cancel(reason.as_deref());
        }
    }

    /// Installs the source of the job's complete-stream spill files.
    pub fn bind_spill_paths(&self, spill: impl Fn() -> Vec<String> + Send + Sync + 'static) {
        let mut jobs = self.registry.shared.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == self.id) {
            job.spill = Some(Arc::new(spill));
        }
    }

    pub fn update_progress(&self, line: &str) {
        let mut jobs = self.registry.shared.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == self.id)
            && !job.status.is_terminal()
        {
            job.progress = Some(line.to_owned());
        }
    }

    /// Reports the terminal outcome.
    pub fn settle(&self, outcome: JobOutcome) {
        self.registry.settle(&self.id, outcome, None, true);
    }
}

impl JobRegistry {
    pub fn new(config: JobConfig) -> Self {
        JobRegistry {
            shared: Arc::new(Shared {
                jobs: Mutex::new(Vec::new()),
                config,
                sinks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// A new feed of lifecycle events (every subscriber sees every event).
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<JobEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.shared.sinks.lock().unwrap().push(tx);
        rx
    }

    fn emit(&self, event: JobEvent) {
        self.shared
            .sinks
            .lock()
            .unwrap()
            .retain(|sink| sink.send(event.clone()).is_ok());
    }

    /// Admits and registers a job. Errors are the model-facing refusal text.
    pub fn start(&self, spec: JobSpec) -> Result<JobSink, String> {
        if spec.kind.is_empty() {
            return Err("invalid job kind: expected a non-empty string".into());
        }
        if spec.label.is_empty() {
            return Err("invalid job label: expected a non-empty string".into());
        }
        let view = {
            let mut jobs = self.shared.jobs.lock().unwrap();
            let active = jobs
                .iter()
                .filter(|j| {
                    j.owner == spec.owner
                        && matches!(j.status, JobStatus::Running | JobStatus::Stopping)
                })
                .count();
            let limit = self.shared.config.max_concurrent_jobs_per_owner;
            if active >= limit {
                return Err(format!(
                    "background job limit reached for this owner (limit: {limit}); use job_kill to stop an unneeded job, wait for it to finish, then retry"
                ));
            }
            let mut id = format!("{}-{}", spec.kind, random_hex(16));
            while jobs.iter().any(|j| j.id == id) {
                id = format!("{}-{}", spec.kind, random_hex(16));
            }
            let (settled, _) = watch::channel(false);
            let job = Job {
                id,
                kind: spec.kind,
                label: spec.label,
                owner: spec.owner,
                tool_call: spec.tool_call,
                status: JobStatus::Running,
                ring: OutputRing::default(),
                model_cursor: 0,
                result: None,
                result_delivered: false,
                progress: None,
                detail: None,
                started_at: now_ms(),
                finished_at: None,
                kill_reason: None,
                settle_cause: None,
                cancel: None,
                pending_cancel: None,
                spill: None,
                settled,
                waiters: 0,
                killed_by_model: false,
            };
            let view = job.view();
            jobs.push(job);
            view
        };
        let sink = JobSink {
            registry: self.clone(),
            id: view.id.clone(),
        };
        self.emit(JobEvent::Registered { job: view });
        Ok(sink)
    }

    fn append(&self, id: &str, channel: JobChannel, text: &str) {
        let cap = self.shared.config.retain_bytes;
        let mut jobs = self.shared.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id)
            && !job.status.is_terminal()
        {
            job.ring.append(text, Some(channel), false, cap);
        }
    }

    fn settle(
        &self,
        id: &str,
        outcome: JobOutcome,
        cause: Option<SettleCause>,
        producer_reported: bool,
    ) {
        let event = {
            let mut jobs = self.shared.jobs.lock().unwrap();
            let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
                return;
            };
            if job.status.is_terminal() {
                return;
            }
            job.status = outcome.status;
            if outcome.status == JobStatus::Killed
                && let Some(reason) = &job.kill_reason
            {
                job.detail = Some(match &outcome.detail {
                    Some(detail) => format!("{detail}; {reason}"),
                    None => reason.clone(),
                });
            } else if outcome.detail.is_some() {
                job.detail = outcome.detail;
            }
            job.progress = None;
            job.result = outcome.result;
            job.finished_at = Some(now_ms());
            let keep = self
                .shared
                .config
                .settled_retain_bytes
                .max((job.ring.total - job.model_cursor) as usize);
            job.ring.trim(keep);
            // The producer is done: keep its spill paths, drop its hooks (and
            // with them the process handle they hold).
            let spill_paths = job.spill.as_ref().map(|f| f()).unwrap_or_default();
            job.spill = Some(Arc::new(move || spill_paths.clone()));
            job.cancel = None;
            let awaited = job.waiters > 0;
            job.settled.send_replace(true);
            JobEvent::Settled {
                job: job.view(),
                cause: cause.or(job.settle_cause).unwrap_or(SettleCause::Producer),
                awaited,
                killed_by_model: job.killed_by_model,
                producer_reported,
            }
        };
        self.emit(event);
    }

    fn with_job<T>(
        &self,
        id: &str,
        caller: Option<&str>,
        f: impl FnOnce(&mut Job) -> T,
    ) -> Result<T, String> {
        let mut jobs = self.shared.jobs.lock().unwrap();
        let job = jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or_else(|| format!("unknown job {id}"))?;
        if job.owner.is_some() && job.owner.as_deref() != caller {
            return Err(format!("job {id} belongs to another session"));
        }
        Ok(f(job))
    }

    /// Jobs visible to `caller` (its own and unowned ones), oldest first.
    pub fn list(&self, caller: Option<&str>) -> Vec<JobView> {
        let jobs = self.shared.jobs.lock().unwrap();
        jobs.iter()
            .filter(|j| j.owner.is_none() || j.owner.as_deref() == caller)
            .map(Job::view)
            .collect()
    }

    pub fn get(&self, id: &str, caller: Option<&str>) -> Result<JobView, String> {
        self.with_job(id, caller, |job| job.view())
    }

    /// Consuming read: output since the model's previous read.
    pub fn read(&self, id: &str, caller: Option<&str>) -> Result<JobRead, String> {
        let settled_cap = self.shared.config.settled_retain_bytes;
        self.with_job(id, caller, |job| {
            let (chunks, lossy) = job.ring.read_from(job.model_cursor);
            job.model_cursor = job.ring.total;
            let mut result = None;
            if job.status.is_terminal() && !job.result_delivered {
                result = job.result.clone();
                if result.is_some() {
                    job.result_delivered = true;
                }
            }
            if job.status.is_terminal() {
                job.ring.trim(settled_cap);
            }
            JobRead {
                chunks,
                lossy,
                result,
                job: job.view(),
            }
        })
    }

    /// Requests cancellation (dsh `kill`).
    pub fn kill(
        &self,
        id: &str,
        caller: Option<&str>,
        reason: Option<&str>,
    ) -> Result<KillResult, String> {
        self.kill_inner(id, caller, reason, false)
    }

    fn kill_inner(
        &self,
        id: &str,
        caller: Option<&str>,
        reason: Option<&str>,
        by_model: bool,
    ) -> Result<KillResult, String> {
        let cancel = self.with_job(id, caller, |job| {
            if job.status.is_terminal() {
                return None;
            }
            job.status = JobStatus::Stopping;
            if let Some(reason) = reason {
                job.kill_reason = Some(reason.to_owned());
            }
            job.settle_cause = Some(SettleCause::Kill);
            if by_model {
                job.killed_by_model = true;
            }
            match &job.cancel {
                Some(cancel) => Some(Some(cancel.clone())),
                None => {
                    job.pending_cancel = Some(reason.map(str::to_owned));
                    Some(None)
                }
            }
        })?;
        match cancel {
            None => Ok(KillResult::AlreadyFinished),
            Some(cancel) => {
                if let Some(cancel) = cancel {
                    cancel(reason);
                }
                Ok(KillResult::Requested)
            }
        }
    }

    /// Waits until the job settles or `timeout` passes; a timeout returns the
    /// still-running view. Fails with `wait aborted` when `cancel` fires.
    pub async fn wait(
        &self,
        id: &str,
        timeout_ms: f64,
        caller: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Result<JobView, String> {
        if !timeout_ms.is_finite() || timeout_ms <= 0.0 {
            return Err(format!(
                "invalid wait timeout: expected a positive number of milliseconds, got {}",
                js_number(timeout_ms)
            ));
        }
        let mut rx = self.with_job(id, caller, |job| {
            if job.status.is_terminal() {
                None
            } else {
                job.waiters += 1;
                Some(job.settled.subscribe())
            }
        })?;
        if let Some(rx) = rx.as_mut() {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                self.release_waiter(id);
                return Err("wait aborted".into());
            }
            let never = CancellationToken::new();
            let cancel = cancel.unwrap_or(&never);
            let aborted = tokio::select! {
                _ = rx.wait_for(|settled| *settled) => false,
                _ = tokio::time::sleep(Duration::from_secs_f64(timeout_ms / 1000.0)) => false,
                _ = cancel.cancelled() => true,
            };
            self.release_waiter(id);
            if aborted {
                return Err("wait aborted".into());
            }
        }
        self.get(id, caller)
    }

    fn release_waiter(&self, id: &str) {
        let mut jobs = self.shared.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.waiters = job.waiters.saturating_sub(1);
        }
    }

    /// Drops a settled job from the registry.
    pub fn remove(&self, id: &str, caller: Option<&str>) -> Result<(), String> {
        let view = {
            let mut jobs = self.shared.jobs.lock().unwrap();
            let index = jobs
                .iter()
                .position(|j| j.id == id)
                .ok_or_else(|| format!("unknown job {id}"))?;
            let job = &jobs[index];
            if job.owner.is_some() && job.owner.as_deref() != caller {
                return Err(format!("job {id} belongs to another session"));
            }
            if !job.status.is_terminal() {
                return Err(format!(
                    "job {id} is still {}; kill it and wait for settlement before removing it",
                    job.status.as_str()
                ));
            }
            jobs.remove(index).view()
        };
        self.emit(JobEvent::Removed { job: view });
        Ok(())
    }

    /// Stops every running job (session or server teardown); their
    /// settlements carry cause `teardown`, so no completion notice follows.
    pub fn shutdown(&self, owner: Option<&str>) {
        let cancels: Vec<(String, Option<CancelFn>)> = {
            let mut jobs = self.shared.jobs.lock().unwrap();
            jobs.iter_mut()
                .filter(|j| {
                    !j.status.is_terminal() && (owner.is_none() || j.owner.as_deref() == owner)
                })
                .map(|job| {
                    job.settle_cause = Some(SettleCause::Teardown);
                    job.status = JobStatus::Stopping;
                    if job.cancel.is_none() {
                        job.pending_cancel = Some(Some("owner disposed".into()));
                    }
                    (job.id.clone(), job.cancel.clone())
                })
                .collect()
        };
        for (_, cancel) in cancels {
            if let Some(cancel) = cancel {
                cancel(Some("owner disposed"));
            }
        }
    }
}

/// The model-facing delta of one read (dsh `renderModelDelta`): stdout chunks,
/// then one `[stderr]` section, then the dropped-output notice when lossy.
pub fn render_model_delta(chunks: &[JobChunk], lossy: bool, spill_paths: &[String]) -> String {
    let visible: Vec<&JobChunk> = chunks
        .iter()
        .filter(|c| c.channel != Some(JobChannel::Log))
        .collect();
    let body = split_streams(visible.iter().copied());
    if !lossy && !visible.iter().any(|c| c.gap_before) {
        return body;
    }
    let notice = dropped_notice(spill_paths);
    let separator = if !body.is_empty() && !body.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{body}{separator}{notice}")
}

/// dsh `ringDelta`: stdout chunks, then every stderr chunk under `[stderr]`.
pub(crate) fn split_streams<'a>(chunks: impl Iterator<Item = &'a JobChunk> + Clone) -> String {
    let out: String = chunks
        .clone()
        .filter(|c| c.channel != Some(JobChannel::Stderr))
        .map(|c| c.text.as_str())
        .collect();
    let err: String = chunks
        .filter(|c| c.channel == Some(JobChannel::Stderr))
        .map(|c| c.text.as_str())
        .collect();
    if err.is_empty() {
        return out;
    }
    let separator = if !out.is_empty() && !out.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{out}{separator}[stderr]\n{err}")
}

pub(crate) fn dropped_notice(spill_paths: &[String]) -> String {
    let paths = if spill_paths.is_empty() {
        "(unavailable)".to_owned()
    } else {
        spill_paths.join(", ")
    };
    format!("[some output was dropped from memory; full output: {paths}]")
}

fn validate_job_id(id: &str) -> Result<(), ToolError> {
    if id.is_empty() {
        return Err(fail(format!(
            "invalid job_id: expected a non-empty string, got {}",
            crate::util::json_string(id)
        )));
    }
    Ok(())
}

/// `job_output`
pub struct JobOutputTool;
/// `job_list`
pub struct JobListTool;
/// `job_kill`
pub struct JobKillTool;

#[derive(Deserialize)]
struct JobOutputArgs {
    job_id: String,
    wait: Option<bool>,
    timeout_ms: Option<f64>,
}

#[derive(Deserialize)]
struct JobKillArgs {
    job_id: String,
    reason: Option<String>,
}

#[async_trait]
impl Tool for JobOutputTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("job_output")
    }

    fn describe(&self, args: &Value) -> String {
        format!(
            "Read output from background job {}",
            args["job_id"].as_str().unwrap_or("")
        )
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: JobOutputArgs = args::parse("job_output", args)?;
        validate_job_id(&args.job_id)?;
        let state = ToolState::get(&ctx)?;
        let jobs = state.jobs();
        let caller = Some(ctx.session_id.as_str());
        if args.wait == Some(true) {
            let timeout = args.timeout_ms.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MAX_MS);
            jobs.wait(&args.job_id, timeout, caller, Some(&ctx.cancel))
                .await
                .map_err(fail)?;
        }
        let read = jobs.read(&args.job_id, caller).map_err(fail)?;
        let delta = render_model_delta(&read.chunks, read.lossy, &read.job.spill_paths);
        let text = match &read.result {
            Some(result) => {
                let separator = if !delta.is_empty() && !delta.ends_with('\n') {
                    "\n"
                } else {
                    ""
                };
                format!("{delta}{separator}{result}")
            }
            None => delta,
        };
        let body = if text.is_empty() {
            "(no new output)"
        } else {
            text.as_str()
        };
        let separator = if body.ends_with('\n') { "" } else { "\n" };
        let rendered = format!("{body}{separator}{}", read.job.status_line());
        Ok(ToolOutput {
            value: Some(json!({"text": text, "job": read.job.public_json()})),
            ..ToolOutput::text(rendered)
        })
    }
}

#[async_trait]
impl Tool for JobListTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("job_list")
    }

    fn describe(&self, _args: &Value) -> String {
        "List background jobs".into()
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let _: Value = args::parse("job_list", args)?;
        let state = ToolState::get(&ctx)?;
        let jobs = state.jobs().list(Some(&ctx.session_id));
        let text = if jobs.is_empty() {
            "(no background jobs)".to_owned()
        } else {
            jobs.iter()
                .map(|j| format!("{} [{}] {} — {}", j.id, j.kind, j.status.as_str(), j.label))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ToolOutput {
            value: Some(Value::Array(
                jobs.iter().map(JobView::public_json).collect(),
            )),
            ..ToolOutput::text(text)
        })
    }
}

#[async_trait]
impl Tool for JobKillTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("job_kill")
    }

    fn describe(&self, args: &Value) -> String {
        format!(
            "Kill background job {}",
            args["job_id"].as_str().unwrap_or("")
        )
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: JobKillArgs = args::parse("job_kill", args)?;
        validate_job_id(&args.job_id)?;
        let state = ToolState::get(&ctx)?;
        let jobs = state.jobs();
        let caller = Some(ctx.session_id.as_str());
        let result = jobs
            .kill_inner(&args.job_id, caller, args.reason.as_deref(), true)
            .map_err(fail)?;
        let job = jobs.get(&args.job_id, caller).map_err(fail)?;
        let (outcome, text) = match result {
            KillResult::AlreadyFinished => (
                "already-finished",
                format!("job {} had already finished {}", job.id, job.status_line()),
            ),
            KillResult::Requested => (
                "cancellation-requested",
                format!("requested cancellation of job {}", job.id),
            ),
        };
        Ok(ToolOutput {
            value: Some(json!({"outcome": outcome, "job": job.public_json()})),
            ..ToolOutput::text(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(owner: &str) -> JobSpec {
        JobSpec {
            kind: "bash".into(),
            label: "sleep 1".into(),
            owner: Some(owner.into()),
            tool_call: None,
        }
    }

    #[test]
    fn ring_trims_whole_chunks_then_the_tail() {
        let mut ring = OutputRing::default();
        ring.append("aaaa", None, false, 6);
        ring.append("bbbb", None, false, 6);
        assert_eq!(ring.chunks.len(), 1);
        assert_eq!(ring.earliest, 4);
        let (chunks, lossy) = ring.read_from(0);
        assert!(lossy);
        assert_eq!(chunks[0].text, "bbbb");
        ring.append("cccccccc", None, false, 6);
        assert_eq!(ring.chunks.len(), 1);
        assert_eq!(ring.chunks[0].text, "cccccc");
        assert!(ring.chunks[0].gap_before);
    }

    #[tokio::test]
    async fn lifecycle_events_and_kill_reason() {
        let registry = JobRegistry::default();
        let mut events = registry.subscribe();
        let sink = registry.start(spec("s1")).unwrap();
        let killed = Arc::new(Mutex::new(None));
        let k = killed.clone();
        sink.bind_cancel(move |reason| *k.lock().unwrap() = Some(reason.map(str::to_owned)));
        sink.append(JobChannel::Stdout, "hi\n");
        assert_eq!(
            registry
                .kill(sink.id(), Some("s1"), Some("stop it"))
                .unwrap(),
            KillResult::Requested
        );
        assert_eq!(*killed.lock().unwrap(), Some(Some("stop it".to_owned())));
        sink.settle(JobOutcome {
            status: JobStatus::Killed,
            detail: Some("signal: SIGTERM".into()),
            result: None,
        });
        let view = registry.get(sink.id(), Some("s1")).unwrap();
        assert_eq!(view.detail.as_deref(), Some("signal: SIGTERM; stop it"));
        assert_eq!(
            registry.kill(sink.id(), Some("s1"), None).unwrap(),
            KillResult::AlreadyFinished
        );
        assert!(matches!(
            events.recv().await,
            Some(JobEvent::Registered { .. })
        ));
        let settled = events.recv().await.unwrap();
        let event = settled.session_event().unwrap();
        assert_eq!(event.kind, "job/settled");
        assert_eq!(event.data["cause"], "kill");
        assert_eq!(event.data["detail"], "signal: SIGTERM; stop it");
        assert!(settled.completion_notice().is_some());
        assert_eq!(
            registry.get(sink.id(), Some("s2")).unwrap_err(),
            format!("job {} belongs to another session", sink.id())
        );
        assert_eq!(
            registry.get("bash-x", Some("s1")).unwrap_err(),
            "unknown job bash-x"
        );
    }

    #[tokio::test]
    async fn waits_are_bounded_and_mark_awaited() {
        let registry = JobRegistry::default();
        let mut events = registry.subscribe();
        let sink = registry.start(spec("s1")).unwrap();
        let view = registry
            .wait(sink.id(), 20.0, Some("s1"), None)
            .await
            .unwrap();
        assert_eq!(view.status, JobStatus::Running);
        let waiter = {
            let registry = registry.clone();
            let id = sink.id().to_owned();
            tokio::spawn(async move { registry.wait(&id, 5_000.0, Some("s1"), None).await })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        sink.settle(JobOutcome {
            status: JobStatus::Completed,
            detail: Some("exit code: 0".into()),
            result: None,
        });
        assert_eq!(waiter.await.unwrap().unwrap().status, JobStatus::Completed);
        let _ = events.recv().await;
        let settled = events.recv().await.unwrap();
        assert!(matches!(settled, JobEvent::Settled { awaited: true, .. }));
        assert!(settled.completion_notice().is_none());
        assert_eq!(
            registry
                .wait(sink.id(), 0.0, Some("s1"), None)
                .await
                .unwrap_err(),
            "invalid wait timeout: expected a positive number of milliseconds, got 0"
        );
    }

    #[test]
    fn admission_limit() {
        let registry = JobRegistry::new(JobConfig {
            max_concurrent_jobs_per_owner: 1,
            ..JobConfig::default()
        });
        let _first = registry.start(spec("s1")).unwrap();
        let error = registry.start(spec("s1")).err().unwrap();
        assert_eq!(
            error,
            "background job limit reached for this owner (limit: 1); use job_kill to stop an unneeded job, wait for it to finish, then retry"
        );
        assert!(registry.start(spec("s2")).is_ok());
    }
}
