//! A CardDAV and CalDAV server and an LDAP directory, in memory, for trying the Contacts
//! and Calendar windows by hand. `cargo run -p alouette-pim --example fake_dav` prints
//! their addresses. The DAV user is "me" with any password.

use alouette_pim::ldap::{Entry, testserver as ldap};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (dav, _state) = alouette_pim::dav::testserver::start().await;
    let people = vec![
        Entry {
            dn: "uid=ada,dc=example,dc=org".into(),
            attrs: vec![
                ("cn".into(), vec!["Ada Lovelace".into()]),
                ("mail".into(), vec!["ada@example.org".into()]),
                ("telephoneNumber".into(), vec!["+44 20 7946 0000".into()]),
                ("title".into(), vec!["Analyst".into()]),
                ("o".into(), vec!["Analytical Engines".into()]),
            ],
        },
        Entry {
            dn: "uid=alan,dc=example,dc=org".into(),
            attrs: vec![("cn".into(), vec!["Alan Kay".into()]), ("mail".into(), vec!["alan@example.org".into()])],
        },
    ];
    let ldap = ldap::start(people).await;
    println!("dav = \"{dav}\"\nldap = \"{ldap}\"");
    std::future::pending::<()>().await;
}
