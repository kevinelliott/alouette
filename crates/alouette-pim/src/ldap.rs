//! Looking people up in an LDAP directory (RFC 4511): a simple bind, then a subtree
//! search for names and addresses. Only as much BER as that takes.

use alouette_net::{Net, TlsOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::contact::{Contact, Typed};

/// One entry found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    pub dn: String,
    pub attrs: Vec<(String, Vec<String>)>,
}

impl Entry {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.first()).map(String::as_str)
    }
    pub fn all(&self, name: &str) -> Vec<String> {
        self.attrs.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).flat_map(|(_, v)| v.clone()).collect()
    }

    /// The entry as a contact, to show or to save.
    pub fn to_contact(&self) -> Contact {
        let name = self
            .get("displayName")
            .or(self.get("cn"))
            .map(str::to_string)
            .unwrap_or_else(|| self.dn.split(',').next().unwrap_or("").split('=').nth(1).unwrap_or("").to_string());
        let mut c = Contact::new(&name);
        if let Some(s) = self.get("sn") {
            c.family = s.to_string();
        }
        if let Some(g) = self.get("givenName") {
            c.given = g.to_string();
        }
        c.emails = self.all("mail").iter().map(|m| Typed::new("work", m)).collect();
        c.phones = self.all("telephoneNumber").iter().map(|t| Typed::new("work", t)).collect();
        c.phones.extend(self.all("mobile").iter().map(|t| Typed::new("cell", t)));
        c.org = self.get("o").or(self.get("ou")).or(self.get("department")).map(str::to_string);
        c.title = self.get("title").map(str::to_string);
        c
    }
}

// --- BER ---

fn len_bytes(n: usize) -> Vec<u8> {
    if n < 0x80 {
        vec![n as u8]
    } else {
        let b = (n as u64).to_be_bytes();
        let skip = b.iter().take_while(|x| **x == 0).count();
        let mut v = vec![0x80 | (8 - skip) as u8];
        v.extend_from_slice(&b[skip..]);
        v
    }
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend(len_bytes(content.len()));
    v.extend_from_slice(content);
    v
}

fn int(tag: u8, n: i64) -> Vec<u8> {
    let b = n.to_be_bytes();
    let mut i = 0;
    while i < 7 && ((b[i] == 0 && b[i + 1] & 0x80 == 0) || (b[i] == 0xff && b[i + 1] & 0x80 != 0)) {
        i += 1;
    }
    tlv(tag, &b[i..])
}

fn octets(tag: u8, s: &str) -> Vec<u8> {
    tlv(tag, s.as_bytes())
}

fn seq(tag: u8, parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(tag, &parts.concat())
}

/// A parsed TLV: tag and content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub body: &'a [u8],
}

/// Read one TLV from the front of `b`, returning it and the rest.
pub fn read_tlv(b: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *b.first()?;
    let l0 = *b.get(1)?;
    let (len, hdr) = if l0 & 0x80 == 0 {
        (l0 as usize, 2)
    } else {
        let n = (l0 & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *b.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let body = b.get(hdr..hdr.checked_add(len)?)?;
    Some((Tlv { tag, body }, &b[hdr + len..]))
}

fn children(b: &[u8]) -> Vec<Tlv<'_>> {
    let mut out = vec![];
    let mut rest = b;
    while let Some((t, r)) = read_tlv(rest) {
        out.push(t);
        rest = r;
    }
    out
}

fn as_int(b: &[u8]) -> i64 {
    let mut v: i64 = if b.first().is_some_and(|x| x & 0x80 != 0) { -1 } else { 0 };
    for x in b.iter().take(8) {
        v = (v << 8) | *x as i64;
    }
    v
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn message(id: i64, op: Vec<u8>) -> Vec<u8> {
    seq(0x30, &[int(0x02, id), op])
}

pub fn bind_request(id: i64, dn: &str, password: &str) -> Vec<u8> {
    message(id, seq(0x60, &[int(0x02, 3), octets(0x04, dn), octets(0x80, password)]))
}

/// The filter Alouette searches with: any of the name and address attributes contains
/// the words.
pub fn people_filter(q: &str) -> Vec<u8> {
    let sub = |attr: &str| seq(0xa4, &[octets(0x04, attr), seq(0x30, &[octets(0x81, q)])]);
    let any = seq(0xa1, &[sub("cn"), sub("mail"), sub("sn"), sub("givenName"), sub("displayName")]);
    if q.trim().is_empty() { octets(0x87, "objectClass") } else { any }
}

pub const ATTRS: &[&str] =
    &["cn", "displayName", "sn", "givenName", "mail", "telephoneNumber", "mobile", "o", "ou", "title", "department"];

pub fn search_request(id: i64, base: &str, filter: Vec<u8>, limit: i64) -> Vec<u8> {
    let attrs: Vec<Vec<u8>> = ATTRS.iter().map(|a| octets(0x04, a)).collect();
    message(
        id,
        seq(
            0x63,
            &[
                octets(0x04, base),
                int(0x0a, 2),
                int(0x0a, 0),
                int(0x02, limit),
                int(0x02, 10),
                tlv(0x01, &[0]),
                filter,
                seq(0x30, &attrs),
            ],
        ),
    )
}

/// A read LDAPMessage: id, operation tag and its content.
pub struct Message {
    pub id: i64,
    pub op: u8,
    pub body: Vec<u8>,
}

async fn read_message<S: AsyncRead + Unpin>(s: &mut S) -> Result<Message, String> {
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr).await.map_err(|e| format!("LDAP: {e}"))?;
    if hdr[0] != 0x30 {
        return Err("LDAP: not an LDAP answer".into());
    }
    let len = if hdr[1] & 0x80 == 0 {
        hdr[1] as usize
    } else {
        let n = (hdr[1] & 0x7f) as usize;
        if n == 0 || n > 4 {
            return Err("LDAP: bad length".into());
        }
        let mut b = vec![0u8; n];
        s.read_exact(&mut b).await.map_err(|e| format!("LDAP: {e}"))?;
        b.iter().fold(0usize, |a, x| (a << 8) | *x as usize)
    };
    if len > 16 << 20 {
        return Err("LDAP: answer too large".into());
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await.map_err(|e| format!("LDAP: {e}"))?;
    let parts = children(&body);
    let id = parts.first().filter(|t| t.tag == 0x02).map(|t| as_int(t.body)).ok_or("LDAP: no message id")?;
    let op = parts.get(1).ok_or("LDAP: no operation")?;
    Ok(Message { id, op: op.tag, body: op.body.to_vec() })
}

/// The result code and message of an LDAPResult.
fn result(body: &[u8]) -> (i64, String) {
    let p = children(body);
    let code = p.first().map(|t| as_int(t.body)).unwrap_or(-1);
    let msg = p.get(2).map(|t| text(t.body)).unwrap_or_default();
    (code, msg)
}

fn parse_entry(body: &[u8]) -> Entry {
    let p = children(body);
    let dn = p.first().map(|t| text(t.body)).unwrap_or_default();
    let attrs = p
        .get(1)
        .map(|a| {
            children(a.body)
                .into_iter()
                .filter_map(|pa| {
                    let kv = children(pa.body);
                    let name = text(kv.first()?.body);
                    let vals =
                        kv.get(1).map(|s| children(s.body).iter().map(|v| text(v.body)).collect()).unwrap_or_default();
                    Some((name, vals))
                })
                .collect()
        })
        .unwrap_or_default();
    Entry { dn, attrs }
}

/// Run a search over an open connection.
pub async fn search_on<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    bind_dn: &str,
    password: &str,
    base: &str,
    q: &str,
    limit: i64,
) -> Result<Vec<Entry>, String> {
    let io = |e: std::io::Error| format!("LDAP: {e}");
    if !bind_dn.is_empty() || !password.is_empty() {
        s.write_all(&bind_request(1, bind_dn, password)).await.map_err(io)?;
        let m = read_message(s).await?;
        let (code, msg) = result(&m.body);
        if m.op != 0x61 || code != 0 {
            return Err(match code {
                49 => "the directory did not accept that login".into(),
                _ => format!("the directory refused the login ({code}{}{msg})", if msg.is_empty() { "" } else { ": " }),
            });
        }
    }
    s.write_all(&search_request(2, base, people_filter(q), limit)).await.map_err(io)?;
    let mut out = vec![];
    loop {
        let m = read_message(s).await?;
        match m.op {
            0x64 => out.push(parse_entry(&m.body)),
            0x73 => {} // a referral; not followed
            0x65 => {
                let (code, msg) = result(&m.body);
                // 4 is "size limit exceeded": the entries so far are still good.
                if code != 0 && code != 4 {
                    return Err(format!(
                        "the directory search failed ({code}{}{msg})",
                        if msg.is_empty() { "" } else { ": " }
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    let _ = s.write_all(&message(3, vec![0x42, 0x00])).await;
    Ok(out)
}

/// Search `ldap://host[:port]/base` or `ldaps://…` for people matching `q`.
pub async fn search(net: &Net, url: &str, bind_dn: &str, password: &str, q: &str) -> Result<Vec<Entry>, String> {
    let u = url::Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
    let host = u.host_str().ok_or("the directory address has no host")?.to_string();
    let base = url::form_urlencoded::parse(format!("x={}", u.path().trim_start_matches('/')).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let fut = async {
        match u.scheme() {
            "ldaps" => {
                let mut c =
                    net.tls(&host, u.port().unwrap_or(636), &TlsOptions::default()).await.map_err(|e| e.to_string())?;
                search_on(&mut c.stream, bind_dn, password, &base, q, 50).await
            }
            "ldap" => {
                let mut c = net.tcp(&host, u.port().unwrap_or(389)).await.map_err(|e| e.to_string())?;
                search_on(&mut c.stream, bind_dn, password, &base, q, 50).await
            }
            s => Err(format!("{s}: not an LDAP address (use ldap:// or ldaps://)")),
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), fut)
        .await
        .map_err(|_| "the directory did not answer".to_string())?
}

pub mod testserver {
    //! A tiny LDAP server that accepts any bind and answers every search with the people
    //! it was given whose cn or mail holds the search text.

    use super::*;
    use tokio::net::TcpListener;

    fn substring_of(filter: &[u8]) -> String {
        // The first substring "any" value in the filter.
        fn walk(b: &[u8]) -> Option<String> {
            for t in children(b) {
                if t.tag == 0x81 {
                    return Some(text(t.body));
                }
                if t.tag & 0x20 != 0
                    && let Some(s) = walk(t.body)
                {
                    return Some(s);
                }
            }
            None
        }
        walk(filter).unwrap_or_default()
    }

    pub async fn start(people: Vec<Entry>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { break };
                let people = people.clone();
                tokio::spawn(async move {
                    while let Ok(m) = read_message(&mut s).await {
                        match m.op {
                            0x60 => {
                                let pw = children(&m.body).get(2).map(|t| text(t.body)).unwrap_or_default();
                                let code = if pw == "wrong" { 49 } else { 0 };
                                let r =
                                    message(m.id, seq(0x61, &[int(0x0a, code), octets(0x04, ""), octets(0x04, "")]));
                                let _ = s.write_all(&r).await;
                            }
                            0x63 => {
                                let parts = children(&m.body);
                                let q = parts.get(6).map(|f| {
                                    let mut v = f.tag.to_be_bytes().to_vec();
                                    v.extend(len_bytes(f.body.len()));
                                    v.extend_from_slice(f.body);
                                    substring_of(&v)
                                });
                                let q = q.unwrap_or_default().to_lowercase();
                                for p in &people {
                                    let hit = p
                                        .all("cn")
                                        .iter()
                                        .chain(p.all("mail").iter())
                                        .any(|v| v.to_lowercase().contains(&q));
                                    if !hit {
                                        continue;
                                    }
                                    let attrs: Vec<Vec<u8>> = p
                                        .attrs
                                        .iter()
                                        .map(|(k, vs)| {
                                            let vals: Vec<Vec<u8>> = vs.iter().map(|v| octets(0x04, v)).collect();
                                            seq(0x30, &[octets(0x04, k), seq(0x31, &vals)])
                                        })
                                        .collect();
                                    let e = message(m.id, seq(0x64, &[octets(0x04, &p.dn), seq(0x30, &attrs)]));
                                    let _ = s.write_all(&e).await;
                                }
                                let done =
                                    message(m.id, seq(0x65, &[int(0x0a, 0), octets(0x04, ""), octets(0x04, "")]));
                                let _ = s.write_all(&done).await;
                            }
                            0x42 => break,
                            _ => {}
                        }
                    }
                });
            }
        });
        format!("ldap://{addr}/dc=example,dc=org")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ber() {
        assert_eq!(int(0x02, 3), [0x02, 0x01, 0x03]);
        assert_eq!(int(0x02, 128), [0x02, 0x02, 0x00, 0x80]);
        assert_eq!(int(0x02, -1), [0x02, 0x01, 0xff]);
        assert_eq!(as_int(&[0x00, 0x80]), 128);
        let long = octets(0x04, &"x".repeat(300));
        assert_eq!(&long[..4], &[0x04, 0x82, 0x01, 0x2c]);
        let (t, rest) = read_tlv(&long).unwrap();
        assert_eq!(t.body.len(), 300);
        assert!(rest.is_empty());
        assert!(read_tlv(&[0x04, 0x85, 1, 1, 1, 1, 1]).is_none());
    }
}
