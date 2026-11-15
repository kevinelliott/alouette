//! One NNTP connection per window, opened on first use and reopened once if it drops.

use std::sync::Arc;

use alouette_core::config::NewsServer;
use alouette_net::{Net, TlsOptions};
use alouette_store::secrets::SecretStore;
use alouette_ui::ask::{Asker, Question};
use proto_nntp::{Group, Nntp, Overview, Selected};

pub struct Conn {
    net: Net,
    pub server: NewsServer,
    secrets: Arc<dyn SecretStore>,
    asker: Asker,
    inner: tokio::sync::Mutex<Option<Nntp>>,
    badge: std::sync::Mutex<String>,
}

/// `host[:port]` and whether TLS starts at once. Port 563 is NNTPS; 119 tries STARTTLS.
pub fn endpoint(host: &str) -> (String, u16, bool) {
    let h = host.trim();
    let (name, port) = match h.rsplit_once(':') {
        Some((n, p)) if !n.contains(':') || n.starts_with('[') => {
            (n.trim_matches(['[', ']']).to_string(), p.parse().unwrap_or(563))
        }
        _ => (h.trim_matches(['[', ']']).to_string(), 563),
    };
    (name, port, port == 563 || port == 443)
}

fn secret_key(server: &str) -> String {
    format!("news/{server}/password")
}

macro_rules! with_nntp {
    ($self:ident, $n:ident => $body:expr) => {{
        let mut guard = $self.inner.lock().await;
        let mut out: Result<_, String> = Err("not connected".into());
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some($self.open().await?);
            }
            let Some($n) = guard.as_mut() else { break };
            match $body.await {
                Ok(v) => {
                    out = Ok(v);
                    break;
                }
                Err(proto_nntp::Error::Io(_) | proto_nntp::Error::Protocol(_)) if attempt == 0 => *guard = None,
                Err(e) => {
                    out = Err(e.to_string());
                    break;
                }
            }
        }
        out
    }};
}

impl Conn {
    pub fn new(net: Net, server: NewsServer, secrets: Arc<dyn SecretStore>, asker: Asker) -> Self {
        Self { net, server, secrets, asker, inner: tokio::sync::Mutex::new(None), badge: Default::default() }
    }

    pub fn badge(&self) -> String {
        self.badge.lock().map(|b| b.clone()).unwrap_or_default()
    }

    async fn open(&self) -> Result<Nntp, String> {
        let (host, port, implicit) = endpoint(&self.server.host);
        let at = |e: &dyn std::fmt::Display| format!("{host}: {e}");
        let conn = self.net.tcp(&host, port).await.map_err(|e| at(&e))?;
        let (mut n, mut info) = if implicit {
            let t = self.net.handshake(conn, &TlsOptions::default()).await.map_err(|e| at(&e))?;
            (Nntp::start(Box::new(t.stream)).await.map_err(|e| at(&e))?, t.info)
        } else {
            (Nntp::start(Box::new(conn.stream)).await.map_err(|e| at(&e))?, conn.info)
        };
        n.capabilities().await.map_err(|e| at(&e))?;
        if !implicit && n.has("STARTTLS") {
            n.starttls().await.map_err(|e| at(&e))?;
            let (greeting, posting) = (n.greeting.clone(), n.posting);
            let c = alouette_net::Conn { stream: Box::new(n.into_inner()), info };
            let t = self.net.handshake(c, &TlsOptions::default()).await.map_err(|e| at(&e))?;
            info = t.info;
            n = Nntp::resume(Box::new(t.stream), greeting, posting);
            n.capabilities().await.map_err(|e| at(&e))?;
        }
        if let Ok(mut b) = self.badge.lock() {
            *b = format!("NNTP · {}", info.badge());
        }
        if let Some(user) = self.server.user.clone().filter(|u| !u.is_empty()) {
            let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
            if info.tls.is_none() && !local {
                return Err(format!("{host} offers no TLS, so Alouette will not send your password. Use port 563."));
            }
            let key = secret_key(&self.server.name);
            let mut ok = false;
            for round in 0..3 {
                let saved = if round == 0 { self.secrets.get(&key).ok().flatten() } else { None };
                let pw = match saved {
                    Some(p) => p,
                    None => {
                        let q = Question {
                            title: "News password".into(),
                            body: vec![
                                format!("Password for {user} on {host}."),
                                if round > 0 { "That did not work; try again.".into() } else { String::new() },
                            ],
                            prompt: "Password: ".into(),
                            secret: true,
                            yes_no: false,
                        };
                        self.asker.ask(q).await.ok_or("signing in was cancelled")?
                    }
                };
                match n.login(&user, &pw).await {
                    Ok(()) => {
                        let _ = self.secrets.set(&key, &pw);
                        ok = true;
                        break;
                    }
                    Err(proto_nntp::Error::Reply { code: 481 | 482 | 502, .. }) => {
                        let _ = self.secrets.delete(&key);
                    }
                    Err(e) => return Err(at(&e)),
                }
            }
            if !ok {
                return Err(format!("{host}: the login was refused three times"));
            }
        }
        n.mode_reader().await.map_err(|e| at(&e))?;
        Ok(n)
    }

    /// Check the connection and sign in, so the first use asks for a password at once.
    pub async fn connect(&self) -> Result<bool, String> {
        with_nntp!(self, n => async { Ok::<bool, proto_nntp::Error>(n.posting) })
    }

    pub async fn groups(&self) -> Result<Vec<Group>, String> {
        with_nntp!(self, n => n.list_active(None))
    }

    pub async fn descriptions(&self) -> Result<Vec<(String, String)>, String> {
        with_nntp!(self, n => n.list_newsgroups(None))
    }

    pub async fn status(&self, group: &str) -> Result<Selected, String> {
        with_nntp!(self, n => n.group(group))
    }

    /// The newest `max` overview lines in `group`.
    pub async fn overview(&self, group: &str, max: u64) -> Result<(Selected, Vec<Overview>), String> {
        with_nntp!(self, n => async {
            let s = n.group(group).await?;
            if s.count == 0 || s.high < s.low {
                return Ok((s, vec![]));
            }
            let from = s.low.max(s.high.saturating_sub(max.max(1) - 1));
            let o = n.over(from, s.high).await?;
            Ok::<_, proto_nntp::Error>((s, o))
        })
    }

    /// An article by number in `group`, or by message ID.
    pub async fn article(&self, group: &str, which: &str) -> Result<Vec<u8>, String> {
        with_nntp!(self, n => async {
            if !which.starts_with('<') {
                n.group(group).await?;
            }
            n.article(which).await
        })
    }

    pub async fn post(&self, raw: &[u8]) -> Result<(), String> {
        with_nntp!(self, n => n.post(raw))
    }

    pub async fn close(&self) {
        if let Some(mut n) = self.inner.lock().await.take() {
            n.quit().await;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn endpoints() {
        assert_eq!(super::endpoint("news.example.org"), ("news.example.org".into(), 563, true));
        assert_eq!(super::endpoint("news.example.org:119"), ("news.example.org".into(), 119, false));
        assert_eq!(super::endpoint("[::1]:119"), ("::1".into(), 119, false));
    }
}
