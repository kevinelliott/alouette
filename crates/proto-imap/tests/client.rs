//! The client against the in-memory test server.

use std::time::Duration;

use proto_imap::testserver::{self, Server};
use proto_imap::{Change, Imap};

const MSG: &[u8] = b"From: Ada <ada@example.org>\r\nTo: me@example.org\r\nSubject: Hello\r\n\r\nHi there.\r\n";

async fn connect(srv: &Server) -> Imap {
    let addr = testserver::start(srv.clone()).await;
    let s = tokio::net::TcpStream::connect(addr).await.unwrap();
    Imap::start(Box::new(s)).await.unwrap()
}

#[tokio::test]
async fn login_list_fetch_and_flags() {
    let srv = Server::new("me", "pä ss\"word");
    srv.deliver(MSG);
    srv.deliver(MSG);
    let mut c = connect(&srv).await;
    assert!(c.has("IDLE") && c.has("MOVE"));
    assert!(c.login("me", "wrong").await.is_err());
    c.login("me", "pä ss\"word").await.unwrap();

    let boxes = c.list().await.unwrap();
    let sent = boxes.iter().find(|b| b.special_use() == Some("\\Sent")).unwrap();
    assert_eq!(sent.name, "Sent");
    assert_eq!(c.status("INBOX").await.unwrap(), (2, 2));

    let sel = c.select("INBOX").await.unwrap();
    assert_eq!((sel.exists, sel.uidvalidity, sel.uidnext), (2, 42, 3));
    assert_eq!(c.uid_search("ALL").await.unwrap(), [1, 2]);

    let heads = c.uid_fetch("1:*", "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])").await.unwrap();
    assert_eq!(heads.len(), 2);
    assert!(heads[0].body().unwrap().starts_with(b"From: Ada"));
    assert_eq!(heads[0].size, Some(MSG.len() as u64));

    let full = c.uid_fetch("2", "(UID BODY.PEEK[])").await.unwrap();
    assert_eq!(full[0].body().unwrap(), MSG);

    c.uid_store("1", true, &["\\Seen"]).await.unwrap();
    assert_eq!(c.uid_search("UNSEEN").await.unwrap(), [2]);

    c.uid_move("1", "Archive").await.unwrap();
    assert_eq!(c.uid_search("ALL").await.unwrap(), [2]);
    assert_eq!(c.status("Archive").await.unwrap(), (0, 1));
    c.logout().await;
}

#[tokio::test]
async fn sasl_plain_append_and_idle() {
    let srv = Server::new("me", "pw");
    let mut c = connect(&srv).await;
    c.authenticate_plain("me", "pw").await.unwrap();
    c.append("Sent", &["\\Seen"], MSG).await.unwrap();
    assert_eq!(c.status("Sent").await.unwrap(), (0, 1));

    c.select("INBOX").await.unwrap();
    let s2 = srv.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        s2.deliver(MSG);
    });
    assert_eq!(c.idle(Duration::from_secs(5)).await.unwrap(), Change::Exists(1));
    assert_eq!(c.idle(Duration::from_millis(200)).await.unwrap(), Change::Timeout);
    // Still usable afterwards.
    assert_eq!(c.uid_search("ALL").await.unwrap(), [1]);
}
