//! FTP and FTPS (explicit `AUTH TLS` on port 21, or implicit on 990), through Alouette's
//! network stack. Connections are pooled so transfers can run side by side.

use std::sync::{Arc, Mutex};

use alouette_net::{Net, TlsOptions};
use proto_ftp::{Connector, DataAddr, Ftp as Conn, Io};

use crate::{BoxFut, Entry, Fs, Progress, Reader, Writer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// Plain FTP. Passwords travel in the clear.
    None,
    /// `AUTH TLS` on the control port, then protected data connections.
    Explicit,
    /// TLS from the first byte (port 990).
    Implicit,
}

#[derive(Clone)]
pub struct FtpSite {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub security: Security,
}

pub struct Ftp {
    net: Net,
    site: FtpSite,
    pool: Mutex<Vec<Conn>>,
    badge: Mutex<Option<String>>,
}

fn err(p: &str, e: impl std::fmt::Display) -> String {
    format!("{p}: {e}")
}

impl Ftp {
    /// Connect once to check the login, and keep that connection.
    pub async fn open(net: Net, site: FtpSite) -> Result<Self, String> {
        let f = Self { net, site, pool: Mutex::new(vec![]), badge: Mutex::new(None) };
        let c = f.connect().await?;
        f.put(c);
        Ok(f)
    }

    fn connector(&self, tls: bool) -> Connector {
        let net = self.net.clone();
        let host = self.site.host.clone();
        Box::new(move |addr: DataAddr| {
            let net = net.clone();
            let host = host.clone();
            Box::pin(async move {
                // A PASV address is often a private one from behind NAT; the control host
                // is the safe choice either way.
                let port = match addr {
                    DataAddr::SameHost(p) | DataAddr::Addr(_, p) => p,
                };
                let conn = net.tcp(&host, port).await.map_err(|e| std::io::Error::other(e.to_string()))?;
                if tls {
                    let t = net
                        .handshake(conn, &TlsOptions::default())
                        .await
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                    Ok(Box::new(t.stream) as Box<dyn Io>)
                } else {
                    Ok(Box::new(conn.stream) as Box<dyn Io>)
                }
            })
        })
    }

    async fn connect(&self) -> Result<Conn, String> {
        let s = &self.site;
        let tls = s.security != Security::None;
        let conn = self.net.tcp(&s.host, s.port).await.map_err(|e| e.to_string())?;
        let info = conn.info.clone();
        let mut badge = conn.info.badge();
        let mut ftp = match s.security {
            Security::Implicit => {
                let t = self.net.handshake(conn, &TlsOptions::default()).await.map_err(|e| e.to_string())?;
                badge = t.info.badge();
                Conn::start(Box::new(t.stream), self.connector(true)).await.map_err(|e| e.to_string())?
            }
            Security::None | Security::Explicit => {
                let mut f = Conn::start(Box::new(conn.stream), self.connector(tls)).await.map_err(|e| e.to_string())?;
                if s.security == Security::Explicit {
                    f.auth_tls().await.map_err(|e| format!("the server does not offer TLS: {e}"))?;
                    let welcome = f.welcome.clone();
                    let (raw, connect) = f.into_inner();
                    let c = alouette_net::Conn { stream: Box::new(raw), info };
                    let t = self.net.handshake(c, &TlsOptions::default()).await.map_err(|e| e.to_string())?;
                    badge = t.info.badge();
                    f = Conn::resume(Box::new(t.stream), connect, welcome);
                }
                f
            }
        };
        if tls {
            ftp.protect_data().await.map_err(|e| e.to_string())?;
        }
        ftp.login(&s.user, &s.password).await.map_err(|e| format!("login failed: {e}"))?;
        let label = if tls { "FTPS" } else { "FTP" };
        *self.badge.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(format!("{label} · {}", badge.replace(" · plain", "").replace("plain", "")));
        Ok(ftp)
    }

    async fn take(&self) -> Result<Conn, String> {
        let pooled = self.pool.lock().unwrap_or_else(|e| e.into_inner()).pop();
        match pooled {
            Some(c) => Ok(c),
            None => self.connect().await,
        }
    }

    fn put(&self, c: Conn) {
        let mut p = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        if p.len() < 4 {
            p.push(c);
        }
    }

    /// Return a connection after use, unless it failed in a way that may have broken it.
    fn finish<T>(&self, c: Conn, r: &proto_ftp::Result<T>) {
        if matches!(r, Ok(_) | Err(proto_ftp::Error::Reply { .. })) {
            self.put(c);
        }
    }
}

/// Take a pooled connection, run one call on it, and put it back.
macro_rules! on_conn {
    ($self:ident, $path:expr, |$c:ident| $call:expr) => {{
        let mut $c = $self.take().await.map_err(|e| err($path, e))?;
        let r = $call.await;
        $self.finish($c, &r);
        r.map_err(|e| err($path, e))
    }};
}

impl Fs for Ftp {
    fn label(&self) -> String {
        let scheme = if self.site.security == Security::None { "ftp" } else { "ftps" };
        format!("{scheme}://{}@{}", self.site.user, self.site.host)
    }
    fn badge(&self) -> Option<String> {
        self.badge.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn home(&self) -> BoxFut<'_, String> {
        Box::pin(async move { on_conn!(self, "/", |c| c.pwd()) })
    }
    fn list<'a>(&'a self, path: &'a str) -> BoxFut<'a, Vec<Entry>> {
        Box::pin(async move {
            let list = on_conn!(self, path, |c| c.list(path))?;
            Ok(list
                .into_iter()
                .map(|e| Entry {
                    name: e.name,
                    is_dir: e.is_dir,
                    is_link: e.is_link,
                    size: e.size,
                    modified: e.modified,
                })
                .collect())
        })
    }
    fn download<'a>(&'a self, path: &'a str, to: Writer<'a>, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move { on_conn!(self, path, |c| c.retr(path, to, progress)) })
    }
    fn upload<'a>(&'a self, path: &'a str, from: Reader<'a>, _size: u64, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move { on_conn!(self, path, |c| c.stor(path, from, progress)) })
    }
    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move { on_conn!(self, path, |c| c.mkdir(path)) })
    }
    fn remove<'a>(&'a self, path: &'a str, is_dir: bool) -> BoxFut<'a, ()> {
        Box::pin(async move {
            if is_dir { on_conn!(self, path, |c| c.rmdir(path)) } else { on_conn!(self, path, |c| c.delete(path)) }
        })
    }
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move { on_conn!(self, from, |c| c.rename(from, to)) })
    }
}

impl Drop for Ftp {
    fn drop(&mut self) {
        self.pool.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

#[doc(hidden)]
pub fn _assert_send(f: Arc<Ftp>) -> Arc<dyn Fs> {
    f
}
