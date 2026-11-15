//! The News connection against the in-memory NNTP server.

use std::sync::Arc;

use alouette_core::config::{Network, NewsServer};
use alouette_store::secrets::{MemoryStore, SecretStore};
use alouette_ui::ask::Asker;
use mod_news::conn::Conn;
use proto_nntp::testserver::{Server, start};

#[tokio::test]
async fn reads_posts_and_signs_in() {
    let mut srv = Server::new();
    srv.login = Some(("me".into(), "pw".into()));
    srv.group("alt.test", "Testing");
    srv.deliver("alt.test", b"From: a@x\r\nNewsgroups: alt.test\r\nSubject: One\r\nMessage-ID: <1@x>\r\n\r\nBody\r\n");
    let addr = start(srv.clone()).await;
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    secrets.set("news/local/password", "pw").unwrap();
    let server = NewsServer {
        name: "local".into(),
        host: format!("127.0.0.1:{}", addr.port()),
        user: Some("me".into()),
        groups: vec![],
    };
    let net = alouette_net::Net::new(&Network::default()).unwrap();
    let conn = Conn::new(net, server, secrets.clone(), Asker::new(|| {}));
    assert!(conn.connect().await.unwrap());
    assert!(conn.badge().starts_with("NNTP"));
    let groups = conn.groups().await.unwrap();
    assert_eq!(groups[0].name, "alt.test");
    let (sel, over) = conn.overview("alt.test", 100).await.unwrap();
    assert_eq!((sel.count, over.len()), (1, 1));
    let raw = conn.article("alt.test", "1").await.unwrap();
    assert!(raw.ends_with(b"Body\r\n"));
    let seed = mod_news::post::Seed {
        groups: "alt.test".into(),
        subject: "Re: One".into(),
        body: "Reply".into(),
        references: vec!["<1@x>".into()],
    };
    conn.post(&mod_news::post::article(&seed, None, "me@example.org").unwrap()).await.unwrap();
    let (_, over) = conn.overview("alt.test", 100).await.unwrap();
    assert_eq!(over.len(), 2);
    assert_eq!(over[1].parents(), ["<1@x>"]);
    conn.close().await;
}
