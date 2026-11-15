//! The built-in `about:` pages: welcome, tasks, help, keys, settings, history and credits.

mod history;
mod pages;
mod settings;
mod tasks;

use url::Url;

use crate::module::{Launcher, Module};
use crate::window::{Cx, Window};

pub use history::HistoryWindow;
pub use pages::{KeysWindow, TextPage};
pub use settings::SettingsWindow;
pub use tasks::TasksWindow;

/// Handles `about:` URLs.
pub struct CoreModule;

impl Module for CoreModule {
    fn id(&self) -> &'static str {
        "core"
    }
    fn name(&self) -> &'static str {
        "Alouette"
    }
    fn description(&self) -> &'static str {
        "Built-in pages: welcome, tasks, help, settings and history"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["about"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Welcome", "about:welcome", "Start here"),
            Launcher::new("Tasks", "about:tasks", "Background jobs"),
            Launcher::new("History", "about:history", "Recently opened URLs"),
        ]
    }
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let page = url.path().trim_matches('/');
        Ok(match page {
            "welcome" | "" => Box::new(TextPage::welcome(cx)),
            "alouette" => Box::new(TextPage::about()),
            "minuet" => Box::new(TextPage::minuet()),
            "help" => Box::new(TextPage::help()),
            "keys" => Box::new(KeysWindow::new(cx)),
            "tasks" => Box::new(TasksWindow::new()),
            "settings" => Box::new(SettingsWindow::new()),
            "history" => Box::new(HistoryWindow::new(cx)),
            other => return Err(format!("there is no about:{other} page")),
        })
    }
}
