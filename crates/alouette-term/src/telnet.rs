//! Telnet, as Minuet's Telnet module did it, through Alouette's network stack (so routes,
//! proxies and Happy Eyeballs apply).

use alouette_net::Net;
use proto_telnet::Telnet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::emu::{Input, Status, Wire};

pub async fn run(wire: Wire, net: Net, host: &str, port: u16, term: &str) -> Result<(), String> {
    let Wire { shared, mut input } = wire;
    shared.set_status(Status::Connecting(format!("Connecting to {host} port {port}")));
    let conn = match net.tcp(host, port).await {
        Ok(c) => c,
        Err(e) => {
            shared.set_status(Status::Exited(format!("Could not connect: {e}")));
            return Err(e.to_string());
        }
    };
    shared.set_badge(format!("Telnet · {}", conn.info.badge()));
    let (cols, rows) = {
        let p = shared.parser();
        let (r, c) = p.screen().size();
        (c.get(), r.get())
    };
    let mut tn = Telnet::new(term, cols, rows);
    let (mut rd, mut wr) = tokio::io::split(conn.stream);
    wr.write_all(&tn.greeting()).await.map_err(|e| e.to_string())?;
    shared.set_status(Status::Running);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            n = rd.read(&mut buf) => {
                let n = match n {
                    Ok(0) | Err(_) => {
                        shared.set_status(Status::Exited("The connection closed".into()));
                        return Ok(());
                    }
                    Ok(n) => n,
                };
                let out = tn.receive(&buf[..n]);
                let mut reply = out.reply;
                if !out.data.is_empty() {
                    reply.extend(tn.encode(&shared.feed(&out.data)));
                }
                if !reply.is_empty() && wr.write_all(&reply).await.is_err() {
                    shared.set_status(Status::Exited("The connection closed".into()));
                    return Ok(());
                }
            }
            msg = input.recv() => {
                let bytes = match msg {
                    Some(Input::Bytes(b)) => tn.encode(&b),
                    Some(Input::Resize { cols, rows }) => tn.resize(cols, rows),
                    None => return Ok(()),
                };
                if !bytes.is_empty() && wr.write_all(&bytes).await.is_err() {
                    shared.set_status(Status::Exited("The connection closed".into()));
                    return Ok(());
                }
            }
        }
    }
}
