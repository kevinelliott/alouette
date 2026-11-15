//! A tiny fake Mastodon server for tests and the `fake_social` example. The token is `tok`.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type Log = Arc<Mutex<Vec<String>>>;

fn status(id: &str, text: &str, fav: bool) -> String {
    format!(
        r#"{{"id":"{id}","created_at":"2024-01-02T03:04:05.000Z","account":{{"id":"9","acct":"ada","display_name":"Ada"}},"content":"<p>{text}</p>","favourited":{fav},"favourites_count":{}}}"#,
        u8::from(fav)
    )
}

/// Starts the server. Returns its base URL and a log of requests.
/// Answers each request by path and keeps the request lines and bodies.
pub async fn start() -> (String, Log) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let log: Log = Arc::default();
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read the head, then the body by Content-Length.
                let (head, body) = loop {
                    let n = s.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let head = text[..i].to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= i + 4 + len {
                            break (head, String::from_utf8_lossy(&buf[i + 4..i + 4 + len]).to_string());
                        }
                    }
                };
                let line = head.lines().next().unwrap().to_string();
                let authed = head.contains("Authorization: Bearer tok");
                seen.lock().unwrap().push(format!("{line} {body}"));
                let path = line.split(' ').nth(1).unwrap();
                let (code, json) = if path.starts_with("/api/v1/apps") {
                    (200, r#"{"client_id":"cid","client_secret":"sec"}"#.to_string())
                } else if path.starts_with("/oauth/token") {
                    (200, r#"{"access_token":"tok"}"#.to_string())
                } else if !authed {
                    (401, r#"{"error":"The access token is invalid"}"#.to_string())
                } else if path.starts_with("/api/v1/accounts/verify_credentials") {
                    (200, r#"{"id":"7","acct":"me","display_name":"Me"}"#.to_string())
                } else if path.starts_with("/api/v1/timelines/") && path.contains("max_id") {
                    (200, "[]".to_string())
                } else if path.starts_with("/api/v1/timelines/") {
                    (200, format!("[{},{}]", status("2", "Second", false), status("1", "First", false)))
                } else if path == "/api/v1/statuses" {
                    (200, status("3", "Posted", false))
                } else if path.ends_with("/favourite") || path.ends_with("/reblog") || path.ends_with("/unfavourite") {
                    (200, status("2", "Second", true))
                } else if path.ends_with("/context") {
                    (200, format!(r#"{{"ancestors":[{}],"descendants":[]}}"#, status("1", "First", false)))
                } else if path.starts_with("/api/v1/notifications") {
                    (
                        200,
                        format!(
                            r#"[{{"id":"n1","type":"favourite","created_at":"2024-01-02T03:04:05Z","account":{{"acct":"bob"}},"status":{}}}]"#,
                            status("3", "Posted", false)
                        ),
                    )
                } else {
                    (404, r#"{"error":"Record not found"}"#.to_string())
                };
                let reply = format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base, log)
}
