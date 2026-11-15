//! Signing in to CardDAV and CalDAV accounts, and syncing them, for the Contacts and
//! Calendar windows.

use std::sync::Arc;

use alouette_core::config::DavAccount;
use alouette_net::Net;
use alouette_pim::dav::{self, Client, Kind, SyncReport};
use alouette_pim::store::Folder;
use alouette_store::secrets::SecretStore;
use alouette_ui::ask::{Asker, Question};
use url::Url;

fn secret_key(kind: Kind, name: &str) -> String {
    match kind {
        Kind::Contacts => format!("carddav/{name}"),
        Kind::Calendars => format!("caldav/{name}"),
    }
}

/// Where the account's collection is: the one remembered from last time, else found on
/// the server (and remembered).
async fn collection(client: &Client, acct: &DavAccount, kind: Kind, folder: &Folder) -> Result<Url, String> {
    let saved = folder.dir.join(".collection");
    if let Ok(u) = std::fs::read_to_string(&saved)
        && let Ok(u) = Url::parse(u.trim())
    {
        return Ok(u);
    }
    let base = Url::parse(acct.url.trim()).map_err(|e| format!("{}: {e}", acct.url))?;
    let found = client.discover(&base, kind).await?;
    let pick = match &acct.collection {
        Some(want) => found
            .iter()
            .find(|c| {
                c.name.eq_ignore_ascii_case(want) || c.url.trim_end_matches('/').ends_with(want.trim_matches('/'))
            })
            .ok_or_else(|| {
                let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
                format!("no collection called {want}; the server has {}", names.join(", "))
            })?,
        None => &found[0],
    };
    let u = Url::parse(&pick.url).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&folder.dir).map_err(|e| e.to_string())?;
    let _ = std::fs::write(&saved, u.as_str());
    Ok(u)
}

/// Sync one account into its folder, asking for the password when there is none saved or
/// the saved one stops working.
pub async fn sync(
    net: &Net,
    secrets: &Arc<dyn SecretStore>,
    asker: &Asker,
    acct: &DavAccount,
    kind: Kind,
    folder: &Folder,
) -> Result<SyncReport, String> {
    let key = secret_key(kind, &acct.name);
    let host = Url::parse(&acct.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    for round in 0..3 {
        let saved = if round == 0 { secrets.get(&key).ok().flatten() } else { None };
        let pw = match saved {
            Some(p) => p,
            None if acct.user.is_empty() => String::new(),
            None => {
                let q = Question {
                    title: format!("{} password", if kind == Kind::Contacts { "Address book" } else { "Calendar" }),
                    body: vec![
                        format!("Password for {} on {host}.", acct.user),
                        if round > 0 { "That did not work; try again.".into() } else { String::new() },
                    ],
                    prompt: "Password".into(),
                    secret: true,
                    yes_no: false,
                };
                asker.ask(q).await.ok_or("signing in was cancelled")?
            }
        };
        let client = Client::new(net.clone(), &acct.user, &pw);
        let res = async {
            let coll = collection(&client, acct, kind, folder).await?;
            dav::sync(&client, &coll, kind, folder).await
        }
        .await;
        match res {
            Ok(r) => {
                if !acct.user.is_empty() {
                    let _ = secrets.set(&key, &pw);
                }
                return Ok(r);
            }
            Err(e) if e.contains("401") && !acct.user.is_empty() => {
                let _ = secrets.delete(&key);
            }
            Err(e) => return Err(e),
        }
    }
    Err("the server did not accept the password".into())
}

/// The password for an LDAP directory, asked for once and kept.
pub async fn directory_password(
    secrets: &Arc<dyn SecretStore>,
    asker: &Asker,
    name: &str,
    bind_dn: &str,
) -> Option<String> {
    if bind_dn.is_empty() {
        return Some(String::new());
    }
    let key = format!("ldap/{name}");
    if let Some(p) = secrets.get(&key).ok().flatten() {
        return Some(p);
    }
    let q = Question {
        title: "Directory password".into(),
        body: vec![format!("Password for {bind_dn} on the {name} directory.")],
        prompt: "Password".into(),
        secret: true,
        yes_no: false,
    };
    let p = asker.ask(q).await?;
    let _ = secrets.set(&key, &p);
    Some(p)
}

pub fn forget_directory_password(secrets: &Arc<dyn SecretStore>, name: &str) {
    let _ = secrets.delete(&format!("ldap/{name}"));
}
