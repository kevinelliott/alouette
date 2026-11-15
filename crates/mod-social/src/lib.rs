//! Social: the fediverse in a window. Mastodon and servers that speak its API
//! (GoToSocial, Pleroma, Akkoma, Sharkey and more).
//!
//! URLs:
//! - `social:` your first account, or sign in to a new one
//! - `social:<account>` another saved account
//! - `social:@you@server.example` sign in to that server

pub mod api;
pub mod compose;
pub mod testserver;
pub mod window;

use std::sync::atomic::{AtomicU64, Ordering};

use alouette_core::config::SocialAccount;
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

pub const MODULE: &str = "social";

static NEXT: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// "5m", "3h", "2d" since a time.
pub fn ago(t: i64) -> String {
    let d = (time::OffsetDateTime::now_utc().unix_timestamp() - t).max(0);
    match d {
        0..60 => "now".into(),
        60..3600 => format!("{}m", d / 60),
        3600..86400 => format!("{}h", d / 3600),
        86400..2_592_000 => format!("{}d", d / 86400),
        _ => format!("{}mo", d / 2_592_000),
    }
}

pub struct SocialModule;

impl Module for SocialModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Social"
    }
    fn description(&self) -> &'static str {
        "The fediverse: Mastodon and friends"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["social"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![Launcher::new("Social", "social:", "Your fediverse timelines; c posts, r replies")]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let what = url.path().trim_matches('/');
        let accounts = cx.config.social.accounts.clone();
        if what.is_empty() {
            return match accounts.first() {
                Some(a) => Ok(Box::new(window::SocialWindow::new(a.clone(), false, cx))),
                None => Err("Open social:@you@your.server to sign in, such as social:@me@mastodon.social".into()),
            };
        }
        if let Some(a) = accounts.iter().find(|a| a.name.eq_ignore_ascii_case(what)) {
            return Ok(Box::new(window::SocialWindow::new(a.clone(), false, cx)));
        }
        // @user@host or host: a new account, saved once signed in.
        let host = api::base(what);
        let host = host.trim_start_matches("https://").trim_start_matches("http://").to_string();
        if !host.contains('.') && !host.starts_with("127.") && !host.starts_with("localhost") {
            return Err(format!("No account called {what}. Open social:@you@your.server to sign in."));
        }
        let user = what
            .trim_start_matches('@')
            .split('@')
            .next()
            .filter(|u| what.matches('@').count() >= 1 && !u.is_empty() && *u != host)
            .unwrap_or("me");
        let name = if accounts.iter().any(|a| a.name == user) { host.clone() } else { user.to_string() };
        Ok(Box::new(window::SocialWindow::new(
            SocialAccount { name, instance: if what.starts_with("http") { what.to_string() } else { host } },
            true,
            cx,
        )))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ages() {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        assert_eq!(super::ago(now), "now");
        assert_eq!(super::ago(now - 7200), "2h");
    }
}
