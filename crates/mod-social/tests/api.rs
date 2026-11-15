//! The Mastodon client against a tiny fake server.

use alouette_net::Net;
use mod_social::api::{Client, Timeline};
use mod_social::testserver;

fn net() -> Net {
    Net::new(&Default::default()).unwrap()
}

#[tokio::test]
async fn signs_in_reads_posts_and_favourites() {
    let (base, log) = testserver::start().await;
    let c = Client::new(net(), &base, None);
    let (id, secret) = c.register().await.unwrap();
    assert_eq!((id.as_str(), secret.as_str()), ("cid", "sec"));
    assert!(c.authorize_url(&id).contains("client_id=cid"));
    let token = c.token(&id, &secret, " code123 ").await.unwrap();
    assert_eq!(token, "tok");

    // Without the token the server refuses, and its words come through.
    assert_eq!(c.timeline(Timeline::Home, None, None).await.unwrap_err(), "The access token is invalid");

    let c = Client::new(net(), &base, Some(token));
    let posts = c.timeline(Timeline::Home, Some("0"), None).await.unwrap();
    assert_eq!(posts.iter().map(|p| p.text()).collect::<Vec<_>>(), ["Second", "First"]);

    let s = c.post("Hello & bye", Some("2"), "spoilers", "unlisted").await.unwrap();
    assert_eq!(s.id, "3");
    let s = c.act("2", "favourite", true).await.unwrap();
    assert_eq!(s.favourited, Some(true));
    let ctx = c.context("2").await.unwrap();
    assert_eq!(ctx.ancestors.len(), 1);
    let n = c.notifications(None).await.unwrap();
    assert_eq!(n.len(), 1);
    assert!(c.act("9", "reblog", false).await.unwrap_err().contains("Record not found"));

    let log = log.lock().unwrap();
    assert!(log.iter().any(|l| l.contains("code=code123")), "{log:?}");
    assert!(log.iter().any(|l| l.starts_with("GET /api/v1/timelines/home?limit=40&since_id=0 ")), "{log:?}");
    let post = log.iter().find(|l| l.starts_with("POST /api/v1/statuses ")).unwrap();
    assert!(
        post.contains("status=Hello+%26+bye")
            && post.contains("in_reply_to_id=2")
            && post.contains("spoiler_text=spoilers")
    );
    assert!(log.iter().any(|l| l.starts_with("POST /api/v1/statuses/9/unreblog ")));
}
