//! An in-memory SMTP server for tests: EHLO with AUTH PLAIN/LOGIN, and it keeps what it
//! is sent. Not a real server.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub from: String,
    pub to: Vec<String>,
    /// The message, un-stuffed.
    pub data: Vec<u8>,
}

#[derive(Clone)]
pub struct Server {
    pub user: String,
    pub password: String,
    pub sent: Arc<Mutex<Vec<Sent>>>,
}

impl Server {
    pub fn new(user: &str, password: &str) -> Self {
        Self { user: user.into(), password: password.into(), sent: Arc::default() }
    }
}

pub async fn start(srv: Server) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let srv = srv.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(s);
                let _ = run(&mut r, srv).await;
            });
        }
    });
    addr
}

async fn run(r: &mut BufReader<tokio::net::TcpStream>, srv: Server) -> std::io::Result<()> {
    let b64 = |s: &str| {
        String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(s.trim()).unwrap_or_default())
            .into_owned()
    };
    r.get_mut().write_all(b"220 test smtpd\r\n").await?;
    let (mut authed, mut from, mut to) = (false, String::new(), vec![]);
    let mut login_user: Option<Option<String>> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let l = line.trim_end();
        let up = l.to_ascii_uppercase();
        let reply: String = if let Some(state) = login_user.take() {
            match state {
                None => {
                    login_user = Some(Some(b64(l)));
                    "334 UGFzc3dvcmQ6".into()
                }
                Some(u) => {
                    authed = u == srv.user && b64(l) == srv.password;
                    if authed { "235 ok".into() } else { "535 no".into() }
                }
            }
        } else if up.starts_with("EHLO") {
            "250-test\r\n250-SIZE 1000000\r\n250-8BITMIME\r\n250-SMTPUTF8\r\n250 AUTH PLAIN LOGIN".into()
        } else if up.starts_with("AUTH PLAIN ") {
            let d = b64(&l[11..]);
            let p: Vec<&str> = d.split('\0').collect();
            authed = p.len() == 3 && p[1] == srv.user && p[2] == srv.password;
            if authed { "235 ok".into() } else { "535 no".into() }
        } else if up == "AUTH LOGIN" {
            login_user = Some(None);
            "334 VXNlcm5hbWU6".into()
        } else if up.starts_with("MAIL FROM:") {
            if !authed {
                "530 authenticate first".into()
            } else {
                from = l[10..].split('>').next().unwrap_or_default().trim_start_matches('<').to_string();
                to.clear();
                "250 ok".into()
            }
        } else if up.starts_with("RCPT TO:") {
            to.push(l[8..].trim_matches(['<', '>']).to_string());
            "250 ok".into()
        } else if up == "DATA" {
            r.get_mut().write_all(b"354 go\r\n").await?;
            let mut data = vec![];
            loop {
                let mut d = vec![];
                if r.read_until(b'\n', &mut d).await? == 0 {
                    return Ok(());
                }
                if d == b".\r\n" {
                    break;
                }
                if d.starts_with(b"..") {
                    d.remove(0);
                }
                data.extend_from_slice(&d);
            }
            srv.sent.lock().unwrap().push(Sent { from: from.clone(), to: to.clone(), data });
            "250 queued".into()
        } else if up == "QUIT" {
            r.get_mut().write_all(b"221 bye\r\n").await?;
            return Ok(());
        } else {
            "502 no".into()
        };
        r.get_mut().write_all(format!("{reply}\r\n").as_bytes()).await?;
    }
}
