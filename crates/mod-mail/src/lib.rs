//! Mail: Minuet's POPmail, grown up. One window per account, with folders, a message
//! list and a reader, and a separate window for writing.
//!
//! URLs:
//! - `mail:` your first account, or a form to add one when there are none
//! - `mail:<account>` an account by name
//! - `mail:setup` the form to add an account
//! - `mailto:someone@example.com?subject=Hi` write a message from your first account

pub mod bridge;
pub mod cache;
pub mod compose;
pub mod invite;
pub mod setup;
pub mod window;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alouette_core::config::MailAccount;
use alouette_mail::{Backend, Draft};
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

pub const MODULE: &str = "mail";

/// A message waiting for its compose window to open.
pub struct Pending {
    pub draft: Draft,
    /// The folder and id of the message being answered, to mark it answered.
    pub reply_of: Option<(String, String)>,
    /// The mail window that asked, which hears when the message is sent.
    pub origin: Option<u64>,
}

#[derive(Default)]
struct Inner {
    backends: HashMap<String, Arc<dyn Backend>>,
    pending: HashMap<u64, Pending>,
}

/// Connections and drafts shared by every mail window, so a second window on the same
/// account does not sign in again.
#[derive(Clone, Default)]
pub struct Shared(Arc<Mutex<Inner>>);

static TOKEN: AtomicU64 = AtomicU64::new(1);

impl Shared {
    pub fn get(&self, account: &str) -> Option<Arc<dyn Backend>> {
        self.0.lock().ok()?.backends.get(account).cloned()
    }
    pub fn put(&self, account: &str, b: Arc<dyn Backend>) {
        if let Ok(mut g) = self.0.lock() {
            g.backends.insert(account.to_string(), b);
        }
    }
    /// Forget a connection that stopped working.
    pub fn drop_backend(&self, account: &str) {
        if let Ok(mut g) = self.0.lock() {
            g.backends.remove(account);
        }
    }
    /// Keep a draft for a compose window and return its token.
    pub fn stash(&self, draft: Draft, reply_of: Option<(String, String)>, origin: Option<u64>) -> u64 {
        let t = TOKEN.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut g) = self.0.lock() {
            g.pending.insert(t, Pending { draft, reply_of, origin });
        }
        t
    }
    pub fn take(&self, token: u64) -> Option<Pending> {
        self.0.lock().ok()?.pending.remove(&token)
    }
}

pub struct MailModule {
    cache_dir: PathBuf,
    data_dir: PathBuf,
    shared: Shared,
}

impl MailModule {
    /// `data_dir` is where contacts and calendars live, for invitations and senders.
    pub fn new(cache_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self { cache_dir, data_dir, shared: Shared::default() }
    }
}

fn find<'a>(cx: &'a Cx, name: &str) -> Result<&'a MailAccount, String> {
    cx.config
        .mail
        .accounts
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("No mail account called {name}. Open mail:setup to add one."))
}

fn decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default()
}

/// Read a `mailto:` URL (RFC 6068) into a draft.
pub fn mailto(url: &Url) -> Draft {
    let mut d = Draft { to: decode(url.path()), ..Default::default() };
    for (k, v) in url.query_pairs() {
        let v = v.into_owned();
        match k.to_ascii_lowercase().as_str() {
            "to" => d.to = if d.to.is_empty() { v } else { format!("{}, {v}", d.to) },
            "cc" => d.cc = v,
            "bcc" => d.bcc = v,
            "subject" => d.subject = v,
            "body" => d.body = v.replace("\r\n", "\n"),
            "in-reply-to" => d.in_reply_to = Some(v.trim_matches(['<', '>']).to_string()),
            _ => {}
        }
    }
    d
}

impl Module for MailModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Mail"
    }
    fn description(&self) -> &'static str {
        "Email over IMAP, JMAP, Gmail and Microsoft 365"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["mail", "mailto"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Mail", "mail:", "Your mailbox; c writes, r replies"),
            Launcher::new("Add a mail account", "mail:setup", "IMAP, JMAP, Gmail or Microsoft 365"),
        ]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        if url.scheme() == "mailto" {
            let acct = cx.config.mail.accounts.first().cloned().ok_or("Add a mail account first: open mail:setup")?;
            let mut d = mailto(url);
            if let Some(s) = &acct.signature {
                d.body.push_str(&format!("\n\n-- \n{s}"));
            }
            return Ok(Box::new(compose::ComposeWindow::new(
                acct,
                self.shared.clone(),
                Pending { draft: d, reply_of: None, origin: None },
                cx,
            )));
        }
        let path = url.path().trim_matches('/');
        let cache = Some(self.cache_dir.clone());
        if path.is_empty() {
            return Ok(match cx.config.mail.accounts.first().cloned() {
                Some(a) => Box::new(window::MailWindow::new(a, self.shared.clone(), cache, self.data_dir.clone(), cx)),
                None => Box::new(setup::SetupWindow::new()),
            });
        }
        if path == "setup" {
            return Ok(Box::new(setup::SetupWindow::new()));
        }
        if let Some(rest) = path.strip_prefix("compose/") {
            let (name, token) = rest.rsplit_once('/').ok_or("Use mail:compose/<account>/<draft>")?;
            let acct = find(cx, &decode(name))?.clone();
            let pending = token.parse().ok().and_then(|t| self.shared.take(t)).unwrap_or(Pending {
                draft: Draft {
                    body: acct.signature.as_ref().map(|s| format!("\n\n-- \n{s}")).unwrap_or_default(),
                    ..Default::default()
                },
                reply_of: None,
                origin: None,
            });
            return Ok(Box::new(compose::ComposeWindow::new(acct, self.shared.clone(), pending, cx)));
        }
        let acct = find(cx, &decode(path))?.clone();
        Ok(Box::new(window::MailWindow::new(acct, self.shared.clone(), cache, self.data_dir.clone(), cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mailto() {
        let u = Url::parse("mailto:a@example.com?subject=Hello%20there&cc=b@example.com&body=Line%201%0D%0ALine%202")
            .unwrap();
        let d = mailto(&u);
        assert_eq!(d.to, "a@example.com");
        assert_eq!(d.cc, "b@example.com");
        assert_eq!(d.subject, "Hello there");
        assert_eq!(d.body, "Line 1\nLine 2");
        let u = Url::parse("mailto:?to=x@example.com").unwrap();
        assert_eq!(mailto(&u).to, "x@example.com");
    }

    #[test]
    fn stashes_drafts_once() {
        let s = Shared::default();
        let t = s.stash(Draft { subject: "x".into(), ..Default::default() }, None, Some(3));
        let p = s.take(t).unwrap();
        assert_eq!(p.draft.subject, "x");
        assert_eq!(p.origin, Some(3));
        assert!(s.take(t).is_none());
    }
}
