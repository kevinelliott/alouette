//! Sign in, sync, read the room and send a message through the SDK against the test
//! homeserver; then restore the saved session without a password.

use std::time::Duration;

use alouette_matrix::{Account, Cmd, Options, Stop, Update, run, testserver};
use tokio::sync::mpsc;

async fn until(rx: &mut mpsc::UnboundedReceiver<Update>, f: impl Fn(&Update) -> bool) -> Update {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let u = rx.recv().await.expect("updates ended");
            if f(&u) {
                return u;
            }
        }
    })
    .await
    .expect("timed out")
}

#[tokio::test(flavor = "multi_thread")]
async fn signs_in_syncs_and_sends() {
    let (url, state) = testserver::start().await;
    let dir = std::env::temp_dir().join(format!("alouette-matrix-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let account = Account { name: "test".into(), homeserver: url.clone(), user: "@me:localhost".into() };
    let opts = |session: Option<String>, password: Option<String>| Options {
        account: account.clone(),
        store: dir.join("store"),
        passphrase: "secret".into(),
        session,
        password,
        proxy: None,
    };

    // No session and no password: ask for one.
    let (tx, _rx) = mpsc::unbounded_channel();
    let (_ctx, crx) = mpsc::unbounded_channel();
    assert_eq!(run(opts(None, None), tx, crx).await, Stop::NeedPassword);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ctx, crx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(opts(None, Some("pw".into())), tx, crx));
    let Update::Ready { session, user_id, .. } = until(&mut rx, |u| matches!(u, Update::Ready { .. })).await else {
        unreachable!()
    };
    assert_eq!(user_id, "@me:localhost");
    let session = session.expect("a new session to keep");
    let welcome =
        until(&mut rx, |u| matches!(u, Update::Message { msg, .. } if msg.text == "Welcome to the lobby")).await;
    assert!(matches!(welcome, Update::Message { msg, .. } if msg.history && msg.from == "Echo"));
    let rooms = until(&mut rx, |u| matches!(u, Update::Rooms(r) if !r.is_empty())).await;
    assert!(matches!(&rooms, Update::Rooms(r) if r[0].name == "Lobby" && r[0].topic.starts_with("Say")));

    ctx.send(Cmd::Send { room: testserver::ROOM.into(), text: "hello".into(), emote: false }).unwrap();
    let own = until(&mut rx, |u| matches!(u, Update::Message { msg, .. } if msg.text == "hello")).await;
    assert!(matches!(own, Update::Message { msg, .. } if msg.own && !msg.history));
    let reply = until(&mut rx, |u| matches!(u, Update::Message { msg, .. } if msg.text == "You said: hello")).await;
    assert!(matches!(reply, Update::Message { msg, .. } if !msg.own));
    assert_eq!(state.0.lock().unwrap().sent, ["hello"]);
    ctx.send(Cmd::Members(testserver::ROOM.into())).unwrap();
    let m = until(&mut rx, |u| matches!(u, Update::Members { .. })).await;
    assert!(matches!(m, Update::Members { users, .. } if users.len() == 2));
    ctx.send(Cmd::Quit).unwrap();
    assert_eq!(task.await.unwrap(), Stop::Quit);

    // The saved session signs in again without a password.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ctx, crx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(opts(Some(session), None), tx, crx));
    let ready = until(&mut rx, |u| matches!(u, Update::Ready { .. })).await;
    assert!(matches!(ready, Update::Ready { session: None, .. }));
    ctx.send(Cmd::Quit).unwrap();
    assert_eq!(task.await.unwrap(), Stop::Quit);
    let _ = std::fs::remove_dir_all(&dir);
}
