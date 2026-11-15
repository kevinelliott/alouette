//! User-facing notifications (toasts).

use std::time::{Duration, Instant};

/// How important a notification is. Controls its color and how long it stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

/// A short message shown in the corner of the screen.
#[derive(Debug, Clone)]
pub struct Notification {
    pub level: Level,
    pub text: String,
    pub created: Instant,
}

impl Notification {
    pub fn new(level: Level, text: impl Into<String>) -> Self {
        Self { level, text: text.into(), created: Instant::now() }
    }
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(Level::Info, text)
    }
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(Level::Success, text)
    }
    pub fn warning(text: impl Into<String>) -> Self {
        Self::new(Level::Warning, text)
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(Level::Error, text)
    }

    /// How long this notification stays on screen.
    pub fn ttl(&self) -> Duration {
        match self.level {
            Level::Info | Level::Success => Duration::from_secs(4),
            Level::Warning => Duration::from_secs(6),
            Level::Error => Duration::from_secs(8),
        }
    }

    pub fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.created) >= self.ttl()
    }
}
