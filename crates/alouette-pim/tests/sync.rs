//! CardDAV and CalDAV sync, and LDAP search, against the built-in test servers.

use alouette_pim::dav::{self, Client, Kind};
use alouette_pim::ldap;
use alouette_pim::store::Stores;
use alouette_pim::{Contact, Event, When};
use url::Url;

fn net() -> alouette_net::Net {
    alouette_net::Net::new(&alouette_core::config::Network::default()).unwrap()
}

#[tokio::test]
async fn carddav_and_caldav_round_trip() {
    let (base, state) = dav::testserver::start().await;
    let client = Client::new(net(), "me", "pw");
    let base = Url::parse(&base).unwrap();
    let books = client.discover(&base, Kind::Contacts).await.unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].name, "Contacts");
    let cals = client.discover(&base, Kind::Calendars).await.unwrap();
    assert_eq!(cals[0].name, "Work");

    let root = std::env::temp_dir().join(format!("alouette-pim-sync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let stores = Stores::new(&root);
    let book = stores.book("Contacts");
    let url = Url::parse(&books[0].url).unwrap();

    // A card made here goes up.
    let mut ada = Contact::new("Ada Lovelace");
    ada.emails.push(alouette_pim::contact::Typed::new("work", "ada@example.org"));
    let file = alouette_pim::store::file_for(&ada.uid, "vcf");
    book.mark_synced().unwrap();
    book.put_local(&file, &ada.to_vcard()).unwrap();
    let r = dav::sync(&client, &url, Kind::Contacts, &book).await.unwrap();
    assert_eq!(r.sent, 1, "{r:?}");
    assert_eq!(state.lock().unwrap().items.len(), 1);

    // A card made on the server comes down; a second sync has nothing to do.
    let grace = Contact::new("Grace Hopper");
    state.lock().unwrap().items.insert("/dav/me/contacts/grace.vcf".into(), (grace.to_vcard(), 99));
    let r = dav::sync(&client, &url, Kind::Contacts, &book).await.unwrap();
    assert_eq!(r.fetched, 1, "{r:?}");
    let r = dav::sync(&client, &url, Kind::Contacts, &book).await.unwrap();
    assert_eq!(r.summary(), "up to date");
    let names: Vec<String> = book.list().iter().flat_map(|i| Contact::parse_all(&i.text)).map(|c| c.name).collect();
    assert!(names.contains(&"Grace Hopper".to_string()), "{names:?}");

    // Deleted here goes there; deleted there goes here.
    book.delete_local(&file).unwrap();
    state.lock().unwrap().items.remove("/dav/me/contacts/grace.vcf");
    let r = dav::sync(&client, &url, Kind::Contacts, &book).await.unwrap();
    assert_eq!((r.deleted_there, r.deleted_here), (1, 1), "{r:?}");
    assert!(book.list().is_empty());
    assert!(state.lock().unwrap().items.is_empty());

    // An event up to the calendar.
    let cal = stores.calendar("Work");
    let d = jiff::civil::date(2026, 5, 1);
    let ev = Event::new("Launch", When::Date(d), When::Date(d.tomorrow().unwrap()));
    cal.mark_synced().unwrap();
    cal.put_local(&alouette_pim::store::file_for(&ev.uid, "ics"), &alouette_pim::event::Calendar::write(&[&ev], None))
        .unwrap();
    let r = dav::sync(&client, &Url::parse(&cals[0].url).unwrap(), Kind::Calendars, &cal).await.unwrap();
    assert_eq!(r.sent, 1);
    let s = state.lock().unwrap();
    let (text, _) = s.items.values().next().unwrap();
    assert!(text.contains("SUMMARY:Launch"));
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn ldap_search() {
    let people = vec![
        ldap::Entry {
            dn: "uid=ada,dc=example,dc=org".into(),
            attrs: vec![
                ("cn".into(), vec!["Ada Lovelace".into()]),
                ("mail".into(), vec!["ada@example.org".into()]),
                ("telephoneNumber".into(), vec!["+1 555 0100".into()]),
                ("title".into(), vec!["Analyst".into()]),
            ],
        },
        ldap::Entry { dn: "uid=bob,dc=example,dc=org".into(), attrs: vec![("cn".into(), vec!["Bob".into()])] },
    ];
    let url = ldap::testserver::start(people).await;
    let found = ldap::search(&net(), &url, "cn=admin,dc=example,dc=org", "secret", "lovelace").await.unwrap();
    assert_eq!(found.len(), 1);
    let c = found[0].to_contact();
    assert_eq!(c.name, "Ada Lovelace");
    assert_eq!(c.email(), Some("ada@example.org"));
    assert_eq!(c.title.as_deref(), Some("Analyst"));
    let e = ldap::search(&net(), &url, "cn=admin", "wrong", "x").await.unwrap_err();
    assert!(e.contains("did not accept"), "{e}");
}
