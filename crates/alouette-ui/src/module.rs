//! The module contract.
//!
//! A module owns some URL schemes and opens windows for them. Launchers are just URLs, so
//! the Modules menu, the command palette and links in other windows all take one path.

use url::Url;

use crate::window::{Cx, Window};

/// An entry in the Modules menu and the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    pub label: String,
    pub url: String,
    pub description: String,
}

impl Launcher {
    pub fn new(label: impl Into<String>, url: impl Into<String>, description: impl Into<String>) -> Self {
        Self { label: label.into(), url: url.into(), description: description.into() }
    }
}

/// One Alouette module: Mail, News, Burrow, Files and so on.
pub trait Module: Send {
    /// A short, stable id such as `mail` or `burrow`.
    fn id(&self) -> &'static str;

    /// The display name, such as "Mail".
    fn name(&self) -> &'static str;

    /// One line on what the module does.
    fn description(&self) -> &'static str;

    /// The URL schemes this module opens.
    fn schemes(&self) -> &'static [&'static str] {
        &[]
    }

    /// Entries for the Modules menu and the palette.
    fn launchers(&self) -> Vec<Launcher> {
        Vec::new()
    }

    /// Build a window for a URL in one of this module's schemes.
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String>;

    /// Called once when the app starts running, for work that goes on without a window,
    /// such as calendar reminders.
    fn start(&mut self, _cx: &mut Cx) {}

    /// Every app event, after the windows have seen it. A reloaded config arrives here too.
    fn on_event(&mut self, _ev: &alouette_core::AppEvent, _cx: &mut Cx) {}
}
