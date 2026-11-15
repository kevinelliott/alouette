//! Providers spoken to over HTTP: Ollama, OpenAI-compatible servers, Anthropic and Gemini.

use alouette_net::Net;
use alouette_net::http::{Body, Sink};
use serde_json::{Value, json};

use crate::{Kind, Provider, Reply, Request, Role};

const MAX_BODY: usize = 8 * 1024 * 1024;
/// The Anthropic API version this code was written against.
pub const ANTHROPIC_VERSION: &str = "2026-10-21";

/// The URL, headers and JSON body for a request. Kept apart from sending so tests can check it.
pub fn build(p: &Provider, req: &Request) -> (String, Vec<(String, String)>, Value) {
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    let key = p.key.clone().unwrap_or_default();
    let max = req.max_tokens.max(1);
    match p.kind {
        Kind::Ollama => {
            let mut msgs = vec![];
            if !req.system.is_empty() {
                msgs.push(json!({ "role": "system", "content": req.system }));
            }
            msgs.extend(req.messages.iter().map(|(r, t)| json!({ "role": role(*r), "content": t })));
            let body =
                json!({ "model": p.model, "messages": msgs, "stream": false, "options": { "num_predict": max } });
            (format!("{}/api/chat", p.base), headers, body)
        }
        Kind::OpenAi => {
            if !key.is_empty() {
                headers.push(("Authorization".into(), format!("Bearer {key}")));
            }
            let mut msgs = vec![];
            if !req.system.is_empty() {
                msgs.push(json!({ "role": "system", "content": req.system }));
            }
            msgs.extend(req.messages.iter().map(|(r, t)| json!({ "role": role(*r), "content": t })));
            let body = json!({ "model": p.model, "messages": msgs, "max_tokens": max });
            (format!("{}/chat/completions", p.base), headers, body)
        }
        Kind::Anthropic => {
            headers.push(("x-api-key".into(), key));
            headers.push(("anthropic-version".into(), ANTHROPIC_VERSION.into()));
            // Let the API retry on another model when the first one declines.
            headers.push(("anthropic-beta".into(), "server-side-fallback-2026-07-01".into()));
            let msgs: Vec<Value> =
                req.messages.iter().map(|(r, t)| json!({ "role": role(*r), "content": t })).collect();
            let mut body = json!({ "model": p.model, "max_tokens": max, "messages": msgs, "fallbacks": "default" });
            if !req.system.is_empty() {
                body["system"] = json!(req.system);
            }
            (format!("{}/v1/messages", p.base), headers, body)
        }
        Kind::Gemini => {
            headers.push(("x-goog-api-key".into(), key));
            let contents: Vec<Value> = req
                .messages
                .iter()
                .map(|(r, t)| {
                    json!({ "role": if *r == Role::User { "user" } else { "model" }, "parts": [{ "text": t }] })
                })
                .collect();
            let mut body = json!({ "contents": contents, "generationConfig": { "maxOutputTokens": max } });
            if !req.system.is_empty() {
                body["systemInstruction"] = json!({ "parts": [{ "text": req.system }] });
            }
            (format!("{}/v1beta/models/{}:generateContent", p.base, p.model), headers, body)
        }
        _ => (String::new(), headers, Value::Null),
    }
}

fn role(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// Read an answer out of a provider's JSON.
pub fn parse(kind: Kind, model: &str, v: &Value) -> Result<Reply, String> {
    let n = |x: &Value| x.as_u64().unwrap_or(0);
    let mut r = Reply { model: model.to_string(), ..Reply::default() };
    match kind {
        Kind::Ollama => {
            r.text = v["message"]["content"].as_str().unwrap_or("").to_string();
            r.input_tokens = n(&v["prompt_eval_count"]);
            r.output_tokens = n(&v["eval_count"]);
        }
        Kind::OpenAi => {
            let c = &v["choices"][0];
            r.text = c["message"]["content"].as_str().unwrap_or("").to_string();
            r.input_tokens = n(&v["usage"]["prompt_tokens"]);
            r.output_tokens = n(&v["usage"]["completion_tokens"]);
            if c["finish_reason"] == "content_filter" && r.text.is_empty() {
                return Err("the provider declined to answer (content filter)".into());
            }
        }
        Kind::Anthropic => {
            r.input_tokens = n(&v["usage"]["input_tokens"]);
            r.output_tokens = n(&v["usage"]["output_tokens"]);
            if let Some(m) = v["model"].as_str() {
                r.model = m.to_string();
            }
            if v["stop_reason"] == "refusal" {
                let why = v["stop_details"]["explanation"].as_str().unwrap_or("no reason given");
                return Err(format!("the model declined to answer: {why}"));
            }
            r.text = v["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            if v["stop_reason"] == "max_tokens" {
                r.text.push_str("\n\n[cut off: the answer reached ai.max_tokens]");
            }
        }
        Kind::Gemini => {
            let c = &v["candidates"][0];
            r.text = c["content"]["parts"]
                .as_array()
                .map(|p| p.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join(""))
                .unwrap_or_default();
            r.input_tokens = n(&v["usageMetadata"]["promptTokenCount"]);
            r.output_tokens = n(&v["usageMetadata"]["candidatesTokenCount"]);
            if r.text.is_empty()
                && let Some(why) = c["finishReason"].as_str().or(v["promptFeedback"]["blockReason"].as_str())
            {
                return Err(format!("the model gave no answer ({why})"));
            }
        }
        _ => return Err("not an HTTP provider".into()),
    }
    if r.text.trim().is_empty() {
        return Err("the provider sent an empty answer".into());
    }
    Ok(r)
}

/// The error message in a provider's error JSON, if there is one.
fn error_text(v: &Value) -> Option<String> {
    v["error"]["message"].as_str().or(v["error"].as_str()).or(v["message"].as_str()).map(str::to_string)
}

pub async fn ask(p: &Provider, net: &Net, req: &Request) -> Result<Reply, String> {
    let (url, headers, body) = build(p, req);
    let url = url::Url::parse(&url).map_err(|e| format!("{url}: {e}"))?;
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let resp =
        net.request(&url, "POST", &h, Body::Bytes(&bytes), Sink::Memory(MAX_BODY)).await.map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
    if !resp.ok() {
        let msg = error_text(&v).unwrap_or_else(|| resp.text().chars().take(200).collect());
        return Err(match resp.status {
            401 | 403 => format!("the provider refused the key ({}): {msg}", resp.status),
            429 => format!("rate limited, try again shortly: {msg}"),
            s => format!("HTTP {s}: {msg}"),
        });
    }
    parse(p.kind, &p.model, &v)
}

/// Models an Ollama server has, for the setup screen.
pub async fn ollama_models(net: &Net, base: &str) -> Result<Vec<String>, String> {
    let resp = net
        .get(&format!("{}/api/tags", base.trim_end_matches('/')), &[], 1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status));
    }
    let v: Value = serde_json::from_slice(&resp.body).map_err(|e| e.to_string())?;
    Ok(v["models"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(kind: Kind, model: &str) -> Provider {
        Provider {
            kind,
            model: model.into(),
            base: kind.default_base().unwrap().into(),
            key: Some("KEY".into()),
            command: vec![],
        }
    }

    #[test]
    fn anthropic_shape() {
        let (url, h, b) = build(&prov(Kind::Anthropic, "claude-opus-5-5"), &Request::new("be brief", "hi"));
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert!(h.iter().any(|(k, v)| k == "x-api-key" && v == "KEY"));
        assert!(h.iter().any(|(k, v)| k == "anthropic-version" && v == ANTHROPIC_VERSION));
        assert_eq!(b["system"], "be brief");
        assert_eq!(b["messages"][0]["content"], "hi");
        let ok = json!({"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"thinking","thinking":""},{"type":"text","text":"Hello"}],"usage":{"input_tokens":10,"output_tokens":2}});
        let r = parse(Kind::Anthropic, "x", &ok).unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens, r.output_tokens), ("Hello", 10, 2));
        let no = json!({"stop_reason":"refusal","stop_details":{"explanation":"nope"},"content":[],"usage":{}});
        assert!(parse(Kind::Anthropic, "x", &no).unwrap_err().contains("nope"));
    }

    #[test]
    fn others() {
        let (url, h, b) = build(&prov(Kind::OpenAi, "m"), &Request::new("s", "u"));
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert!(h.iter().any(|(k, v)| k == "Authorization" && v == "Bearer KEY"));
        assert_eq!(b["messages"][0]["role"], "system");
        let r = parse(
            Kind::OpenAi,
            "m",
            &json!({"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}}),
        )
        .unwrap();
        assert_eq!(r.text, "ok");

        let (url, _, b) = build(&prov(Kind::Gemini, "g"), &Request::new("s", "u"));
        assert!(url.ends_with("/v1beta/models/g:generateContent"));
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "s");
        let r = parse(
            Kind::Gemini,
            "g",
            &json!({"candidates":[{"content":{"parts":[{"text":"a"},{"text":"b"}]}}],"usageMetadata":{"promptTokenCount":4}}),
        )
        .unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens), ("ab", 4));

        let (url, _, b) = build(&prov(Kind::Ollama, "llama"), &Request::new("", "u"));
        assert_eq!(url, "http://127.0.0.1:11434/api/chat");
        assert_eq!(b["stream"], false);
        let r = parse(Kind::Ollama, "llama", &json!({"message":{"content":"yo"},"eval_count":2})).unwrap();
        assert_eq!(r.output_tokens, 2);
        assert!(parse(Kind::Ollama, "llama", &json!({"message":{"content":""}})).is_err());
    }
}
