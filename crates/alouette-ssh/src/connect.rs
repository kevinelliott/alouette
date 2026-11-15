//! Connect, check the host key, authenticate, and hop through jump hosts.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alouette_net::Net;
use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, MethodKind, MethodSet};

use crate::config::{self, HostConfig, SshConfig};

/// A question for the person: a password, a passphrase, or whether to trust a host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub title: String,
    pub body: Vec<String>,
    pub prompt: String,
    pub secret: bool,
    pub yes_no: bool,
}

/// How the SSH code talks to the person. The terminal shows a dialog; tests answer directly.
pub trait Prompter: Send + Sync {
    /// None means the person cancelled.
    fn ask(&self, q: Ask) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>;
    /// Progress worth showing, such as "Authenticated with the agent".
    fn note(&self, text: &str);
}

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("{0}")]
    Net(#[from] alouette_net::NetError),
    #[error("SSH: {0}")]
    Ssh(#[from] russh::Error),
    #[error(
        "the host key for {0} has CHANGED since you last connected. This can mean someone is \
         intercepting the connection. If the server was reinstalled, remove line {1} of {2}"
    )]
    KeyChanged(String, usize, String),
    #[error("you did not trust the host key for {0}")]
    KeyRejected(String),
    #[error("{0}")]
    Auth(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

/// Settings that are not in ssh_config.
#[derive(Debug, Clone)]
pub struct Options {
    pub agent: bool,
    pub keepalive_secs: u64,
    pub connect_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self { agent: true, keepalive_secs: 30, connect_timeout: Duration::from_secs(20) }
    }
}

/// The russh handler: checks host keys against known_hosts and asks about new ones.
pub struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    strict: Option<String>,
    prompter: Arc<dyn Prompter>,
    problem: Arc<Mutex<Option<SshError>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: PublicKey = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(c) => PublicKey::new(c.public_key().clone(), ""),
        };
        let path = &self.known_hosts;
        match russh::keys::check_known_hosts_path(&self.host, self.port, &key, path) {
            Ok(true) => return Ok(true),
            Err(russh::keys::Error::KeyChanged { line }) => {
                *self.problem.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(SshError::KeyChanged(self.host.clone(), line, path.display().to_string()));
                return Ok(false);
            }
            _ => {}
        }
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let trust = match self.strict.as_deref() {
            Some("accept-new") | Some("no") | Some("off") => true,
            Some("yes") => false,
            _ => {
                let name = if self.port == 22 { self.host.clone() } else { format!("[{}]:{}", self.host, self.port) };
                self.prompter
                    .ask(Ask {
                        title: "New host".into(),
                        body: vec![
                            format!("This is the first time you have connected to {name}."),
                            format!("Its {} key fingerprint is", key.algorithm().as_str()),
                            fp.clone(),
                            "Check it with the server's owner if you can.".into(),
                        ],
                        prompt: "Trust this host and remember it?".into(),
                        secret: false,
                        yes_no: true,
                    })
                    .await
                    .is_some()
            }
        };
        if !trust {
            *self.problem.lock().unwrap_or_else(|e| e.into_inner()) = Some(SshError::KeyRejected(self.host.clone()));
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, path) {
            self.prompter.note(&format!("Could not save the host key to {}: {e}", path.display()));
        } else {
            self.prompter.note(&format!("Saved {} to {}", fp, path.display()));
        }
        Ok(true)
    }
}

/// A connected, authenticated SSH session.
pub struct Session {
    pub handle: Handle<Client>,
    /// Jump host sessions, kept alive for as long as this one.
    _jumps: Vec<Handle<Client>>,
    pub host: HostConfig,
    pub user: String,
    /// Such as `SSH · IPv6 · via bastion`.
    pub badge: String,
}

pub fn default_known_hosts() -> PathBuf {
    config::home().unwrap_or_default().join(".ssh").join("known_hosts")
}

/// Resolve `dest` (an alias, `host`, `user@host:port` or `ssh://…`) through ssh_config.
pub fn resolve(cfg: &SshConfig, dest: &str) -> (HostConfig, String) {
    let (user, host, port) = config::parse_destination(dest);
    let mut h = cfg.resolve(&host);
    if let Some(p) = port {
        h.port = p;
    }
    let user = user.or_else(|| h.user.clone()).unwrap_or_else(config::local_user);
    (h, user)
}

/// Connect to `dest`, through its jump hosts, and authenticate.
pub async fn connect(
    net: &Net,
    cfg: &SshConfig,
    dest: &str,
    opts: &Options,
    prompter: Arc<dyn Prompter>,
) -> Result<Session, SshError> {
    let (host, user) = resolve(cfg, dest);
    if host.jumps.len() > 8 {
        return Err(SshError::Other("too many jump hosts".into()));
    }
    let mut jumps: Vec<Handle<Client>> = vec![];
    let mut first_badge = String::new();
    // Each hop's ProxyJump is ignored, as OpenSSH does for hops named in a ProxyJump list.
    for hop in &host.jumps {
        let (hc, hu) = resolve(cfg, hop);
        prompter.note(&format!("Connecting to jump host {} as {hu}", hc.hostname));
        let handle = match jumps.last() {
            None => {
                let (h, b) = dial(net, &hc, opts, prompter.clone()).await?;
                first_badge = b;
                h
            }
            Some(prev) => tunnel(prev, &hc, opts, prompter.clone()).await?,
        };
        let mut handle = handle;
        authenticate(&mut handle, &hu, &hc, opts, prompter.as_ref()).await?;
        jumps.push(handle);
    }
    let mut handle = match jumps.last() {
        None => {
            let (h, b) = dial(net, &host, opts, prompter.clone()).await?;
            first_badge = b;
            h
        }
        Some(prev) => tunnel(prev, &host, opts, prompter.clone()).await?,
    };
    authenticate(&mut handle, &user, &host, opts, prompter.as_ref()).await?;
    let mut badge = format!("SSH · {first_badge}");
    if let Some(j) = host.jumps.first() {
        badge.push_str(&format!(" · via {j}"));
    }
    Ok(Session { handle, _jumps: jumps, host, user, badge })
}

fn client_config(host: &HostConfig, opts: &Options) -> Arc<client::Config> {
    let keepalive = host.keepalive_secs.unwrap_or(opts.keepalive_secs);
    Arc::new(client::Config {
        keepalive_interval: (keepalive > 0).then(|| Duration::from_secs(keepalive)),
        keepalive_max: 4,
        nodelay: true,
        ..client::Config::default()
    })
}

fn handler(host: &HostConfig, prompter: Arc<dyn Prompter>) -> (Client, Arc<Mutex<Option<SshError>>>) {
    let problem = Arc::new(Mutex::new(None));
    (
        Client {
            host: host.hostname.clone(),
            port: host.port,
            known_hosts: host.known_hosts.clone().unwrap_or_else(default_known_hosts),
            strict: host.strict.clone(),
            prompter,
            problem: problem.clone(),
        },
        problem,
    )
}

fn explain(e: russh::Error, problem: &Mutex<Option<SshError>>) -> SshError {
    problem.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or(SshError::Ssh(e))
}

/// Connect over TCP, through Alouette's routes and proxies.
async fn dial(
    net: &Net,
    host: &HostConfig,
    opts: &Options,
    prompter: Arc<dyn Prompter>,
) -> Result<(Handle<Client>, String), SshError> {
    let conn = net.tcp(&host.hostname, host.port).await?;
    let badge = conn.info.badge().replace(" · plain", "").replace("plain", "");
    let (h, problem) = handler(host, prompter);
    let fut = client::connect_stream(client_config(host, opts), conn.stream, h);
    match tokio::time::timeout(opts.connect_timeout, fut).await {
        Err(_) => Err(SshError::Other(format!("{} did not answer the SSH handshake", host.hostname))),
        Ok(Err(e)) => Err(explain(e, &problem)),
        Ok(Ok(handle)) => Ok((handle, badge)),
    }
}

/// Connect through an earlier session, as `ssh -J` does.
async fn tunnel(
    prev: &Handle<Client>,
    host: &HostConfig,
    opts: &Options,
    prompter: Arc<dyn Prompter>,
) -> Result<Handle<Client>, SshError> {
    let ch = prev.channel_open_direct_tcpip(host.hostname.clone(), u32::from(host.port), "127.0.0.1", 0).await?;
    let (h, problem) = handler(host, prompter);
    client::connect_stream(client_config(host, opts), ch.into_stream(), h).await.map_err(|e| explain(e, &problem))
}

fn allows(methods: &Option<MethodSet>, kind: MethodKind) -> bool {
    methods.as_ref().is_none_or(|m| m.contains(&kind))
}

fn remaining(r: russh::client::AuthResult) -> Result<(), Option<MethodSet>> {
    match r {
        russh::client::AuthResult::Success => Ok(()),
        russh::client::AuthResult::Failure { remaining_methods, .. } => Err(Some(remaining_methods)),
    }
}

/// Try the agent, then key files, then keyboard-interactive, then a password.
pub async fn authenticate(
    handle: &mut Handle<Client>,
    user: &str,
    host: &HostConfig,
    opts: &Options,
    prompter: &dyn Prompter,
) -> Result<(), SshError> {
    let mut methods: Option<MethodSet> = None;
    let rsa_hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();

    if opts.agent && allows(&methods, MethodKind::PublicKey) {
        match crate::agent::try_agent(handle, user, rsa_hash, host).await {
            Ok(true) => {
                prompter.note("Signed in with a key from your SSH agent");
                return Ok(());
            }
            Ok(false) => {}
            Err(e) => tracing::debug!("agent: {e}"),
        }
    }

    // Key files: the configured ones, else OpenSSH's defaults.
    let mut files = host.identity_files.clone();
    if files.is_empty() {
        let dir = config::home().unwrap_or_default().join(".ssh");
        files = ["id_ed25519", "id_ecdsa", "id_rsa"].iter().map(|n| dir.join(n)).collect();
    }
    for f in files.iter().filter(|f| f.exists()) {
        if !allows(&methods, MethodKind::PublicKey) {
            break;
        }
        let key = match russh::keys::load_secret_key(f, None) {
            Ok(k) => k,
            Err(russh::keys::Error::KeyIsEncrypted) => {
                let Some(pass) = prompter
                    .ask(Ask {
                        title: "Key passphrase".into(),
                        body: vec![format!("{} is protected by a passphrase.", f.display())],
                        prompt: "Passphrase".into(),
                        secret: true,
                        yes_no: false,
                    })
                    .await
                else {
                    continue;
                };
                match russh::keys::load_secret_key(f, Some(&pass)) {
                    Ok(k) => k,
                    Err(e) => {
                        prompter.note(&format!("Could not unlock {}: {e}", f.display()));
                        continue;
                    }
                }
            }
            Err(e) => {
                tracing::debug!("{}: {e}", f.display());
                continue;
            }
        };
        let r = handle.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash)).await?;
        match remaining(r) {
            Ok(()) => {
                prompter.note(&format!("Signed in with {}", f.display()));
                return Ok(());
            }
            Err(m) => methods = m,
        }
    }

    if allows(&methods, MethodKind::KeyboardInteractive) {
        let mut resp = handle.authenticate_keyboard_interactive_start(user, None).await?;
        for _ in 0..8 {
            match resp {
                KeyboardInteractiveAuthResponse::Success => return Ok(()),
                KeyboardInteractiveAuthResponse::Failure { remaining_methods, .. } => {
                    methods = Some(remaining_methods);
                    break;
                }
                KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                    let mut answers = vec![];
                    for p in &prompts {
                        let mut body = vec![];
                        if !name.is_empty() {
                            body.push(name.clone());
                        }
                        if !instructions.is_empty() {
                            body.push(instructions.clone());
                        }
                        body.push(format!("{user}@{}", host.hostname));
                        let a = prompter
                            .ask(Ask {
                                title: "Sign in".into(),
                                body,
                                prompt: p.prompt.trim().trim_end_matches(':').to_string(),
                                secret: !p.echo,
                                yes_no: false,
                            })
                            .await
                            .ok_or(SshError::Cancelled)?;
                        answers.push(a);
                    }
                    resp = handle.authenticate_keyboard_interactive_respond(answers).await?;
                }
            }
        }
    }

    if allows(&methods, MethodKind::Password) {
        for attempt in 0..3 {
            let mut body = vec![format!("{user}@{}", host.hostname)];
            if attempt > 0 {
                body.push("That password did not work.".into());
            }
            let pass = prompter
                .ask(Ask { title: "Password".into(), body, prompt: "Password".into(), secret: true, yes_no: false })
                .await
                .ok_or(SshError::Cancelled)?;
            if handle.authenticate_password(user, pass).await?.success() {
                return Ok(());
            }
        }
    }
    Err(SshError::Auth(format!("{user}@{} did not accept any of your keys or a password", host.hostname)))
}

impl Session {
    /// Open an interactive shell with a terminal.
    pub async fn shell(&self, term: &str, cols: u16, rows: u16) -> Result<russh::Channel<client::Msg>, SshError> {
        let ch = self.handle.channel_open_session().await?;
        ch.request_pty(true, term, u32::from(cols), u32::from(rows), 0, 0, &[]).await?;
        ch.request_shell(true).await?;
        Ok(ch)
    }

    /// Run one command and collect its output (stdout and stderr) and exit status.
    pub async fn exec(&self, command: &str) -> Result<(Vec<u8>, Option<u32>), SshError> {
        let mut ch = self.handle.channel_open_session().await?;
        ch.exec(true, command).await?;
        let mut out = vec![];
        let mut status = None;
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, .. } => out.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        Ok((out, status))
    }

    /// Start the SFTP subsystem.
    pub async fn sftp(&self) -> Result<russh_sftp::client::SftpSession, SshError> {
        let ch = self.handle.channel_open_session().await?;
        ch.request_subsystem(true, "sftp").await?;
        russh_sftp::client::SftpSession::new(ch.into_stream()).await.map_err(|e| SshError::Other(format!("SFTP: {e}")))
    }
}
