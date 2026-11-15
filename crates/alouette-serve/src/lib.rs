//! `alouette serve`: one Alouette server, many people, each in their own session over SSH.
//!
//! Users sign in with an SSH key listed in `users/<name>/authorized_keys` under the server's
//! folder, and each gets their own config, data, cache and sealed secrets there. A guest
//! profile (off by default) lets anyone in with a fresh, empty profile that is thrown away
//! at the end. Modules can be allowed or denied per profile, and limits cap sessions,
//! windows, idle time and guest time. Local shells, files and programs are always refused,
//! because they would run as the server's own account.

pub mod admin;
pub mod backend;
pub mod input;
pub mod registry;
pub mod secrets;
pub mod users;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use alouette_core::config::Serve;
use alouette_core::{Config, ConfigWatcher, Notification, Paths};
use alouette_store::Store;
use alouette_store::secrets::{MemoryStore, SecretStore};
use alouette_ui::theme::{ColorMode, ThemeSet};
use alouette_ui::window::{Cx, Window};
use alouette_ui::{App, AppOptions, Launcher, Module};
use crossterm::event::Event;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Server as _, Session as SshSession};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::backend::{ChannelWriter, SharedSize, SshBackend};
use crate::registry::{Registry, Session};
use crate::users::{GUEST, Users};

/// Builds a session's modules for its folders.
pub type ModuleFactory = Arc<dyn Fn(&Paths) -> Vec<Box<dyn Module>> + Send + Sync>;

pub struct ServeOptions {
    pub config: Serve,
    /// The server's folder: host key, users, guests.
    pub root: PathBuf,
    pub modules: ModuleFactory,
    pub local_offset: time::UtcOffset,
}

/// A started server.
pub struct Running {
    pub addr: SocketAddr,
    pub registry: Arc<Registry>,
    pub host_key: PublicKey,
}

struct Shared {
    opts: ServeOptions,
    users: Users,
    registry: Arc<Registry>,
    master: [u8; 32],
}

/// Load the host key, making one on first start.
pub fn host_key(root: &Path) -> Result<PrivateKey, String> {
    let p = root.join("host_ed25519");
    if let Ok(text) = std::fs::read_to_string(&p) {
        return PrivateKey::from_openssh(&text).map_err(|e| format!("{}: {e}", p.display()));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
    let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    let text = key.to_openssh(russh::keys::ssh_key::LineEnding::LF).map_err(|e| e.to_string())?;
    secrets::write_private(&p, text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(key)
}

/// Start listening. Returns once the socket is bound; sessions run in the background.
pub async fn start(opts: ServeOptions, listen: &str) -> Result<Running, String> {
    alouette_core::shared::set_restricted(true);
    alouette_core::shared::set_private_ok(opts.config.allow_private);
    std::fs::create_dir_all(&opts.root).map_err(|e| format!("{}: {e}", opts.root.display()))?;
    // Guests from a previous run that ended badly.
    let _ = std::fs::remove_dir_all(opts.root.join("guests"));
    let key = host_key(&opts.root)?;
    let public = key.public_key().clone();
    let master = secrets::master_key(&opts.root).map_err(|e| e.to_string())?;
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::PublicKey);
    if opts.config.guest {
        methods.push(MethodKind::None);
    }
    let config = Arc::new(server::Config {
        keys: vec![key],
        methods,
        auth_rejection_time: Duration::from_millis(300),
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(Duration::from_secs(3600 * 12)),
        keepalive_interval: Some(Duration::from_secs(60)),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(listen).await.map_err(|e| format!("{listen}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let registry = Arc::new(Registry::new(opts.config.clone()));
    let users = Users::new(&opts.root);
    let shared = Arc::new(Shared { opts, users, registry: registry.clone(), master });
    tokio::spawn(async move {
        let mut s = Server { shared };
        if let Err(e) = s.run_on_socket(config, &listener).await {
            tracing::error!("serve stopped: {e}");
        }
    });
    Ok(Running { addr, registry, host_key: public })
}

struct Server {
    shared: Arc<Shared>,
}

impl server::Server for Server {
    type Handler = Conn;
    fn new_client(&mut self, addr: Option<SocketAddr>) -> Conn {
        Conn {
            shared: self.shared.clone(),
            addr,
            user: None,
            guest: false,
            chans: HashMap::new(),
            live: HashMap::new(),
        }
    }
}

/// What a session channel has asked for before its shell starts.
struct Pending {
    channel: Option<Channel<Msg>>,
    term: String,
    size: (u16, u16),
    truecolor: bool,
    pty: bool,
}

struct Live {
    input: mpsc::UnboundedSender<std::io::Result<Event>>,
    size: SharedSize,
}

struct Conn {
    shared: Arc<Shared>,
    addr: Option<SocketAddr>,
    user: Option<String>,
    guest: bool,
    chans: HashMap<ChannelId, Pending>,
    live: HashMap<ChannelId, Live>,
}

impl server::Handler for Conn {
    type Error = russh::Error;

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        if self.shared.opts.config.guest && user == GUEST {
            self.user = Some(GUEST.into());
            self.guest = true;
            return Ok(Auth::Accept);
        }
        Ok(Auth::reject())
    }

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: server::PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        if self.shared.users.allows(user, key.public_key) { Ok(Auth::Accept) } else { Ok(Auth::reject()) }
    }

    async fn auth_publickey(&mut self, user: &str, key: server::VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        if self.shared.users.allows(user, key.public_key) {
            self.user = Some(user.to_string());
            self.guest = false;
            tracing::info!("serve: {user} signed in from {:?}", self.addr);
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        self.chans.insert(
            channel.id(),
            Pending { channel: Some(channel), term: "xterm".into(), size: (80, 24), truecolor: false, pty: false },
        );
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if let Some(p) = self.chans.get_mut(&channel) {
            p.term = term.chars().take(64).collect();
            p.size = (cols.clamp(20, 1000) as u16, rows.clamp(8, 500) as u16);
            p.pty = true;
        }
        session.channel_success(channel)
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if name == "COLORTERM"
            && (value.contains("truecolor") || value.contains("24bit"))
            && let Some(p) = self.chans.get_mut(&channel)
        {
            p.truecolor = true;
        }
        session.channel_success(channel)
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut SshSession) -> Result<(), Self::Error> {
        let Some(user) = self.user.clone() else { return session.channel_failure(channel) };
        let Some(mut p) = self.chans.remove(&channel) else { return session.channel_failure(channel) };
        let Some(ch) = p.channel.take() else { return session.channel_failure(channel) };
        session.channel_success(channel)?;
        let handle = session.handle();
        if !p.pty {
            let _ = handle.data(channel, &b"Alouette needs a terminal: connect with ssh -t.\r\n"[..]).await;
            let _ = handle.close(channel).await;
            return Ok(());
        }
        let size: SharedSize = Arc::new(Mutex::new(p.size));
        let sess = match self.shared.registry.join(&user, self.guest, self.addr, size.clone()) {
            Ok(s) => s,
            Err(why) => {
                let _ = handle.data(channel, format!("{why}\r\n").into_bytes()).await;
                let _ = handle.eof(channel).await;
                let _ = handle.close(channel).await;
                return Ok(());
            }
        };
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        self.live.insert(channel, Live { input: in_tx.clone(), size: size.clone() });
        let (mut rd, mut wr) = tokio::io::split(ch.into_stream());
        // Client to app.
        let s2 = sess.clone();
        tokio::spawn(async move {
            let mut parser = input::Parser::new();
            let mut buf = vec![0u8; 8192];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        s2.kill("Disconnected.");
                        break;
                    }
                    Ok(n) => {
                        s2.touch();
                        for ev in parser.feed(&buf[..n]) {
                            if in_tx.send(Ok(ev)).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
        // App to client; when the app is done, close the channel.
        let h2 = handle.clone();
        tokio::spawn(async move {
            while let Some(b) = out_rx.recv().await {
                if wr.write_all(&b).await.is_err() {
                    break;
                }
                let _ = wr.flush().await;
            }
            let _ = h2.eof(channel).await;
            let _ = h2.close(channel).await;
        });
        let shared = self.shared.clone();
        let wire = Wire { input: in_rx, out: out_tx, size, term: p.term, truecolor: p.truecolor };
        std::thread::Builder::new()
            .name(format!("serve-{}", sess.user))
            .spawn(move || {
                let id = sess.id;
                let user = sess.user.clone();
                let guest_dir = sess.guest.then(|| shared.opts.root.join("guests").join(id.to_string()));
                // A fault in one session must not end everyone else's.
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_session(&shared, sess, wire)));
                if run.is_err() {
                    tracing::error!("serve: {user}'s session {id} hit a fault and was closed");
                }
                if let Some(d) = guest_dir {
                    let _ = std::fs::remove_dir_all(d);
                }
                shared.registry.leave(id);
                tracing::info!("serve: {user}'s session {id} ended");
            })
            .map_err(|e| russh::Error::IO(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, &b"This Alouette server offers an interactive session only: ssh -t.\r\n"[..])?;
        session.exit_status_request(channel, 1)?;
        session.eof(channel)?;
        session.close(channel)
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if let Some(l) = self.live.get(&channel) {
            let (c, r) = (cols.clamp(20, 1000) as u16, rows.clamp(8, 500) as u16);
            *l.size.lock().unwrap_or_else(|e| e.into_inner()) = (c, r);
            let _ = l.input.send(Ok(Event::Resize(c, r)));
        }
        Ok(())
    }
}

struct Wire {
    input: mpsc::UnboundedReceiver<std::io::Result<Event>>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    size: SharedSize,
    term: String,
    truecolor: bool,
}

fn color_mode(term: &str, truecolor: bool) -> ColorMode {
    let t = term.to_ascii_lowercase();
    if truecolor || t.contains("direct") || t.contains("kitty") || t.contains("alacritty") || t.contains("ghostty") {
        ColorMode::TrueColor
    } else if t == "linux" || t == "vt100" || t == "vt220" || t == "ansi" {
        ColorMode::Ansi16
    } else if t == "dumb" {
        ColorMode::Mono
    } else {
        ColorMode::Ansi256
    }
}

/// Which modules a profile may open.
pub fn allowed(cfg: &Serve, id: &str, guest: bool) -> bool {
    if id == "core" {
        return true;
    }
    if guest {
        return cfg.guest_modules.iter().any(|m| m == id);
    }
    !cfg.deny.iter().any(|m| m == id) && (cfg.modules.is_empty() || cfg.modules.iter().any(|m| m == id))
}

/// Answers for the schemes of modules a profile may not use.
struct Blocked {
    schemes: &'static [&'static str],
}

fn blocked_schemes(list: Vec<&'static str>) -> &'static [&'static str] {
    // One leaked list per distinct set, not per session.
    static CACHE: OnceLock<Mutex<HashMap<Vec<&'static str>, &'static [&'static str]>>> = OnceLock::new();
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = c.get(&list) {
        return s;
    }
    let leaked: &'static [&'static str] = Box::leak(list.clone().into_boxed_slice());
    c.insert(list, leaked);
    leaked
}

impl Module for Blocked {
    fn id(&self) -> &'static str {
        "blocked"
    }
    fn name(&self) -> &'static str {
        "Turned off"
    }
    fn description(&self) -> &'static str {
        "Modules this server does not offer"
    }
    fn schemes(&self) -> &'static [&'static str] {
        self.schemes
    }
    fn launchers(&self) -> Vec<Launcher> {
        Vec::new()
    }
    fn open(&mut self, url: &url::Url, _cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        Err(format!("{}: is turned off on this server", url.scheme()))
    }
}

fn run_session(shared: &Arc<Shared>, sess: Arc<Session>, wire: Wire) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("serve: no runtime for a session: {e}");
            return;
        }
    };
    let guest_dir = shared.opts.root.join("guests").join(sess.id.to_string());
    rt.block_on(session_main(shared, &sess, wire, &guest_dir));
    // Give modules a moment to close their connections, then drop everything.
    rt.shutdown_timeout(Duration::from_secs(2));
}

async fn session_main(shared: &Arc<Shared>, sess: &Arc<Session>, wire: Wire, guest_dir: &Path) {
    let cfg = &shared.opts.config;
    let Wire { mut input, out, size, term, truecolor } = wire;
    let dir = if sess.guest { guest_dir.to_path_buf() } else { shared.users.dir(&sess.user) };
    let paths = Paths::under(&dir);
    let _ = paths.ensure();
    let config_path = paths.config_file();
    let mut note = None;
    if !sess.guest {
        let _ = Config::write_default_if_missing(&config_path);
    }
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) if config_path.exists() => {
            note = Some(format!("config: {e}. Using defaults."));
            Config::default()
        }
        Err(_) => Config::default(),
    };
    let secrets: Box<dyn SecretStore> = if sess.guest {
        Box::new(MemoryStore::default())
    } else {
        match secrets::SealedStore::open(dir.join("secrets.sealed"), shared.master) {
            Ok(s) => Box::new(s),
            Err(e) => {
                note = Some(format!("Saved passwords could not be opened ({e}); they last for this session only."));
                Box::new(MemoryStore::default())
            }
        }
    };
    let store = if sess.guest { Store::in_memory().ok() } else { Store::open(&paths.state_db()).ok() };
    let admin = !sess.guest && cfg.admins.iter().any(|a| a == &sess.user);
    let mut modules = Vec::new();
    let mut blocked = Vec::new();
    for m in (shared.opts.modules)(&paths) {
        if allowed(cfg, m.id(), sess.guest) {
            modules.push(m);
        } else {
            blocked.extend(m.schemes().iter().copied());
        }
    }
    if admin {
        modules.push(Box::new(admin::AdminModule::new(shared.registry.clone(), sess.id)));
    } else {
        blocked.push("serve");
    }
    blocked.sort_unstable();
    blocked.dedup();
    modules.push(Box::new(Blocked { schemes: blocked_schemes(blocked) }));

    let opts = AppOptions {
        config,
        config_path: config_path.clone(),
        themes: ThemeSet::load(&paths.themes_dir()),
        store,
        secrets,
        modules,
        color_mode: color_mode(&term, truecolor),
        show_splash: true,
        local_offset: shared.opts.local_offset,
        deterministic: false,
        max_windows: Some(if sess.guest { cfg.guest_max_windows } else { cfg.max_windows }.max(1)),
        window_count: Some(sess.window_counter()),
    };
    let (mut app, events) = App::new(opts);
    let (w, h) = *size.lock().unwrap_or_else(|e| e.into_inner());
    app.set_screen(ratatui::layout::Rect::new(0, 0, w, h));
    let welcome = if sess.guest {
        format!("Welcome, guest. This profile is thrown away when you leave ({} minutes at most).", cfg.guest_minutes)
    } else {
        format!("Welcome, {}.", sess.user)
    };
    app.notify(Notification::info(if cfg.banner.is_empty() { welcome } else { format!("{welcome} {}", cfg.banner) }));
    if let Some(n) = note {
        app.notify(Notification::warning(n));
    }
    if !sess.guest && cfg.quota_mb > 0 {
        let used = shared.users.usage(&sess.user) / (1024 * 1024);
        if used > cfg.quota_mb {
            app.notify(Notification::warning(format!(
                "Your folder holds {used} MB, more than the {} MB this server allows. Please tidy up.",
                cfg.quota_mb
            )));
        }
    }
    let urls = app.config.general.startup.clone();
    app.open_all(&urls);
    let _watcher = (!sess.guest).then(|| ConfigWatcher::start(config_path.clone(), app.sender()).ok()).flatten();

    let mut writer = ChannelWriter::new(out.clone());
    let mouse = app.config.general.mouse;
    {
        use crossterm::{event as ev, execute, terminal};
        let _ = execute!(writer, terminal::EnterAlternateScreen, ev::EnableBracketedPaste);
        if mouse {
            let _ = execute!(writer, ev::EnableMouseCapture);
        }
    }
    let mut terminal = match ratatui::Terminal::new(SshBackend::new(writer, size.clone())) {
        Ok(t) => t,
        Err(_) => return,
    };
    let _ = terminal.clear();
    let stream = futures::stream::poll_fn(move |cx| input.poll_recv(cx));
    let limit_check = async {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            if cfg.idle_minutes > 0 && sess.idle() >= cfg.idle_minutes * 60 {
                return format!("Disconnected after {} minutes without a key press.", cfg.idle_minutes);
            }
            if sess.guest && cfg.guest_minutes > 0 && sess.on_for() >= cfg.guest_minutes * 60 {
                return format!("Guest sessions last {} minutes. Thanks for visiting.", cfg.guest_minutes);
            }
        }
    };
    let why: Option<String> = tokio::select! {
        r = alouette_ui::run::run_with(&mut terminal, &mut app, events, stream) => r.err().map(|e| e.to_string()),
        _ = sess.killed() => sess.reason(),
        w = limit_check => Some(w),
    };
    app.shutdown();
    drop(app);
    let _ = terminal.flush();
    drop(terminal);
    let mut w = ChannelWriter::new(out.clone());
    let w = &mut w;
    {
        use crossterm::{cursor, event as ev, execute, terminal};
        let _ = execute!(
            w,
            ev::DisableMouseCapture,
            ev::DisableBracketedPaste,
            terminal::LeaveAlternateScreen,
            cursor::Show
        );
    }
    use std::io::Write;
    let _ = write!(w, "{}\r\n", why.unwrap_or_else(|| "Goodbye from Alouette.".into()));
    let _ = w.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy() {
        let cfg = Serve::default();
        assert!(allowed(&cfg, "core", true));
        assert!(allowed(&cfg, "mail", false));
        assert!(!allowed(&cfg, "terminal", false));
        assert!(!allowed(&cfg, "mail", true));
        assert!(allowed(&cfg, "burrow", true));
        let only = Serve { modules: vec!["burrow".into()], ..Serve::default() };
        assert!(!allowed(&only, "mail", false));
        assert_eq!(color_mode("xterm-256color", false), ColorMode::Ansi256);
        assert_eq!(color_mode("xterm-256color", true), ColorMode::TrueColor);
        let a = blocked_schemes(vec!["a", "b"]);
        assert!(std::ptr::eq(a, blocked_schemes(vec!["a", "b"])));
    }
}
