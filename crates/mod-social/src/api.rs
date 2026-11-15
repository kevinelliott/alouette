//! A Mastodon API client: sign-in, timelines, notifications, threads, posting and the
//! favourite and boost buttons. Works with Mastodon, GoToSocial, Pleroma/Akkoma and
//! other servers that speak the same API.

use alouette_net::Net;
use alouette_net::http::{Body, Sink};
use serde::{Deserialize, Serialize};

pub const SCOPES: &str = "read write";
pub const OOB: &str = "urn:ietf:wg:oauth:2.0:oob";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub id: String,
    pub acct: String,
    pub display_name: String,
    pub url: String,
}

impl Account {
    pub fn name(&self) -> &str {
        if self.display_name.trim().is_empty() { &self.acct } else { &self.display_name }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Media {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub id: String,
    pub created_at: String,
    pub account: Account,
    /// HTML.
    pub content: String,
    pub spoiler_text: String,
    pub url: Option<String>,
    pub in_reply_to_id: Option<String>,
    pub visibility: String,
    pub replies_count: u64,
    pub reblogs_count: u64,
    pub favourites_count: u64,
    pub favourited: Option<bool>,
    pub reblogged: Option<bool>,
    pub reblog: Option<Box<Status>>,
    pub media_attachments: Vec<Media>,
}

impl Status {
    /// The post to show: the boosted one for a boost.
    pub fn shown(&self) -> &Status {
        self.reblog.as_deref().unwrap_or(self)
    }
    pub fn time(&self) -> Option<i64> {
        time::OffsetDateTime::parse(&self.created_at, &time::format_description::well_known::Rfc3339)
            .ok()
            .map(|d| d.unix_timestamp())
    }
    pub fn text(&self) -> String {
        alouette_mail::message::html_to_text(&self.content).trim().to_string()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notification {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub created_at: String,
    pub account: Account,
    pub status: Option<Status>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Context {
    pub ancestors: Vec<Status>,
    pub descendants: Vec<Status>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeline {
    Home,
    Notifications,
    Local,
    Federated,
}

impl Timeline {
    pub const ALL: [Timeline; 4] = [Timeline::Home, Timeline::Notifications, Timeline::Local, Timeline::Federated];
    pub fn label(self) -> &'static str {
        match self {
            Timeline::Home => "Home",
            Timeline::Notifications => "Notifications",
            Timeline::Local => "Local",
            Timeline::Federated => "Federated",
        }
    }
}

/// Who can see a post.
pub const VISIBILITIES: [&str; 4] = ["public", "unlisted", "private", "direct"];

/// The instance's address as a base URL, from `mastodon.social` or a full URL.
pub fn base(instance: &str) -> String {
    let i = instance.trim().trim_end_matches('/');
    let i = i.strip_prefix('@').unwrap_or(i);
    // @user@host: keep the host.
    let i = match i.rsplit_once('@') {
        Some((_, h)) if !i.contains("://") => h,
        _ => i,
    };
    if i.starts_with("http://") || i.starts_with("https://") { i.to_string() } else { format!("https://{i}") }
}

pub struct Client {
    net: Net,
    pub base: String,
    pub token: Option<String>,
}

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()
}

impl Client {
    pub fn new(net: Net, instance: &str, token: Option<String>) -> Self {
        Self { net, base: base(instance), token }
    }

    async fn call(&self, method: &str, path: &str, body: Option<String>) -> Result<serde_json::Value, String> {
        let url = url::Url::parse(&format!("{}{path}", self.base)).map_err(|e| e.to_string())?;
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let mut headers = vec![("Accept", "application/json")];
        if let Some(a) = &auth {
            headers.push(("Authorization", a.as_str()));
        }
        if body.is_some() {
            headers.push(("Content-Type", "application/x-www-form-urlencoded"));
        }
        let b = body.as_deref().map(str::as_bytes);
        let r = self
            .net
            .request(&url, method, &headers, b.map(Body::Bytes).unwrap_or(Body::Empty), Sink::Memory(8 << 20))
            .await
            .map_err(|e| format!("{}: {e}", self.base))?;
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or(serde_json::Value::Null);
        if !r.ok() {
            let msg = v["error_description"]
                .as_str()
                .or(v["error"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("the server said {}", r.status));
            return Err(msg);
        }
        Ok(v)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let v = self.call("GET", path, None).await?;
        serde_json::from_value(v).map_err(|e| format!("unexpected answer from {}: {e}", self.base))
    }

    /// Register Alouette with the instance. Returns (client id, client secret).
    pub async fn register(&self) -> Result<(String, String), String> {
        let v = self
            .call(
                "POST",
                "/api/v1/apps",
                Some(form(&[
                    ("client_name", "Alouette"),
                    ("redirect_uris", OOB),
                    ("scopes", SCOPES),
                    ("website", "https://github.com/kevinelliott/alouette"),
                ])),
            )
            .await?;
        match (v["client_id"].as_str(), v["client_secret"].as_str()) {
            (Some(i), Some(s)) => Ok((i.to_string(), s.to_string())),
            _ => Err("the server did not register the app".into()),
        }
    }

    /// The page where you approve Alouette and get a code to paste back.
    pub fn authorize_url(&self, client_id: &str) -> String {
        format!(
            "{}/oauth/authorize?{}",
            self.base,
            form(&[("client_id", client_id), ("response_type", "code"), ("redirect_uri", OOB), ("scope", SCOPES)])
        )
    }

    /// Trade the code for an access token.
    pub async fn token(&self, client_id: &str, secret: &str, code: &str) -> Result<String, String> {
        let v = self
            .call(
                "POST",
                "/oauth/token",
                Some(form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", client_id),
                    ("client_secret", secret),
                    ("redirect_uri", OOB),
                    ("code", code.trim()),
                    ("scope", SCOPES),
                ])),
            )
            .await?;
        v["access_token"].as_str().map(str::to_string).ok_or_else(|| "the server gave no access token".into())
    }

    pub async fn me(&self) -> Result<Account, String> {
        self.get("/api/v1/accounts/verify_credentials").await
    }

    /// Posts newer than `since` (or the newest page), newest first.
    pub async fn timeline(
        &self,
        which: Timeline,
        since: Option<&str>,
        max: Option<&str>,
    ) -> Result<Vec<Status>, String> {
        let mut q = vec![("limit", "40".to_string())];
        if let Some(s) = since {
            q.push(("since_id", s.to_string()));
        }
        if let Some(m) = max {
            q.push(("max_id", m.to_string()));
        }
        let path = match which {
            Timeline::Home => "/api/v1/timelines/home",
            Timeline::Local => {
                q.push(("local", "true".into()));
                "/api/v1/timelines/public"
            }
            Timeline::Federated => "/api/v1/timelines/public",
            Timeline::Notifications => return Err("use notifications()".into()),
        };
        let qs = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(q.iter().map(|(k, v)| (*k, v.as_str())))
            .finish();
        self.get(&format!("{path}?{qs}")).await
    }

    pub async fn notifications(&self, since: Option<&str>) -> Result<Vec<Notification>, String> {
        let mut path = "/api/v1/notifications?limit=40".to_string();
        if let Some(s) = since {
            path.push_str(&format!("&since_id={s}"));
        }
        self.get(&path).await
    }

    pub async fn context(&self, id: &str) -> Result<Context, String> {
        self.get(&format!("/api/v1/statuses/{id}/context")).await
    }

    pub async fn post(
        &self,
        text: &str,
        reply_to: Option<&str>,
        spoiler: &str,
        visibility: &str,
    ) -> Result<Status, String> {
        let mut f = vec![("status", text), ("visibility", visibility)];
        if let Some(r) = reply_to {
            f.push(("in_reply_to_id", r));
        }
        if !spoiler.is_empty() {
            f.push(("spoiler_text", spoiler));
        }
        let v = self.call("POST", "/api/v1/statuses", Some(form(&f))).await?;
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    /// Favourite or boost, or undo either. Returns the post as it is now.
    pub async fn act(&self, id: &str, what: &str, on: bool) -> Result<Status, String> {
        let verb = if on { what.to_string() } else { format!("un{what}") };
        let v = self.call("POST", &format!("/api/v1/statuses/{id}/{verb}"), Some(String::new())).await?;
        serde_json::from_value(v).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_addresses() {
        assert_eq!(base("mastodon.social"), "https://mastodon.social");
        assert_eq!(base("@me@hachyderm.io"), "https://hachyderm.io");
        assert_eq!(base("http://127.0.0.1:9/"), "http://127.0.0.1:9");
    }

    #[test]
    fn statuses() {
        let s: Status = serde_json::from_str(
            r#"{"id":"1","created_at":"2024-01-02T03:04:05.000Z","account":{"id":"9","acct":"ada@example.org","display_name":""},
            "content":"<p>Hello <a href=\"x\">world</a></p>","reblog":{"id":"2","created_at":"2024-01-02T03:04:05Z","account":{"acct":"b"},"content":"<p>Inner</p>"},"unknown_field":1}"#,
        )
        .unwrap();
        assert_eq!(s.account.name(), "ada@example.org");
        assert_eq!(s.time(), Some(1704164645));
        assert_eq!(s.text(), "Hello world");
        assert_eq!(s.shown().text(), "Inner");
    }
}
