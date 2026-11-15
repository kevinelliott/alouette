//! M9's exit test: three users and a guest connect at once, over real SSH, each with their
//! own data, and the server's limits hold.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use alouette_core::config::Serve;
use alouette_serve::{ServeOptions, start};
use alouette_ui::Module;
use alouette_ui::builtin::CoreModule;
use russh::ChannelMsg;
use russh::client;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, ssh_key::private::Ed25519Keypair};

struct Trusting;

impl client::Handler for Trusting {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

fn key(n: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[n; 32]))
}

/// One person at a terminal: what the server drew, and what it wrote raw.
struct Person {
    _handle: client::Handle<Trusting>,
    channel: russh::Channel<client::Msg>,
    screen: vt100::Parser,
    raw: String,
    closed: bool,
}

impl Person {
    async fn connect(addr: SocketAddr, user: &str, key: Option<&PrivateKey>) -> Result<Self, String> {
        let cfg = Arc::new(client::Config::default());
        let mut h = client::connect(cfg, addr, Trusting).await.map_err(|e| e.to_string())?;
        let ok = match key {
            Some(k) => h
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(k.clone()), None))
                .await
                .map_err(|e| e.to_string())?
                .success(),
            None => h.authenticate_none(user).await.map_err(|e| e.to_string())?.success(),
        };
        if !ok {
            return Err("sign-in refused".into());
        }
        let channel = h.channel_open_session().await.map_err(|e| e.to_string())?;
        channel.request_pty(false, "xterm-256color", 100, 30, 0, 0, &[]).await.map_err(|e| e.to_string())?;
        channel.request_shell(false).await.map_err(|e| e.to_string())?;
        Ok(Self {
            _handle: h,
            channel,
            screen: vt100::Parser::new(30.try_into().unwrap(), 100.try_into().unwrap(), 0),
            raw: String::new(),
            closed: false,
        })
    }

    fn sees(&self, text: &str) -> bool {
        self.screen.screen().contents().contains(text) || self.raw.contains(text)
    }

    /// Read until `text` shows up, or fail after a while.
    async fn wait_for(&mut self, text: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !self.sees(text) {
            if self.closed {
                panic!("the session closed before showing {text:?}; raw: {:?}", self.raw);
            }
            match tokio::time::timeout_at(deadline, self.channel.wait()).await {
                Ok(Some(ChannelMsg::Data { data })) => {
                    self.screen.process(&data);
                    self.raw.push_str(&String::from_utf8_lossy(&data));
                }
                Ok(Some(ChannelMsg::Eof | ChannelMsg::Close)) | Ok(None) => self.closed = true,
                Ok(Some(_)) => {}
                Err(_) => panic!("timed out waiting for {text:?}; screen:\n{}", self.screen.screen().contents()),
            }
        }
    }

    async fn wait_closed(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !self.closed {
            match tokio::time::timeout_at(deadline, self.channel.wait()).await {
                Ok(Some(ChannelMsg::Data { data })) => self.raw.push_str(&String::from_utf8_lossy(&data)),
                Ok(Some(ChannelMsg::Eof | ChannelMsg::Close)) | Ok(None) => self.closed = true,
                Ok(Some(_)) => {}
                Err(_) => panic!("the session did not close; raw: {:?}", self.raw),
            }
        }
    }

    async fn send(&self, bytes: &[u8]) {
        self.channel.data_bytes(bytes.to_vec()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn write_config(root: &std::path::Path, user: &str, startup: &str) {
    let dir = root.join("users").join(user).join("config");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("alouette.toml"),
        format!("[general]\nstartup = [{startup}]\n\n[nostalgia]\nsplash = false\n"),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_users_and_a_guest() {
    let root = std::env::temp_dir().join(format!("alouette-serve-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let users = alouette_serve::users::Users::new(&root);
    for (i, name) in ["ann", "bo", "cy"].iter().enumerate() {
        let k = key(i as u8 + 1);
        users.add(name, &k.public_key().to_openssh().unwrap()).unwrap();
    }
    // Each person's own settings: ann starts on the server console, bo on Tasks, cy on Welcome.
    write_config(&root, "ann", "\"serve:\"");
    write_config(&root, "bo", "\"about:tasks\"");
    write_config(&root, "cy", "\"about:welcome\"");

    let config = Serve {
        guest: true,
        admins: vec!["ann".into()],
        max_sessions: 4,
        sessions_per_user: 1,
        max_guests: 1,
        guest_max_windows: 1,
        ..Serve::default()
    };
    let modules: alouette_serve::ModuleFactory = Arc::new(|_p| vec![Box::new(CoreModule) as Box<dyn Module>]);
    let opts = ServeOptions { config, root: root.clone(), modules, local_offset: time::UtcOffset::UTC };
    let running = start(opts, "127.0.0.1:0").await.unwrap();
    let addr = running.addr;

    // A stranger's key, and a known name with the wrong key, are both turned away.
    assert!(Person::connect(addr, "dee", Some(&key(9))).await.is_err());
    assert!(Person::connect(addr, "bo", Some(&key(1))).await.is_err());

    let mut ann = Person::connect(addr, "ann", Some(&key(1))).await.unwrap();
    let mut bo = Person::connect(addr, "bo", Some(&key(2))).await.unwrap();
    let mut cy = Person::connect(addr, "cy", Some(&key(3))).await.unwrap();
    let mut guest = Person::connect(addr, "guest", None).await.unwrap();

    ann.wait_for("Server console").await;
    bo.wait_for("Clear done").await;
    cy.wait_for("Welcome, cy.").await;
    guest.wait_for("Welcome, guest.").await;
    assert_eq!(running.registry.count(), 4);

    // Everyone sees their own things only.
    ann.wait_for("bo").await;
    ann.wait_for("(guest)").await;
    assert!(!bo.sees("Server console") && !cy.sees("Server console"));
    assert!(!cy.sees("Clear done"));

    // Each user's data is in their own folder; the guest's is apart and temporary.
    for u in ["ann", "bo", "cy"] {
        assert!(root.join("users").join(u).join("data").join("state.sqlite3").is_file(), "{u} has a state file");
    }
    let guests: Vec<_> = std::fs::read_dir(root.join("guests")).unwrap().flatten().collect();
    assert_eq!(guests.len(), 1);
    assert!(!guests[0].path().join("data").join("state.sqlite3").exists(), "a guest's state stays in memory");

    // Limits: the server is full; one more for ann and one more guest are refused once there is room.
    let mut fifth = Person::connect(addr, "guest", None).await.unwrap();
    fifth.wait_for("This server is full").await;

    // A user who is not an admin cannot open the console.
    cy.send(b"\x0f").await;
    cy.send(b"serve:\r").await;
    cy.wait_for("turned off on this server").await;

    // The guest may have one window, and already has it.
    // Let the guest's splash screen run out first: a key would only dismiss it.
    let _ = tokio::time::timeout(Duration::from_secs(2), guest.wait_for("never shown")).await;
    guest.send(b"\x14").await;
    guest.wait_for("allows 1 window").await;

    // The admin ends bo's session from the console.
    ann.send(b"\x1b[B").await;
    ann.send(b"k").await;
    ann.wait_for("Ended bo's session").await;
    bo.wait_for("An administrator ended this session.").await;
    bo.wait_closed().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(running.registry.count(), 3);

    let mut ann2 = Person::connect(addr, "ann", Some(&key(1))).await.unwrap();
    ann2.wait_for("You already have 1 sessions").await;
    let mut guest2 = Person::connect(addr, "guest", None).await.unwrap();
    guest2.wait_for("All 1 guest places are taken").await;

    // When the guest leaves, their folder goes with them.
    guest.send(b"\x11").await;
    guest.wait_closed().await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while root.join("guests").read_dir().map(|d| d.count()).unwrap_or(0) > 0 {
        assert!(std::time::Instant::now() < deadline, "the guest folder was not removed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // People on the server cannot reach the server itself: here, its own SSH port.
    let net = alouette_net::Net::new(&Default::default()).unwrap();
    let err = net.tcp("127.0.0.1", addr.port()).await.err().expect("refused").to_string();
    assert!(err.contains("this server's own network"), "{err}");

    drop((ann, cy));
    let _ = std::fs::remove_dir_all(&root);
}
