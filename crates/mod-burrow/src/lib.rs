//! Burrow: Gopher, Gemini and Finger in one browser, the descendant of Minuet's Gopher
//! client. Every page loads in the background, so several can load at once.

pub mod browser;
pub mod doc;
pub mod fetch;

use std::path::PathBuf;

use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

pub use browser::Browser;
use fetch::Pins;

/// The state-database key for certificate pins.
pub const PINS_KEY: &str = "burrow.pins";

pub struct BurrowModule {
    downloads: PathBuf,
    pins: Option<Pins>,
}

impl BurrowModule {
    pub fn new(downloads: PathBuf) -> Self {
        Self { downloads, pins: None }
    }

    fn pins(&mut self, cx: &Cx) -> Pins {
        self.pins
            .get_or_insert_with(|| {
                let text = cx.store.and_then(|s| s.kv_get(PINS_KEY).ok().flatten()).unwrap_or_default();
                Pins::load(&text)
            })
            .clone()
    }
}

impl Module for BurrowModule {
    fn id(&self) -> &'static str {
        browser::MODULE
    }
    fn name(&self) -> &'static str {
        "Burrow"
    }
    fn description(&self) -> &'static str {
        "Gopher, Gemini and Finger"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["gopher", "gophers", "gemini", "finger"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Floodgap Gopher", "gopher://gopher.floodgap.com/", "The best known Gopher server"),
            Launcher::new("Veronica-2 search", "gopher://gopher.floodgap.com/7/v2/vs", "Search all of Gopherspace"),
            Launcher::new("Project Gemini", "gemini://geminiprotocol.net/", "Home of the Gemini protocol"),
            Launcher::new("Kennedy search", "gemini://kennedy.gemi.dev/search", "Search Geminispace"),
            Launcher::new("Weather by finger", "finger://graph.no/oslo", "A forecast over Finger"),
        ]
    }
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let pins = self.pins(cx);
        Ok(Box::new(Browser::new(url.as_str(), pins, self.downloads.clone(), cx)))
    }
}
