//! An account over IMAP (reading) and SMTP (sending).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alouette_core::config::MailKind;
use alouette_net::{Net, TlsOptions};
use proto_imap::Imap;
use proto_smtp::Smtp;
use tokio::sync::Mutex as AsyncMutex;

use crate::account::{Account, Login, Server, Tls};
use crate::alouette_store_like::Secrets;
use crate::message::{flags_from_imap, summarize};
use crate::{Backend, BoxFut, Flags, Folder, Prompt, Role, Summary, oauth};

/// A connection and the mailbox it has selected.
struct Open {
    imap: Imap,
    selected: Option<String>,
}

pub struct ImapSmtp {
    net: Net,
    acct: Account,
    secrets: Arc<dyn Secrets>,
    prompt: Arc<dyn Prompt>,
    conn: AsyncMutex<Option<Open>>,
    idle: AsyncMutex<Option<Open>>,
    badge: Mutex<String>,
    /// Folder names by role, learned from LIST.
    roles: Mutex<Vec<(Role, String)>>,
}

/// A login to use on both servers.
#[derive(Clone)]
enum Cred {
    Password(String),
    Bearer(String),
}

fn broken(e: &proto_imap::Error) -> bool {
    matches!(e, proto_imap::Error::Io(_) | proto_imap::Error::Bye(_) | proto_imap::Error::Protocol(_))
}

fn auth_failed(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("authenticationfailed")
        || l.contains("authentication failed")
        || l.contains("invalid credentials")
        || l.contains(" no ")
        || l.starts_with("the server said no")
}

/// Run `$body` (using `$c: &mut Imap`) on the pooled connection, with `$folder` selected,
/// reconnecting once if the connection broke.
macro_rules! with_imap {
    ($self:ident, $folder:expr, |$c:ident| $body:expr) => {{
        let mut guard = $self.conn.lock().await;
        let folder: Option<&str> = $folder;
        let mut tries = 0;
        loop {
            if guard.is_none() {
                *guard = Some(Open { imap: $self.open().await?, selected: None });
            }
            let st = guard.as_mut().expect("connected");
            if let Some(f) = folder
                && st.selected.as_deref() != Some(f)
            {
                match st.imap.select(f).await {
                    Ok(_) => st.selected = Some(f.to_string()),
                    Err(e) if broken(&e) && tries == 0 => {
                        *guard = None;
                        tries += 1;
                        continue;
                    }
                    Err(e) => break Err(format!("{f}: {e}")),
                }
            }
            let $c = &mut st.imap;
            match $body.await {
                Ok(v) => break Ok(v),
                Err(e) if broken(&e) && tries == 0 => {
                    *guard = None;
                    tries += 1;
                }
                Err(e) => break Err(e.to_string()),
            }
        }
    }};
}

impl ImapSmtp {
    pub async fn connect(
        net: Net,
        acct: Account,
        secrets: Arc<dyn Secrets>,
        prompt: Arc<dyn Prompt>,
    ) -> crate::Result<Self> {
        let me = Self {
            net,
            acct,
            secrets,
            prompt,
            conn: AsyncMutex::new(None),
            idle: AsyncMutex::new(None),
            badge: Mutex::new("IMAP".into()),
            roles: Mutex::new(vec![]),
        };
        let first = me.open().await?;
        *me.conn.lock().await = Some(Open { imap: first, selected: None });
        Ok(me)
    }

    async fn cred(&self, retry: bool) -> crate::Result<Cred> {
        match self.acct.login {
            Login::Password => {
                let key = self.acct.secret_key("password");
                if !retry && let Some(p) = self.secrets.get(&key) {
                    return Ok(Cred::Password(p));
                }
                let body = if retry {
                    vec![format!("The server did not accept the password for {}.", self.acct.user)]
                } else {
                    vec![format!(
                        "Password for {} on {}.",
                        self.acct.user,
                        self.acct.imap.as_ref().map(|s| s.host.as_str()).unwrap_or("the server")
                    )]
                };
                let p = self.prompt.ask("Mail password", body, "Password", true).await.ok_or("cancelled")?;
                Ok(Cred::Password(p))
            }
            Login::Google | Login::Microsoft => Ok(Cred::Bearer(
                oauth::token(&self.net, &self.acct, self.secrets.as_ref(), self.prompt.as_ref(), retry).await?,
            )),
        }
    }

    fn remember(&self, c: &Cred) {
        if let Cred::Password(p) = c {
            self.secrets.set(&self.acct.secret_key("password"), p);
        }
    }

    async fn stream(&self, s: &Server) -> crate::Result<(Box<dyn alouette_net::Io>, alouette_net::ConnInfo)> {
        let conn = self.net.tcp(&s.host, s.port).await.map_err(|e| format!("{}: {e}", s.host))?;
        if s.tls == Tls::Implicit {
            let t = self.net.handshake(conn, &TlsOptions::default()).await.map_err(|e| format!("{}: {e}", s.host))?;
            Ok((Box::new(t.stream), t.info))
        } else {
            Ok((Box::new(conn.stream), conn.info))
        }
    }

    async fn starttls<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
        &self,
        s: &Server,
        raw: T,
        info: alouette_net::ConnInfo,
    ) -> crate::Result<(Box<dyn alouette_net::Io>, alouette_net::ConnInfo)> {
        let c = alouette_net::Conn { stream: Box::new(raw), info };
        let t = self.net.handshake(c, &TlsOptions::default()).await.map_err(|e| format!("{}: {e}", s.host))?;
        Ok((Box::new(t.stream), t.info))
    }

    /// A new IMAP connection, logged in.
    async fn open(&self) -> crate::Result<Imap> {
        let s = self.acct.imap.clone().ok_or("no IMAP server")?;
        let mut retry = false;
        for _ in 0..3 {
            let (io, info) = self.stream(&s).await?;
            let mut imap = Imap::start(Box::new(io)).await.map_err(|e| e.to_string())?;
            let mut info = info;
            if s.tls == Tls::StartTls {
                imap.starttls().await.map_err(|e| format!("the server does not offer STARTTLS: {e}"))?;
                let (tag, greeting) = (imap.tag_counter(), imap.greeting.clone());
                let (io, i) = self.starttls(&s, imap.into_inner(), info).await?;
                info = i;
                imap = Imap::resume(Box::new(io), tag, greeting).await.map_err(|e| e.to_string())?;
            }
            *self.badge.lock().unwrap_or_else(|e| e.into_inner()) = format!("IMAP · {}", info.badge());
            let cred = self.cred(retry).await?;
            let r = match &cred {
                Cred::Password(p) if imap.has("AUTH=PLAIN") => imap.authenticate_plain(&self.acct.user, p).await,
                Cred::Password(p) => imap.login(&self.acct.user, p).await,
                Cred::Bearer(t) => imap.authenticate_xoauth2(&self.acct.email, t).await,
            };
            match r {
                Ok(()) => {
                    self.remember(&cred);
                    return Ok(imap);
                }
                Err(e) if matches!(e, proto_imap::Error::No { .. }) && auth_failed(&format!(" {e} ")) => retry = true,
                Err(e) => return Err(format!("login failed: {e}")),
            }
        }
        Err("login failed three times".into())
    }

    fn role(&self, r: Role) -> Option<String> {
        self.roles.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(x, _)| *x == r).map(|(_, n)| n.clone())
    }

    async fn ensure_roles(&self) -> crate::Result<()> {
        if self.roles.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            self.folders().await?;
        }
        Ok(())
    }

    async fn submit(&self, raw: &[u8], from: &str, to: &[String]) -> crate::Result<()> {
        let s = self.acct.smtp.clone().ok_or("no SMTP server set for this account")?;
        let mut retry = false;
        for _ in 0..3 {
            let (io, info) = self.stream(&s).await?;
            let mut smtp = Smtp::start(Box::new(io)).await.map_err(|e| e.to_string())?;
            let helo = "alouette.localdomain";
            smtp.ehlo(helo).await.map_err(|e| e.to_string())?;
            if s.tls == Tls::StartTls {
                smtp.starttls().await.map_err(|e| e.to_string())?;
                let greeting = smtp.greeting.clone();
                let (io, _) = self.starttls(&s, smtp.into_inner(), info).await?;
                smtp = Smtp::resume(Box::new(io), greeting);
                smtp.ehlo(helo).await.map_err(|e| e.to_string())?;
            }
            let cred = self.cred(retry).await?;
            let r = match &cred {
                Cred::Password(p) => smtp.login(&self.acct.user, p).await,
                Cred::Bearer(t) => smtp.login_xoauth2(&self.acct.email, t).await,
            };
            match r {
                Ok(()) => {}
                Err(proto_smtp::Error::Reply { code: 535, .. }) => {
                    retry = true;
                    continue;
                }
                Err(e) => return Err(format!("sending: {e}")),
            }
            let to: Vec<&str> = to.iter().map(String::as_str).collect();
            smtp.send(from, &to, raw).await.map_err(|e| format!("sending: {e}"))?;
            smtp.quit().await;
            return Ok(());
        }
        Err("the mail server did not accept the login for sending".into())
    }
}

fn guess_role(name: &str) -> Role {
    let n = name.rsplit(['/', '.']).next().unwrap_or(name).to_ascii_lowercase();
    match n.as_str() {
        "inbox" => Role::Inbox,
        "sent" | "sent items" | "sent mail" | "sent messages" | "gesendet" => Role::Sent,
        "drafts" | "draft" | "entwürfe" => Role::Drafts,
        "trash" | "deleted items" | "deleted messages" | "bin" | "papierkorb" => Role::Trash,
        "junk" | "spam" | "junk e-mail" | "junk email" => Role::Junk,
        "archive" | "archives" => Role::Archive,
        _ => Role::Other,
    }
}

impl Backend for ImapSmtp {
    fn badge(&self) -> String {
        self.badge.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn folders(&self) -> BoxFut<'_, Vec<Folder>> {
        Box::pin(async move {
            let boxes = with_imap!(self, None, |c| c.list())?;
            let mut out = vec![];
            for b in boxes.into_iter().filter(|b| b.selectable()) {
                let (unread, total) = with_imap!(self, None, |c| c.status(&b.raw)).unwrap_or((0, 0));
                let role = b
                    .special_use()
                    .map(Role::from_flag)
                    .filter(|r| *r != Role::Other)
                    .unwrap_or_else(|| guess_role(&b.name));
                let name = match b.delimiter {
                    Some(d) if d != '/' => b.name.replace(d, "/"),
                    _ => b.name.clone(),
                };
                out.push(Folder { id: b.raw, name, role, unread, total });
            }
            out.sort_by(|a, b| a.role.cmp(&b.role).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            let mut roles = vec![];
            for f in &out {
                if f.role != Role::Other && !roles.iter().any(|(r, _)| *r == f.role) {
                    roles.push((f.role, f.id.clone()));
                }
            }
            *self.roles.lock().unwrap_or_else(|e| e.into_inner()) = roles;
            Ok(out)
        })
    }

    fn list<'a>(&'a self, folder: &'a str, limit: usize) -> BoxFut<'a, Vec<Summary>> {
        Box::pin(async move {
            let uids = with_imap!(self, Some(folder), |c| c.uid_search("ALL"))?;
            let take: Vec<String> = uids.iter().rev().take(limit).map(u32::to_string).collect();
            if take.is_empty() {
                return Ok(vec![]);
            }
            let set = take.join(",");
            let items = "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER.FIELDS (FROM TO CC SUBJECT DATE MESSAGE-ID)])";
            let fetched = with_imap!(self, Some(folder), |c| c.uid_fetch(&set, items))?;
            let mut out: Vec<Summary> = fetched
                .into_iter()
                .map(|f| summarize(f.uid.to_string(), f.body().unwrap_or_default(), flags_from_imap(&f.flags), f.size))
                .collect();
            out.sort_by_key(|s| std::cmp::Reverse(s.id.parse::<u32>().unwrap_or(0)));
            Ok(out)
        })
    }

    fn fetch<'a>(&'a self, folder: &'a str, id: &'a str) -> BoxFut<'a, Vec<u8>> {
        Box::pin(async move {
            // BODY[] (not PEEK) marks the message read, as opening it should.
            let f = with_imap!(self, Some(folder), |c| c.uid_fetch(id, "(UID FLAGS BODY[])"))?;
            f.into_iter()
                .next()
                .and_then(|f| f.body().map(<[u8]>::to_vec))
                .ok_or_else(|| "the message is gone".to_string())
        })
    }

    fn set_flags<'a>(&'a self, folder: &'a str, ids: &'a [String], flags: Flags, on: bool) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let mut names = vec![];
            if flags.seen {
                names.push("\\Seen");
            }
            if flags.flagged {
                names.push("\\Flagged");
            }
            if flags.answered {
                names.push("\\Answered");
            }
            if flags.draft {
                names.push("\\Draft");
            }
            let set = ids.join(",");
            with_imap!(self, Some(folder), |c| c.uid_store(&set, on, &names))
        })
    }

    fn move_to<'a>(&'a self, folder: &'a str, ids: &'a [String], to: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let set = ids.join(",");
            with_imap!(self, Some(folder), |c| c.uid_move(&set, to))
        })
    }

    fn expunge<'a>(&'a self, folder: &'a str, ids: &'a [String]) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let set = ids.join(",");
            with_imap!(self, Some(folder), |c| async {
                c.uid_store(&set, true, &["\\Deleted"]).await?;
                c.uid_expunge(&set).await
            })
        })
    }

    fn send<'a>(&'a self, raw: &'a [u8], from: &'a str, to: &'a [String]) -> BoxFut<'a, ()> {
        Box::pin(async move {
            self.submit(raw, from, to).await?;
            // Gmail and Microsoft file sent mail themselves.
            if self.acct.kind == MailKind::Imap {
                self.ensure_roles().await?;
                if let Some(sent) = self.role(Role::Sent) {
                    with_imap!(self, None, |c| c.append(&sent, &["\\Seen"], raw))?;
                }
            }
            Ok(())
        })
    }

    fn save_draft<'a>(&'a self, raw: &'a [u8]) -> BoxFut<'a, ()> {
        Box::pin(async move {
            self.ensure_roles().await?;
            let drafts = self.role(Role::Drafts).unwrap_or_else(|| "Drafts".into());
            with_imap!(self, None, |c| c.append(&drafts, &["\\Seen", "\\Draft"], raw))
        })
    }

    fn wait<'a>(&'a self, folder: &'a str, max: Duration) -> BoxFut<'a, bool> {
        Box::pin(async move {
            let mut g = self.idle.lock().await;
            if g.is_none() {
                *g = Some(Open { imap: self.open().await?, selected: None });
            }
            let st = g.as_mut().expect("connected");
            if st.selected.as_deref() != Some(folder) {
                if let Err(e) = st.imap.examine(folder).await {
                    *g = None;
                    return Err(e.to_string());
                }
                st.selected = Some(folder.to_string());
            }
            match st.imap.idle(max).await {
                Ok(proto_imap::Change::Timeout) => Ok(false),
                Ok(_) => Ok(true),
                Err(e) => {
                    *g = None;
                    Err(e.to_string())
                }
            }
        })
    }
}
