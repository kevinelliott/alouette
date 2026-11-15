//! The `alouette.toml` model.
//!
//! Every field has a default, so an empty or missing file is valid. Unknown keys are
//! rejected with a clear message, which catches typos such as `[nostalgia] splashh = true`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

use crate::event::{AppEvent, EventSender};

/// The three ways to arrange windows. All three show the same windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LayoutKind {
    /// Minuet style: menu bar, overlapping windows, mouse.
    #[default]
    Desktop,
    /// Splits that fill the screen.
    Tiled,
    /// One full-screen window per tab.
    Tabs,
}

impl LayoutKind {
    pub const ALL: [LayoutKind; 3] = [LayoutKind::Desktop, LayoutKind::Tiled, LayoutKind::Tabs];

    pub fn name(self) -> &'static str {
        match self {
            LayoutKind::Desktop => "desktop",
            LayoutKind::Tiled => "tiled",
            LayoutKind::Tabs => "tabs",
        }
    }

    pub fn next(self) -> Self {
        match self {
            LayoutKind::Desktop => LayoutKind::Tiled,
            LayoutKind::Tiled => LayoutKind::Tabs,
            LayoutKind::Tabs => LayoutKind::Desktop,
        }
    }
}

impl std::str::FromStr for LayoutKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "desktop" => Ok(LayoutKind::Desktop),
            "tiled" | "tile" => Ok(LayoutKind::Tiled),
            "tabs" | "tab" => Ok(LayoutKind::Tabs),
            other => Err(format!("unknown layout `{other}` (expected desktop, tiled or tabs)")),
        }
    }
}

/// Built-in key binding sets. Individual keys can be overridden in `[keys]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KeymapPreset {
    /// F-keys, Alt menus and Ctrl shortcuts, as in Minuet and Turbo Vision.
    #[default]
    Cua,
    Vim,
    Emacs,
}

impl KeymapPreset {
    pub const ALL: [KeymapPreset; 3] = [KeymapPreset::Cua, KeymapPreset::Vim, KeymapPreset::Emacs];

    pub fn name(self) -> &'static str {
        match self {
            KeymapPreset::Cua => "cua",
            KeymapPreset::Vim => "vim",
            KeymapPreset::Emacs => "emacs",
        }
    }
}

impl std::str::FromStr for KeymapPreset {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cua" | "minuet" => Ok(KeymapPreset::Cua),
            "vim" => Ok(KeymapPreset::Vim),
            "emacs" => Ok(KeymapPreset::Emacs),
            other => Err(format!("unknown keymap `{other}` (expected cua, vim or emacs)")),
        }
    }
}

/// Window border drawing style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BorderStyle {
    /// Use whatever the theme asks for.
    #[default]
    Theme,
    Rounded,
    Plain,
    Double,
    Thick,
    Ascii,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub layout: LayoutKind,
    pub theme: String,
    pub keymap: KeymapPreset,
    pub border: BorderStyle,
    /// Enable mouse capture. Turn off to let the terminal handle text selection.
    pub mouse: bool,
    /// Windows to open on startup, as URLs.
    pub startup: Vec<String>,
    /// Show the clock in the status line.
    pub clock: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            layout: LayoutKind::Desktop,
            theme: "minuet".into(),
            keymap: KeymapPreset::Cua,
            border: BorderStyle::Theme,
            mouse: true,
            startup: vec!["about:welcome".into()],
            clock: true,
        }
    }
}

/// Retro touches. Every switch is separate, and `enabled = false` turns them all off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Nostalgia {
    pub enabled: bool,
    /// ASCII-art splash with a fake memory check on startup.
    pub splash: bool,
    /// Longest the splash may stay up, in milliseconds.
    pub splash_ms: u64,
    /// Use Minuet's names in menus: POPmail, NetNews, Gopher, Telnet.
    pub minuet_names: bool,
    /// Drop shadows under windows and menus in the Desktop layout.
    pub shadows: bool,
    /// Fill the desktop background with the DOS-style shaded pattern.
    pub desktop_pattern: bool,
    /// Play sounds (modem handshake, new-mail chime). Off by default.
    pub sounds: bool,
    /// Retro commands such as `archie`, `veronica` and `minuet` in the palette.
    pub easter_eggs: bool,
}

impl Default for Nostalgia {
    fn default() -> Self {
        Self {
            enabled: true,
            splash: true,
            splash_ms: 1500,
            minuet_names: false,
            shadows: true,
            desktop_pattern: true,
            sounds: false,
            easter_eggs: true,
        }
    }
}

impl Nostalgia {
    pub fn splash(&self) -> bool {
        self.enabled && self.splash
    }
    pub fn minuet_names(&self) -> bool {
        self.enabled && self.minuet_names
    }
    pub fn shadows(&self) -> bool {
        self.enabled && self.shadows
    }
    pub fn desktop_pattern(&self) -> bool {
        self.enabled && self.desktop_pattern
    }
    pub fn sounds(&self) -> bool {
        self.enabled && self.sounds
    }
    pub fn easter_eggs(&self) -> bool {
        self.enabled && self.easter_eggs
    }
}

/// The optional offline cache. Off by default; each account can opt in later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cache {
    pub enabled: bool,
    pub max_mb: u64,
    pub max_age_days: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self { enabled: false, max_mb: 2048, max_age_days: 365 }
    }
}

/// AI features. Nothing is sent anywhere until `enabled = true` and a provider is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Ai {
    pub enabled: bool,
    /// Provider id, such as `claude-cli`, `ollama` or `anthropic`.
    pub provider: Option<String>,
}

/// Network behavior shared by every module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
    /// Prefer IPv6 when both families are available (Happy Eyeballs still races both).
    pub prefer_ipv6: bool,
    /// `system`, `doh`, `dot` or `doq`.
    pub dns: String,
    /// For encrypted DNS: `cloudflare`, `quad9`, `google`, or `IP#name` such as
    /// `9.9.9.9#dns.quad9.net`.
    pub dns_server: Option<String>,
    /// Default proxy for everything: `socks5://host:port`, `socks5h://host:port` (the proxy
    /// resolves names, as Tor needs) or `http://host:port`. Unset means direct.
    pub proxy: Option<String>,
    /// Connect timeout in seconds.
    pub connect_timeout_secs: u64,
    /// Lowest TLS version to accept: `1.2` or `1.3`.
    pub tls_min: String,
    /// Per-host routing rules, checked in order before `proxy`.
    pub routes: Vec<Route>,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            prefer_ipv6: true,
            dns: "system".into(),
            dns_server: None,
            proxy: None,
            connect_timeout_secs: 15,
            tls_min: "1.2".into(),
            routes: vec![],
        }
    }
}

/// Send some hosts a different way, for example `.onion` names through Tor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Host patterns: `example.org`, `*.example.org`, `*.onion` or `*`.
    pub hosts: Vec<String>,
    /// `direct`, or a proxy URL as in `proxy`.
    pub via: String,
}

/// The terminal emulator used by local shells, SSH, Telnet and Mosh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Terminal {
    /// Program for local shells. Unset uses `$SHELL` (or `%COMSPEC%` on Windows).
    pub shell: Option<String>,
    /// Lines kept for scrolling back.
    pub scrollback: usize,
    /// What `TERM` says to the remote side.
    pub term: String,
}

impl Default for Terminal {
    fn default() -> Self {
        Self { shell: None, scrollback: 10_000, term: "xterm-256color".into() }
    }
}

/// SSH client settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ssh {
    /// Read hosts from `~/.ssh/config`.
    pub import_config: bool,
    /// Try keys held by the SSH agent first.
    pub agent: bool,
    /// Seconds between keepalives. 0 turns them off.
    pub keepalive_secs: u64,
    /// Hosts saved in Alouette, in addition to `~/.ssh/config`.
    pub hosts: Vec<SshHost>,
}

impl Default for Ssh {
    fn default() -> Self {
        Self { import_config: true, agent: true, keepalive_secs: 30, hosts: vec![] }
    }
}

/// A saved SSH host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SshHost {
    /// What you type: `ssh://name`.
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// Private key file.
    pub identity: Option<String>,
    /// Jump hosts, as in `ProxyJump`: `user@bastion:22,other`.
    pub jump: Option<String>,
    /// Local forwards, as in `-L`: `8080:localhost:80`.
    pub forward: Vec<String>,
}

/// The Files module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Files {
    /// Show a README when you open a folder that has one, as Minuet's FTP client did.
    pub readme_viewer: bool,
    pub show_hidden: bool,
    /// Transfers that run at the same time.
    pub parallel_transfers: usize,
    /// Saved sites: SFTP, FTP, FTPS, WebDAV and S3.
    pub sites: Vec<Site>,
}

impl Default for Files {
    fn default() -> Self {
        Self { readme_viewer: true, show_hidden: false, parallel_transfers: 3, sites: vec![] }
    }
}

/// A saved file site. Passwords and keys live in the secret store, not here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Site {
    pub name: String,
    /// `sftp://user@host/path`, `ftp://`, `ftps://`, `dav://`, `davs://` or
    /// `s3://bucket/prefix`.
    pub url: String,
    /// For S3: the endpoint, such as `https://s3.us-east-1.amazonaws.com` or a MinIO URL.
    pub endpoint: Option<String>,
    /// For S3: the region used for signing.
    pub region: Option<String>,
}

/// The Mail module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mail {
    /// Keep copies of messages on disk for reading offline. Off: mail stays on the server.
    pub offline_cache: bool,
    /// Load remote images in HTML mail. Off, so senders cannot tell when you read.
    pub remote_images: bool,
    /// Check for new mail this often when the server cannot push (IDLE), in seconds.
    pub poll_secs: u64,
    pub accounts: Vec<MailAccount>,
}

impl Default for Mail {
    fn default() -> Self {
        Self { offline_cache: false, remote_images: false, poll_secs: 300, accounts: vec![] }
    }
}

/// The News module: Usenet over NNTP.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct News {
    /// Your name and address on articles you post.
    pub name: Option<String>,
    pub email: Option<String>,
    /// The most articles to fetch from one group at a time.
    pub max_articles: u64,
    pub servers: Vec<NewsServer>,
}

impl Default for News {
    fn default() -> Self {
        Self { name: None, email: None, max_articles: 500, servers: vec![] }
    }
}

/// A news server. Passwords live in the secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct NewsServer {
    pub name: String,
    /// `host[:port]`. Port 563 (the default) uses TLS from the start; 119 uses STARTTLS
    /// when offered.
    pub host: String,
    /// The login, if the server needs one.
    pub user: Option<String>,
    /// Groups you read, in order.
    pub groups: Vec<String>,
}

/// The Feeds module: RSS, Atom and JSON Feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Feeds {
    /// Fetch feeds this often while the window is open, in minutes.
    pub refresh_mins: u64,
    pub subscriptions: Vec<FeedSub>,
}

impl Default for Feeds {
    fn default() -> Self {
        Self { refresh_mins: 60, subscriptions: vec![] }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct FeedSub {
    pub url: String,
    /// Shown instead of the feed's own title.
    pub title: Option<String>,
    pub folder: Option<String>,
}

/// The Social module: the fediverse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Social {
    /// Check for new posts this often while the window is open, in seconds.
    pub poll_secs: u64,
    /// Show content warnings folded until you open them.
    pub fold_content_warnings: bool,
    pub accounts: Vec<SocialAccount>,
}

impl Default for Social {
    fn default() -> Self {
        Self { poll_secs: 60, fold_content_warnings: true, accounts: vec![] }
    }
}

/// A fediverse account. The access token lives in the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SocialAccount {
    /// What to call it, such as "home".
    pub name: String,
    /// The server, such as `mastodon.social`.
    pub instance: String,
}

/// How an account connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MailKind {
    /// IMAP for reading and SMTP for sending, with a password.
    #[default]
    Imap,
    /// JMAP (Fastmail, Stalwart, Cyrus).
    Jmap,
    /// Gmail over IMAP and SMTP, signed in with Google (OAuth).
    Gmail,
    /// Outlook.com and Microsoft 365 over IMAP and SMTP, signed in with Microsoft (OAuth).
    M365,
}

/// One mail account. Passwords and tokens live in the secret store, not here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct MailAccount {
    /// What to call it, such as "work".
    pub name: String,
    /// Your address, used as the sender.
    pub email: String,
    /// Your name as recipients see it.
    pub display_name: Option<String>,
    pub kind: MailKind,
    /// The login, if not the address.
    pub user: Option<String>,
    /// IMAP server `host[:port]`. Port 993 means TLS from the start; 143 means STARTTLS.
    pub imap: Option<String>,
    /// SMTP server `host[:port]`. Port 465 means TLS from the start; 587 means STARTTLS.
    pub smtp: Option<String>,
    /// JMAP session URL, or just the host (`/.well-known/jmap` is tried).
    pub jmap: Option<String>,
    /// For Gmail and Microsoft 365: your own OAuth app's client ID (and secret, for Google).
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// Appended to messages you write.
    pub signature: Option<String>,
}

/// The whole configuration file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub nostalgia: Nostalgia,
    pub cache: Cache,
    pub ai: Ai,
    pub network: Network,
    pub terminal: Terminal,
    pub ssh: Ssh,
    pub files: Files,
    pub mail: Mail,
    pub news: News,
    pub feeds: Feeds,
    pub social: Social,
    /// Key overrides: action name to one key or a list of keys, such as
    /// `palette = "ctrl+k"` or `quit = ["alt+x", "ctrl+q"]`.
    pub keys: BTreeMap<String, KeySpec>,
}

/// One key or several keys bound to an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeySpec {
    One(String),
    Many(Vec<String>),
}

impl KeySpec {
    pub fn keys(&self) -> Vec<&str> {
        match self {
            KeySpec::One(k) => vec![k.as_str()],
            KeySpec::Many(ks) => ks.iter().map(String::as_str).collect(),
        }
    }
}

/// A config file problem, with the path so the message makes sense on its own.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("could not write {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("{path} is not valid: {message}")]
    Parse { path: PathBuf, message: String },
}

/// The file written on first run: every default, with comments.
pub const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

impl Config {
    /// Parse config text. `path` is only used in error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text)
            .map_err(|e| ConfigError::Parse { path: path.to_path_buf(), message: e.to_string().trim().to_string() })
    }

    /// Load from `path`. A missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read { path: path.to_path_buf(), source }),
        }
    }

    /// Write the commented default file if nothing exists at `path` yet.
    pub fn write_default_if_missing(path: &Path) -> Result<bool, ConfigError> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| ConfigError::Write { path: path.to_path_buf(), source })?;
        }
        std::fs::write(path, DEFAULT_CONFIG)
            .map_err(|source| ConfigError::Write { path: path.to_path_buf(), source })?;
        Ok(true)
    }

    /// Save, replacing the file. Comments in the old file are not kept.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Parse { path: path.to_path_buf(), message: e.to_string() })?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| ConfigError::Write { path: path.to_path_buf(), source })?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|source| ConfigError::Write { path: path.to_path_buf(), source })
    }
}

/// Watches the config file and sends [`AppEvent::ConfigReloaded`] or
/// [`AppEvent::ConfigError`] when it changes. Drop it to stop watching.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    pub fn start(path: PathBuf, events: EventSender) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(tx)?;
        // Watch the folder, because editors often replace the file instead of writing it.
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
        std::thread::Builder::new()
            .name("config-watch".into())
            .spawn(move || {
                let mut last: Option<Config> = Config::load(&path).ok();
                while let Ok(first) = rx.recv() {
                    let touches = |r: &notify::Result<notify::Event>| {
                        r.as_ref().map(|e| e.paths.iter().any(|p| p == &path)).unwrap_or(false)
                    };
                    let mut relevant = touches(&first);
                    // Debounce: editors write several events in a burst.
                    while let Ok(more) = rx.recv_timeout(Duration::from_millis(150)) {
                        relevant |= touches(&more);
                    }
                    if !relevant {
                        continue;
                    }
                    let event = match Config::load(&path) {
                        Ok(cfg) if last.as_ref() == Some(&cfg) => continue,
                        Ok(cfg) => {
                            last = Some(cfg.clone());
                            AppEvent::ConfigReloaded(Box::new(cfg))
                        }
                        Err(e) => AppEvent::ConfigError(e.to_string()),
                    };
                    if events.send(event).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| notify::Error::generic(&e.to_string()))?;
        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let cfg = Config::parse("", Path::new("alouette.toml")).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(!cfg.cache.enabled, "offline cache must be off by default");
        assert!(!cfg.ai.enabled, "AI must be off by default");
    }

    #[test]
    fn default_file_parses_to_defaults() {
        let cfg = Config::parse(DEFAULT_CONFIG, Path::new("alouette.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let cfg = Config::parse(
            "[general]\nlayout = \"tiled\"\n[nostalgia]\nenabled = false\n[keys]\npalette = \"ctrl+p\"\nquit = [\"alt+x\", \"ctrl+q\"]\n",
            Path::new("a.toml"),
        )
        .unwrap();
        assert_eq!(cfg.general.layout, LayoutKind::Tiled);
        assert_eq!(cfg.general.theme, "minuet");
        assert!(!cfg.nostalgia.splash());
        assert_eq!(cfg.keys["quit"].keys(), vec!["alt+x", "ctrl+q"]);
    }

    #[test]
    fn typos_are_reported_with_the_path() {
        let err =
            Config::parse("[nostalgia]\nsplashh = true\n", Path::new("/x/alouette.toml")).unwrap_err().to_string();
        assert!(err.contains("/x/alouette.toml"), "{err}");
        assert!(err.contains("splashh"), "{err}");
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("alouette-cfg-{}", std::process::id()));
        let path = dir.join("alouette.toml");
        let mut cfg = Config::default();
        cfg.general.layout = LayoutKind::Tabs;
        cfg.keys.insert("palette".into(), KeySpec::One("ctrl+p".into()));
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
        std::fs::remove_dir_all(dir).ok();
    }
}
