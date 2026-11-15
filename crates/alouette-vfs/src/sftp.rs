//! SFTP over an Alouette SSH session.

use std::sync::Arc;

use alouette_ssh::Session;
use russh_sftp::client::SftpSession;

use crate::local::copy_progress;
use crate::{BoxFut, Entry, Fs, Progress, Reader, Writer};

pub struct Sftp {
    session: Arc<Session>,
    sftp: SftpSession,
}

impl Sftp {
    pub async fn open(session: Arc<Session>) -> Result<Self, String> {
        let sftp = session.sftp().await.map_err(|e| e.to_string())?;
        Ok(Self { session, sftp })
    }
}

fn err(p: &str, e: impl std::fmt::Display) -> String {
    format!("{p}: {e}")
}

impl Fs for Sftp {
    fn label(&self) -> String {
        format!("sftp://{}@{}", self.session.user, self.session.host.hostname)
    }
    fn badge(&self) -> Option<String> {
        Some(format!("SFTP · {}", self.session.badge.trim_start_matches("SSH · ")))
    }
    fn home(&self) -> BoxFut<'_, String> {
        Box::pin(async move { self.sftp.canonicalize(".").await.map_err(|e| err(".", e)) })
    }
    fn list<'a>(&'a self, path: &'a str) -> BoxFut<'a, Vec<Entry>> {
        Box::pin(async move {
            let rd = self.sftp.read_dir(path).await.map_err(|e| err(path, e))?;
            Ok(rd
                .map(|d| {
                    let m = d.metadata();
                    let ft = d.file_type();
                    Entry {
                        name: d.file_name(),
                        is_dir: ft.is_dir(),
                        is_link: ft.is_symlink(),
                        size: if ft.is_dir() { None } else { m.size },
                        modified: m.mtime.map(i64::from),
                    }
                })
                .filter(|e| e.name != "." && e.name != "..")
                .collect())
        })
    }
    fn download<'a>(&'a self, path: &'a str, to: Writer<'a>, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let mut f = self.sftp.open(path).await.map_err(|e| err(path, e))?;
            copy_progress(&mut f, to, progress).await.map_err(|e| err(path, e))
        })
    }
    fn upload<'a>(&'a self, path: &'a str, from: Reader<'a>, _size: u64, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let mut f = self.sftp.create(path).await.map_err(|e| err(path, e))?;
            let n = copy_progress(from, &mut f, progress).await.map_err(|e| err(path, e))?;
            f.shutdown_flush().await;
            Ok(n)
        })
    }
    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move { self.sftp.create_dir(path).await.map_err(|e| err(path, e)) })
    }
    fn remove<'a>(&'a self, path: &'a str, is_dir: bool) -> BoxFut<'a, ()> {
        Box::pin(async move {
            if is_dir { self.sftp.remove_dir(path).await } else { self.sftp.remove_file(path).await }
                .map_err(|e| err(path, e))
        })
    }
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move { self.sftp.rename(from, to).await.map_err(|e| err(from, e)) })
    }
}

trait ShutdownFlush {
    async fn shutdown_flush(&mut self);
}

impl ShutdownFlush for russh_sftp::client::fs::File {
    async fn shutdown_flush(&mut self) {
        use tokio::io::AsyncWriteExt;
        let _ = self.shutdown().await;
    }
}
