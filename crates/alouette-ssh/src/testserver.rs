//! A small SSH server for tests: password login, a shell that echoes in capitals, `exec`,
//! and direct-tcpip so jump hosts and forwards can be tested. Not for real use.

use std::net::SocketAddr;
use std::sync::Arc;

use russh::keys::PrivateKey;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;

/// A host key that is the same on every run for a given seed.
pub fn host_key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

#[derive(Clone)]
pub struct TestServer {
    pub user: String,
    pub password: String,
    /// Handles SFTP subsystem requests, if set.
    pub sftp_root: Option<std::path::PathBuf>,
}

/// Session channels that run the echo shell (data on forwarded channels is left alone).
#[derive(Default)]
pub struct Conn {
    server: Option<TestServer>,
    shells: Arc<std::sync::Mutex<std::collections::HashSet<ChannelId>>>,
    /// Session channels not yet given a shell or subsystem.
    fresh: std::collections::HashMap<ChannelId, Channel<Msg>>,
}

impl Conn {
    fn s(&self) -> &TestServer {
        self.server.as_ref().expect("server")
    }
}

impl server::Server for TestServer {
    type Handler = Conn;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Conn {
        Conn { server: Some(self.clone()), shells: Default::default(), fresh: Default::default() }
    }
}

impl server::Handler for Conn {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == self.s().user && password == self.s().password { Ok(Auth::Accept) } else { Ok(Auth::reject()) }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.s().sftp_root.is_some() {
            self.fresh.insert(channel.id(), channel);
        }
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _c: u32,
        _r: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.fresh.remove(&channel);
        session.channel_success(channel)?;
        self.shells.lock().unwrap_or_else(|e| e.into_inner()).insert(channel);
        session.data(channel, &b"Welcome to the test server\r\n$ "[..])
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.fresh.remove(&channel);
        session.channel_success(channel)?;
        let out = format!("ran {}\n", String::from_utf8_lossy(data));
        session.data(channel, out.into_bytes())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.lock().unwrap_or_else(|e| e.into_inner()).contains(&channel) {
            return Ok(());
        }
        if data == b"exit\r" {
            session.exit_status_request(channel, 3)?;
            session.eof(channel)?;
            return session.close(channel);
        }
        session.data(channel, data.to_ascii_uppercase())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        c: u32,
        r: u32,
        _pw: u32,
        _ph: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("[size {c}x{r}]").into_bytes())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp"
            && let Some(root) = self.s().sftp_root.clone()
            && let Some(ch) = self.fresh.remove(&channel)
        {
            session.channel_success(channel)?;
            tokio::spawn(russh_sftp::server::run(ch.into_stream(), crate::testsftp::Sftp::new(root)));
            Ok(())
        } else {
            session.channel_failure(channel)
        }
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _oa: &str,
        _op: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let target = format!("{host}:{port}");
        match tokio::net::TcpStream::connect(&target).await {
            Ok(mut sock) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
                });
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }
}

/// Start a server on 127.0.0.1 with a random port. Returns its address.
pub async fn start(server: TestServer, key_seed: u8) -> SocketAddr {
    let config = Arc::new(server::Config {
        keys: vec![host_key(key_seed)],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let mut s = server;
        let _ = s.run_on_socket(config, &listener).await;
    });
    addr
}
