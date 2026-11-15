//! Turn a URL into a connected file system, asking for passwords when needed.

use std::sync::Arc;

use alouette_net::Net;
use alouette_ssh::{Options, Prompter, SshConfig};
use alouette_ui::ask::{Asker, Question};
use alouette_vfs::dav::Dav;
use alouette_vfs::ftp::{Ftp, FtpSite, Security};
use alouette_vfs::local::Local;
use alouette_vfs::s3::{S3, S3Site, credentials_from_env};
use alouette_vfs::sftp::Sftp;
use alouette_vfs::sigv4::Credentials;
use alouette_vfs::{Fs, posix_join};
use url::Url;

/// Everything needed to connect, gathered on the UI thread.
#[derive(Clone)]
pub struct Target {
    pub url: Url,
    pub net: Net,
    pub ssh: SshConfig,
    pub ssh_options: Options,
    /// For S3: from a saved site or the environment.
    pub endpoint: Option<String>,
    pub region: Option<String>,
}

struct AskPrompter(Asker);

impl Prompter for AskPrompter {
    fn ask(
        &self,
        q: alouette_ssh::Ask,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + '_>> {
        Box::pin(self.0.ask(Question {
            title: q.title,
            body: q.body,
            prompt: q.prompt,
            secret: q.secret,
            yes_no: q.yes_no,
        }))
    }
    fn note(&self, _text: &str) {}
}

fn decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

async fn password(asker: &Asker, what: &str, given: Option<&str>) -> Result<String, String> {
    if let Some(p) = given {
        return Ok(decode(p));
    }
    asker
        .ask(Question {
            title: "Password".into(),
            body: vec![what.to_string()],
            prompt: "Password".into(),
            secret: true,
            yes_no: false,
        })
        .await
        .ok_or_else(|| "cancelled".to_string())
}

/// Connect. Returns the file system and the folder to show first.
pub async fn connect(t: Target, asker: Asker) -> Result<(Arc<dyn Fs>, String), String> {
    let u = &t.url;
    let path = decode(u.path());
    let host = u.host_str().unwrap_or_default().trim_matches(['[', ']']).to_string();
    let user = decode(u.username());
    let start = |home: String| if path.is_empty() || path == "/" { home } else { path.clone() };
    match u.scheme() {
        "file" | "files" => {
            if alouette_core::shared::restricted() {
                return Err(format!("Local files are {}", alouette_core::shared::REFUSED));
            }
            let fs: Arc<dyn Fs> = Arc::new(Local);
            let home = fs.home().await?;
            let p = if u.scheme() == "file" {
                u.to_file_path().ok().map(|p| p.to_string_lossy().into_owned())
            } else {
                None
            };
            Ok((fs, p.unwrap_or(home)))
        }
        "sftp" => {
            let mut dest = String::new();
            if !user.is_empty() {
                dest.push_str(&user);
                dest.push('@');
            }
            dest.push_str(&host);
            if let Some(p) = u.port() {
                dest.push_str(&format!(":{p}"));
            }
            let prompter: Arc<dyn Prompter> = Arc::new(AskPrompter(asker));
            let session = alouette_ssh::connect(&t.net, &t.ssh, &dest, &t.ssh_options, prompter)
                .await
                .map_err(|e| e.to_string())?;
            let fs = Sftp::open(Arc::new(session)).await?;
            let home = fs.home().await?;
            Ok((Arc::new(fs), start(home)))
        }
        "ftp" | "ftps" | "ftpes" => {
            let security = match u.scheme() {
                "ftp" => Security::None,
                "ftpes" => Security::Explicit,
                _ => Security::Implicit,
            };
            let port = u.port().unwrap_or(if security == Security::Implicit { 990 } else { 21 });
            let (user, pass) = if user.is_empty() {
                ("anonymous".to_string(), "alouette@".to_string())
            } else {
                let p = password(&asker, &format!("{user}@{host}"), u.password()).await?;
                (user, p)
            };
            let fs = Ftp::open(t.net.clone(), FtpSite { host, port, user, password: pass, security }).await?;
            let home = fs.home().await?;
            Ok((Arc::new(fs), start(home)))
        }
        "dav" | "davs" | "webdav" | "webdavs" => {
            let https = matches!(u.scheme(), "davs" | "webdavs");
            let mut base = Url::parse(&format!(
                "{}://{}{}/",
                if https { "https" } else { "http" },
                if host.contains(':') { format!("[{host}]") } else { host.clone() },
                u.port().map(|p| format!(":{p}")).unwrap_or_default()
            ))
            .map_err(|e| e.to_string())?;
            base.set_path(u.path());
            let pass = if user.is_empty() {
                String::new()
            } else {
                password(&asker, &format!("{user}@{host}"), u.password()).await?
            };
            let fs = Dav::new(t.net.clone(), base, &user, &pass);
            let home = fs.home().await?;
            // Check the login now rather than on the first click.
            fs.list(&home).await?;
            Ok((Arc::new(fs), home))
        }
        "s3" => {
            let bucket = host;
            let region = t.region.clone().or_else(|| std::env::var("AWS_REGION").ok()).unwrap_or("us-east-1".into());
            let endpoint = t
                .endpoint
                .clone()
                .or_else(|| std::env::var("AWS_ENDPOINT_URL").ok())
                .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
            let creds = match credentials_from_env(None) {
                Some(c) => c,
                None => {
                    let ask = |prompt: &str, secret: bool| {
                        asker.ask(Question {
                            title: "S3 keys".into(),
                            body: vec![
                                format!("No keys for s3://{bucket} in the environment or ~/.aws/credentials."),
                                "Keys typed here are used for this window only.".into(),
                            ],
                            prompt: prompt.into(),
                            secret,
                            yes_no: false,
                        })
                    };
                    let a = ask("Access key ID", false).await.ok_or("cancelled")?;
                    let s = ask("Secret access key", true).await.ok_or("cancelled")?;
                    Credentials { access_key: a, secret_key: s, session_token: None }
                }
            };
            let fs = S3::new(t.net.clone(), S3Site { endpoint, region, bucket, creds })?;
            let p = if path.is_empty() { "/".to_string() } else { path.clone() };
            fs.list(&p).await?;
            Ok((Arc::new(fs), p))
        }
        s => Err(format!("Files does not open {s}: URLs")),
    }
}

/// The URL that shows `path` on `fs`, for history and the window title.
pub fn url_for(base: &Url, path: &str) -> String {
    let mut u = base.clone();
    u.set_password(None).ok();
    if u.scheme() == "files" {
        return "files:".into();
    }
    u.set_path(&posix_join("", path.trim_start_matches('/')));
    u.to_string()
}
