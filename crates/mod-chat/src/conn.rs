//! Connecting networks: IRC over TCP or TLS with SASL, Matrix through the SDK, each
//! reconnecting with a growing delay until the person quits.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alouette_core::config::{Chat, IrcNetwork, MatrixAccount};
use alouette_net::{ClientCert, Net, TlsOptions};
use alouette_store::secrets::SecretStore;
use alouette_ui::ask::{Asker, Question};
use proto_irc::client::Command as IrcCmd;
use proto_irc::{Sasl, Session};
use tokio::sync::mpsc;

use crate::hub::{BufKind, Hub, LineKind, Link, Shared, State, safe_name};

/// What a connection needs from the app.
#[derive(Clone)]
pub struct Ctx {
    pub hub: Shared,
    pub net: Net,
    pub secrets: Arc<dyn SecretStore>,
    pub asker: Asker,
    pub data_dir: PathBuf,
    pub chat: Chat,
    pub proxy: Option<String>,
}

fn tilde(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(s),
    }
}

/// The nick to use: the network's, else the chat default, else the login name.
pub fn nick_for(cfg: &IrcNetwork, chat: &Chat) -> String {
    [cfg.nick.as_str(), chat.nick.as_str()]
        .into_iter()
        .find(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("USER").ok().or_else(|| std::env::var("USERNAME").ok()))
        .map(|n| n.chars().filter(|c| c.is_ascii_alphanumeric() || "_-[]\\^{}|`".contains(*c)).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "alouette".into())
}

fn with_net<T>(hub: &Shared, name: &str, f: impl FnOnce(&mut Hub, usize) -> T) -> Option<T> {
    let mut h = hub.lock().ok()?;
    let i = h.net(name)?;
    let out = f(&mut h, i);
    h.wake();
    Some(out)
}

fn status(hub: &Shared, name: &str, kind: LineKind, text: impl Into<String>) {
    let text = text.into();
    with_net(hub, name, |h, i| h.status(i, kind, text));
}

/// A secret, asked for once and kept.
async fn secret(ctx: &Ctx, key: &str, title: &str, body: Vec<String>) -> Option<String> {
    if let Ok(Some(v)) = ctx.secrets.get(key) {
        return Some(v);
    }
    let q = Question { title: title.into(), body, prompt: "Password".into(), secret: true, yes_no: false };
    let v = ctx.asker.ask(q).await?;
    let _ = ctx.secrets.set(key, &v);
    Some(v)
}

/// Start (or restart) a network's connection.
pub fn connect(ctx: &Ctx, name: &str) {
    let found = ctx.hub.lock().ok().and_then(|h| h.net(name).map(|i| h.nets[i].is_matrix()));
    let (ctx, name) = (ctx.clone(), name.to_string());
    match found {
        Some(false) => {
            tokio::spawn(async move { irc_loop(ctx, name).await });
        }
        Some(true) => {
            tokio::spawn(async move { matrix_loop(ctx, name).await });
        }
        None => {}
    }
}

/// Disconnect on purpose.
pub fn disconnect(hub: &Shared, name: &str, reason: &str) {
    with_net(hub, name, |h, i| {
        let n = &mut h.nets[i];
        n.quitting = true;
        match &n.link {
            Link::Irc { tx: Some(tx), .. } => {
                let _ = tx.send(IrcCmd::Quit(reason.to_string()));
            }
            Link::Matrix { tx: Some(tx) } => {
                let _ = tx.send(alouette_matrix::Cmd::Quit);
            }
            _ => {}
        }
        n.state = State::Off;
    });
}

/// Begin a connection attempt: returns its generation, or None if one is running.
fn begin(hub: &Shared, name: &str) -> Option<u64> {
    with_net(hub, name, |h, i| {
        let n = &mut h.nets[i];
        if matches!(n.state, State::Connecting | State::Online) {
            return None;
        }
        n.generation += 1;
        n.quitting = false;
        n.state = State::Connecting;
        Some(n.generation)
    })
    .flatten()
}

/// After a connection ends: whether to try again. A connection that was replaced (a
/// reconnect started meanwhile) touches nothing.
fn ended(hub: &Shared, name: &str, generation_id: u64, err: Option<String>, delay: u64) -> bool {
    with_net(hub, name, |h, i| {
        let n = &mut h.nets[i];
        if n.generation != generation_id {
            return false;
        }
        for b in n.buffers.iter_mut().filter(|b| b.kind == BufKind::Channel) {
            b.users.clear();
        }
        match &mut n.link {
            Link::Irc { session, tx } => {
                *session = None;
                *tx = None;
            }
            Link::Matrix { tx } => *tx = None,
        }
        let Some(e) = err.filter(|_| !n.quitting) else {
            n.state = State::Off;
            h.status(i, LineKind::Info, "Disconnected.");
            return false;
        };
        n.state = State::Failed(e.clone());
        h.status(i, LineKind::Error, format!("Disconnected: {e}. Trying again in {delay} seconds; /quit stops."));
        true
    })
    .unwrap_or(false)
}

fn still_wanted(hub: &Shared, name: &str, generation_id: u64) -> bool {
    with_net(hub, name, |h, i| {
        let n = &mut h.nets[i];
        let ok = !n.quitting && n.generation == generation_id;
        if ok {
            n.state = State::Off;
        }
        ok
    })
    .unwrap_or(false)
}

async fn irc_loop(ctx: Ctx, name: String) {
    let mut delay = 5u64;
    loop {
        let Some(generation_id) = begin(&ctx.hub, &name) else { return };
        let (registered, err) = irc_once(&ctx, &name).await;
        if registered {
            delay = 5;
        }
        let err = Some(err.unwrap_or_else(|| "the server closed the connection".into()));
        if !ended(&ctx.hub, &name, generation_id, err, delay) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(300);
        if !still_wanted(&ctx.hub, &name, generation_id) {
            return;
        }
    }
}

/// One IRC connection, from dial to drop. Returns whether registration finished.
async fn irc_once(ctx: &Ctx, name: &str) -> (bool, Option<String>) {
    let Some(cfg) = with_net(&ctx.hub, name, |h, i| h.nets[i].irc.clone()).flatten() else {
        return (false, Some("no such network".into()));
    };
    let (host, port, tls) = proto_irc::endpoint(&cfg.host, None);
    status(
        &ctx.hub,
        name,
        LineKind::Info,
        format!("Connecting to {host}:{port}{}…", if tls { " with TLS" } else { "" }),
    );
    let nick = nick_for(&cfg, &ctx.chat);
    let sasl_key = format!("irc/{name}/sasl");
    let sasl = match cfg.sasl.as_str() {
        "plain" => {
            let user = if cfg.account.is_empty() { nick.clone() } else { cfg.account.clone() };
            let body = vec![format!("Your services password for {user} on {name}."), "Kept in your keychain.".into()];
            match secret(ctx, &sasl_key, &format!("Sign in to {name}"), body).await {
                Some(pass) => Sasl::Plain { user, pass },
                None => Sasl::None,
            }
        }
        "external" => Sasl::External,
        _ => Sasl::None,
    };
    let password = if cfg.server_password {
        let body = vec![format!("The server password for {name}, such as a bouncer's.")];
        secret(ctx, &format!("irc/{name}/server"), &format!("Connect to {name}"), body).await
    } else {
        None
    };
    let client_cert = if cfg.cert.is_empty() {
        None
    } else if alouette_core::shared::restricted() {
        // The file would be the server's, not this person's.
        return (false, Some(format!("a client certificate file is {}", alouette_core::shared::REFUSED)));
    } else {
        match ClientCert::load(&tilde(&cfg.cert), None) {
            Ok(c) => {
                if let Some(fp) = c.fingerprint() {
                    status(&ctx.hub, name, LineKind::Info, format!("Using client certificate {fp}"));
                }
                Some(c)
            }
            Err(e) => return (false, Some(e.to_string())),
        }
    };
    let conn = match ctx.net.tcp(&host, port).await {
        Ok(c) => c,
        Err(e) => return (false, Some(e.to_string())),
    };
    let stream: Box<dyn alouette_net::Io> = if tls {
        let opts = TlsOptions { client_cert, ..TlsOptions::default() };
        match ctx.net.handshake(conn, &opts).await {
            Ok(t) => {
                status(&ctx.hub, name, LineKind::Info, t.info.badge());
                Box::new(t.stream)
            }
            Err(e) => return (false, Some(e.to_string())),
        }
    } else {
        conn.stream
    };
    let user: String = nick.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).take(10).collect();
    let session_cfg = proto_irc::Config {
        alt_nicks: vec![format!("{nick}_"), format!("{nick}__")],
        nick,
        user: if user.is_empty() { "alouette".into() } else { user },
        realname: ctx.chat.realname.clone(),
        password,
        sasl,
        highlights: ctx.chat.highlights.clone(),
        version: format!("Alouette {} https://github.com/kevinelliott/alouette", env!("CARGO_PKG_VERSION")),
        history: 50,
    };
    let session = Arc::new(Mutex::new(Session::new(session_cfg)));
    let (tx, rx) = mpsc::unbounded_channel();
    with_net(&ctx.hub, name, |h, i| {
        h.nets[i].link = Link::Irc { session: Some(session.clone()), tx: Some(tx.clone()) };
    });
    let registered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (hub, nm, reg, secrets) = (ctx.hub.clone(), name.to_string(), registered.clone(), ctx.secrets.clone());
    let channels = cfg.channels.clone();
    let r = proto_irc::client::run(stream, session, rx, move |ev| {
        match &ev {
            proto_irc::Event::Registered { .. } => {
                reg.store(true, std::sync::atomic::Ordering::Relaxed);
                // Join the configured channels and any left open from before a reconnect.
                let mut want: Vec<String> = channels.clone();
                if let Ok(h) = hub.lock()
                    && let Some(i) = h.net(&nm)
                {
                    for b in h.nets[i].buffers.iter().filter(|b| b.kind == BufKind::Channel && b.joined) {
                        if !want.iter().any(|w| w.eq_ignore_ascii_case(&b.key)) {
                            want.push(b.key.clone());
                        }
                    }
                }
                if !want.is_empty() {
                    let _ = tx.send(IrcCmd::Send(vec![proto_irc::Message::new("JOIN", &[&want.join(",")])]));
                }
            }
            proto_irc::Event::SaslFailed(_) => {
                // Ask again next time.
                let _ = secrets.delete(&format!("irc/{nm}/sasl"));
            }
            _ => {}
        }
        with_net(&hub, &nm, |h, i| h.irc_event(i, ev));
    })
    .await;
    (registered.load(std::sync::atomic::Ordering::Relaxed), r.err())
}

/// Where a Matrix account keeps its store, and a small file fallback for secrets when
/// there is no keychain.
fn matrix_dir(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join("chat").join("matrix").join(safe_name(name))
}

fn keep(ctx: &Ctx, dir: &Path, key: &str, file: &str, value: Option<&str>) {
    if ctx.secrets.persistent() {
        let _ = match value {
            Some(v) => ctx.secrets.set(key, v),
            None => ctx.secrets.delete(key),
        };
        return;
    }
    let path = dir.join(file);
    match value {
        Some(v) => {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(&path, v);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn recall(ctx: &Ctx, dir: &Path, key: &str, file: &str) -> Option<String> {
    if ctx.secrets.persistent() {
        return ctx.secrets.get(key).ok().flatten();
    }
    std::fs::read_to_string(dir.join(file)).ok()
}

async fn matrix_loop(ctx: Ctx, name: String) {
    let mut delay = 5u64;
    loop {
        let Some(generation_id) = begin(&ctx.hub, &name) else { return };
        let (ready, err) = matrix_once(&ctx, &name).await;
        if ready {
            delay = 5;
        }
        if !ended(&ctx.hub, &name, generation_id, err, delay) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(300);
        if !still_wanted(&ctx.hub, &name, generation_id) {
            return;
        }
    }
}

/// One Matrix connection. Returns whether it signed in, and why it stopped (None when on
/// purpose).
async fn matrix_once(ctx: &Ctx, name: &str) -> (bool, Option<String>) {
    let Some(acct) = with_net(&ctx.hub, name, |h, i| h.nets[i].matrix.clone()).flatten() else {
        return (false, Some("no such account".into()));
    };
    let MatrixAccount { homeserver, user, .. } = acct;
    let dir = matrix_dir(&ctx.data_dir, name);
    let (pass_key, sess_key) = (format!("matrix/{name}/store"), format!("matrix/{name}/session"));
    let passphrase = match recall(ctx, &dir, &pass_key, "passphrase") {
        Some(p) => p,
        None => {
            let p = alouette_matrix::new_passphrase();
            keep(ctx, &dir, &pass_key, "passphrase", Some(&p));
            p
        }
    };
    let mut password = None;
    let mut ready = false;
    loop {
        let session = recall(ctx, &dir, &sess_key, "session.json");
        let (tx, mut updates) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        with_net(&ctx.hub, name, |h, i| h.nets[i].link = Link::Matrix { tx: Some(cmd_tx) });
        let opts = alouette_matrix::Options {
            account: alouette_matrix::Account {
                name: name.to_string(),
                homeserver: homeserver.clone(),
                user: user.clone(),
            },
            store: dir.join("store"),
            passphrase: passphrase.clone(),
            session,
            password: password.take(),
            proxy: ctx.proxy.clone(),
        };
        let run = tokio::spawn(alouette_matrix::run(opts, tx, cmd_rx));
        while let Some(u) = updates.recv().await {
            let is_ready = matches!(u, alouette_matrix::Update::Ready { .. });
            let saved = with_net(&ctx.hub, name, |h, i| h.matrix_update(i, u)).flatten();
            if is_ready {
                ready = true;
            }
            if let Some(s) = saved {
                keep(ctx, &dir, &sess_key, "session.json", Some(&s));
            }
        }
        let stop = run.await.unwrap_or(alouette_matrix::Stop::Failed("the connection task stopped".into()));
        match stop {
            alouette_matrix::Stop::Quit => return (ready, None),
            alouette_matrix::Stop::LoggedOut => {
                keep(ctx, &dir, &sess_key, "session.json", None);
                status(&ctx.hub, name, LineKind::Info, "Signed out. This session's keys were removed from the server.");
                return (ready, None);
            }
            alouette_matrix::Stop::NeedPassword => {
                let q = Question {
                    title: format!("Sign in to Matrix as {user}"),
                    body: vec![format!(
                        "Your password on {homeserver}. It is used once; Alouette keeps the session, not the password."
                    )],
                    prompt: "Password".into(),
                    secret: true,
                    yes_no: false,
                };
                match ctx.asker.ask(q).await {
                    Some(p) => password = Some(p),
                    None => return (false, None),
                }
            }
            alouette_matrix::Stop::Failed(e) => {
                if e.contains("M_UNKNOWN_TOKEN") {
                    keep(ctx, &dir, &sess_key, "session.json", None);
                    status(&ctx.hub, name, LineKind::Error, "The server ended this session. Sign in again.");
                    continue;
                }
                if e.contains("M_FORBIDDEN") {
                    status(&ctx.hub, name, LineKind::Error, format!("Sign-in refused: {e}"));
                    continue;
                }
                return (ready, Some(e));
            }
        }
    }
}
