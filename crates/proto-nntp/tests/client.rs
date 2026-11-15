use proto_nntp::testserver::{Server, start};
use proto_nntp::{Error, Nntp};

const A1: &[u8] = b"From: Ada <ada@example.org>\r\nNewsgroups: comp.lang.rust\r\nSubject: Hello\r\nDate: Tue, 2 Jan 2024 03:04:05 +0000\r\nMessage-ID: <a1@example.org>\r\n\r\nFirst post.\r\n.hidden dot line\r\n";

async fn connect(srv: Server) -> Nntp {
    let addr = start(srv).await;
    let s = tokio::net::TcpStream::connect(addr).await.unwrap();
    Nntp::start(Box::new(s)).await.unwrap()
}

#[tokio::test]
async fn read_and_post() {
    let srv = Server::new();
    srv.group("comp.lang.rust", "The Rust language");
    srv.group("alt.test", "Testing");
    srv.deliver("comp.lang.rust", A1);
    let mut n = connect(srv.clone()).await;
    assert!(n.posting);
    n.capabilities().await.unwrap();
    assert!(n.has("OVER"));
    n.mode_reader().await.unwrap();
    let groups = n.list_active(None).await.unwrap();
    assert_eq!(groups.len(), 2);
    let rust = groups.iter().find(|g| g.name == "comp.lang.rust").unwrap();
    assert_eq!((rust.low, rust.high), (1, 1));
    let desc = n.list_newsgroups(None).await.unwrap();
    assert!(desc.contains(&("alt.test".into(), "Testing".into())));
    let sel = n.group("comp.lang.rust").await.unwrap();
    assert_eq!(sel.count, 1);
    let over = n.over(sel.low, sel.high).await.unwrap();
    assert_eq!(over[0].subject, "Hello");
    assert_eq!(over[0].message_id, "<a1@example.org>");
    let art = n.article("1").await.unwrap();
    assert_eq!(art, A1);
    assert_eq!(n.article("<a1@example.org>").await.unwrap(), A1);
    assert!(n.head("1").await.unwrap().ends_with(b"Message-ID: <a1@example.org>\r\n"));
    let reply = b"From: Me <me@example.org>\r\nNewsgroups: comp.lang.rust\r\nSubject: Re: Hello\r\nReferences: <a1@example.org>\r\nMessage-ID: <r1@example.org>\r\n\r\n.Dots survive.\n";
    n.post(reply).await.unwrap();
    let over = n.over(1, 10).await.unwrap();
    assert_eq!(over.len(), 2);
    assert_eq!(over[1].parents(), ["<a1@example.org>"]);
    assert!(String::from_utf8_lossy(&n.article("2").await.unwrap()).contains("\r\n.Dots survive.\r\n"));
    assert!(matches!(n.group("no.such").await, Err(Error::Reply { code: 411, .. })));
    n.quit().await;
}

#[tokio::test]
async fn sign_in() {
    let mut srv = Server::new();
    srv.login = Some(("me".into(), "pw".into()));
    srv.group("alt.test", "Testing");
    let mut n = connect(srv).await;
    assert!(matches!(n.group("alt.test").await, Err(Error::Reply { code: 480, .. })));
    assert!(n.login("me", "wrong").await.is_err());
    n.login("me", "pw").await.unwrap();
    assert_eq!(n.group("alt.test").await.unwrap().count, 0);
}
