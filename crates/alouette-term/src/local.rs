//! A shell on this computer, through a pseudo-terminal (ConPTY on Windows).

use std::io::{Read, Write};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::oneshot;

use crate::emu::{Input, Status, Wire};

/// The shell to run when none is configured.
pub fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    } else {
        std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
    }
}

/// Run `program` (split on spaces; the first word is the command) until it exits or the
/// window closes.
pub async fn run(wire: Wire, program: &str, term: &str) -> Result<(), String> {
    let Wire { shared, mut input } = wire;
    let size = {
        let p = shared.parser();
        let (r, c) = p.screen().size();
        PtySize { rows: r.get(), cols: c.get(), pixel_width: 0, pixel_height: 0 }
    };
    let pair = native_pty_system().openpty(size).map_err(|e| format!("could not open a terminal: {e}"))?;
    let mut words = program.split_whitespace();
    let mut cmd = CommandBuilder::new(words.next().unwrap_or("sh"));
    cmd.args(words);
    cmd.env("TERM", term);
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "Alouette");
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        cmd.cwd(home);
    }
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| format!("could not start {program}: {e}"))?;
    drop(pair.slave);
    let master = pair.master;
    let mut reader = master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = master.take_writer().map_err(|e| e.to_string())?;
    shared.set_status(Status::Running);
    shared.set_badge(format!("local · {}", program.split_whitespace().next().unwrap_or(program)));

    // Writes happen on their own thread because the pty writer blocks.
    let (wtx, wrx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut w = writer;
        while let Ok(b) = wrx.recv() {
            if w.write_all(&b).and_then(|_| w.flush()).is_err() {
                break;
            }
        }
    });

    let (done_tx, mut done_rx) = oneshot::channel::<()>();
    {
        let shared = shared.clone();
        let wtx = wtx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let replies = shared.feed(&buf[..n]);
                        if !replies.is_empty() {
                            let _ = wtx.send(replies);
                        }
                    }
                }
            }
            let _ = done_tx.send(());
        });
    }
    let mut killer = child.clone_killer();
    let (exit_tx, mut exit_rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = exit_tx.send(child.wait());
    });

    loop {
        tokio::select! {
            msg = input.recv() => match msg {
                Some(Input::Bytes(b)) => { let _ = wtx.send(b); }
                Some(Input::Resize { cols, rows }) => {
                    let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                }
                None => {
                    // The window closed.
                    let _ = killer.kill();
                    return Ok(());
                }
            },
            status = &mut exit_rx => {
                // Let the reader drain what the program printed last.
                let _ = tokio::time::timeout(std::time::Duration::from_millis(300), &mut done_rx).await;
                let why = match status {
                    Ok(Ok(s)) if s.success() => "The shell exited".to_string(),
                    Ok(Ok(s)) => format!("The shell exited with status {}", s.exit_code()),
                    _ => "The shell stopped".to_string(),
                };
                shared.set_status(Status::Exited(why));
                return Ok(());
            }
        }
    }
}
