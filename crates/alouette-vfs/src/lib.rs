//! One file API over many places: this computer, SFTP, FTP and FTPS, WebDAV, and S3.
//!
//! Every backend implements [`Fs`]. Paths are strings: native paths for [`local::Local`],
//! `/`-separated paths everywhere else. [`copy`] moves a file or folder between any two.

pub mod dav;
pub mod ftp;
pub mod local;
pub mod s3;
pub mod sftp;
pub mod sigv4;
pub mod xml;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

pub type Result<T> = std::result::Result<T, String>;
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
pub type Writer<'a> = &'a mut (dyn AsyncWrite + Unpin + Send);
pub type Reader<'a> = &'a mut (dyn AsyncRead + Unpin + Send);
pub type Progress<'a> = &'a (dyn Fn(u64) + Sync);

/// One item in a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: Option<u64>,
    /// Seconds since 1970.
    pub modified: Option<i64>,
}

/// A place files live.
pub trait Fs: Send + Sync {
    /// Such as `sftp://alice@example.org` or `Local`.
    fn label(&self) -> String;
    /// How it is connected, for the status line.
    fn badge(&self) -> Option<String> {
        None
    }
    /// Where to start browsing.
    fn home(&self) -> BoxFut<'_, String>;
    fn list<'a>(&'a self, path: &'a str) -> BoxFut<'a, Vec<Entry>>;
    /// Write the file at `path` into `to`. Returns the bytes copied.
    fn download<'a>(&'a self, path: &'a str, to: Writer<'a>, progress: Progress<'a>) -> BoxFut<'a, u64>;
    /// Create or replace `path` with `size` bytes read from `from`.
    fn upload<'a>(&'a self, path: &'a str, from: Reader<'a>, size: u64, progress: Progress<'a>) -> BoxFut<'a, u64>;
    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFut<'a, ()>;
    /// Remove a file, or an empty folder.
    fn remove<'a>(&'a self, path: &'a str, is_dir: bool) -> BoxFut<'a, ()>;
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFut<'a, ()>;

    fn join(&self, dir: &str, name: &str) -> String {
        posix_join(dir, name)
    }
    fn parent(&self, path: &str) -> Option<String> {
        posix_parent(path)
    }
}

pub fn posix_join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

pub fn posix_parent(path: &str) -> Option<String> {
    let p = path.trim_end_matches('/');
    if p.is_empty() {
        return None;
    }
    match p.rfind('/') {
        Some(0) => Some("/".into()),
        Some(i) => Some(p[..i].to_string()),
        None => None,
    }
}

/// The last part of a path.
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Copy one file between two places, streaming (nothing is held in memory).
pub async fn copy_file(
    src: &dyn Fs,
    src_path: &str,
    dst: &dyn Fs,
    dst_path: &str,
    size: u64,
    progress: Progress<'_>,
) -> Result<u64> {
    let (mut w, r) = tokio::io::duplex(256 * 1024);
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut r = Guarded { inner: r, finished: finished.clone() };
    let mut failed = None;
    let down = async {
        let res = src.download(src_path, &mut w, &|_| {}).await;
        match res {
            // Only a complete download lets the upload see a clean end.
            Ok(_) => finished.store(true, std::sync::atomic::Ordering::Release),
            Err(e) => failed = Some(e),
        }
        drop(w);
        Ok::<(), String>(())
    };
    let up = dst.upload(dst_path, &mut r, size, progress);
    // If the upload fails, stop the download too; if the download fails, the upload
    // gets an error at the end of the stream instead of an ordinary end, and so does
    // not replace the destination.
    let res = tokio::try_join!(down, up);
    if let Some(e) = failed {
        return Err(e);
    }
    res.map(|(_, n)| n)
}

/// The read side of a copy: reports an error, not an end, when the download stopped early.
struct Guarded {
    inner: tokio::io::DuplexStream,
    finished: Arc<std::sync::atomic::AtomicBool>,
}

impl AsyncRead for Guarded {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = r
            && buf.filled().len() == before
            && buf.remaining() > 0
            && !self.finished.load(std::sync::atomic::Ordering::Acquire)
        {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the download stopped early",
            )));
        }
        r
    }
}

/// What [`copy`] reports as it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyStatus {
    pub file: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub files_done: usize,
    pub files_total: usize,
}

/// Copy a file or a whole folder. `cancelled` is checked between files.
pub async fn copy(
    src: Arc<dyn Fs>,
    src_path: String,
    entry: Entry,
    dst: Arc<dyn Fs>,
    dst_dir: String,
    status: &(dyn Fn(CopyStatus) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<()> {
    // Walk first, so progress has a total.
    let mut files: Vec<(String, String, u64)> = vec![];
    let mut dirs: Vec<String> = vec![];
    let target = dst.join(&dst_dir, &entry.name);
    if entry.is_dir {
        dirs.push(target.clone());
        let mut stack = vec![(src_path.clone(), target)];
        while let Some((s, d)) = stack.pop() {
            if cancelled() {
                return Err("cancelled".into());
            }
            for e in src.list(&s).await? {
                let sp = src.join(&s, &e.name);
                let dp = dst.join(&d, &e.name);
                if e.is_dir {
                    dirs.push(dp.clone());
                    stack.push((sp, dp));
                } else {
                    files.push((sp, dp, e.size.unwrap_or(0)));
                }
            }
        }
    } else {
        files.push((src_path, target, entry.size.unwrap_or(0)));
    }
    for d in &dirs {
        // An existing folder is fine.
        if let Err(e) = dst.mkdir(d).await {
            let parent_listing = match dst.parent(d) {
                Some(p) => dst.list(&p).await.unwrap_or_default(),
                None => vec![],
            };
            if !parent_listing.iter().any(|x| x.is_dir && x.name == file_name(d)) {
                return Err(e);
            }
        }
    }
    let total: u64 = files.iter().map(|f| f.2).sum();
    let mut before = 0u64;
    let n = files.len();
    for (i, (sp, dp, size)) in files.into_iter().enumerate() {
        if cancelled() {
            return Err("cancelled".into());
        }
        let name = file_name(&sp).to_string();
        let report = |b: u64| {
            status(CopyStatus {
                file: name.clone(),
                done_bytes: before + b,
                total_bytes: total,
                files_done: i,
                files_total: n,
            })
        };
        report(0);
        copy_file(src.as_ref(), &sp, dst.as_ref(), &dp, size, &report).await.map_err(|e| format!("{name}: {e}"))?;
        before += size;
    }
    status(CopyStatus { file: String::new(), done_bytes: total, total_bytes: total, files_done: n, files_total: n });
    Ok(())
}

/// Delete a file or a folder and everything in it.
pub async fn delete(fs: &dyn Fs, path: &str, is_dir: bool) -> Result<()> {
    if !is_dir {
        return fs.remove(path, false).await;
    }
    // Depth first: empty each folder, then remove it.
    let mut order = vec![path.to_string()];
    let mut i = 0;
    while i < order.len() {
        let dir = order[i].clone();
        for e in fs.list(&dir).await? {
            let p = fs.join(&dir, &e.name);
            if e.is_dir && !e.is_link {
                order.push(p);
            } else {
                fs.remove(&p, false).await?;
            }
        }
        i += 1;
    }
    for d in order.iter().rev() {
        fs.remove(d, true).await?;
    }
    Ok(())
}

/// Read the start of a file, for previews and READMEs.
pub async fn read_head(fs: &dyn Fs, path: &str, max: usize) -> Result<Vec<u8>> {
    struct Capped {
        buf: Vec<u8>,
        max: usize,
    }
    impl AsyncWrite for Capped {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            data: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self.buf.len() >= self.max {
                return std::task::Poll::Ready(Err(std::io::Error::other("preview limit")));
            }
            let room = self.max - self.buf.len();
            let take = data.len().min(room);
            self.buf.extend_from_slice(&data[..take]);
            std::task::Poll::Ready(Ok(take))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    let mut c = Capped { buf: Vec::new(), max };
    // Hitting the cap ends the download early; that is the point.
    let _ = fs.download(path, &mut c, &|_| {}).await;
    Ok(c.buf)
}

/// Format a byte count the way people read it.
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// A source that sends `bytes` and then fails, or ends cleanly.
    struct Flaky {
        bytes: usize,
        fail: bool,
    }

    impl Fs for Flaky {
        fn label(&self) -> String {
            "flaky".into()
        }
        fn home(&self) -> BoxFut<'_, String> {
            Box::pin(async { Ok("/".into()) })
        }
        fn list<'a>(&'a self, _: &'a str) -> BoxFut<'a, Vec<Entry>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn download<'a>(&'a self, _: &'a str, to: Writer<'a>, _: Progress<'a>) -> BoxFut<'a, u64> {
            Box::pin(async move {
                to.write_all(&vec![b'x'; self.bytes]).await.map_err(|e| e.to_string())?;
                if self.fail { Err("connection lost".into()) } else { Ok(self.bytes as u64) }
            })
        }
        fn upload<'a>(&'a self, _: &'a str, _: Reader<'a>, _: u64, _: Progress<'a>) -> BoxFut<'a, u64> {
            Box::pin(async { Err("read only".into()) })
        }
        fn mkdir<'a>(&'a self, _: &'a str) -> BoxFut<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn remove<'a>(&'a self, _: &'a str, _: bool) -> BoxFut<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn rename<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFut<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn a_failed_download_keeps_the_old_file() {
        let dir = std::env::temp_dir().join(format!("alouette-vfs-flaky-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("keep.txt");
        std::fs::write(&dst, b"precious").unwrap();
        let path = dst.to_string_lossy().to_string();
        let r = rt().block_on(copy_file(&Flaky { bytes: 1000, fail: true }, "/f", &local::Local, &path, 5000, &|_| {}));
        assert_eq!(r.unwrap_err(), "connection lost");
        assert_eq!(std::fs::read(&dst).unwrap(), b"precious");
        assert!(!dir.join("keep.txt.alouette-part").exists());
        // A complete download replaces it.
        let r = rt().block_on(copy_file(&Flaky { bytes: 10, fail: false }, "/f", &local::Local, &path, 10, &|_| {}));
        assert_eq!(r.unwrap(), 10);
        assert_eq!(std::fs::read(&dst).unwrap(), b"xxxxxxxxxx");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_upload_stops_the_download() {
        // More than the pipe holds, into a folder that does not exist: must not hang.
        let path = std::env::temp_dir().join("alouette-no-such-dir/x/y.bin").to_string_lossy().to_string();
        let r = rt().block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                copy_file(&Flaky { bytes: 2 << 20, fail: false }, "/f", &local::Local, &path, 2 << 20, &|_| {}),
            )
            .await
        });
        assert!(r.expect("copy hung").is_err());
    }

    #[test]
    fn paths() {
        assert_eq!(posix_join("/a", "b"), "/a/b");
        assert_eq!(posix_join("/", "b"), "/b");
        assert_eq!(posix_parent("/a/b"), Some("/a".into()));
        assert_eq!(posix_parent("/a"), Some("/".into()));
        assert_eq!(posix_parent("/"), None);
        assert_eq!(file_name("/a/b/"), "b");
        assert_eq!(file_name("C:\\x\\y.txt"), "y.txt");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
