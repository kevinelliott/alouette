//! OAuth 2.0 sign-in for Gmail (browser and a loopback redirect, with PKCE) and Microsoft
//! 365 (device code: type a code at microsoft.com/devicelogin). People bring their own app
//! registration (client ID); tokens live in the secret store and refresh themselves.

use alouette_net::Net;
use alouette_net::http::{Body, Sink};
use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::Prompt;
use crate::account::{Account, Login};
use crate::alouette_store_like::Secrets;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    /// Seconds since 1970.
    pub expires_at: i64,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<String>,
    error_description: Option<String>,
}

pub struct Provider {
    pub auth: &'static str,
    pub token: &'static str,
    pub device: Option<&'static str>,
    pub scope: &'static str,
}

pub const GOOGLE: Provider = Provider {
    auth: "https://accounts.google.com/o/oauth2/v2/auth",
    token: "https://oauth2.googleapis.com/token",
    device: None,
    scope: "https://mail.google.com/",
};

pub const MICROSOFT: Provider = Provider {
    auth: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    token: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
    device: Some("https://login.microsoftonline.com/common/oauth2/v2.0/devicecode"),
    scope: "offline_access https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send",
};

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()
}

async fn post(net: &Net, url: &str, body: &str) -> crate::Result<(u16, String)> {
    let u = url::Url::parse(url).map_err(|e| e.to_string())?;
    let r = net
        .request(
            &u,
            "POST",
            &[("Content-Type", "application/x-www-form-urlencoded"), ("Accept", "application/json")],
            Body::Bytes(body.as_bytes()),
            Sink::Memory(1 << 20),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok((r.status, r.text()))
}

fn tokens_from(reply: &str, old_refresh: Option<String>) -> crate::Result<Tokens> {
    let t: TokenReply = serde_json::from_str(reply).map_err(|e| format!("odd answer from the sign-in server: {e}"))?;
    if let Some(e) = t.error {
        return Err(t.error_description.unwrap_or(e));
    }
    Ok(Tokens {
        access: t.access_token.ok_or("no access token in the answer")?,
        refresh: t.refresh_token.or(old_refresh),
        expires_at: now() + t.expires_in.unwrap_or(3600),
    })
}

/// A random string for PKCE and state.
fn random(n: usize) -> String {
    let mut b = vec![0u8; n];
    aws_lc_rs::rand::fill(&mut b).expect("random");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn pkce_challenge(verifier: &str) -> String {
    let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(d.as_ref())
}

/// A current access token: the saved one, a refreshed one, or a new sign-in.
/// `force` skips the saved access token (after the server rejected it).
pub async fn token(
    net: &Net,
    acct: &Account,
    secrets: &dyn Secrets,
    prompt: &dyn Prompt,
    force: bool,
) -> crate::Result<String> {
    let p = match acct.login {
        Login::Google => &GOOGLE,
        Login::Microsoft => &MICROSOFT,
        Login::Password => return Err("this account signs in with a password".into()),
    };
    let key = acct.secret_key("oauth");
    let saved: Option<Tokens> = secrets.get(&key).and_then(|s| serde_json::from_str(&s).ok());
    if let Some(t) = &saved {
        if !force && t.expires_at > now() + 60 {
            return Ok(t.access.clone());
        }
        if let Some(r) = &t.refresh {
            let mut pairs = vec![
                ("grant_type", "refresh_token"),
                ("refresh_token", r.as_str()),
                ("client_id", acct.client_id.as_deref().unwrap_or_default()),
            ];
            if let Some(s) = &acct.client_secret {
                pairs.push(("client_secret", s));
            }
            let (_, reply) = post(net, p.token, &form(&pairs)).await?;
            if let Ok(t) = tokens_from(&reply, Some(r.clone())) {
                secrets.set(&key, &serde_json::to_string(&t).unwrap_or_default());
                return Ok(t.access);
            }
        }
    }
    let t = if p.device.is_some() {
        device_flow(net, p, acct, prompt).await?
    } else {
        loopback_flow(net, p, acct, prompt).await?
    };
    secrets.set(&key, &serde_json::to_string(&t).unwrap_or_default());
    Ok(t.access)
}

#[derive(Deserialize)]
struct DeviceReply {
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    interval: Option<u64>,
    expires_in: Option<u64>,
    error_description: Option<String>,
}

async fn device_flow(net: &Net, p: &Provider, acct: &Account, prompt: &dyn Prompt) -> crate::Result<Tokens> {
    let client = acct.client_id.as_deref().unwrap_or_default();
    let (_, reply) =
        post(net, p.device.unwrap_or_default(), &form(&[("client_id", client), ("scope", p.scope)])).await?;
    let d: DeviceReply = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    let (Some(code), Some(user_code)) = (d.device_code, d.user_code) else {
        return Err(d.error_description.unwrap_or("the sign-in server said no".into()));
    };
    let uri = d.verification_uri.unwrap_or("https://microsoft.com/devicelogin".into());
    prompt.note(&format!("Sign in to {}: open {uri} and type {user_code}", acct.email));
    let mut interval = d.interval.unwrap_or(5).max(1);
    let deadline = now() + d.expires_in.unwrap_or(900) as i64;
    while now() < deadline {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let body = form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client),
            ("device_code", &code),
        ]);
        let (_, reply) = post(net, p.token, &body).await?;
        let t: TokenReply = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
        match t.error.as_deref() {
            None => return tokens_from(&reply, None),
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some(_) => return Err(t.error_description.unwrap_or("sign-in failed".into())),
        }
    }
    Err("the sign-in code expired; try again".into())
}

/// Open a URL in the person's browser, if we can. They also see it in a note.
fn open_browser(url: &str) {
    if alouette_core::shared::restricted() || !url.starts_with("https://") {
        return;
    }
    let r = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        // Not `cmd /C start`: cmd would treat & and | in the URL as commands.
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn()
    } else {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    let _ = r;
}

async fn loopback_flow(net: &Net, p: &Provider, acct: &Account, prompt: &dyn Prompt) -> crate::Result<Tokens> {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = random(48);
    let state = random(16);
    let client = acct.client_id.as_deref().unwrap_or_default();
    let mut u = url::Url::parse(p.auth).map_err(|e| e.to_string())?;
    u.query_pairs_mut()
        .append_pair("client_id", client)
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_type", "code")
        .append_pair("scope", p.scope)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("login_hint", &acct.email);
    prompt.note(&format!("Sign in to {} in your browser: {u}", acct.email));
    open_browser(u.as_str());
    let code = tokio::time::timeout(std::time::Duration::from_secs(600), wait_for_code(&l, &state))
        .await
        .map_err(|_| "no sign-in after 10 minutes".to_string())??;
    let mut pairs = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("client_id", client),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(s) = &acct.client_secret {
        pairs.push(("client_secret", s));
    }
    let (_, reply) = post(net, p.token, &form(&pairs)).await?;
    tokens_from(&reply, None)
}

/// Accept requests on the loopback listener until one brings `code` with our `state`.
pub async fn wait_for_code(l: &tokio::net::TcpListener, state: &str) -> crate::Result<String> {
    loop {
        let (mut s, _) = l.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = s.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let target = req.split_whitespace().nth(1).unwrap_or("/");
        let q = url::Url::parse(&format!("http://x{target}"))
            .map(|u| u.query_pairs().into_owned().collect::<Vec<_>>())
            .unwrap_or_default();
        let get = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        let (msg, result) = match (get("code"), get("state"), get("error")) {
            (Some(c), Some(st), _) if st == state => {
                ("Signed in. You can close this tab and go back to Alouette.", Some(Ok(c)))
            }
            (_, _, Some(e)) => ("Sign-in was cancelled. You can close this tab.", Some(Err(e))),
            _ => ("Waiting for sign-in…", None),
        };
        let page = format!(
            "<!doctype html><meta charset=utf-8><title>Alouette</title><p style=\"font:16px sans-serif\">{msg}</p>"
        );
        let _ = s
            .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).as_bytes())
            .await;
        if let Some(r) = result {
            return r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn token_replies() {
        let t = tokens_from(r#"{"access_token":"a","expires_in":10}"#, Some("r".into())).unwrap();
        assert_eq!((t.access.as_str(), t.refresh.as_deref()), ("a", Some("r")));
        assert!(
            tokens_from(r#"{"error":"invalid_grant","error_description":"expired"}"#, None)
                .unwrap_err()
                .contains("expired")
        );
    }

    #[tokio::test]
    async fn loopback_code() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            for path in ["/favicon.ico", "/?state=wrong&code=nope", "/?state=s1&code=c0de"] {
                let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
                let mut b = vec![];
                let _ = s.read_to_end(&mut b).await;
            }
        });
        assert_eq!(wait_for_code(&l, "s1").await.unwrap(), "c0de");
    }
}
