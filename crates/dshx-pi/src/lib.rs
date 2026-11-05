//! Pi (`@earendil-works/pi-coding-agent`, pinned to [`PI_VERSION`]) as
//! Aether's second chat engine next to DSH.
//!
//! One `pi --mode rpc` process per active Pi chat ([`process`], [`rpc`]),
//! started with Aether's own agent folder, no project resources and only
//! the embedded bridge extension. Its events become ordinary Aether log
//! events ([`mapper`]), so the web UI, the Mac app and iOS show a Pi chat
//! with the same transcript, tool rows, queue, stop, costs and compaction
//! as a DSH chat. Keys and Aether tools reach Pi only through the bridge
//! socket ([`bridge`]). [`chat::PiChat`] drives one chat; [`pool::Pool`]
//! caps the processes.
pub mod bridge;
pub mod chat;
pub mod install;
pub mod mapper;
pub mod meta;
pub mod pool;
pub mod process;
pub mod rpc;

pub use chat::{
    Live, ModelChoice, PiChat, PiEnv, PiHost, TurnEndCompaction, gate_write, thinking_level,
};
pub use install::{Install, PI_VERSION};
pub use mapper::{Delta, Mapper, QueueChange, Route, Settled, Target, TurnEnd};
pub use pool::Pool;
pub use process::PiDirs;
