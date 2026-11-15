//! Web: a text browser for the web. Pages load in the background over HTTP/2 or HTTP/1.1,
//! show as styled text with numbered hints for links, and can switch to reader mode.
//! Forms, history, bookmarks, downloads and "save as Markdown" are here; pages that need
//! JavaScript are handed to your own browser.

pub mod fetch;
pub mod pages;
pub mod window;

use std::path::PathBuf;

use alouette_net::web::Browser;
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

pub use window::WebWindow;

pub const MODULE: &str = "web";

pub struct WebModule {
    downloads: PathBuf,
    browser: Option<Browser>,
}

impl WebModule {
    pub fn new(downloads: PathBuf) -> Self {
        Self { downloads, browser: None }
    }
}

impl Module for WebModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Web"
    }
    fn description(&self) -> &'static str {
        "Read the web as text, with reader mode"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["http", "https", "web"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Start page", "web:", "Search, bookmarks and recent pages"),
            Launcher::new("Wikipedia", "https://en.wikipedia.org/wiki/Special:Random", "A random article"),
            Launcher::new("Hacker News", "https://news.ycombinator.com/", "Technology news"),
            Launcher::new("Lite news", "https://lite.cnn.com/", "Text-only headlines"),
            Launcher::new("Bookmarks", "web:bookmarks", "Pages you bookmarked"),
            Launcher::new("History", "web:history", "Pages you visited"),
        ]
    }
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let browser = self.browser.get_or_insert_with(|| Browser::new(cx.net.clone())).clone();
        let target = match url.scheme() {
            "web" => {
                let rest = url.as_str()["web:".len()..].trim_start_matches("//");
                if rest.is_empty() {
                    if cx.config.web.home.trim().is_empty() {
                        "web:start".to_string()
                    } else {
                        cx.config.web.home.clone()
                    }
                } else if rest.contains("://") {
                    rest.to_string()
                } else {
                    format!("web:{rest}")
                }
            }
            _ => url.to_string(),
        };
        Ok(Box::new(WebWindow::new(&target, browser, self.downloads.clone(), cx)))
    }
}
