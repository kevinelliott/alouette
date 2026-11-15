//! The JMAP account against a small in-memory JMAP server.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use alouette_core::config::{MailAccount, MailKind};
use alouette_mail::alouette_store_like::{Memory, Secrets};
use alouette_mail::jmap::Jmap;
use alouette_mail::{Account, Backend, Flags, Prompt, Role};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

struct NoAsk;
impl Prompt for NoAsk {
    fn ask(
        &self,
        _: &str,
        _: Vec<String>,
        _: &str,
        _: bool,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(async { None })
    }
    fn note(&self, _: &str) {}
}

/// Mailbox ids, keywords and the raw message.
type Email = (Vec<String>, BTreeMap<String, bool>, Vec<u8>);

#[derive(Default)]
struct World {
    /// id -> (mailbox ids, keywords, raw)
    emails: BTreeMap<String, Email>,
    blobs: BTreeMap<String, Vec<u8>>,
    submissions: Vec<Value>,
    next: u32,
}

const MSG: &[u8] = b"From: Ada <ada@example.org>\r\nTo: me@example.org\r\nSubject: Via JMAP\r\n\r\nHello.\r\n";

fn handle(w: &mut World, method: &str, path: &str, body: &[u8], port: u16) -> (u16, Vec<u8>) {
    let j = |v: Value| (200, v.to_string().into_bytes());
    match (method, path) {
        ("GET", "/.well-known/jmap") => (307, format!("http://127.0.0.1:{port}/session").into_bytes()),
        ("GET", "/session") => j(json!({
            "apiUrl": "/api", "downloadUrl": "/dl/{accountId}/{blobId}/{name}?type={type}", "uploadUrl": "/up/{accountId}",
            "primaryAccounts": { "urn:ietf:params:jmap:mail": "A1" }
        })),
        ("POST", p) if p.starts_with("/up/") => {
            w.next += 1;
            let id = format!("b{}", w.next);
            w.blobs.insert(id.clone(), body.to_vec());
            j(json!({ "blobId": id }))
        }
        ("GET", p) if p.starts_with("/dl/") => {
            let blob = p.split('/').nth(3).unwrap_or_default();
            let raw = w
                .emails
                .values()
                .find(|e| format!("blob-{}", String::from_utf8_lossy(&e.2).len()) == blob)
                .map(|e| e.2.clone());
            match raw.or_else(|| w.blobs.get(blob).cloned()) {
                Some(b) => (200, b),
                None => (404, vec![]),
            }
        }
        ("POST", "/api") => {
            let req: Value = serde_json::from_slice(body).unwrap();
            let mut out = vec![];
            let mut last_ids: Vec<String> = vec![];
            for call in req["methodCalls"].as_array().unwrap() {
                let (name, args, tag) = (call[0].as_str().unwrap(), &call[1], call[2].clone());
                let res = match name {
                    "Mailbox/get" => {
                        let boxes = [
                            ("m1", "Inbox", "inbox"),
                            ("m2", "Sent", "sent"),
                            ("m3", "Drafts", "drafts"),
                            ("m4", "Trash", "trash"),
                        ];
                        let list: Vec<Value> = boxes
                            .iter()
                            .map(|(id, n, r)| {
                                let total = w.emails.values().filter(|e| e.0.iter().any(|m| m == id)).count();
                                let unread = w.emails.values().filter(|e| e.0.iter().any(|m| m == id) && !e.1.contains_key("$seen")).count();
                                json!({ "id": id, "name": n, "role": r, "parentId": null, "totalEmails": total, "unreadEmails": unread })
                            })
                            .collect();
                        json!({ "list": list })
                    }
                    "Email/query" => {
                        let mb = args["filter"]["inMailbox"].as_str().unwrap();
                        last_ids = w
                            .emails
                            .iter()
                            .rev()
                            .filter(|(_, e)| e.0.iter().any(|m| m == mb))
                            .map(|(k, _)| k.clone())
                            .collect();
                        json!({ "ids": last_ids })
                    }
                    "Email/get" => {
                        let ids: Vec<String> = if args.get("#ids").is_some() {
                            last_ids.clone()
                        } else {
                            args["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
                        };
                        let list: Vec<Value> = ids
                            .iter()
                            .filter_map(|id| w.emails.get(id).map(|e| (id, e)))
                            .map(|(id, e)| json!({
                                "id": id, "blobId": format!("blob-{}", e.2.len()), "subject": "Via JMAP",
                                "from": [{ "name": "Ada", "email": "ada@example.org" }], "to": [{ "email": "me@example.org" }],
                                "receivedAt": "2024-01-02T03:04:05Z", "keywords": e.1, "size": e.2.len()
                            }))
                            .collect();
                        json!({ "list": list })
                    }
                    "Email/set" => {
                        if let Some(u) = args["update"].as_object() {
                            for (id, patch) in u {
                                let e = w.emails.get_mut(id).unwrap();
                                for (k, v) in patch.as_object().unwrap() {
                                    if let Some(kw) = k.strip_prefix("keywords/") {
                                        if v.is_null() {
                                            e.1.remove(kw);
                                        } else {
                                            e.1.insert(kw.into(), true);
                                        }
                                    } else if k == "mailboxIds" {
                                        e.0 = v.as_object().unwrap().keys().cloned().collect();
                                    }
                                }
                            }
                        }
                        if let Some(d) = args["destroy"].as_array() {
                            for id in d {
                                w.emails.remove(id.as_str().unwrap());
                            }
                        }
                        json!({})
                    }
                    "Email/import" => {
                        let e = &args["emails"]["e"];
                        let raw = w.blobs[e["blobId"].as_str().unwrap()].clone();
                        w.next += 1;
                        let id = format!("e{:03}", w.next);
                        let kws = e["keywords"].as_object().unwrap().keys().map(|k| (k.clone(), true)).collect();
                        w.emails.insert(
                            id.clone(),
                            (e["mailboxIds"].as_object().unwrap().keys().cloned().collect(), kws, raw),
                        );
                        json!({ "created": { "e": { "id": id } } })
                    }
                    "Identity/get" => json!({ "list": [{ "id": "i1", "email": "me@example.org" }] }),
                    "EmailSubmission/set" => {
                        w.submissions.push(args["create"]["s"].clone());
                        json!({ "created": { "s": { "id": "s1" } } })
                    }
                    _ => json!({ "type": "unknownMethod" }),
                };
                out.push(json!([name, res, tag]));
            }
            j(json!({ "methodResponses": out }))
        }
        _ => (404, vec![]),
    }
}

async fn server(world: Arc<Mutex<World>>) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let world = world.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(s);
                let mut line = String::new();
                r.read_line(&mut line).await.unwrap();
                let mut parts = line.split_whitespace();
                let (m, p) =
                    (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
                let (mut len, mut auth) = (0usize, false);
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).await.unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    // me@example.org:pw
                    if lower.starts_with("authorization: basic bwvazxhhbxbszs5vcmc6chc") {
                        auth = true;
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).await.unwrap();
                let path = p.split('?').next().unwrap_or_default().to_string();
                let (status, out) =
                    if auth { handle(&mut world.lock().unwrap(), &m, &path, &body, port) } else { (401, vec![]) };
                let extra = if status == 307 {
                    format!("Location: {}\r\n", String::from_utf8_lossy(&out))
                } else {
                    String::new()
                };
                let mut s = r.into_inner();
                s.write_all(
                    format!("HTTP/1.1 {status} X\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", out.len())
                        .as_bytes(),
                )
                .await
                .unwrap();
                s.write_all(&out).await.unwrap();
            });
        }
    });
    port
}

#[tokio::test]
async fn jmap_round_trip() {
    let world = Arc::new(Mutex::new(World::default()));
    world.lock().unwrap().emails.insert("e000".into(), (vec!["m1".into()], BTreeMap::new(), MSG.to_vec()));
    let port = server(world.clone()).await;
    let cfg = MailAccount {
        name: "j".into(),
        email: "me@example.org".into(),
        kind: MailKind::Jmap,
        jmap: Some(format!("http://127.0.0.1:{port}/.well-known/jmap")),
        ..Default::default()
    };
    let acct = Account::from_config(&cfg).unwrap();
    let secrets: Arc<dyn Secrets> = Arc::new(Memory::default());
    secrets.set("mail/j/password", "pw");
    let net = alouette_net::Net::new(&alouette_core::config::Network::default()).unwrap();
    let b = Jmap::connect(net, acct, secrets, Arc::new(NoAsk)).await.unwrap();
    assert!(b.badge().starts_with("JMAP"));

    let f = b.folders().await.unwrap();
    assert_eq!((f[0].role, f[0].unread), (Role::Inbox, 1));
    let l = b.list("m1", 10).await.unwrap();
    assert_eq!((l[0].from.as_str(), l[0].subject.as_str(), l[0].date), ("Ada", "Via JMAP", Some(1704164645)));
    assert_eq!(b.fetch("m1", "e000").await.unwrap(), MSG);
    assert!(b.list("m1", 10).await.unwrap()[0].flags.seen);

    b.set_flags("m1", &["e000".into()], Flags { flagged: true, ..Default::default() }, true).await.unwrap();
    assert!(b.list("m1", 10).await.unwrap()[0].flags.flagged);
    b.move_to("m1", &["e000".into()], "m4").await.unwrap();
    assert!(b.list("m1", 10).await.unwrap().is_empty());
    b.expunge("m4", &["e000".into()]).await.unwrap();

    b.send(MSG, "me@example.org", &["ada@example.org".into()]).await.unwrap();
    assert_eq!(world.lock().unwrap().submissions[0]["envelope"]["rcptTo"][0]["email"], "ada@example.org");
    assert_eq!(b.list("m2", 10).await.unwrap().len(), 1);
    b.save_draft(MSG).await.unwrap();
    assert!(b.list("m3", 10).await.unwrap()[0].flags.draft);
}
