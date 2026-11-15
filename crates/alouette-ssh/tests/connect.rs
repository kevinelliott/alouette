//! SSH against the test server on localhost: host keys, passwords, jump hosts and forwards.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use alouette_net::Net;
use alouette_ssh::testserver::{self, TestServer};
use alouette_ssh::{Ask, Options, Prompter, SshConfig, SshError, connect};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers yes to host keys (unless told not to) and gives the password.
struct Bot {
    password: String,
    trust: bool,
    asked: Mutex<Vec<Ask>>,
}

impl Prompter for Bot {
    fn ask(&self, q: Ask) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        self.asked.lock().unwrap().push(q.clone());
        let a = if q.yes_no { self.trust.then(|| "yes".to_string()) } else { Some(self.password.clone()) };
        Box::pin(async move { a })
    }
    fn note(&self, _text: &str) {}
}

fn bot(trust: bool) -> Arc<Bot> {
    Arc::new(Bot { password: "sekrit".into(), trust, asked: Mutex::new(vec![]) })
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("alouette-ssh-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(addr: std::net::SocketAddr, kh: &std::path::Path, extra: &str) -> SshConfig {
    SshConfig::parse(&format!(
        "Host test\n  HostName 127.0.0.1\n  Port {}\n  User alice\n  UserKnownHostsFile {}\n  IdentityFile /nonexistent/key\n{extra}",
        addr.port(),
        kh.display()
    ))
}

fn opts() -> Options {
    Options { agent: false, ..Options::default() }
}

fn net() -> Net {
    Net::new(&alouette_core::config::Network::default()).unwrap()
}

fn server() -> TestServer {
    TestServer { user: "alice".into(), password: "sekrit".into(), sftp_root: None }
}

#[tokio::test]
async fn password_login_and_trust_on_first_use() {
    let addr = testserver::start(server(), 1).await;
    let dir = tmpdir("tofu");
    let kh = dir.join("known_hosts");
    let cfg = config(addr, &kh, "");

    let b = bot(true);
    let s = connect(&net(), &cfg, "test", &opts(), b.clone()).await.expect("connect");
    assert!(s.badge.starts_with("SSH · IPv4"), "{}", s.badge);
    let (out, status) = s.exec("uptime").await.unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "ran uptime\n");
    assert_eq!(status, Some(0));
    {
        let asked = b.asked.lock().unwrap();
        assert!(asked[0].yes_no, "first question is about the host key");
        assert!(asked[0].body.iter().any(|l| l.starts_with("SHA256:")));
        assert!(asked.iter().any(|q| q.secret), "a password was asked for");
    }
    assert!(std::fs::read_to_string(&kh).unwrap().contains("ssh-ed25519"));

    // Second time: the key is known, so no yes/no question.
    let b = bot(true);
    connect(&net(), &cfg, "test", &opts(), b.clone()).await.expect("connect again");
    assert!(b.asked.lock().unwrap().iter().all(|q| !q.yes_no));
}

#[tokio::test]
async fn refusing_the_host_key_stops() {
    let addr = testserver::start(server(), 2).await;
    let dir = tmpdir("refuse");
    let cfg = config(addr, &dir.join("known_hosts"), "");
    let err = connect(&net(), &cfg, "test", &opts(), bot(false)).await.err().expect("refused");
    assert!(matches!(err, SshError::KeyRejected(_)), "{err}");
    assert!(!dir.join("known_hosts").exists());
}

#[tokio::test]
async fn a_changed_host_key_is_an_error() {
    let addr = testserver::start(server(), 3).await;
    let dir = tmpdir("changed");
    let kh = dir.join("known_hosts");
    // Record a different key for this host and port.
    let other = testserver::host_key(99).public_key().clone();
    russh::keys::known_hosts::learn_known_hosts_path("127.0.0.1", addr.port(), &other, &kh).unwrap();
    let cfg = config(addr, &kh, "");
    let err = connect(&net(), &cfg, "test", &opts(), bot(true)).await.err().expect("changed");
    assert!(matches!(err, SshError::KeyChanged(..)), "{err}");
    assert!(err.to_string().contains("CHANGED"));
}

#[tokio::test]
async fn wrong_password_fails_after_three_tries() {
    let addr = testserver::start(server(), 4).await;
    let dir = tmpdir("wrongpw");
    let cfg = config(addr, &dir.join("known_hosts"), "");
    let b = Arc::new(Bot { password: "nope".into(), trust: true, asked: Mutex::new(vec![]) });
    let err = connect(&net(), &cfg, "test", &opts(), b.clone()).await.err().expect("rejected");
    assert!(matches!(err, SshError::Auth(_)), "{err}");
    assert_eq!(b.asked.lock().unwrap().iter().filter(|q| q.secret).count(), 3);
}

#[tokio::test]
async fn through_a_jump_host() {
    let jump = testserver::start(server(), 5).await;
    let target = testserver::start(server(), 6).await;
    let dir = tmpdir("jump");
    let kh = dir.join("known_hosts");
    let cfg = SshConfig::parse(&format!(
        "Host bastion\n  HostName 127.0.0.1\n  Port {}\n  User alice\n  UserKnownHostsFile {kh}\n  IdentityFile /nonexistent\n\
         Host inner\n  HostName 127.0.0.1\n  Port {}\n  User alice\n  ProxyJump bastion\n  UserKnownHostsFile {kh}\n  IdentityFile /nonexistent\n",
        jump.port(),
        target.port(),
        kh = kh.display()
    ));
    let b = bot(true);
    let s = connect(&net(), &cfg, "inner", &opts(), b.clone()).await.expect("connect via jump");
    assert!(s.badge.ends_with("via bastion"), "{}", s.badge);
    let (out, _) = s.exec("hostname").await.unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "ran hostname\n");
    // Two host keys learned: the bastion's and the target's.
    assert_eq!(b.asked.lock().unwrap().iter().filter(|q| q.yes_no).count(), 2);
}

#[tokio::test]
async fn local_forward_carries_traffic() {
    use alouette_ssh::forward::{LocalForward, run_local};
    // Something to forward to: an echo server.
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    let addr = testserver::start(server(), 7).await;
    let dir = tmpdir("fwd");
    let cfg = config(addr, &dir.join("known_hosts"), "");
    let s = Arc::new(connect(&net(), &cfg, "test", &opts(), bot(true)).await.unwrap());

    // Pick a free local port.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let fwd = LocalForward::parse(&format!("{port}:127.0.0.1:{echo_port}")).unwrap();
    tokio::spawn(run_local(s, fwd, |_| {}));
    let mut c = None;
    for _ in 0..50 {
        if let Ok(x) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            c = Some(x);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let mut c = c.expect("forward listening");
    c.write_all(b"ping through ssh").await.unwrap();
    let mut buf = [0u8; 16];
    c.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping through ssh");
}
