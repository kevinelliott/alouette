//! A tiny Matrix homeserver for tests and live checks. It knows one user, `@me:localhost`
//! with password `pw`, and one room, `#lobby:localhost`, where a bot called Echo repeats
//! what it hears. Encryption endpoints answer with empty results, so the SDK starts, but
//! nothing here is encrypted.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

pub const ROOM: &str = "!lobby:localhost";
pub const USER: &str = "@me:localhost";

#[derive(Default)]
pub struct State {
    /// Timeline events of the room, in order.
    pub events: Vec<Value>,
    pub sent: Vec<String>,
    next: u64,
}

pub type Shared = Arc<(Mutex<State>, Notify)>;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn push(st: &mut State, sender: &str, kind: &str, content: Value, state_key: Option<&str>) {
    st.next += 1;
    let mut ev = json!({
        "type": kind,
        "sender": sender,
        "content": content,
        "event_id": format!("$e{}:localhost", st.next),
        "origin_server_ts": now(),
    });
    if let Some(k) = state_key {
        ev["state_key"] = json!(k);
    }
    st.events.push(ev);
}

fn sync_body(st: &State, since: usize) -> Value {
    let timeline: Vec<Value> = st.events.iter().skip(since).cloned().collect();
    let mut rooms = json!({});
    if !timeline.is_empty() || since == 0 {
        rooms = json!({
            "join": {
                ROOM: {
                    "timeline": { "events": timeline, "limited": false },
                    "state": { "events": [] },
                    "summary": { "m.joined_member_count": 2, "m.heroes": ["@echo:localhost"] },
                    "unread_notifications": { "notification_count": 0, "highlight_count": 0 },
                }
            }
        });
    }
    json!({ "next_batch": st.events.len().to_string(), "rooms": rooms })
}

async fn handle(sh: &Shared, method: &str, path: &str, body: &str) -> (u16, Value) {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let q =
        |k: &str| query.split('&').find_map(|p| p.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v.to_string()));
    let p = path.trim_start_matches("/_matrix/client");
    match (method, p) {
        ("GET", "/versions") => (
            200,
            json!({ "versions": ["v1.1", "v1.2", "v1.3", "v1.4", "v1.5", "v1.6", "v1.7", "v1.8", "v1.9", "v1.10", "v1.11", "v1.12"] }),
        ),
        ("GET", "/v3/login") => (200, json!({ "flows": [{ "type": "m.login.password" }] })),
        ("POST", "/v3/login") => {
            let v: Value = serde_json::from_str(body).unwrap_or_default();
            let user = v["identifier"]["user"].as_str().or(v["user"].as_str()).unwrap_or("");
            if v["password"] == "pw" && (user == USER || user == "me") {
                (200, json!({ "user_id": USER, "access_token": "tok", "device_id": "ALOUETTE1" }))
            } else {
                (403, json!({ "errcode": "M_FORBIDDEN", "error": "Invalid username or password" }))
            }
        }
        ("GET", "/v3/account/whoami") => (200, json!({ "user_id": USER, "device_id": "ALOUETTE1" })),
        ("GET", "/v3/sync") => {
            let since: usize = q("since").and_then(|s| s.parse().ok()).unwrap_or(0);
            let timeout: u64 = q("timeout").and_then(|s| s.parse().ok()).unwrap_or(0).min(2000);
            let has_new = { sh.0.lock().unwrap().events.len() > since };
            if !has_new && since > 0 {
                let _ = tokio::time::timeout(Duration::from_millis(timeout), sh.1.notified()).await;
            }
            let st = sh.0.lock().unwrap();
            (200, sync_body(&st, since))
        }
        ("POST", "/v3/keys/upload") => (200, json!({ "one_time_key_counts": { "signed_curve25519": 50 } })),
        ("POST", "/v3/keys/query") => (200, json!({ "device_keys": {}, "failures": {} })),
        ("POST", "/v3/keys/claim") => (200, json!({ "one_time_keys": {}, "failures": {} })),
        ("POST", p) if p.ends_with("/filter") => (200, json!({ "filter_id": "1" })),
        ("PUT", p) if p.contains("/sendToDevice/") => (200, json!({})),
        ("PUT", p) if p.contains("/send/m.room.message/") => {
            let content: Value = serde_json::from_str(body).unwrap_or_default();
            let text = content["body"].as_str().unwrap_or("").to_string();
            let id = {
                let mut st = sh.0.lock().unwrap();
                push(&mut st, USER, "m.room.message", content.clone(), None);
                st.sent.push(text.clone());
                let id = st.events.last().map(|e| e["event_id"].clone()).unwrap_or_default();
                push(
                    &mut st,
                    "@echo:localhost",
                    "m.room.message",
                    json!({ "msgtype": "m.text", "body": format!("You said: {text}") }),
                    None,
                );
                id
            };
            sh.1.notify_waiters();
            (200, json!({ "event_id": id }))
        }
        ("GET", p) if p.ends_with("/members") => {
            let st = sh.0.lock().unwrap();
            let chunk: Vec<Value> = st.events.iter().filter(|e| e["type"] == "m.room.member").cloned().collect();
            (200, json!({ "chunk": chunk }))
        }
        ("GET", p) if p.ends_with("/messages") => (200, json!({ "chunk": [], "start": "0" })),
        ("GET", p) if p.ends_with("/joined_members") => (200, json!({ "joined": {} })),
        ("POST", p) if p.starts_with("/v3/join/") => (200, json!({ "room_id": ROOM })),
        ("GET", p) if p.starts_with("/v3/directory/room/") => {
            (200, json!({ "room_id": ROOM, "servers": ["localhost"] }))
        }
        ("GET", "/v3/pushrules/") | ("GET", "/v3/pushrules") => (200, json!({ "global": {} })),
        ("GET", "/v3/capabilities") => (200, json!({ "capabilities": {} })),
        _ => (404, json!({ "errcode": "M_NOT_FOUND", "error": format!("{method} {path} is not here") })),
    }
}

/// Start on a free port. Returns the homeserver URL and the state.
pub async fn start() -> (String, Shared) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let sh: Shared = Arc::new((Mutex::new(State::default()), Notify::new()));
    {
        let mut st = sh.0.lock().unwrap();
        push(&mut st, USER, "m.room.create", json!({ "creator": USER, "room_version": "10" }), Some(""));
        push(&mut st, USER, "m.room.member", json!({ "membership": "join", "displayname": "Me" }), Some(USER));
        push(
            &mut st,
            "@echo:localhost",
            "m.room.member",
            json!({ "membership": "join", "displayname": "Echo" }),
            Some("@echo:localhost"),
        );
        push(&mut st, USER, "m.room.name", json!({ "name": "Lobby" }), Some(""));
        push(&mut st, USER, "m.room.topic", json!({ "topic": "Say something; Echo answers" }), Some(""));
        push(
            &mut st,
            "@echo:localhost",
            "m.room.message",
            json!({ "msgtype": "m.text", "body": "Welcome to the lobby" }),
            None,
        );
    }
    let s2 = sh.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = l.accept().await else { break };
            let sh = s2.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let (head, body) = loop {
                    let Ok(n) = sock.read(&mut tmp).await else { return };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).into_owned();
                        let len: usize = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok())?
                            })
                            .unwrap_or(0);
                        let mut body = buf[p + 4..].to_vec();
                        while body.len() < len {
                            let Ok(n) = sock.read(&mut tmp).await else { return };
                            if n == 0 {
                                break;
                            }
                            body.extend_from_slice(&tmp[..n]);
                        }
                        break (head, String::from_utf8_lossy(&body).into_owned());
                    }
                };
                let mut first = head.lines().next().unwrap_or("").split(' ');
                let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
                let (status, v) = handle(&sh, method, path, &body).await;
                let out = v.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (url, sh)
}
