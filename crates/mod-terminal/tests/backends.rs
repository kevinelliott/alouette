//! Each terminal backend against a local peer, checked through the emulator's screen.

use std::sync::Arc;
use std::time::Duration;

use alouette_net::Net;
use alouette_ssh::testserver::{self, TestServer};
use alouette_ssh::{Options, SshConfig};
use alouette_term::{Input, Shared, Status, Wire};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

fn wire() -> (Arc<Shared>, mpsc::UnboundedSender<Input>, Wire) {
    let shared = Shared::new(24, 80, 100, || {});
    let (tx, rx) = mpsc::unbounded_channel();
    (shared.clone(), tx, Wire { shared, input: rx })
}

async fn wait_for(shared: &Shared, text: &str) {
    for _ in 0..300 {
        if shared.parser().screen().contents().contains(text) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never saw {text:?}; screen:\n{}", shared.parser().screen().contents());
}

fn net() -> Net {
    Net::new(&alouette_core::config::Network::default()).unwrap()
}

#[tokio::test]
async fn ssh_shell_with_prompts() {
    let addr = testserver::start(TestServer { user: "bob".into(), password: "pw".into(), sftp_root: None }, 11).await;
    let dir = std::env::temp_dir().join(format!("alouette-term-ssh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = SshConfig::parse(&format!(
        "Host t\n HostName 127.0.0.1\n Port {}\n User bob\n UserKnownHostsFile {}\n IdentityFile /none\n",
        addr.port(),
        dir.join("kh").display()
    ));
    let (shared, tx, w) = wire();
    let job = mod_terminal::ssh::SshJob {
        net: net(),
        config: cfg,
        dest: "t".into(),
        options: Options { agent: false, ..Options::default() },
        term: "xterm-256color".into(),
        forwards: vec![],
    };
    let task = tokio::spawn(mod_terminal::ssh::run(w, job));
    // Answer the host key question, then the password.
    let mut answered = 0;
    for _ in 0..300 {
        if let Some(q) = shared.question() {
            shared.answer(Some(if q.yes_no { "yes".into() } else { "pw".into() }));
            answered += 1;
        }
        if shared.status() == Status::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(answered, 2);
    wait_for(&shared, "Welcome to the test server").await;
    assert!(shared.badge().unwrap().starts_with("SSH"));
    tx.send(Input::Bytes(b"hello".to_vec())).unwrap();
    wait_for(&shared, "HELLO").await;
    tx.send(Input::Resize { cols: 100, rows: 30 }).unwrap();
    wait_for(&shared, "[size 100x30]").await;
    tx.send(Input::Bytes(b"exit\r".to_vec())).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
    assert_eq!(shared.status(), Status::Exited("The remote shell exited with status 3".into()));
}

#[tokio::test]
async fn telnet_negotiates_and_shows_data() {
    use proto_telnet::{DO, IAC, NAWS, SB, SE, TTYPE, WILL};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        s.write_all(&[IAC, DO, NAWS, IAC, DO, TTYPE, IAC, SB, TTYPE, 1, IAC, SE]).await.unwrap();
        s.write_all(b"Welcome to the BBS\r\n").await.unwrap();
        let mut got = vec![];
        let mut buf = [0u8; 256];
        while !got.windows(3).any(|w| w == b"x\r\0") {
            let n = s.read(&mut buf).await.unwrap();
            assert!(n > 0, "client hung up; got {got:?}");
            got.extend_from_slice(&buf[..n]);
        }
        got
    });
    let (shared, tx, w) = wire();
    let task = tokio::spawn(async move { alouette_term::telnet::run(w, net(), "127.0.0.1", port, "xterm").await });
    wait_for(&shared, "Welcome to the BBS").await;
    assert!(shared.badge().unwrap().starts_with("Telnet"));
    tx.send(Input::Bytes(b"x\r".to_vec())).unwrap();
    let got = server.await.unwrap();
    // The client agreed to NAWS and TTYPE, sent its size and its type.
    assert!(got.windows(3).any(|w| w == [IAC, WILL, NAWS]));
    assert!(got.windows(9).any(|w| w == [IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE]));
    assert!(got.windows(5).any(|w| w == b"XTERM"));
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

#[cfg(unix)]
#[tokio::test]
async fn local_program_runs_and_exits() {
    let (shared, _tx, w) = wire();
    let script = std::env::temp_dir().join(format!("alouette-pty-{}.sh", std::process::id()));
    std::fs::write(&script, "printf hi-from-pty\nexit 4\n").unwrap();
    let cmd = format!("sh {}", script.display());
    let task = tokio::spawn(async move { alouette_term::local::run(w, &cmd, "xterm").await });
    tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap().unwrap();
    wait_for(&shared, "hi-from-pty").await;
    assert!(matches!(shared.status(), Status::Exited(s) if s.contains("status 4")));
}
