//! Files: Minuet's FTP module, grown up. Two panes, each on a local disk or a remote
//! server, with copies between them running in the background.
//!
//! URLs:
//! - `files:` your home folder beside the last place you went; `files:<site>` a saved site
//! - `file:///path` a local folder
//! - `sftp://[user@]host[:port]/path` through your SSH config, agent and known hosts
//! - `ftp://`, `ftpes://` (explicit TLS) and `ftps://` (implicit TLS, port 990)
//! - `dav://`, `davs://` WebDAV, such as Nextcloud
//! - `s3://bucket/prefix` with keys from the environment or `~/.aws/credentials`

pub mod open;
pub mod window;

use alouette_ssh::Options;
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

use open::Target;
use window::FilesWindow;

pub const MODULE: &str = "files";

pub struct FilesModule;

fn target(cx: &Cx, url: Url, endpoint: Option<String>, region: Option<String>) -> Target {
    Target {
        url,
        net: cx.net.clone(),
        ssh: mod_terminal::ssh_config(cx),
        ssh_options: Options {
            agent: cx.config.ssh.agent,
            keepalive_secs: cx.config.ssh.keepalive_secs,
            connect_timeout: std::time::Duration::from_secs(cx.config.network.connect_timeout_secs.max(5)),
        },
        endpoint,
        region,
    }
}

impl Module for FilesModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Files"
    }
    fn description(&self) -> &'static str {
        "Two-pane file manager for SFTP, FTP, WebDAV and S3"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["files", "file", "sftp", "ftp", "ftps", "ftpes", "dav", "davs", "webdav", "webdavs", "s3"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Files", "files:", "Your home folder in two panes; press o to open a server"),
            Launcher::new("FTP archive", "ftp://ftp.gnu.org/gnu/", "Anonymous FTP, the way it was"),
        ]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let home = Url::parse("files:").map_err(|e| e.to_string())?;
        let right = match url.scheme() {
            "files" => {
                let name = url.path().trim_matches('/');
                if name.is_empty() {
                    target(cx, home.clone(), None, None)
                } else {
                    let site = cx
                        .config
                        .files
                        .sites
                        .iter()
                        .find(|s| s.name.eq_ignore_ascii_case(name))
                        .cloned()
                        .ok_or_else(|| format!("No saved site called {name}. Add one under [[files.sites]]."))?;
                    let u = Url::parse(&site.url).map_err(|e| format!("{}: {e}", site.url))?;
                    target(cx, u, site.endpoint, site.region)
                }
            }
            "sftp" | "ftp" | "ftps" | "ftpes" | "dav" | "davs" | "webdav" | "webdavs" | "s3"
                if url.host_str().is_none_or(str::is_empty) =>
            {
                return Err(format!("Add a host, as in {}://user@example.com/", url.scheme()));
            }
            _ => target(cx, url.clone(), None, None),
        };
        let left = target(cx, home, None, None);
        Ok(Box::new(FilesWindow::new(left, right, cx)))
    }
}
