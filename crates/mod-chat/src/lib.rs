//! Chat: IRC networks and Matrix accounts in one window, with a buffer list, nick list
//! and a line to type in. Connections live in the module, so closing the window keeps
//! you connected.
//!
//! URLs:
//! - `chat:` the Chat window; `chat:<network>` that network's buffer
//! - `irc://irc.libera.chat/#rust`, `ircs://host:6697/channel`, `irc://host/nick,isnick`
//! - `matrix:r/alouette:matrix.org`, `matrix:roomid/abc:matrix.org` (MSC2312 links)

pub mod conn;
pub mod hub;
pub mod window;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use alouette_core::AppEvent;
use alouette_core::config::{Chat, Config, IrcNetwork};
use alouette_ui::ask::Asker;
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

use crate::hub::{Hub, Link, Network, Shared, State};

pub const MODULE: &str = "chat";

/// Where an `irc://` link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrcTarget {
    /// `host` or `host:port`.
    pub host: String,
    pub tls: bool,
    pub target: Option<String>,
    pub is_nick: bool,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read an `irc://` or `ircs://` link.
pub fn parse_irc(url: &Url) -> Option<IrcTarget> {
    let host = url.host_str()?.trim_matches(['[', ']']).to_string();
    let tls = url.scheme() == "ircs";
    let host = match url.port() {
        Some(p) => format!("{host}:{}{p}", if tls { "+" } else { "" }),
        None if tls => host,
        None => format!("{host}:6667"),
    };
    // A `#` in the link is a fragment unless escaped: `irc://host/#rust` puts it there.
    let mut path = decode(url.path().trim_start_matches('/'));
    if path.is_empty()
        && let Some(f) = url.fragment()
    {
        path = format!("#{}", decode(f));
    }
    let mut parts = path.split(',');
    let target = parts.next().filter(|t| !t.is_empty()).map(str::to_string);
    let flags: Vec<&str> = parts.collect();
    let is_nick = flags.iter().any(|f| *f == "isnick" || *f == "isuser");
    let target = target.map(|t| if is_nick || t.starts_with(['#', '&', '+', '!']) { t } else { format!("#{t}") });
    Some(IrcTarget { host, tls, target, is_nick })
}

/// A `matrix:` link as a room alias or ID (`#a:b`, `!id:b`) or user (`@u:b`).
pub fn parse_matrix(url: &Url) -> Option<String> {
    let path = url.path();
    let (kind, rest) = path.split_once('/')?;
    let rest = decode(rest.split('/').next().unwrap_or(""));
    if rest.is_empty() {
        return None;
    }
    match kind {
        "r" => Some(format!("#{rest}")),
        "roomid" => Some(format!("!{rest}")),
        "u" => Some(format!("@{rest}")),
        _ => None,
    }
}

pub struct ChatModule {
    data_dir: PathBuf,
    hub: Shared,
    asker: Option<Asker>,
    started: bool,
}

impl ChatModule {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir, hub: Arc::new(Mutex::new(Hub::default())), asker: None, started: false }
    }

    fn ctx(&mut self, cx: &Cx) -> conn::Ctx {
        let asker = self
            .asker
            .get_or_insert_with(|| {
                let wake = cx.tasks.events();
                Asker::new(move || {
                    let _ = wake.send(AppEvent::Module {
                        module: MODULE,
                        payload: alouette_core::event::ModulePayload::new(()),
                    });
                    let _ = wake.send(AppEvent::Notify(alouette_core::Notification::info(
                        "Chat is asking for a password: open the Chat window",
                    )));
                })
            })
            .clone();
        conn::Ctx {
            hub: self.hub.clone(),
            net: cx.net.clone(),
            secrets: cx.secrets.clone(),
            asker,
            data_dir: self.data_dir.clone(),
            chat: cx.config.chat.clone(),
            proxy: cx.config.network.proxy.clone(),
        }
    }

    /// Bring the hub in line with the config: new networks appear, settings refresh.
    fn sync(&self, cfg: &Config, cx_events: Option<alouette_core::event::EventSender>) {
        let Ok(mut h) = self.hub.lock() else { return };
        apply_config(&mut h, &cfg.chat, &self.data_dir);
        if let Some(e) = cx_events {
            h.events = Some(e);
        }
    }
}

pub fn apply_config(h: &mut Hub, chat: &Chat, data_dir: &std::path::Path) {
    h.logs = chat.logs.then(|| data_dir.join("chat").join("logs"));
    h.notify = chat.notify.clone();
    h.show_joins = chat.show_joins;
    h.scrollback = chat.scrollback;
    for n in &chat.irc {
        match h.net(&n.name) {
            Some(i) => h.nets[i].irc = Some(n.clone()),
            None => h.nets.push(Network::from_irc(n)),
        }
    }
    for m in &chat.matrix {
        match h.net(&m.name) {
            Some(i) => h.nets[i].matrix = Some(m.clone()),
            None => h.nets.push(Network::from_matrix(m)),
        }
    }
}

impl Module for ChatModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Chat"
    }
    fn description(&self) -> &'static str {
        "IRC and Matrix, side by side, with encryption"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["chat", "irc", "ircs", "matrix"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Chat", "chat:", "IRC and Matrix; /help lists commands"),
            Launcher::new("Chat: Libera.Chat", "ircs://irc.libera.chat/#alouette", "The Libera.Chat IRC network"),
        ]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        self.sync(cx.config, Some(cx.tasks.events()));
        let ctx = self.ctx(cx);
        match url.scheme() {
            "irc" | "ircs" => {
                let t = parse_irc(url).ok_or("That IRC link has no server")?;
                let (want_host, _, _) = proto_irc::endpoint(&t.host, Some(t.tls));
                let found = {
                    let h = self.hub.lock().map_err(|e| e.to_string())?;
                    h.nets.iter().find_map(|n| {
                        let cfg = n.irc.as_ref()?;
                        let (host, _, _) = proto_irc::endpoint(&cfg.host, None);
                        host.eq_ignore_ascii_case(&want_host).then(|| n.name.clone())
                    })
                };
                let name = match found {
                    Some(n) => n,
                    None => {
                        let cfg = IrcNetwork { name: want_host.clone(), host: t.host.clone(), ..Default::default() };
                        let saved = cfg.clone();
                        cx.edit_config(move |c| {
                            if !c.chat.irc.iter().any(|n| n.name == saved.name) {
                                c.chat.irc.push(saved);
                            }
                        });
                        self.hub.lock().map_err(|e| e.to_string())?.nets.push(Network::from_irc(&cfg));
                        want_host
                    }
                };
                self.join_irc(&ctx, &name, t.target.as_deref(), t.is_nick);
                Ok(Box::new(window::ChatWindow::new(self.hub.clone(), ctx, cx)))
            }
            "matrix" => {
                let what = parse_matrix(url).ok_or("That Matrix link isn't one Alouette understands")?;
                let name = {
                    let h = self.hub.lock().map_err(|e| e.to_string())?;
                    h.nets.iter().find(|n| n.is_matrix()).map(|n| n.name.clone())
                }
                .ok_or("Sign in to Matrix first: open chat: and type /matrix @you:your.server")?;
                if what.starts_with('@') {
                    return Err(format!("{what} is a person; message them from a room you share, or /verify {what}"));
                }
                {
                    let mut h = self.hub.lock().map_err(|e| e.to_string())?;
                    let i = h.net(&name).ok_or("no account")?;
                    h.focus = Some((name.clone(), what.clone()));
                    match (&h.nets[i].link, &h.nets[i].state) {
                        (Link::Matrix { tx: Some(tx) }, State::Online) => {
                            let _ = tx.send(alouette_matrix::Cmd::Join(what));
                        }
                        _ => h.nets[i].pending.push(what),
                    }
                }
                conn::connect(&ctx, &name);
                Ok(Box::new(window::ChatWindow::new(self.hub.clone(), ctx, cx)))
            }
            _ => {
                let what = url.path().trim_matches('/');
                if !what.is_empty() {
                    let mut h = self.hub.lock().map_err(|e| e.to_string())?;
                    if h.net(what).is_none() {
                        return Err(format!("No network called {what}. Add one with /server or /matrix in chat:"));
                    }
                    h.focus = Some((what.to_string(), String::new()));
                }
                Ok(Box::new(window::ChatWindow::new(self.hub.clone(), ctx, cx)))
            }
        }
    }

    fn start(&mut self, cx: &mut Cx) {
        if self.started {
            return;
        }
        self.started = true;
        self.sync(cx.config, Some(cx.tasks.events()));
        let ctx = self.ctx(cx);
        for n in cx.config.chat.irc.iter().filter(|n| n.autoconnect) {
            conn::connect(&ctx, &n.name);
        }
        for m in cx.config.chat.matrix.iter().filter(|m| m.autoconnect) {
            conn::connect(&ctx, &m.name);
        }
    }

    fn on_event(&mut self, ev: &AppEvent, _cx: &mut Cx) {
        if let AppEvent::ConfigReloaded(c) = ev {
            self.sync(c, None);
        }
    }
}

impl ChatModule {
    fn join_irc(&self, ctx: &conn::Ctx, name: &str, target: Option<&str>, is_nick: bool) {
        if let Ok(mut h) = self.hub.lock()
            && let Some(i) = h.net(name)
        {
            if let Some(t) = target {
                h.focus = Some((name.to_string(), t.to_string()));
                let n = &mut h.nets[i];
                if is_nick {
                    n.buffer(t, t, hub::BufKind::Query);
                } else {
                    match (&n.link, &n.state) {
                        (Link::Irc { tx: Some(tx), .. }, State::Online) => {
                            let _ =
                                tx.send(proto_irc::client::Command::Send(vec![proto_irc::Message::new("JOIN", &[t])]));
                        }
                        _ => {
                            if let Some(cfg) = n.irc.as_mut()
                                && !cfg.channels.iter().any(|c| c.eq_ignore_ascii_case(t))
                            {
                                cfg.channels.push(t.to_string());
                            }
                        }
                    }
                }
            } else {
                h.focus = Some((name.to_string(), String::new()));
            }
        }
        conn::connect(ctx, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irc_links() {
        let t = |s: &str| parse_irc(&Url::parse(s).unwrap()).unwrap();
        let a = t("ircs://irc.libera.chat/%23rust");
        assert_eq!(
            a,
            IrcTarget { host: "irc.libera.chat".into(), tls: true, target: Some("#rust".into()), is_nick: false }
        );
        assert_eq!(t("irc://irc.libera.chat/#rust").target.as_deref(), Some("#rust"));
        assert_eq!(t("irc://irc.libera.chat/rust").target.as_deref(), Some("#rust"));
        assert_eq!(t("irc://irc.libera.chat").host, "irc.libera.chat:6667");
        assert_eq!(t("ircs://h:7000/x").host, "h:+7000");
        let n = t("irc://h/bob,isnick");
        assert!(n.is_nick);
        assert_eq!(n.target.as_deref(), Some("bob"));
    }

    #[test]
    fn matrix_links() {
        let m = |s: &str| parse_matrix(&Url::parse(s).unwrap());
        assert_eq!(m("matrix:r/alouette:matrix.org").as_deref(), Some("#alouette:matrix.org"));
        assert_eq!(m("matrix:roomid/abc:matrix.org?via=matrix.org").as_deref(), Some("!abc:matrix.org"));
        assert_eq!(m("matrix:u/me:matrix.org").as_deref(), Some("@me:matrix.org"));
        assert_eq!(m("matrix:x/y"), None);
    }
}
