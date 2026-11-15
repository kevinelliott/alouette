//! The same round trip against every backend: make a folder, upload, list, download,
//! rename, copy a whole tree to the local disk and back, and delete it all.

mod servers;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use alouette_net::Net;
use alouette_vfs::dav::Dav;
use alouette_vfs::ftp::{Ftp, FtpSite, Security};
use alouette_vfs::local::Local;
use alouette_vfs::s3::{S3, S3Site};
use alouette_vfs::sigv4::Credentials;
use alouette_vfs::{Entry, Fs, copy, delete, read_head};
use servers::tmpdir;

fn net() -> Net {
    Net::new(&alouette_core::config::Network::default()).unwrap()
}

async fn put(fs: &dyn Fs, path: &str, data: &[u8]) {
    let mut r: &[u8] = data;
    let n = fs.upload(path, &mut r, data.len() as u64, &|_| {}).await.unwrap_or_else(|e| panic!("upload {path}: {e}"));
    assert_eq!(n, data.len() as u64);
}

async fn names(fs: &dyn Fs, dir: &str) -> Vec<String> {
    let mut v: Vec<String> = fs
        .list(dir)
        .await
        .unwrap_or_else(|e| panic!("list {dir}: {e}"))
        .into_iter()
        .map(|e| if e.is_dir { format!("{}/", e.name) } else { e.name })
        .collect();
    v.sort();
    v
}

async fn exercise(name: &str, fs: Arc<dyn Fs>, root: &str) {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let dir = fs.join(root, "t dir");
    fs.mkdir(&dir).await.unwrap_or_else(|e| panic!("mkdir: {e}"));
    put(fs.as_ref(), &fs.join(&dir, "héllo.txt"), b"hello, world\n").await;
    put(fs.as_ref(), &fs.join(&dir, "big.bin"), &big).await;
    let sub = fs.join(&dir, "sub");
    fs.mkdir(&sub).await.unwrap();
    put(fs.as_ref(), &fs.join(&sub, "deep.txt"), b"deep").await;

    assert!(names(fs.as_ref(), root).await.contains(&"t dir/".to_string()));
    assert_eq!(names(fs.as_ref(), &dir).await, ["big.bin", "héllo.txt", "sub/"]);
    let listed: Vec<Entry> = fs.list(&dir).await.unwrap();
    let b = listed.iter().find(|e| e.name == "big.bin").unwrap();
    assert_eq!(b.size, Some(big.len() as u64));

    assert_eq!(read_head(fs.as_ref(), &fs.join(&dir, "héllo.txt"), 1024).await.unwrap(), b"hello, world\n");
    let mut got = vec![];
    fs.download(&fs.join(&dir, "big.bin"), &mut got, &|_| {}).await.unwrap();
    assert!(got == big, "big.bin came back different ({} bytes)", got.len());

    fs.rename(&fs.join(&dir, "héllo.txt"), &fs.join(&dir, "renamed.txt"))
        .await
        .unwrap_or_else(|e| panic!("rename: {e}"));
    assert_eq!(names(fs.as_ref(), &dir).await, ["big.bin", "renamed.txt", "sub/"]);

    // The whole tree to the local disk, then back under a new name.
    let local: Arc<dyn Fs> = Arc::new(Local);
    let ltmp = tmpdir(&format!("{name}-copy"));
    let ldir = ltmp.to_string_lossy().into_owned();
    let entry = Entry { name: "t dir".into(), is_dir: true, ..Default::default() };
    copy(fs.clone(), dir.clone(), entry, local.clone(), ldir.clone(), &|_| {}, &|| false).await.unwrap();
    assert_eq!(std::fs::read(ltmp.join("t dir").join("sub").join("deep.txt")).unwrap(), b"deep");
    assert_eq!(std::fs::read(ltmp.join("t dir").join("big.bin")).unwrap(), big);

    std::fs::rename(ltmp.join("t dir"), ltmp.join("back")).unwrap();
    let entry = Entry { name: "back".into(), is_dir: true, ..Default::default() };
    copy(local.clone(), local.join(&ldir, "back"), entry, fs.clone(), root.to_string(), &|_| {}, &|| false)
        .await
        .unwrap();
    assert_eq!(
        read_head(fs.as_ref(), &fs.join(&fs.join(&fs.join(root, "back"), "sub"), "deep.txt"), 64).await.unwrap(),
        b"deep"
    );

    delete(fs.as_ref(), &dir, true).await.unwrap_or_else(|e| panic!("delete: {e}"));
    delete(fs.as_ref(), &fs.join(root, "back"), true).await.unwrap();
    let left = names(fs.as_ref(), root).await;
    assert!(!left.iter().any(|n| n == "t dir/" || n == "back/"), "{left:?}");
    let _ = std::fs::remove_dir_all(ltmp);
}

#[tokio::test]
async fn local() {
    let d = tmpdir("local");
    exercise("local", Arc::new(Local), &d.to_string_lossy()).await;
}

#[tokio::test]
async fn ftp() {
    let d = tmpdir("ftp");
    let addr = servers::ftp(d.clone(), "kim", "pw").await;
    let site = FtpSite {
        host: "127.0.0.1".into(),
        port: addr.port(),
        user: "kim".into(),
        password: "pw".into(),
        security: Security::None,
    };
    let fs = Ftp::open(net(), site.clone()).await.unwrap();
    assert_eq!(fs.home().await.unwrap(), "/");
    assert!(fs.badge().unwrap().starts_with("FTP"));
    exercise("ftp", Arc::new(fs), "/").await;

    let bad = Ftp::open(net(), FtpSite { password: "nope".into(), ..site }).await.err().unwrap();
    assert!(bad.contains("login failed"), "{bad}");
}

#[tokio::test]
async fn webdav() {
    let d = tmpdir("dav");
    let addr = servers::dav(d.clone(), "kim", "pw").await;
    let base = url::Url::parse(&format!("http://{addr}/dav/")).unwrap();
    let fs = Dav::new(net(), base.clone(), "kim", "pw");
    assert_eq!(fs.home().await.unwrap(), "/dav/");
    exercise("dav", Arc::new(fs), "/dav/").await;

    let bad = Dav::new(net(), base, "kim", "wrong").list("/dav/").await.err().unwrap();
    assert!(bad.contains("401"), "{bad}");
}

#[tokio::test]
async fn s3() {
    let (addr, store) = servers::s3("bkt").await;
    let creds = Credentials { access_key: "AKID".into(), secret_key: "secret".into(), session_token: None };
    let site = S3Site { endpoint: format!("http://{addr}"), region: "us-east-1".into(), bucket: "bkt".into(), creds };
    let fs = S3::new(net(), site.clone()).unwrap();
    exercise("s3", Arc::new(fs), "/").await;
    let left: Vec<String> = store.lock().unwrap().keys().cloned().collect();
    assert!(left.is_empty(), "{left:?}");

    let creds = Credentials { access_key: "OTHER".into(), ..site.creds.clone() };
    let bad = S3::new(net(), S3Site { creds, ..site }).unwrap().list("/").await.err().unwrap();
    assert!(bad.contains("AccessDenied") || bad.contains("403"), "{bad}");
}

struct Bot;

impl alouette_ssh::Prompter for Bot {
    fn ask(&self, q: alouette_ssh::Ask) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        let a = if q.yes_no { "yes" } else { "sekrit" };
        Box::pin(async move { Some(a.to_string()) })
    }
    fn note(&self, _: &str) {}
}

#[tokio::test]
async fn sftp() {
    use alouette_ssh::testserver::{self, TestServer};
    let d = tmpdir("sftp");
    let server = TestServer { user: "alice".into(), password: "sekrit".into(), sftp_root: Some(d.clone()) };
    let addr = testserver::start(server, 7).await;
    let kh = tmpdir("sftp-kh").join("known_hosts");
    let cfg = alouette_ssh::SshConfig::parse(&format!(
        "Host t\n  HostName 127.0.0.1\n  Port {}\n  User alice\n  UserKnownHostsFile {}\n  IdentityFile /nonexistent/key\n",
        addr.port(),
        kh.display()
    ));
    let opts = alouette_ssh::Options { agent: false, ..Default::default() };
    let session = alouette_ssh::connect(&net(), &cfg, "t", &opts, Arc::new(Bot)).await.unwrap();
    let fs = alouette_vfs::sftp::Sftp::open(Arc::new(session)).await.unwrap();
    assert_eq!(fs.home().await.unwrap(), "/");
    exercise("sftp", Arc::new(fs), "/").await;
}
