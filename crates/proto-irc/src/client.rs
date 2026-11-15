//! Drive a session over a byte stream: read lines, answer, write what the window queues,
//! keep the connection alive and stay under flood limits.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::message::Message;
use crate::session::{Event, Session};

pub enum Command {
    Send(Vec<Message>),
    Quit(String),
}

/// Read a line as UTF-8, or as Latin-1 when it isn't (old clients and servers).
pub fn decode(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

/// A token bucket: a burst of five lines, then two a second.
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn wait(&mut self) -> Duration {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * 2.0).min(5.0);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / 2.0)
        }
    }
}

/// Run until the server closes, the connection fails or a `Quit` arrives. Events go to
/// `on`, which may lock the session too: it is never held across that call.
pub async fn run<S>(
    stream: S,
    session: Arc<Mutex<Session>>,
    mut cmds: mpsc::UnboundedReceiver<Command>,
    mut on: impl FnMut(Event) + Send,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);
    let mut queue: VecDeque<Message> = session.lock().map_err(|e| e.to_string())?.start().into();
    let mut bucket = Bucket { tokens: 5.0, at: Instant::now() };
    let mut buf = Vec::with_capacity(1024);
    let mut last_read = Instant::now();
    let mut pinged = false;
    let quitting = false;
    loop {
        // Write what we can without blocking reads for long.
        while let Some(m) = queue.front() {
            let registering = !session.lock().map(|s| s.registered).unwrap_or(true);
            let pause = if registering || m.command == "PONG" { Duration::ZERO } else { bucket.wait() };
            if !pause.is_zero() {
                break;
            }
            let line = format!("{}\r\n", m.to_line());
            w.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
            queue.pop_front();
        }
        w.flush().await.map_err(|e| e.to_string())?;
        let next_write = if queue.is_empty() { Duration::from_secs(3600) } else { Duration::from_millis(100) };
        let idle = last_read.elapsed();
        let keepalive = Duration::from_secs(if pinged { 180 } else { 120 }).saturating_sub(idle);
        buf.clear();
        tokio::select! {
            n = r.read_until(b'\n', &mut buf) => {
                let n = n.map_err(|e| e.to_string())?;
                if n == 0 {
                    return if quitting { Ok(()) } else { Err("The server closed the connection".into()) };
                }
                last_read = Instant::now();
                pinged = false;
                let line = decode(&buf);
                let Some(m) = Message::parse(&line) else { continue };
                let out = session.lock().map_err(|e| e.to_string())?.handle(&m);
                queue.extend(out.send);
                for e in out.events {
                    on(e);
                }
                if m.command == "ERROR" && quitting {
                    return Ok(());
                }
            }
            c = cmds.recv() => match c {
                Some(Command::Send(ms)) => queue.extend(ms),
                Some(Command::Quit(why)) => {
                    let why = if why.is_empty() { "Alouette".to_string() } else { why };
                    let line = format!("{}\r\n", Message::new("QUIT", &[&why]).to_line());
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.flush().await;
                    // Give the server a moment to say goodbye.
                    let _ = tokio::time::timeout(Duration::from_secs(2), async {
                        loop {
                            buf.clear();
                            if r.read_until(b'\n', &mut buf).await.map(|n| n == 0).unwrap_or(true) {
                                break;
                            }
                        }
                    }).await;
                    return Ok(());
                }
                None => return Ok(()),
            },
            _ = tokio::time::sleep(next_write) => {}
            _ = tokio::time::sleep(keepalive) => {
                if pinged {
                    return Err("The connection timed out".into());
                }
                pinged = true;
                queue.push_front(Message::new("PING", &["alouette"]));
            }
        }
    }
}
