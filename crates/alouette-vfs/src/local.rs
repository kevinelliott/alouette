//! This computer's disks.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{BoxFut, Entry, Fs, Progress, Reader, Writer};

#[derive(Debug, Clone, Default)]
pub struct Local;

fn err(p: &str, e: std::io::Error) -> String {
    format!("{p}: {e}")
}

pub fn home_dir() -> String {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into())
}

pub async fn copy_progress(
    r: &mut (dyn tokio::io::AsyncRead + Unpin + Send),
    w: &mut (dyn tokio::io::AsyncWrite + Unpin + Send),
    progress: Progress<'_>,
) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 128 * 1024];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).await?;
        total += n as u64;
        progress(total);
    }
    w.flush().await?;
    Ok(total)
}

impl Fs for Local {
    fn label(&self) -> String {
        "This computer".into()
    }
    fn home(&self) -> BoxFut<'_, String> {
        Box::pin(async { Ok(home_dir()) })
    }
    fn list<'a>(&'a self, path: &'a str) -> BoxFut<'a, Vec<Entry>> {
        Box::pin(async move {
            let mut rd = tokio::fs::read_dir(path).await.map_err(|e| err(path, e))?;
            let mut out = vec![];
            while let Some(d) = rd.next_entry().await.map_err(|e| err(path, e))? {
                let name = d.file_name().to_string_lossy().into_owned();
                let link = d.file_type().await.map(|t| t.is_symlink()).unwrap_or(false);
                // Follow links for the kind and size, as a file manager shows them.
                let meta = tokio::fs::metadata(d.path()).await.ok();
                let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
                out.push(Entry {
                    name,
                    is_dir,
                    is_link: link,
                    size: meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
                    modified: meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64),
                });
            }
            Ok(out)
        })
    }
    fn download<'a>(&'a self, path: &'a str, to: Writer<'a>, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let mut f = tokio::fs::File::open(path).await.map_err(|e| err(path, e))?;
            copy_progress(&mut f, to, progress).await.map_err(|e| err(path, e))
        })
    }
    fn upload<'a>(&'a self, path: &'a str, from: Reader<'a>, _size: u64, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            // Write beside the target, then rename, so a failed copy never leaves half a file.
            let tmp = format!("{path}.alouette-part");
            let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| err(path, e))?;
            let n = match copy_progress(from, &mut f, progress).await {
                Ok(n) => n,
                Err(e) => {
                    drop(f);
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(err(path, e));
                }
            };
            f.sync_all().await.map_err(|e| err(path, e))?;
            drop(f);
            tokio::fs::rename(&tmp, path).await.map_err(|e| err(path, e))?;
            Ok(n)
        })
    }
    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move { tokio::fs::create_dir(path).await.map_err(|e| err(path, e)) })
    }
    fn remove<'a>(&'a self, path: &'a str, is_dir: bool) -> BoxFut<'a, ()> {
        Box::pin(async move {
            if is_dir { tokio::fs::remove_dir(path).await } else { tokio::fs::remove_file(path).await }
                .map_err(|e| err(path, e))
        })
    }
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move { tokio::fs::rename(from, to).await.map_err(|e| err(from, e)) })
    }
    fn join(&self, dir: &str, name: &str) -> String {
        Path::new(dir).join(name).to_string_lossy().into_owned()
    }
    fn parent(&self, path: &str) -> Option<String> {
        Path::new(path).parent().map(|p| p.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_on_disk() {
        let dir = std::env::temp_dir().join(format!("alouette-vfs-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().into_owned();
        let fs = Local;
        let sub = fs.join(&d, "sub");
        fs.mkdir(&sub).await.unwrap();
        let file = fs.join(&sub, "a.txt");
        let mut src: &[u8] = b"hello files";
        fs.upload(&file, &mut src, 11, &|_| {}).await.unwrap();
        let list = fs.list(&sub).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].size), ("a.txt", Some(11)));
        assert_eq!(crate::read_head(&fs, &file, 5).await.unwrap(), b"hello");
        let moved = fs.join(&d, "b.txt");
        fs.rename(&file, &moved).await.unwrap();
        crate::delete(&fs, &d, true).await.unwrap();
        assert!(!dir.exists());
    }
}
