//! A fake model server for tests and live checks. It answers the Ollama, OpenAI, Anthropic
//! and Gemini request shapes with a canned reply that quotes the request, and records what
//! it was sent. Keys: any non-empty key works except `bad`.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Default)]
pub struct State {
    /// (path, body) of every request.
    pub requests: Vec<(String, Value)>,
}

pub type Shared = Arc<Mutex<State>>;

fn last_user(v: &Value) -> String {
    let from_msgs = v["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|x| x["role"] == "user"))
        .map(|x| x["content"].as_str().unwrap_or("").to_string());
    let from_gemini = v["contents"]
        .as_array()
        .and_then(|c| c.last())
        .and_then(|x| x["parts"][0]["text"].as_str())
        .map(str::to_string);
    from_msgs.or(from_gemini).unwrap_or_default()
}

fn answer(prompt: &str) -> String {
    let first = prompt.lines().next().unwrap_or("").chars().take(60).collect::<String>();
    if prompt.starts_with("Summarize") {
        "The message asks to meet on Friday.\n- Lunch at noon\n- Reply needed\nTo do: answer by Thursday.".into()
    } else if prompt.starts_with("Draft") {
        "Hi,\n\nFriday works for me. See you at noon.\n\nBest".into()
    } else {
        format!("You asked: {first}")
    }
}

fn handle(sh: &Shared, path: &str, headers: &str, body: &str) -> (u16, Value) {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    sh.lock().unwrap().requests.push((path.to_string(), v.clone()));
    let has = |h: &str| headers.lines().any(|l| l.to_lowercase().starts_with(&format!("{h}:")));
    let bad = headers.lines().any(|l| l.ends_with(" bad") || l.ends_with("Bearer bad"));
    let text = answer(&last_user(&v));
    let words = |s: &str| s.split_whitespace().count() as u64;
    let inp = words(&last_user(&v)) + words(v["system"].as_str().unwrap_or(""));
    if path == "/api/tags" {
        return (200, json!({ "models": [{ "name": "tinyllama:latest" }, { "name": "qwen:0.5b" }] }));
    }
    if path == "/api/chat" {
        return (
            200,
            json!({ "model": v["model"], "message": { "role": "assistant", "content": text }, "done": true, "prompt_eval_count": inp, "eval_count": words(&text) }),
        );
    }
    if bad || (path == "/v1/messages" && !has("x-api-key")) {
        return (401, json!({ "error": { "type": "authentication_error", "message": "invalid x-api-key" } }));
    }
    if path == "/v1/messages" {
        return (
            200,
            json!({
                "id": "msg_1", "type": "message", "role": "assistant", "model": v["model"],
                "content": [{ "type": "text", "text": text }], "stop_reason": "end_turn",
                "usage": { "input_tokens": inp, "output_tokens": words(&text) }
            }),
        );
    }
    if path.ends_with("/chat/completions") {
        return (
            200,
            json!({
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": inp, "completion_tokens": words(&text) }
            }),
        );
    }
    if path.contains(":generateContent") {
        return (
            200,
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": text }] }, "finishReason": "STOP" }],
                "usageMetadata": { "promptTokenCount": inp, "candidatesTokenCount": words(&text) }
            }),
        );
    }
    (404, json!({ "error": { "message": format!("{path} is not here") } }))
}

/// Start on a free port. Returns the base URL and the state.
pub async fn start() -> (String, Shared) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let sh: Shared = Arc::default();
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
                let path = head.lines().next().unwrap_or("").split(' ').nth(1).unwrap_or("").to_string();
                let (status, v) = handle(&sh, &path, &head, &body);
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
