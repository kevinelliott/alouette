//! Terminal: Minuet's Telnet module, grown up. Local shells, SSH (with your `~/.ssh/config`,
//! agent, known hosts, jump hosts and forwards), Telnet and Mosh, each in its own window.
//!
//! URLs:
//! - `term:` a local shell; `term:/usr/bin/fish` runs that program
//! - `ssh:` the host list; `ssh://[user@]host[:port]` connects; `?L=8080:localhost:80` forwards
//! - `telnet:` and `telnet://host[:port]`
//! - `mosh:` and `mosh://[user@]host`

pub mod hosts;
pub mod ssh;

use alouette_ssh::{Options, SshConfig};
use alouette_term::{Status, Wire, local, telnet};
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

use hosts::{Entry, HostsWindow, Kind};

pub const MODULE: &str = "terminal";

pub struct TerminalModule;

impl Module for TerminalModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Terminal"
    }
    fn description(&self) -> &'static str {
        "Local shells, SSH, Telnet and Mosh"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["term", "ssh", "telnet", "mosh"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Local shell", "term:", "A shell on this computer"),
            Launcher::new("SSH", "ssh:", "Hosts from ~/.ssh/config and your saved hosts"),
            Launcher::new("Telnet", "telnet:", "As Minuet did it, VT100 and all"),
            Launcher::new("Mosh", "mosh:", "SSH that survives roaming and sleep"),
        ]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let term = cx.config.terminal.term.clone();
        match url.scheme() {
            "term" => {
                let path = url.path().trim();
                let program = if path.is_empty() || path == "/" {
                    cx.config.terminal.shell.clone().unwrap_or_else(local::default_shell)
                } else {
                    percent_decode(path)
                };
                let name = program.rsplit(['/', '\\']).next().unwrap_or(&program).to_string();
                let p2 = program.clone();
                Ok(Box::new(alouette_term::open(
                    cx,
                    MODULE,
                    format!("Shell: {name}"),
                    url.as_str(),
                    move |w| async move { local::run(w, &p2, &term).await },
                )))
            }
            "ssh" | "telnet" | "mosh" if url.host_str().is_none_or(str::is_empty) => {
                let kind = match url.scheme() {
                    "ssh" => Kind::Ssh,
                    "telnet" => Kind::Telnet,
                    _ => Kind::Mosh,
                };
                let entries = if kind == Kind::Telnet { telnet_suggestions() } else { host_entries(cx) };
                Ok(Box::new(HostsWindow::new(kind, entries)))
            }
            "ssh" => {
                let dest = destination(url);
                let forwards: Vec<String> =
                    url.query_pairs().filter(|(k, _)| k == "L").map(|(_, v)| v.into_owned()).collect();
                let job = ssh::SshJob {
                    net: cx.net.clone(),
                    config: ssh_config(cx),
                    dest: dest.clone(),
                    options: Options {
                        agent: cx.config.ssh.agent,
                        keepalive_secs: cx.config.ssh.keepalive_secs,
                        connect_timeout: std::time::Duration::from_secs(cx.config.network.connect_timeout_secs.max(5)),
                    },
                    term,
                    forwards,
                };
                Ok(Box::new(alouette_term::open(cx, MODULE, format!("SSH: {dest}"), url.as_str(), move |w| {
                    ssh::run(w, job)
                })))
            }
            "telnet" => {
                let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']).to_string();
                let port = url.port().unwrap_or(23);
                let net = cx.net.clone();
                let title = if port == 23 { format!("Telnet: {host}") } else { format!("Telnet: {host}:{port}") };
                Ok(Box::new(alouette_term::open(cx, MODULE, title, url.as_str(), move |w| async move {
                    telnet::run(w, net, &host, port, &term).await
                })))
            }
            "mosh" => {
                let dest = destination(url);
                let title = format!("Mosh: {dest}");
                Ok(Box::new(alouette_term::open(cx, MODULE, title, url.as_str(), move |w| async move {
                    mosh(w, &dest, &term).await
                })))
            }
            s => Err(format!("Terminal does not open {s}: URLs")),
        }
    }
}

/// `user@host:port` from an ssh:// or mosh:// URL.
fn destination(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    let mut d = String::new();
    if !url.username().is_empty() {
        d.push_str(&percent_decode(url.username()));
        d.push('@');
    }
    d.push_str(host);
    if let Some(p) = url.port() {
        d.push_str(&format!(":{p}"));
    }
    d
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| s.to_string())
}

/// `~/.ssh/config` (if imported) plus hosts saved in alouette.toml, which win.
pub fn ssh_config(cx: &Cx) -> SshConfig {
    let mut cfg = if cx.config.ssh.import_config { SshConfig::load_default() } else { SshConfig::default() };
    for h in cx.config.ssh.hosts.iter().rev() {
        let mut opts = vec![("HostName", h.host.clone())];
        if let Some(u) = &h.user {
            opts.push(("User", u.clone()));
        }
        if let Some(p) = h.port {
            opts.push(("Port", p.to_string()));
        }
        if let Some(i) = &h.identity {
            opts.push(("IdentityFile", i.clone()));
        }
        if let Some(j) = &h.jump {
            opts.push(("ProxyJump", j.clone()));
        }
        for f in &h.forward {
            opts.push(("LocalForward", f.clone()));
        }
        cfg.add_host(&h.name, opts);
    }
    cfg
}

fn host_entries(cx: &Cx) -> Vec<Entry> {
    let cfg = ssh_config(cx);
    let saved: Vec<&str> = cx.config.ssh.hosts.iter().map(|h| h.name.as_str()).collect();
    cfg.hosts()
        .into_iter()
        .map(|name| {
            let h = cfg.resolve(&name);
            let mut detail = String::new();
            if let Some(u) = &h.user {
                detail.push_str(u);
                detail.push('@');
            }
            detail.push_str(&h.hostname);
            if h.port != 22 {
                detail.push_str(&format!(":{}", h.port));
            }
            if let Some(j) = h.jumps.first() {
                detail.push_str(&format!("  via {j}"));
            }
            let source = if saved.contains(&name.as_str()) { "saved" } else { "~/.ssh/config" };
            Entry { name, detail, source }
        })
        .collect()
}

fn telnet_suggestions() -> Vec<Entry> {
    [
        ("telehack.com", "A recreation of the 1980s ARPANET and Usenet"),
        ("towel.blinkenlights.nl", "Star Wars, in ASCII (IPv6 gets the colour version)"),
        ("mud.darkerrealms.org:2000", "A MUD running since 1992"),
        ("freechess.org:5000", "The Free Internet Chess Server"),
    ]
    .into_iter()
    .map(|(n, d)| Entry { name: n.into(), detail: d.into(), source: "classic" })
    .collect()
}

/// Mosh runs through the `mosh` program when it is installed: it starts `mosh-server` over
/// SSH and then talks UDP. A native client is planned; until then this says what to install.
async fn mosh(wire: Wire, dest: &str, term: &str) -> Result<(), String> {
    match find_program("mosh") {
        Some(path) => local::run(wire, &format!("{} {dest}", path.display()), term).await,
        None => {
            wire.shared.note(
                "Mosh needs the mosh program on this computer for now (brew install mosh, apt install mosh, \
                 or https://mosh.org). Alouette will speak Mosh itself in a later release.",
            );
            wire.shared.set_status(Status::Exited("mosh is not installed".into()));
            Err("mosh is not installed".into())
        }
    }
}

fn find_program(name: &str) -> Option<std::path::PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .find_map(|dir| exts.iter().map(|e| dir.join(format!("{name}{e}"))).find(|p| p.is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_from_urls() {
        assert_eq!(destination(&Url::parse("ssh://me@example.org:2222").unwrap()), "me@example.org:2222");
        assert_eq!(destination(&Url::parse("ssh://web").unwrap()), "web");
        assert_eq!(destination(&Url::parse("ssh://[::1]:22").unwrap()), "[::1]:22");
        assert_eq!(destination(&Url::parse("mosh://a%40b@h").unwrap()), "a@b@h");
    }

    #[test]
    fn decoding() {
        assert_eq!(percent_decode("/usr/bin/my%20shell"), "/usr/bin/my shell");
        assert_eq!(percent_decode("a+b"), "a+b");
    }
}
