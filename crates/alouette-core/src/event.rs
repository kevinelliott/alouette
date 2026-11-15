//! Events sent from background work to the app loop.

use crate::config::Config;
use crate::notify::Notification;
use crate::task::TaskId;

/// Everything that can wake the UI apart from terminal input.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A task changed state or reported progress. The task manager already holds the new state.
    TaskUpdated(TaskId),
    /// The config file changed on disk and parsed cleanly.
    ConfigReloaded(Box<Config>),
    /// The config file changed on disk but could not be parsed.
    ConfigError(String),
    /// Something asked to open a URL (a link, the command line, another module).
    OpenUrl(String),
    /// Show a toast.
    Notify(Notification),
    /// A module-specific message, addressed by module id. Modules downcast the payload.
    Module { module: &'static str, payload: ModulePayload },
    /// Stop the app.
    Quit,
}

/// An opaque, cloneable payload for module events.
#[derive(Clone)]
pub struct ModulePayload(pub std::sync::Arc<dyn std::any::Any + Send + Sync>);

impl ModulePayload {
    pub fn new<T: std::any::Any + Send + Sync>(value: T) -> Self {
        Self(std::sync::Arc::new(value))
    }
    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for ModulePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModulePayload(..)")
    }
}

/// The sending half that background work uses to wake the UI.
pub type EventSender = tokio::sync::mpsc::UnboundedSender<AppEvent>;
/// The receiving half owned by the app loop.
pub type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<AppEvent>;

/// Create a new event channel.
pub fn channel() -> (EventSender, EventReceiver) {
    tokio::sync::mpsc::unbounded_channel()
}
