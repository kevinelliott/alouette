//! An FTP client (RFC 959, 2228 explicit TLS, 2428 EPSV, 3659 MLSD), small and async.
//!
//! The control connection is any stream; data connections are opened by a callback, so
//! the caller decides how to connect (and how to start TLS) for both.

use std::future::Future;
use std::pin::Pin;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the server said {code} {text}")]
    Reply { code: u16, text: String },
    #[error("bad reply from the server: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A reply: a three-digit code and its text (all lines of a multi-line reply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub text: String,
}

impl Reply {
    fn check(self, ok: impl Fn(u16) -> bool) -> Result<Reply> {
        if ok(self.code) { Ok(self) } else { Err(Error::Reply { code: self.code, text: self.text }) }
    }
}

/// One entry from a directory listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: Option<u64>,
    /// Seconds since 1970, when the server says.
    pub modified: Option<i64>,
}

/// Where the data connection should go, from PASV or EPSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataAddr {
    /// EPSV: the same host as the control connection, this port.
    SameHost(u16),
    /// PASV: an address and port. Many servers behind NAT send a private address here, so
    /// callers usually connect to the control host instead.
    Addr(std::net::Ipv4Addr, u16),
}

/// Opens data connections. Given where to connect, returns a stream (already in TLS if the
/// session is protected).
pub type Connector = Box<dyn Fn(DataAddr) -> Pin<Box<dyn Future<Output = Result<Box<dyn Io>>> + Send>> + Send + Sync>;

/// An FTP control connection.
pub struct Ftp {
    ctrl: BufReader<Box<dyn Io>>,
    connect: Connector,
    /// MLSD works (FEAT said so).
    mlsd: bool,
    epsv: bool,
    pub welcome: String,
}

impl Ftp {
    /// Read the greeting from a fresh control connection.
    pub async fn start(stream: Box<dyn Io>, connect: Connector) -> Result<Self> {
        let mut f = Self { ctrl: BufReader::new(stream), connect, mlsd: false, epsv: true, welcome: String::new() };
        f.welcome = f.read_reply().await?.check(|c| c == 220)?.text;
        Ok(f)
    }

    /// Ask for TLS on the control connection (AUTH TLS). Afterwards, hand the inner stream
    /// to [`Ftp::into_inner`], wrap it in TLS, and call [`Ftp::resume`].
    pub async fn auth_tls(&mut self) -> Result<()> {
        self.cmd("AUTH TLS").await?.check(|c| c == 234)?;
        Ok(())
    }

    pub fn into_inner(self) -> (Box<dyn Io>, Connector) {
        (self.ctrl.into_inner(), self.connect)
    }

    /// Continue on a control stream that is now in TLS.
    pub fn resume(stream: Box<dyn Io>, connect: Connector, welcome: String) -> Self {
        Self { ctrl: BufReader::new(stream), connect, mlsd: false, epsv: true, welcome }
    }

    /// After TLS: protect data connections too (PBSZ 0, PROT P).
    pub async fn protect_data(&mut self) -> Result<()> {
        self.cmd("PBSZ 0").await?.check(|c| c == 200)?;
        self.cmd("PROT P").await?.check(|c| c == 200)?;
        Ok(())
    }

    pub async fn login(&mut self, user: &str, pass: &str) -> Result<()> {
        let r = self.cmd(&format!("USER {user}")).await?;
        match r.code {
            230 => {}
            331 | 332 => {
                self.cmd(&format!("PASS {pass}")).await?.check(|c| c == 230 || c == 202)?;
            }
            _ => return Err(Error::Reply { code: r.code, text: r.text }),
        }
        // Binary transfers, and find out what the server can do.
        self.cmd("TYPE I").await?.check(|c| c == 200)?;
        if let Ok(r) = self.cmd("FEAT").await
            && r.code == 211
        {
            let up = r.text.to_ascii_uppercase();
            // RFC 3659 names the feature MLST; a few servers list MLSD instead.
            self.mlsd = up.lines().any(|l| l.trim_start().starts_with("MLST") || l.trim_start().starts_with("MLSD"));
        }
        let _ = self.cmd("OPTS UTF8 ON").await;
        Ok(())
    }

    pub async fn pwd(&mut self) -> Result<String> {
        let r = self.cmd("PWD").await?.check(|c| c == 257)?;
        // 257 "/home/me" is the current directory. Quotes inside are doubled.
        let t = &r.text;
        match (t.find('"'), t.rfind('"')) {
            (Some(a), Some(b)) if b > a => Ok(t[a + 1..b].replace("\"\"", "\"")),
            _ => Ok("/".into()),
        }
    }

    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        self.cmd(&format!("MKD {path}")).await?.check(|c| c == 257 || c == 250)?;
        Ok(())
    }
    pub async fn rmdir(&mut self, path: &str) -> Result<()> {
        self.cmd(&format!("RMD {path}")).await?.check(|c| c == 250)?;
        Ok(())
    }
    pub async fn delete(&mut self, path: &str) -> Result<()> {
        self.cmd(&format!("DELE {path}")).await?.check(|c| c == 250)?;
        Ok(())
    }
    pub async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.cmd(&format!("RNFR {from}")).await?.check(|c| c == 350)?;
        self.cmd(&format!("RNTO {to}")).await?.check(|c| c == 250)?;
        Ok(())
    }
    pub async fn size(&mut self, path: &str) -> Result<u64> {
        let r = self.cmd(&format!("SIZE {path}")).await?.check(|c| c == 213)?;
        r.text.trim().parse().map_err(|_| Error::Protocol(format!("SIZE said {}", r.text)))
    }
    pub async fn quit(&mut self) {
        let _ = self.cmd("QUIT").await;
    }

    /// List a directory, with MLSD when the server has it and LIST otherwise.
    pub async fn list(&mut self, path: &str) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        let cmd = if self.mlsd { format!("MLSD {path}") } else { format!("LIST -a {path}") };
        let mut data = self.open_data(&cmd).await?;
        let mut raw = Vec::new();
        tokio::io::copy(&mut data, &mut raw).await?;
        let _ = data.shutdown().await;
        drop(data);
        self.read_reply().await?.check(|c| c == 226 || c == 250)?;
        let text = String::from_utf8_lossy(&raw);
        for line in text.lines() {
            let e = if self.mlsd { parse_mlsd(line) } else { parse_list(line) };
            if let Some(e) = e.filter(|e| e.name != "." && e.name != "..") {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// Download `path` into `sink`. Returns the bytes copied.
    pub async fn retr<W: AsyncWrite + Unpin + ?Sized>(
        &mut self,
        path: &str,
        sink: &mut W,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<u64> {
        let mut data = self.open_data(&format!("RETR {path}")).await?;
        let n = copy_progress(&mut data, sink, progress).await?;
        drop(data);
        self.read_reply().await?.check(|c| c == 226 || c == 250)?;
        Ok(n)
    }

    /// Upload from `src` to `path`.
    pub async fn stor<R: AsyncRead + Unpin + ?Sized>(
        &mut self,
        path: &str,
        src: &mut R,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<u64> {
        let mut data = self.open_data(&format!("STOR {path}")).await?;
        let n = copy_progress(src, &mut data, progress).await?;
        data.shutdown().await?;
        drop(data);
        self.read_reply().await?.check(|c| c == 226 || c == 250)?;
        Ok(n)
    }

    async fn open_data(&mut self, cmd: &str) -> Result<Box<dyn Io>> {
        let addr = self.passive().await?;
        let fut = (self.connect)(addr);
        let data = fut.await?;
        let r = self.cmd(cmd).await?;
        if r.code != 150 && r.code != 125 {
            return Err(Error::Reply { code: r.code, text: r.text });
        }
        Ok(data)
    }

    async fn passive(&mut self) -> Result<DataAddr> {
        if self.epsv {
            let r = self.cmd("EPSV").await?;
            if r.code == 229 {
                return parse_epsv(&r.text);
            }
            self.epsv = false;
        }
        let r = self.cmd("PASV").await?.check(|c| c == 227)?;
        parse_pasv(&r.text)
    }

    /// Send a command and read its reply.
    pub async fn cmd(&mut self, line: &str) -> Result<Reply> {
        if line.contains(['\r', '\n']) {
            return Err(Error::Protocol("a command cannot contain a line break".into()));
        }
        let s = self.ctrl.get_mut();
        s.write_all(line.as_bytes()).await?;
        s.write_all(b"\r\n").await?;
        s.flush().await?;
        self.read_reply().await
    }

    async fn read_reply(&mut self) -> Result<Reply> {
        let mut first = String::new();
        if self.ctrl.read_line(&mut first).await? == 0 {
            return Err(Error::Protocol("the server closed the connection".into()));
        }
        let code: u16 =
            first.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| Error::Protocol(first.trim().to_string()))?;
        let mut text = first.get(4..).unwrap_or("").trim_end().to_string();
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            for _ in 0..10_000 {
                let mut line = String::new();
                if self.ctrl.read_line(&mut line).await? == 0 {
                    break;
                }
                if line.starts_with(&end) {
                    text.push('\n');
                    text.push_str(line[4..].trim_end());
                    break;
                }
                text.push('\n');
                text.push_str(line.trim_end());
            }
        }
        Ok(Reply { code, text })
    }
}

async fn copy_progress<R: AsyncRead + Unpin + ?Sized, W: AsyncWrite + Unpin + ?Sized>(
    r: &mut R,
    w: &mut W,
    progress: &(dyn Fn(u64) + Sync),
) -> std::io::Result<u64> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = match r.read(&mut buf).await {
            Ok(n) => n,
            // TLS data connections often end without close_notify.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Err(e) => return Err(e),
        };
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

/// `229 Entering Extended Passive Mode (|||6446|)`
pub fn parse_epsv(text: &str) -> Result<DataAddr> {
    let inner = text.split('(').nth(1).and_then(|s| s.split(')').next()).unwrap_or("");
    let d = inner.chars().next().unwrap_or('|');
    let port = inner.split(d).nth(3).and_then(|p| p.parse().ok());
    port.map(DataAddr::SameHost).ok_or_else(|| Error::Protocol(format!("EPSV said {text}")))
}

/// `227 Entering Passive Mode (192,168,1,2,19,137)`
pub fn parse_pasv(text: &str) -> Result<DataAddr> {
    let nums: Vec<u16> =
        text.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
    // The reply code itself may be in the text; take the last six numbers.
    if nums.len() < 6 || nums[nums.len() - 6..].iter().any(|&n| n > 255) {
        return Err(Error::Protocol(format!("PASV said {text}")));
    }
    let n = &nums[nums.len() - 6..];
    let ip = std::net::Ipv4Addr::new(n[0] as u8, n[1] as u8, n[2] as u8, n[3] as u8);
    Ok(DataAddr::Addr(ip, n[4] * 256 + n[5]))
}

/// `type=dir;modify=20240102030405;size=123; name`
pub fn parse_mlsd(line: &str) -> Option<Entry> {
    let (facts, name) = line.split_once(' ')?;
    let mut e = Entry { name: name.to_string(), ..Entry::default() };
    for f in facts.split(';') {
        let Some((k, v)) = f.split_once('=') else { continue };
        match k.to_ascii_lowercase().as_str() {
            "type" => {
                let v = v.to_ascii_lowercase();
                if v == "cdir" || v == "pdir" {
                    return None;
                }
                e.is_dir = v == "dir";
                e.is_link = v.starts_with("os.unix=symlink") || v.starts_with("os.unix=slink");
            }
            "size" => e.size = v.parse().ok(),
            "modify" => e.modified = parse_mlsd_time(v),
            _ => {}
        }
    }
    Some(e)
}

fn parse_mlsd_time(v: &str) -> Option<i64> {
    let d = v.get(..14)?;
    let n = |a: usize, b: usize| d.get(a..b)?.parse::<i64>().ok();
    Some(days_from_civil(n(0, 4)?, n(4, 6)?, n(6, 8)?) * 86400 + n(8, 10)? * 3600 + n(10, 12)? * 60 + n(12, 14)?)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A Unix `ls -l` line, or a DOS/IIS one.
pub fn parse_list(line: &str) -> Option<Entry> {
    let line = line.trim_end();
    if line.is_empty() || line.starts_with("total ") {
        return None;
    }
    let first = line.chars().next()?;
    if matches!(first, 'd' | '-' | 'l' | 'b' | 'c' | 'p' | 's') {
        // perms links owner group size month day time-or-year name
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 9 {
            return None;
        }
        let size = fields[4].parse().ok();
        let mut name = line[nth_field_offset(line, 8)?..].to_string();
        let is_link = first == 'l';
        if is_link && let Some((n, _)) = name.split_once(" -> ") {
            name = n.to_string();
        }
        return Some(Entry { name, is_dir: first == 'd', is_link, size, modified: None });
    }
    // 01-02-24  03:04PM       <DIR>          folder
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() >= 4 && fields[0].contains('-') {
        let start = nth_field_offset(line, 3)?;
        let is_dir = fields[2].eq_ignore_ascii_case("<DIR>");
        return Some(Entry {
            name: line[start..].to_string(),
            is_dir,
            is_link: false,
            size: if is_dir { None } else { fields[2].parse().ok() },
            modified: None,
        });
    }
    None
}

/// Byte offset where the nth whitespace-separated field starts (0-based).
fn nth_field_offset(line: &str, n: usize) -> Option<usize> {
    let mut count = 0;
    let mut in_field = false;
    for (i, c) in line.char_indices() {
        if c.is_whitespace() {
            in_field = false;
        } else if !in_field {
            if count == n {
                return Some(i);
            }
            count += 1;
            in_field = true;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_replies() {
        assert_eq!(parse_epsv("Entering Extended Passive Mode (|||6446|)").unwrap(), DataAddr::SameHost(6446));
        assert_eq!(
            parse_pasv("Entering Passive Mode (192,168,1,2,19,137).").unwrap(),
            DataAddr::Addr(std::net::Ipv4Addr::new(192, 168, 1, 2), 19 * 256 + 137)
        );
        assert!(parse_pasv("nope").is_err());
        assert!(parse_epsv("()").is_err());
    }

    #[test]
    fn mlsd_lines() {
        let e = parse_mlsd("type=file;size=1234;modify=20240102030405;perm=r; notes.txt").unwrap();
        assert_eq!((e.name.as_str(), e.is_dir, e.size), ("notes.txt", false, Some(1234)));
        assert_eq!(e.modified, Some(1_704_164_645));
        assert!(parse_mlsd("type=dir;modify=20240102030405; my folder").unwrap().is_dir);
        assert!(parse_mlsd("type=cdir; .").is_none());
    }

    #[test]
    fn list_lines() {
        let e = parse_list("-rw-r--r--    1 ftp      ftp          1024 Jan 02 03:04 read me.txt").unwrap();
        assert_eq!((e.name.as_str(), e.size, e.is_dir), ("read me.txt", Some(1024), false));
        let d = parse_list("drwxr-xr-x    2 ftp      ftp          4096 Jan  2  2023 pub").unwrap();
        assert!(d.is_dir && d.name == "pub");
        let l = parse_list("lrwxrwxrwx    1 0 0 7 Jan 02 03:04 latest -> v1.2.3").unwrap();
        assert!(l.is_link && l.name == "latest");
        let w = parse_list("01-02-24  03:04PM       <DIR>          My Docs").unwrap();
        assert!(w.is_dir && w.name == "My Docs");
        let f = parse_list("01-02-24  03:04PM              12345 file.zip").unwrap();
        assert_eq!(f.size, Some(12345));
        assert!(parse_list("total 12").is_none());
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
    }
}
