//! DICT (RFC 2229): look words up in dictionaries such as those at dict.org.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

pub const PORT: u16 = 2628;
pub const DEFAULT_SERVER: &str = "dict.org";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the server said: {0}")]
    Server(String),
    #[error("the server closed the connection")]
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub word: String,
    pub database: String,
    pub database_name: String,
    pub text: String,
}

/// Define a word in every database (`*`), or one named database. An unknown word gives
/// an empty list.
pub async fn define<S: AsyncRead + AsyncWrite + Unpin>(
    s: S,
    word: &str,
    database: &str,
) -> Result<Vec<Definition>, Error> {
    let mut r = BufReader::new(s);
    let banner = line(&mut r).await?;
    if !banner.starts_with("220") {
        return Err(Error::Server(banner));
    }
    let word = quote(word);
    r.get_mut().write_all(format!("CLIENT Alouette\r\nDEFINE {database} {word}\r\nQUIT\r\n").as_bytes()).await?;
    r.get_mut().flush().await?;
    let mut defs = vec![];
    loop {
        let l = line(&mut r).await?;
        let code = &l[..l.len().min(3)];
        match code {
            "250" if l.contains("ok") && !defs.is_empty() => break,
            "250" => {} // CLIENT ok
            "552" => break,
            "150" => {}
            "151" => {
                // 151 "word" database "Database name"
                let parts = split_quoted(&l[4..]);
                let mut text = String::new();
                loop {
                    let body = line(&mut r).await?;
                    if body == "." {
                        break;
                    }
                    text.push_str(body.strip_prefix("..").map(|b| format!(".{b}")).as_deref().unwrap_or(&body));
                    text.push('\n');
                }
                defs.push(Definition {
                    word: parts.first().cloned().unwrap_or_default(),
                    database: parts.get(1).cloned().unwrap_or_default(),
                    database_name: parts.get(2).cloned().unwrap_or_default(),
                    text,
                });
            }
            "221" => break,
            c if c.starts_with('5') || c.starts_with('4') => return Err(Error::Server(l)),
            _ => {}
        }
    }
    Ok(defs)
}

async fn line<R: AsyncBufReadExt + Unpin>(r: &mut R) -> Result<String, Error> {
    let mut s = String::new();
    if r.read_line(&mut s).await? == 0 {
        return Err(Error::Closed);
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

fn quote(w: &str) -> String {
    let clean: String = w.chars().filter(|c| !c.is_control() && *c != '"').collect();
    format!("\"{clean}\"")
}

fn split_quoted(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut inq = false;
    for c in s.chars() {
        match c {
            '"' => inq = !inq,
            ' ' if !inq => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn defines_words() {
        let (c, mut s) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            s.write_all(b"220 dict.example ready\r\n").await.unwrap();
            let mut buf = vec![0u8; 256];
            let n = s.read(&mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).contains("DEFINE * \"minuet\""));
            s.write_all(
                b"250 ok\r\n150 1 definitions retrieved\r\n151 \"minuet\" wn \"WordNet (r) 3.0\"\r\nminuet\r\n  n 1: a stately court dance\r\n..dot\r\n.\r\n250 ok [d/m/c = 1/0/2]\r\n221 bye\r\n",
            )
            .await
            .unwrap();
        });
        let d = define(c, "minuet", "*").await.unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].database.as_str(), d[0].database_name.as_str()), ("wn", "WordNet (r) 3.0"));
        assert!(d[0].text.contains("stately court dance") && d[0].text.contains("\n.dot\n"));
    }

    #[tokio::test]
    async fn unknown_word_is_empty() {
        let (c, mut s) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            s.write_all(b"220 ready\r\n").await.unwrap();
            let mut buf = vec![0u8; 256];
            let _ = s.read(&mut buf).await.unwrap();
            s.write_all(b"250 ok\r\n552 no match\r\n221 bye\r\n").await.unwrap();
        });
        assert!(define(c, "zzxq", "*").await.unwrap().is_empty());
    }
}
