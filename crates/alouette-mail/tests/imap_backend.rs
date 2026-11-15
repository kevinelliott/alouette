//! The IMAP and SMTP account against the in-memory servers.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alouette_core::config::{MailAccount, MailKind};
use alouette_mail::alouette_store_like::{Memory, Secrets};
use alouette_mail::imap::ImapSmtp;
use alouette_mail::{Account, Backend, Draft, Flags, Prompt, Role, Tls};

struct Bot {
    answers: Mutex<Vec<String>>,
    asked: Mutex<usize>,
}

impl Prompt for Bot {
    fn ask(
        &self,
        _t: &str,
        _b: Vec<String>,
        _p: &str,
        _s: bool,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        *self.asked.lock().unwrap() += 1;
        let a = self.answers.lock().unwrap().pop();
        Box::pin(async move { a })
    }
    fn note(&self, _: &str) {}
}

const MSG: &[u8] = b"From: Ada <ada@example.org>\r\nTo: me@example.org\r\nSubject: Hello there\r\nDate: Tue, 2 Jan 2024 03:04:05 +0000\r\nMessage-ID: <a1@example.org>\r\n\r\nHi.\r\n";

async fn setup() -> (proto_imap::testserver::Server, proto_smtp::testserver::Server, Account) {
    let imap = proto_imap::testserver::Server::new("me@example.org", "pw");
    let smtp = proto_smtp::testserver::Server::new("me@example.org", "pw");
    let ia = proto_imap::testserver::start(imap.clone()).await;
    let sa = proto_smtp::testserver::start(smtp.clone()).await;
    let cfg = MailAccount {
        name: "test".into(),
        email: "me@example.org".into(),
        kind: MailKind::Imap,
        imap: Some(format!("127.0.0.1:{}", ia.port())),
        smtp: Some(format!("127.0.0.1:{}", sa.port())),
        ..Default::default()
    };
    let mut acct = Account::from_config(&cfg).unwrap();
    acct.imap.as_mut().unwrap().tls = Tls::None;
    acct.smtp.as_mut().unwrap().tls = Tls::None;
    (imap, smtp, acct)
}

fn net() -> alouette_net::Net {
    alouette_net::Net::new(&alouette_core::config::Network::default()).unwrap()
}

#[tokio::test]
async fn read_flag_move_send_and_draft() {
    let (imap, smtp, acct) = setup().await;
    imap.deliver(MSG);
    imap.deliver(MSG);
    let secrets: Arc<dyn Secrets> = Arc::new(Memory::default());
    // Wrong password first: it asks again, then remembers the right one.
    let bot = Arc::new(Bot { answers: Mutex::new(vec!["pw".into(), "wrong".into()]), asked: Mutex::new(0) });
    let b = ImapSmtp::connect(net(), acct.clone(), secrets.clone(), bot.clone()).await.unwrap();
    assert_eq!(*bot.asked.lock().unwrap(), 2);
    assert_eq!(secrets.get("mail/test/password").as_deref(), Some("pw"));
    assert!(b.badge().starts_with("IMAP"));

    let folders = b.folders().await.unwrap();
    assert_eq!(folders[0].role, Role::Inbox);
    assert_eq!((folders[0].unread, folders[0].total), (2, 2));
    assert!(folders.iter().any(|f| f.role == Role::Sent && f.name == "Sent"));

    let list = b.list("INBOX", 50).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, "2", "newest first");
    assert_eq!((list[0].from.as_str(), list[0].subject.as_str()), ("Ada", "Hello there"));
    assert!(!list[0].flags.seen);

    let raw = b.fetch("INBOX", "2").await.unwrap();
    assert_eq!(raw, MSG);
    let after = b.list("INBOX", 50).await.unwrap();
    assert!(after[0].flags.seen, "opening marks it read");

    b.set_flags("INBOX", &["1".into()], Flags { flagged: true, ..Default::default() }, true).await.unwrap();
    assert!(b.list("INBOX", 50).await.unwrap()[1].flags.flagged);

    b.move_to("INBOX", &["1".into()], "Trash").await.unwrap();
    assert_eq!(b.list("INBOX", 50).await.unwrap().len(), 1);
    assert_eq!(b.list("Trash", 50).await.unwrap().len(), 1);
    b.expunge("Trash", &["1".into()]).await.unwrap();
    assert!(b.list("Trash", 50).await.unwrap().is_empty());

    let p = alouette_mail::message::parse(&raw);
    let mut d = Draft::reply(&p, false, "me@example.org");
    d.body = format!("Hello back{}", d.body);
    let out = d.build(None, "me@example.org", "example.org").unwrap();
    b.send(&out, "me@example.org", &d.recipients()).await.unwrap();
    let sent = smtp.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, ["ada@example.org"]);
    assert_eq!(b.list("Sent", 10).await.unwrap()[0].subject, "Re: Hello there");

    b.save_draft(&out).await.unwrap();
    assert!(b.list("Drafts", 10).await.unwrap()[0].flags.draft);
}

#[tokio::test]
async fn waits_for_new_mail() {
    let (imap, _smtp, acct) = setup().await;
    let secrets: Arc<dyn Secrets> = Arc::new(Memory::default());
    secrets.set("mail/test/password", "pw");
    let bot = Arc::new(Bot { answers: Mutex::new(vec![]), asked: Mutex::new(0) });
    let b = ImapSmtp::connect(net(), acct, secrets, bot.clone()).await.unwrap();
    assert_eq!(*bot.asked.lock().unwrap(), 0, "the saved password was used");
    let i2 = imap.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        i2.deliver(MSG);
    });
    assert!(b.wait("INBOX", Duration::from_secs(5)).await.unwrap());
    // The main connection still works while another waits.
    assert_eq!(b.list("INBOX", 5).await.unwrap().len(), 1);
}
