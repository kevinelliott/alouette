//! Alouette core: the parts every module shares.
//!
//! * [`event`]: the typed events that flow from background work to the UI.
//! * [`task`]: the task manager behind the Tasks window. Every network job is a task.
//! * [`router`]: turns URLs (and bare input such as `example.org`) into a module to open them.
//! * [`config`]: the `alouette.toml` model, loading, saving and hot reload.
//! * [`paths`]: platform directories for config, data, cache and logs.
//! * [`message`]: the unified message model shared by mail, news, feeds and chat.

pub mod config;
pub mod event;
pub mod message;
pub mod notify;
pub mod paths;
pub mod router;
pub mod shared;
pub mod task;

pub use config::{Config, ConfigWatcher};
pub use event::AppEvent;
pub use notify::{Level, Notification};
pub use paths::Paths;
pub use router::{Route, Router};
pub use task::{TaskCtx, TaskId, TaskInfo, TaskManager, TaskState};

/// The Alouette version, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
