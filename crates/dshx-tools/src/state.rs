//! Per-session tool state, carried in `ToolContext::state`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dshx_core::{ToolContext, ToolError};

use crate::jobs::JobRegistry;
use crate::persistent_bash::Shells;
use crate::spill::SpillStore;

/// What a session last saw of a file (dsh `FsObservation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// The file existed with this version (`dev:ino:size:mtime:ctime`).
    Present(String),
    /// A read found nothing at the path.
    Absent,
}

/// Shared state of one session's tools: the fs observation gate (read before
/// write/edit), the job registry, the spill store, the persistent shells and
/// the current model name.
///
/// Build one per session and put it into every `ToolContext` of that session:
///
/// ```
/// # use std::sync::Arc;
/// let state = Arc::new(dshx_tools::ToolState::new());
/// // ToolContext { state: state.clone(), .. }
/// // The agent subscribes to job lifecycle events once:
/// let _events = state.jobs().subscribe();
/// ```
pub struct ToolState {
    jobs: JobRegistry,
    spill: SpillStore,
    observations: Mutex<HashMap<PathBuf, Observation>>,
    model: Mutex<Option<String>>,
    shells: Shells,
}

impl Default for ToolState {
    fn default() -> Self {
        ToolState::new()
    }
}

impl ToolState {
    /// Fresh state with its own job registry and the default spill store.
    pub fn new() -> Self {
        ToolState::with_parts(JobRegistry::default(), SpillStore::new())
    }

    /// State sharing a job registry (e.g. one per server) and a spill store.
    pub fn with_parts(jobs: JobRegistry, spill: SpillStore) -> Self {
        ToolState {
            jobs,
            spill,
            observations: Mutex::new(HashMap::new()),
            model: Mutex::new(None),
            shells: Shells::default(),
        }
    }

    /// The state of the calling session.
    pub fn get(ctx: &ToolContext) -> Result<&ToolState, ToolError> {
        ctx.state.downcast_ref::<ToolState>().ok_or_else(|| {
            ToolError::Failed(
                "tool state is missing: ToolContext.state must be a dshx_tools::ToolState".into(),
            )
        })
    }

    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }

    pub fn spill(&self) -> &SpillStore {
        &self.spill
    }

    /// The persistent `bash` shells (Minimal preset); the agent closes them
    /// when it is disposed.
    pub fn shells(&self) -> &Shells {
        &self.shells
    }

    /// Records the active model id; `read_image` names it in its refusal for
    /// text-only models.
    pub fn set_model(&self, model: impl Into<String>) {
        *self.model.lock().unwrap() = Some(model.into());
    }

    pub fn model(&self) -> Option<String> {
        self.model.lock().unwrap().clone()
    }

    pub(crate) fn observe(&self, target: &Path, observation: Observation) {
        self.observations
            .lock()
            .unwrap()
            .insert(target.to_path_buf(), observation);
    }

    pub(crate) fn observation(&self, target: &Path) -> Option<Observation> {
        self.observations.lock().unwrap().get(target).cloned()
    }
}
