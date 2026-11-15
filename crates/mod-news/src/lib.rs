//! News and Feeds: Minuet's NetNews, grown up, beside a reader for RSS, Atom and JSON Feed.
//!
//! URLs:
//! - `news:` your first news server; `news:<server>` another; `news:<group>` a group on
//!   the first server
//! - `nntp://host/group` and `nntps://host/group` a server that is not saved
//! - `feeds:` your feeds; `feed:https://example.org/feed.xml` one feed (s subscribes)

pub mod conn;
pub mod feeds;
pub mod news;
pub mod post;
pub mod readstate;
pub mod thread;
pub mod util;

use std::sync::atomic::{AtomicU64, Ordering};

use alouette_core::config::NewsServer;
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

pub const NEWS: &str = "news";
pub const FEEDS: &str = "feeds";

static NEXT: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct NewsModule;

impl Module for NewsModule {
    fn id(&self) -> &'static str {
        NEWS
    }
    fn name(&self) -> &'static str {
        "News"
    }
    fn description(&self) -> &'static str {
        "Usenet newsgroups over NNTP, threaded"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["news", "nntp", "nntps", "snews"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![Launcher::new("News", "news:", "Your newsgroups; L lists every group on the server")]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let servers = cx.config.news.servers.clone();
        match url.scheme() {
            "nntp" | "nntps" | "snews" => {
                let host =
                    url.host_str().filter(|h| !h.is_empty()).ok_or("Add a host, as in nntps://news.example.org/")?;
                let port = url.port().unwrap_or(if url.scheme() == "nntp" { 119 } else { 563 });
                let group = url.path().trim_matches('/').to_string();
                // A saved server with this host keeps its login and groups.
                let mut server = servers
                    .iter()
                    .find(|s| conn::endpoint(&s.host).0.eq_ignore_ascii_case(host))
                    .cloned()
                    .unwrap_or_else(|| NewsServer {
                        name: host.to_string(),
                        host: format!("{host}:{port}"),
                        user: (!url.username().is_empty()).then(|| url.username().to_string()),
                        groups: vec![],
                    });
                if !group.is_empty() && !server.groups.contains(&group) {
                    server.groups.insert(0, group);
                }
                Ok(Box::new(news::NewsWindow::new(server, cx)))
            }
            _ => {
                let what = url.path().trim_matches('/');
                if what.is_empty() {
                    let s = servers
                        .first()
                        .cloned()
                        .ok_or("Add a news server under [[news.servers]] in Settings, or open nntps://host/")?;
                    return Ok(Box::new(news::NewsWindow::new(s, cx)));
                }
                if let Some(s) = servers.iter().find(|s| s.name.eq_ignore_ascii_case(what)) {
                    return Ok(Box::new(news::NewsWindow::new(s.clone(), cx)));
                }
                if what.contains('@') {
                    return Err("Opening an article by its message ID is not supported yet".into());
                }
                // A group name on the first server.
                let mut s = servers.first().cloned().ok_or("Add a news server under [[news.servers]] in Settings")?;
                if !s.groups.iter().any(|g| g == what) {
                    s.groups.insert(0, what.to_string());
                }
                Ok(Box::new(news::NewsWindow::new(s, cx)))
            }
        }
    }
}

pub struct FeedsModule;

impl Module for FeedsModule {
    fn id(&self) -> &'static str {
        FEEDS
    }
    fn name(&self) -> &'static str {
        "Feeds"
    }
    fn description(&self) -> &'static str {
        "RSS, Atom and JSON Feed, with podcasts"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["feeds", "feed"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Feeds", "feeds:", "Your feeds; a adds one"),
            Launcher::new("Rust blog", "feed:https://blog.rust-lang.org/feed.xml", "An example feed"),
        ]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let extra = match url.scheme() {
            "feed" => {
                // feed:https://x/y and the older feed://x/y.
                let s = url.as_str();
                let rest = s.strip_prefix("feed:").unwrap_or(s).trim_start_matches("//");
                let rest = if rest.starts_with("http://") || rest.starts_with("https://") {
                    rest.to_string()
                } else {
                    format!("https://{rest}")
                };
                Some(rest)
            }
            _ => None,
        };
        Ok(Box::new(feeds::FeedsWindow::new(extra, cx)))
    }
}
