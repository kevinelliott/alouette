//! JMAP for mail (RFC 8620, 8621, and 8621's submission), over Alouette's HTTP client.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alouette_net::Net;
use alouette_net::http::{Body, Sink};
use base64::Engine;
use serde_json::{Value, json};

use crate::account::Account;
use crate::alouette_store_like::Secrets;
use crate::message::Summary;
use crate::{Backend, BoxFut, Flags, Folder, Prompt, Role};

const USING: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"];

pub struct Jmap {
    net: Net,
    auth: String,
    api: url::Url,
    download: String,
    upload: String,
    account_id: String,
    host: String,
    badge: Mutex<String>,
    roles: Mutex<Vec<(Role, String)>>,
}

fn get<'a>(v: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(v, |v, k| &v[*k])
}

impl Jmap {
    pub async fn connect(
        net: Net,
        acct: Account,
        secrets: Arc<dyn Secrets>,
        prompt: Arc<dyn Prompt>,
    ) -> crate::Result<Self> {
        let given = acct.jmap.clone().unwrap_or_default();
        let mut url = if given.starts_with("http://") || given.starts_with("https://") {
            given.clone()
        } else {
            format!("https://{}/.well-known/jmap", given.trim_end_matches('/'))
        };
        let key = acct.secret_key("password");
        let mut retry = false;
        for _ in 0..3 {
            let pass = match secrets.get(&key) {
                Some(p) if !retry => p,
                _ => prompt
                    .ask(
                        "Mail password",
                        vec![format!("Password (or app password) for {} on {given}.", acct.user)],
                        "Password",
                        true,
                    )
                    .await
                    .ok_or("cancelled")?,
            };
            let auth =
                format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{pass}", acct.user)));
            let mut hops = 0;
            let r = loop {
                let u = url::Url::parse(&url).map_err(|e| format!("{url}: {e}"))?;
                let r = net
                    .request(
                        &u,
                        "GET",
                        &[("Authorization", &auth), ("Accept", "application/json")],
                        Body::Empty,
                        Sink::Memory(4 << 20),
                    )
                    .await
                    .map_err(|e| format!("{url}: {e}"))?;
                if matches!(r.status, 301 | 302 | 307 | 308) && hops < 5 {
                    let loc = r.header("location").ok_or("a redirect with nowhere to go")?;
                    url = u.join(loc).map_err(|e| e.to_string())?.to_string();
                    hops += 1;
                    continue;
                }
                break r;
            };
            if r.status == 401 {
                retry = true;
                continue;
            }
            if !r.ok() {
                return Err(format!("{url}: HTTP {}", r.status));
            }
            let s: Value = serde_json::from_str(&r.text()).map_err(|e| format!("{url}: not a JMAP session ({e})"))?;
            let account_id = get(&s, &["primaryAccounts", "urn:ietf:params:jmap:mail"])
                .as_str()
                .ok_or("the server has no mail account for you")?
                .to_string();
            let base = url::Url::parse(&url).map_err(|e| e.to_string())?;
            let api = base.join(s["apiUrl"].as_str().ok_or("no apiUrl")?).map_err(|e| e.to_string())?;
            secrets.set(&key, &pass);
            let host = api.host_str().unwrap_or_default().to_string();
            return Ok(Self {
                badge: Mutex::new(format!("JMAP · {}", r.info.badge())),
                net,
                auth,
                download: s["downloadUrl"].as_str().unwrap_or_default().to_string(),
                upload: s["uploadUrl"].as_str().unwrap_or_default().to_string(),
                account_id,
                api,
                host,
                roles: Mutex::new(vec![]),
            });
        }
        Err("the server did not accept the password".into())
    }

    fn fill(&self, template: &str, blob: &str) -> crate::Result<url::Url> {
        let t = template
            .replace("{accountId}", &self.account_id)
            .replace("{blobId}", blob)
            .replace("{type}", "message%2Frfc822")
            .replace("{name}", "message.eml");
        self.api.join(&t).map_err(|e| e.to_string())
    }

    /// Run method calls; returns the responses, failing on any error response.
    async fn call(&self, calls: Value) -> crate::Result<Vec<Value>> {
        let body = json!({ "using": USING, "methodCalls": calls }).to_string();
        let r = self
            .net
            .request(
                &self.api,
                "POST",
                &[("Authorization", &self.auth), ("Content-Type", "application/json"), ("Accept", "application/json")],
                Body::Bytes(body.as_bytes()),
                Sink::Memory(32 << 20),
            )
            .await
            .map_err(|e| e.to_string())?;
        if !r.ok() {
            return Err(format!("JMAP: HTTP {} {}", r.status, r.text().chars().take(200).collect::<String>()));
        }
        let v: Value = serde_json::from_str(&r.text()).map_err(|e| format!("JMAP: {e}"))?;
        let out: Vec<Value> = v["methodResponses"].as_array().cloned().unwrap_or_default();
        for m in &out {
            if m[0] == "error" {
                return Err(format!(
                    "JMAP: {} {}",
                    m[1]["type"].as_str().unwrap_or("error"),
                    m[1]["description"].as_str().unwrap_or_default()
                ));
            }
            for k in ["notCreated", "notUpdated", "notDestroyed"] {
                if let Some(o) = m[1][k].as_object()
                    && let Some((_, e)) = o.iter().next()
                {
                    return Err(format!(
                        "JMAP: {} {}",
                        e["type"].as_str().unwrap_or("error"),
                        e["description"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
        Ok(out)
    }

    async fn upload_blob(&self, raw: &[u8]) -> crate::Result<String> {
        let u = self.fill(&self.upload, "")?;
        let r = self
            .net
            .request(
                &u,
                "POST",
                &[("Authorization", &self.auth), ("Content-Type", "message/rfc822")],
                Body::Bytes(raw),
                Sink::Memory(1 << 20),
            )
            .await
            .map_err(|e| e.to_string())?;
        if !r.ok() {
            return Err(format!("upload: HTTP {}", r.status));
        }
        let v: Value = serde_json::from_str(&r.text()).map_err(|e| e.to_string())?;
        v["blobId"].as_str().map(str::to_string).ok_or_else(|| "upload: no blobId".into())
    }

    fn role(&self, r: Role) -> Option<String> {
        self.roles.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(x, _)| *x == r).map(|(_, id)| id.clone())
    }

    async fn role_id(&self, r: Role) -> crate::Result<String> {
        if self.role(r).is_none() {
            self.folders().await?;
        }
        self.role(r).ok_or_else(|| format!("the account has no {r:?} folder"))
    }

    async fn import(&self, raw: &[u8], mailbox: &str, keywords: Value) -> crate::Result<String> {
        let blob = self.upload_blob(raw).await?;
        let r = self
            .call(json!([["Email/import", { "accountId": self.account_id, "emails": { "e": { "blobId": blob, "mailboxIds": { mailbox: true }, "keywords": keywords } } }, "0"]]))
            .await?;
        r[0][1]["created"]["e"]["id"].as_str().map(str::to_string).ok_or_else(|| "import: no id".into())
    }
}

fn keywords(f: Flags, on: bool) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let v = if on { Value::Bool(true) } else { Value::Null };
    for (set, k) in [(f.seen, "$seen"), (f.flagged, "$flagged"), (f.answered, "$answered"), (f.draft, "$draft")] {
        if set {
            m.insert(format!("keywords/{k}"), v.clone());
        }
    }
    m
}

fn addr(v: &Value) -> (String, String) {
    let a = &v[0];
    let email = a["email"].as_str().unwrap_or_default().to_string();
    let name = a["name"].as_str().filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| email.clone());
    (name, email)
}

impl Backend for Jmap {
    fn badge(&self) -> String {
        self.badge.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn folders(&self) -> BoxFut<'_, Vec<Folder>> {
        Box::pin(async move {
            let r = self.call(json!([["Mailbox/get", { "accountId": self.account_id, "ids": null }, "0"]])).await?;
            let list = r[0][1]["list"].as_array().cloned().unwrap_or_default();
            let name_of = |id: &str| -> String {
                let mut parts = vec![];
                let mut cur = Some(id.to_string());
                let mut depth = 0;
                while let Some(c) = cur.take() {
                    let Some(m) = list.iter().find(|m| m["id"] == c.as_str()) else { break };
                    parts.push(m["name"].as_str().unwrap_or_default().to_string());
                    cur = m["parentId"].as_str().map(str::to_string);
                    depth += 1;
                    if depth > 20 {
                        break;
                    }
                }
                parts.reverse();
                parts.join("/")
            };
            let mut out: Vec<Folder> = list
                .iter()
                .map(|m| {
                    let id = m["id"].as_str().unwrap_or_default().to_string();
                    Folder {
                        name: name_of(&id),
                        role: m["role"].as_str().map(Role::from_flag).unwrap_or(Role::Other),
                        unread: m["unreadEmails"].as_u64().unwrap_or(0) as u32,
                        total: m["totalEmails"].as_u64().unwrap_or(0) as u32,
                        id,
                    }
                })
                .collect();
            out.sort_by(|a, b| a.role.cmp(&b.role).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            *self.roles.lock().unwrap_or_else(|e| e.into_inner()) =
                out.iter().filter(|f| f.role != Role::Other).map(|f| (f.role, f.id.clone())).collect();
            Ok(out)
        })
    }

    fn list<'a>(&'a self, folder: &'a str, limit: usize) -> BoxFut<'a, Vec<Summary>> {
        Box::pin(async move {
            let r = self
                .call(json!([
                    ["Email/query", { "accountId": self.account_id, "filter": { "inMailbox": folder }, "sort": [{ "property": "receivedAt", "isAscending": false }], "limit": limit }, "q"],
                    ["Email/get", { "accountId": self.account_id, "#ids": { "resultOf": "q", "name": "Email/query", "path": "/ids" },
                        "properties": ["id", "from", "to", "subject", "receivedAt", "sentAt", "keywords", "size"] }, "g"]
                ]))
                .await?;
            let list = r.get(1).map(|m| m[1]["list"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
            Ok(list
                .iter()
                .map(|e| {
                    let (from, from_addr) = addr(&e["from"]);
                    let kw = &e["keywords"];
                    let date = e["sentAt"].as_str().or(e["receivedAt"].as_str()).and_then(|d| {
                        time::OffsetDateTime::parse(d, &time::format_description::well_known::Rfc3339).ok()
                    });
                    Summary {
                        id: e["id"].as_str().unwrap_or_default().to_string(),
                        from,
                        from_addr,
                        to: e["to"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|x| x["email"].as_str()).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default(),
                        subject: e["subject"].as_str().unwrap_or("(no subject)").to_string(),
                        date: date.map(|d| d.unix_timestamp()),
                        flags: Flags {
                            seen: kw["$seen"] == true,
                            flagged: kw["$flagged"] == true,
                            answered: kw["$answered"] == true,
                            draft: kw["$draft"] == true,
                        },
                        size: e["size"].as_u64(),
                    }
                })
                .collect())
        })
    }

    fn fetch<'a>(&'a self, _folder: &'a str, id: &'a str) -> BoxFut<'a, Vec<u8>> {
        Box::pin(async move {
            let r = self.call(json!([["Email/get", { "accountId": self.account_id, "ids": [id], "properties": ["blobId", "keywords"] }, "0"]])).await?;
            let blob = r[0][1]["list"][0]["blobId"].as_str().ok_or("the message is gone")?.to_string();
            let u = self.fill(&self.download, &blob)?;
            let resp = self
                .net
                .request(&u, "GET", &[("Authorization", &self.auth)], Body::Empty, Sink::Memory(64 << 20))
                .await
                .map_err(|e| e.to_string())?;
            if !resp.ok() {
                return Err(format!("download: HTTP {}", resp.status));
            }
            // Opening a message marks it read, as with IMAP.
            let _ = self.call(json!([["Email/set", { "accountId": self.account_id, "update": { id: { "keywords/$seen": true } } }, "0"]])).await;
            Ok(resp.body)
        })
    }

    fn set_flags<'a>(&'a self, _folder: &'a str, ids: &'a [String], flags: Flags, on: bool) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let patch = Value::Object(keywords(flags, on));
            let update: serde_json::Map<String, Value> = ids.iter().map(|i| (i.clone(), patch.clone())).collect();
            self.call(json!([["Email/set", { "accountId": self.account_id, "update": update }, "0"]])).await.map(|_| ())
        })
    }

    fn move_to<'a>(&'a self, _folder: &'a str, ids: &'a [String], to: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let update: serde_json::Map<String, Value> =
                ids.iter().map(|i| (i.clone(), json!({ "mailboxIds": { to: true } }))).collect();
            self.call(json!([["Email/set", { "accountId": self.account_id, "update": update }, "0"]])).await.map(|_| ())
        })
    }

    fn expunge<'a>(&'a self, _folder: &'a str, ids: &'a [String]) -> BoxFut<'a, ()> {
        Box::pin(async move {
            self.call(json!([["Email/set", { "accountId": self.account_id, "destroy": ids }, "0"]])).await.map(|_| ())
        })
    }

    fn send<'a>(&'a self, raw: &'a [u8], from: &'a str, to: &'a [String]) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let sent = self.role_id(Role::Sent).await?;
            let r = self.call(json!([["Identity/get", { "accountId": self.account_id, "ids": null }, "0"]])).await?;
            let ids = r[0][1]["list"].as_array().cloned().unwrap_or_default();
            let identity = ids
                .iter()
                .find(|i| i["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(from)))
                .or(ids.first())
                .and_then(|i| i["id"].as_str())
                .ok_or("the server has no sending identity for you")?
                .to_string();
            let email = self.import(raw, &sent, json!({ "$seen": true })).await?;
            let rcpt: Vec<Value> = to.iter().map(|t| json!({ "email": t })).collect();
            self.call(json!([["EmailSubmission/set", { "accountId": self.account_id, "create": { "s": {
                "identityId": identity, "emailId": email,
                "envelope": { "mailFrom": { "email": from }, "rcptTo": rcpt } } } }, "0"]]))
                .await
                .map(|_| ())
        })
    }

    fn save_draft<'a>(&'a self, raw: &'a [u8]) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let drafts = self.role_id(Role::Drafts).await?;
            self.import(raw, &drafts, json!({ "$seen": true, "$draft": true })).await.map(|_| ())
        })
    }

    fn wait<'a>(&'a self, _folder: &'a str, max: Duration) -> BoxFut<'a, bool> {
        // Push (EventSource) can come later; a quiet poll is enough to start.
        let _ = &self.host;
        Box::pin(async move {
            tokio::time::sleep(max.min(Duration::from_secs(120))).await;
            Ok(true)
        })
    }
}
