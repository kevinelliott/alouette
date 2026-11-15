//! The Chat connection logic against the test IRC server: SASL with a password the
//! person types, autojoin, a reconnect after the server drops us, and /quit.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alouette_core::config::{Chat, IrcNetwork};
use alouette_store::secrets::{MemoryStore, SecretStore};
use alouette_ui::ask::Asker;
use mod_chat::conn::{self, Ctx};
use mod_chat::hub::{BufKind, Hub, Network, State};

async fn wait(hub: &Arc<Mutex<Hub>>, what: &str, f: impl Fn(&Hub) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f(&hub.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn connects_signs_in_and_joins() {
    let (port, server) = proto_irc::testserver::start().await;
    let cfg = IrcNetwork {
        name: "test".into(),
        host: format!("127.0.0.1:{port}"),
        nick: "ada".into(),
        sasl: "plain".into(),
        account: "me".into(),
        channels: vec!["#alouette".into()],
        ..Default::default()
    };
    let hub = Arc::new(Mutex::new(Hub::default()));
    hub.lock().unwrap().nets.push(Network::from_irc(&cfg));
    let asker = Asker::new(|| {});
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let ctx = Ctx {
        hub: hub.clone(),
        net: alouette_net::Net::new(&Default::default()).unwrap(),
        secrets: secrets.clone(),
        asker: asker.clone(),
        data_dir: std::env::temp_dir(),
        chat: Chat::default(),
        proxy: None,
    };
    conn::connect(&ctx, "test");
    // The password is asked for, then kept.
    tokio::time::timeout(Duration::from_secs(5), async {
        while asker.question().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    asker.answer(Some("pw".into()));
    wait(&hub, "joining", |h| {
        let n = &h.nets[0];
        n.state == State::Online
            && n.find("#alouette").is_some_and(|i| n.buffers[i].joined && n.buffers[i].kind == BufKind::Channel)
    })
    .await;
    assert_eq!(secrets.get("irc/test/sasl").unwrap().as_deref(), Some("pw"));
    assert_eq!(server.lock().unwrap().account("ada").as_deref(), Some("me"));
    assert_eq!(server.lock().unwrap().members("#alouette"), ["ada"]);

    conn::disconnect(&hub, "test", "bye");
    wait(&hub, "disconnecting", |h| h.nets[0].state == State::Off).await;
    wait(&hub, "the server to see us go", |_| server.lock().unwrap().nicks().is_empty()).await;
    // Connecting again uses the kept password and rejoins.
    conn::connect(&ctx, "test");
    wait(&hub, "rejoining", |h| h.nets[0].state == State::Online).await;
    wait(&hub, "the channel", |_| server.lock().unwrap().members("#alouette") == ["ada"]).await;
    assert!(asker.question().is_none());
}
