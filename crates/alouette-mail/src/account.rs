//! Account settings resolved from the config, with server defaults for Gmail and Microsoft 365.

use alouette_core::config::{MailAccount, MailKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tls {
    /// TLS from the first byte (993, 465).
    Implicit,
    /// Plain first, then STARTTLS, which is required (143, 587).
    StartTls,
    /// No TLS at all. Only for localhost.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub tls: Tls,
}

impl Server {
    /// `host[:port]`, with the port deciding TLS. `plain_ok` allows no TLS for localhost.
    pub fn parse(s: &str, default_port: u16) -> Result<Server, String> {
        let s = s.trim();
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') || h.ends_with(']') => {
                (h.trim_matches(['[', ']']).to_string(), p.parse().map_err(|_| format!("{s}: bad port"))?)
            }
            _ => (s.trim_matches(['[', ']']).to_string(), default_port),
        };
        if host.is_empty() {
            return Err("no server name".into());
        }
        let local = host == "localhost" || host == "127.0.0.1" || host == "::1";
        let tls = match port {
            993 | 465 => Tls::Implicit,
            _ if local && std::env::var_os("ALOUETTE_MAIL_PLAIN").is_some() => Tls::None,
            _ => Tls::StartTls,
        };
        Ok(Server { host, port, tls })
    }
}

/// How the account signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    Password,
    Google,
    Microsoft,
}

#[derive(Debug, Clone)]
pub struct Account {
    pub name: String,
    pub email: String,
    pub display_name: Option<String>,
    pub kind: MailKind,
    pub user: String,
    pub login: Login,
    pub imap: Option<Server>,
    pub smtp: Option<Server>,
    pub jmap: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub signature: Option<String>,
}

impl Account {
    pub fn from_config(a: &MailAccount) -> Result<Account, String> {
        if a.email.trim().is_empty() {
            return Err(format!("mail account {:?} has no email address", a.name));
        }
        let name = if a.name.is_empty() { a.email.clone() } else { a.name.clone() };
        let (imap_default, smtp_default, login) = match a.kind {
            MailKind::Gmail => (Some("imap.gmail.com:993"), Some("smtp.gmail.com:465"), Login::Google),
            MailKind::M365 => (Some("outlook.office365.com:993"), Some("smtp.office365.com:587"), Login::Microsoft),
            MailKind::Imap | MailKind::Jmap => (None, None, Login::Password),
        };
        let imap = a.imap.as_deref().or(imap_default).map(|s| Server::parse(s, 993)).transpose()?;
        let smtp = a.smtp.as_deref().or(smtp_default).map(|s| Server::parse(s, 465)).transpose()?;
        match a.kind {
            MailKind::Jmap if a.jmap.is_none() => {
                return Err(format!("mail account {name}: set jmap to the server's address"));
            }
            MailKind::Imap if imap.is_none() => return Err(format!("mail account {name}: set imap = \"host:993\"")),
            _ => {}
        }
        if matches!(login, Login::Google | Login::Microsoft) && a.client_id.is_none() {
            return Err(format!(
                "mail account {name}: signing in with {} needs your own OAuth app's client_id in the config",
                if login == Login::Google { "Google" } else { "Microsoft" }
            ));
        }
        Ok(Account {
            user: a.user.clone().unwrap_or_else(|| a.email.clone()),
            email: a.email.clone(),
            display_name: a.display_name.clone(),
            kind: a.kind,
            login,
            imap,
            smtp,
            jmap: a.jmap.clone(),
            client_id: a.client_id.clone(),
            client_secret: a.client_secret.clone(),
            signature: a.signature.clone(),
            name,
        })
    }

    /// Where the password or token is kept in the secret store.
    pub fn secret_key(&self, what: &str) -> String {
        format!("mail/{}/{what}", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers() {
        assert_eq!(
            Server::parse("imap.example.org", 993).unwrap(),
            Server { host: "imap.example.org".into(), port: 993, tls: Tls::Implicit }
        );
        assert_eq!(Server::parse("smtp.example.org:587", 465).unwrap().tls, Tls::StartTls);
        assert_eq!(Server::parse("[::1]:1143", 993).unwrap().host, "::1");
        assert!(Server::parse(":993", 993).is_err());
    }

    #[test]
    fn gmail_defaults_need_a_client_id() {
        let mut a = MailAccount { email: "me@gmail.com".into(), kind: MailKind::Gmail, ..Default::default() };
        assert!(Account::from_config(&a).unwrap_err().contains("client_id"));
        a.client_id = Some("x".into());
        let acct = Account::from_config(&a).unwrap();
        assert_eq!(acct.imap.unwrap().host, "imap.gmail.com");
        assert_eq!(acct.smtp.unwrap().port, 465);
        assert_eq!(acct.login, Login::Google);
        assert_eq!(acct.name, "me@gmail.com");
    }
}
