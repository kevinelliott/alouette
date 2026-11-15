//! A client against the in-process server: registration with SASL, a channel, history,
//! and a private message to the echo bot.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use proto_irc::client::{self, Command};
use proto_irc::session::{Config, Event, Kind, Sasl, Session};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

async fn connect(
    port: u16,
    nick: &str,
    sasl: Sasl,
) -> (Arc<Mutex<Session>>, mpsc::UnboundedSender<Command>, mpsc::UnboundedReceiver<Event>) {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let session = Arc::new(Mutex::new(Session::new(Config { nick: nick.into(), sasl, ..Default::default() })));
    let (ctx, crx) = mpsc::unbounded_channel();
    let (etx, erx) = mpsc::unbounded_channel();
    let s = session.clone();
    tokio::spawn(async move {
        let _ = client::run(stream, s, crx, move |e| {
            let _ = etx.send(e);
        })
        .await;
    });
    (session, ctx, erx)
}

async fn until(rx: &mut mpsc::UnboundedReceiver<Event>, f: impl Fn(&Event) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = rx.recv().await.expect("events ended");
            if f(&e) {
                return e;
            }
        }
    })
    .await
    .expect("timed out waiting for an event")
}

fn send(tx: &mpsc::UnboundedSender<Command>, s: &Arc<Mutex<Session>>, buffer: Option<&str>, line: &str) {
    let t = proto_irc::command::typed(&s.lock().unwrap(), buffer, line).unwrap();
    tx.send(Command::Send(t.out.send)).unwrap();
}

#[tokio::test]
async fn talks_to_the_test_server() {
    let (port, state) = proto_irc::testserver::start().await;
    let (sa, ta, mut ea) = connect(port, "ada", Sasl::Plain { user: "me".into(), pass: "pw".into() }).await;
    until(&mut ea, |e| matches!(e, Event::SaslDone { .. })).await;
    until(&mut ea, |e| matches!(e, Event::Registered { .. })).await;
    assert_eq!(state.lock().unwrap().account("ada").as_deref(), Some("me"));
    assert!(sa.lock().unwrap().caps.contains("echo-message"));

    send(&ta, &sa, None, "/join #alouette");
    until(&mut ea, |e| matches!(e, Event::Joined(c) if c == "#alouette")).await;
    send(&ta, &sa, Some("#alouette"), "first!");
    // echo-message: our own line comes back from the server.
    let e = until(&mut ea, |e| matches!(e, Event::Line { line, .. } if line.own)).await;
    assert!(matches!(e, Event::Line { line, .. } if line.text == "first!"));

    // A second person with a wrong password still gets on, without an account.
    let (sb, tb, mut eb) = connect(port, "ada", Sasl::Plain { user: "me".into(), pass: "nope".into() }).await;
    until(&mut eb, |e| matches!(e, Event::SaslFailed(_))).await;
    let Event::Registered { nick } = until(&mut eb, |e| matches!(e, Event::Registered { .. })).await else { panic!() };
    assert_eq!(nick, "ada_");
    send(&tb, &sb, None, "/join #alouette");
    until(&mut eb, |e| matches!(e, Event::Users { users, .. } if users.len() == 2)).await;
    // History from before we joined arrives marked as history.
    let e = until(&mut eb, |e| matches!(e, Event::Line { line, .. } if line.text == "first!")).await;
    assert!(matches!(e, Event::Line { line, .. } if line.history && line.kind == Kind::Message));
    send(&ta, &sa, Some("#alouette"), "hello ada_");
    let e = until(&mut eb, |e| matches!(e, Event::Line { line, .. } if line.text == "hello ada_")).await;
    assert!(matches!(e, Event::Line { line, .. } if line.highlight));

    send(&tb, &sb, None, "/msg echo ping");
    let e = until(&mut eb, |e| matches!(e, Event::Line { line, .. } if line.from == "echo")).await;
    assert!(matches!(e, Event::Line { buffer: Some(b), line } if b == "echo" && line.text == "You said: ping"));

    tb.send(Command::Quit("bye".into())).unwrap();
    until(&mut ea, |e| matches!(e, Event::Line { line, .. } if line.kind == Kind::Quit)).await;
}
