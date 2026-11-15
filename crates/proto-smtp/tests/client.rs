use proto_smtp::Smtp;
use proto_smtp::testserver::{self, Server};

async fn connect(srv: &Server) -> Smtp {
    let addr = testserver::start(srv.clone()).await;
    let s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut c = Smtp::start(Box::new(s)).await.unwrap();
    c.ehlo("client.example").await.unwrap();
    c
}

#[tokio::test]
async fn plain_login_and_send() {
    let srv = Server::new("me@example.org", "pw");
    let mut c = connect(&srv).await;
    assert!(c.has("SMTPUTF8") && c.has("AUTH"));
    assert!(c.login("me@example.org", "nope").await.is_err());
    c.login("me@example.org", "pw").await.unwrap();
    c.send("me@example.org", &["a@example.net", "b@example.net"], b"Subject: hi\n\n.leading dot\nend").await.unwrap();
    c.quit().await;
    let sent = srv.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, ["a@example.net", "b@example.net"]);
    assert_eq!(sent[0].data, b"Subject: hi\r\n\r\n.leading dot\r\nend\r\n");
}

#[tokio::test]
async fn login_mechanism_and_limits() {
    let srv = Server::new("me", "pw");
    let mut c = connect(&srv).await;
    // Force AUTH LOGIN by pretending PLAIN is not offered.
    c.ext.retain(|(k, _)| k != "AUTH");
    c.ext.push(("AUTH".into(), "LOGIN".into()));
    c.login("me", "pw").await.unwrap();
    let big = vec![b'x'; 2_000_000];
    let e = c.send("me", &["x@y"], &big).await.unwrap_err().to_string();
    assert!(e.contains("at most"), "{e}");
    c.send("mé@example.org", &["ü@example.org"], b"Subject: utf8\r\n\r\nok\r\n").await.unwrap();
}
