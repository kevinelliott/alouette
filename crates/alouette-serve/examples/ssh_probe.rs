//! Sign in to an `alouette serve` server and print the screen it draws, for checking a
//! server (or the container image) without an SSH client at hand.
//!
//!   cargo run -p alouette-serve --example ssh_probe -- 127.0.0.1:2222 ann key-file
//!
//! A missing key file is created, and its public key printed for `serve add-user`.
//! Without a key file it signs in as `guest`.

use std::sync::Arc;
use std::time::Duration;

use russh::ChannelMsg;
use russh::client;
use russh::keys::ssh_key::{LineEnding, private::Ed25519Keypair};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg};

struct Trusting;

impl client::Handler for Trusting {
    type Error = russh::Error;
    async fn check_server_key(&mut self, k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        eprintln!("server key: {k:?}");
        Ok(true)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args.first().cloned().unwrap_or_else(|| "127.0.0.1:2222".into());
    let user = args.get(1).cloned().unwrap_or_else(|| "guest".into());
    let key = match args.get(2) {
        Some(p) if std::path::Path::new(p).exists() => Some(PrivateKey::from_openssh(std::fs::read_to_string(p)?)?),
        Some(p) => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
            let k = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
            std::fs::write(p, k.to_openssh(LineEnding::LF)?.as_bytes())?;
            println!("{}", k.public_key().to_openssh()?);
            return Ok(());
        }
        None => None,
    };
    let mut h = client::connect(Arc::new(client::Config::default()), addr, Trusting).await?;
    let ok = match &key {
        Some(k) => h.authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(k.clone()), None)).await?,
        None => h.authenticate_none(&user).await?,
    };
    if !ok.success() {
        return Err("the server refused the sign-in".into());
    }
    let mut ch = h.channel_open_session().await?;
    ch.request_pty(false, "xterm-256color", 100, 30, 0, 0, &[]).await?;
    ch.request_shell(false).await?;
    let mut screen = vt100::Parser::new(30.try_into()?, 100.try_into()?, 0);
    let stop = tokio::time::sleep(Duration::from_secs(4));
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            m = ch.wait() => match m {
                Some(ChannelMsg::Data { data }) => screen.process(&data),
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                _ => {}
            },
        }
    }
    println!("{}", screen.screen().contents());
    Ok(())
}
