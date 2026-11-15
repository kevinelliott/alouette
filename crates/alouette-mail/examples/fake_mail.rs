//! In-memory IMAP and SMTP servers for trying the Mail window by hand.
//!
//! `cargo run -p alouette-mail --example fake_mail` prints the ports. Log in as
//! `me@example.org` with password `pw`, and set `ALOUETTE_MAIL_PLAIN=1` so Alouette
//! talks to them without TLS.

use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let imap = proto_imap::testserver::Server::new("me@example.org", "pw");
    let smtp = proto_smtp::testserver::Server::new("me@example.org", "pw");
    let people =
        [("Ada Lovelace", "ada@example.org"), ("Grace Hopper", "grace@example.org"), ("Alan Kay", "alan@example.org")];
    for i in 0..12 {
        let (n, a) = people[i % 3];
        let raw = format!(
            "From: {n} <{a}>\r\nTo: me@example.org\r\nSubject: Note number {i}\r\nDate: Tue, {} Jan 2024 10:0{}:00 +0000\r\nMessage-ID: <m{i}@example.org>\r\n\r\nHello,\r\n\r\nThis is message {i}. It has a few lines of text so the reader has something to show.\r\n\r\n-- \r\n{n}\r\n",
            i + 1,
            i % 10
        );
        imap.deliver(raw.as_bytes());
    }
    let ia = proto_imap::testserver::start(imap.clone()).await;
    let sa = proto_smtp::testserver::start(smtp.clone()).await;
    println!("imap = \"127.0.0.1:{}\"\nsmtp = \"127.0.0.1:{}\"", ia.port(), sa.port());
    let mut seen = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let sent = smtp.sent.lock().unwrap();
        for s in sent.iter().skip(seen) {
            println!("SENT from {} to {:?}:\n{}", s.from, s.to, String::from_utf8_lossy(&s.data));
        }
        seen = sent.len();
    }
}
