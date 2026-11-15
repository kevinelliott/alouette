//! The SSH backend for a terminal window.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use alouette_net::Net;
use alouette_ssh::forward::{self, LocalForward};
use alouette_ssh::{Ask, Options, Prompter, SshConfig};
use alouette_term::{Input, Question, Shared, Status, Wire};
use russh::ChannelMsg;

/// Asks questions in the terminal window and writes notes into it.
pub struct TermPrompter(pub Arc<Shared>);

impl Prompter for TermPrompter {
    fn ask(&self, q: Ask) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(self.0.ask(Question {
            title: q.title,
            body: q.body,
            prompt: q.prompt,
            secret: q.secret,
            yes_no: q.yes_no,
        }))
    }
    fn note(&self, text: &str) {
        self.0.note(text);
    }
}

pub struct SshJob {
    pub net: Net,
    pub config: SshConfig,
    pub dest: String,
    pub options: Options,
    pub term: String,
    /// Extra `-L` forwards on top of the host's LocalForward lines.
    pub forwards: Vec<String>,
}

pub async fn run(wire: Wire, job: SshJob) -> Result<(), String> {
    let Wire { shared, mut input } = wire;
    shared.set_status(Status::Connecting(format!("Connecting to {}", job.dest)));
    let prompter: Arc<dyn Prompter> = Arc::new(TermPrompter(shared.clone()));
    let session = match alouette_ssh::connect(&job.net, &job.config, &job.dest, &job.options, prompter).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            let msg = e.to_string();
            shared.note(&format!("Could not connect: {msg}"));
            shared.set_status(Status::Exited("Not connected".into()));
            return Err(msg);
        }
    };
    shared.set_badge(session.badge.clone());

    let mut forwards = session.host.local_forwards.clone();
    forwards.extend(job.forwards.iter().cloned());
    let mut tasks = vec![];
    for spec in forwards {
        match LocalForward::parse(&spec) {
            Ok(f) => {
                shared.note(&format!("Forwarding {}:{} to {}:{} on the server", f.bind, f.port, f.host, f.host_port));
                let s2 = shared.clone();
                let sess = session.clone();
                tasks.push(tokio::spawn(async move {
                    if let Err(e) = forward::run_local(sess, f, |_| {}).await {
                        s2.note(&format!("Forward stopped: {e}"));
                    }
                }));
            }
            Err(e) => shared.note(&e),
        }
    }

    let (rows, cols) = {
        let p = shared.parser();
        let (r, c) = p.screen().size();
        (r.get(), c.get())
    };
    let ch = match session.shell(&job.term, cols, rows).await {
        Ok(c) => c,
        Err(e) => {
            shared.set_status(Status::Exited(format!("The server refused a shell: {e}")));
            return Err(e.to_string());
        }
    };
    shared.set_status(Status::Running);
    let (mut rd, wr) = ch.split();
    let mut exit: Option<u32> = None;
    let result = loop {
        tokio::select! {
            msg = rd.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    let replies = shared.feed(&data);
                    if !replies.is_empty() {
                        let _ = wr.data_bytes(replies).await;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    shared.note(&format!("Killed by signal {signal_name:?}"));
                }
                Some(ChannelMsg::Close) | None => {
                    let why = match exit {
                        Some(0) | None => format!("Connection to {} closed", session.host.hostname),
                        Some(n) => format!("The remote shell exited with status {n}"),
                    };
                    shared.set_status(Status::Exited(why));
                    break Ok(());
                }
                _ => {}
            },
            msg = input.recv() => match msg {
                Some(Input::Bytes(b)) => { let _ = wr.data_bytes(b).await; }
                Some(Input::Resize { cols, rows }) => {
                    let _ = wr.window_change(u32::from(cols), u32::from(rows), 0, 0).await;
                }
                None => {
                    let _ = wr.close().await;
                    break Ok(());
                }
            }
        }
    };
    for t in tasks {
        t.abort();
    }
    let _ = session.handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    result
}
