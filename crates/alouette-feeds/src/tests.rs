use super::*;

#[test]
fn rss2() {
    let x = br#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<channel><title>Example &amp; Co</title><link>https://example.org/</link><description>News</description>
<item><title>One</title><link>/one</link><guid>g1</guid><pubDate>Tue, 02 Jan 2024 03:04:05 GMT</pubDate>
<dc:creator>Ada</dc:creator><description>short</description><content:encoded><![CDATA[<p>The long one&nbsp;here</p>]]></content:encoded>
<enclosure url="https://example.org/a.mp3" type="audio/mpeg" length="1234"/></item>
<item><title>Two &hellip;</title><link>https://example.org/two</link></item>
</channel></rss>"#;
    let f = parse(x, Url::parse("https://example.org/feed").ok().as_ref()).unwrap();
    assert_eq!(f.title, "Example & Co");
    assert_eq!(f.items.len(), 2);
    let a = &f.items[0];
    assert_eq!(a.id, "g1");
    assert_eq!(a.link.as_deref(), Some("https://example.org/one"));
    assert_eq!(a.date, Some(1704164645));
    assert_eq!(a.author.as_deref(), Some("Ada"));
    assert!(a.body.contains("The long one"));
    assert_eq!(a.enclosures, [("https://example.org/a.mp3".into(), "audio/mpeg".into(), 1234)]);
    assert_eq!(f.items[1].title, "Two …");
    assert_eq!(f.items[1].id, "https://example.org/two");
}

#[test]
fn atom() {
    let x = br#"<feed xmlns="http://www.w3.org/2005/Atom"><title type="text">Blog</title>
<link rel="self" href="https://b.example/atom"/><link href="https://b.example/"/>
<author><name>Grace</name></author>
<entry><id>tag:b,2024:1</id><title>Hi</title><link rel="alternate" href="https://b.example/1"/>
<updated>2024-01-02T03:04:05Z</updated><summary>s</summary><content type="html">&lt;b&gt;full&lt;/b&gt;</content></entry></feed>"#;
    let f = parse(x, None).unwrap();
    assert_eq!(f.title, "Blog");
    assert_eq!(f.link.as_deref(), Some("https://b.example/"));
    let e = &f.items[0];
    assert_eq!(e.id, "tag:b,2024:1");
    assert_eq!(e.link.as_deref(), Some("https://b.example/1"));
    assert_eq!(e.date, Some(1704164645));
    assert_eq!(e.author.as_deref(), Some("Grace"));
    assert_eq!(e.body, "<b>full</b>");
}

#[test]
fn rdf_and_json() {
    let x = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/">
<channel rdf:about="x"><title>Old</title><link>http://o.example/</link></channel>
<item rdf:about="http://o.example/1"><title>First</title><link>http://o.example/1</link></item></rdf:RDF>"#;
    let f = parse(x, None).unwrap();
    assert_eq!((f.title.as_str(), f.items.len()), ("Old", 1));
    assert_eq!(f.items[0].id, "http://o.example/1");
    let j = br#"{"version":"https://jsonfeed.org/version/1.1","title":"J","items":[{"id":7,"url":"https://j.example/7","content_text":"hello","date_published":"2024-01-02T03:04:05+00:00","authors":[{"name":"Kay"}]}]}"#;
    let f = parse(j, None).unwrap();
    assert_eq!(f.items[0].id, "7");
    assert_eq!(f.items[0].author.as_deref(), Some("Kay"));
    assert!(parse(b"<html><body>no</body></html>", None).is_err());
}

#[test]
fn dates() {
    assert_eq!(parse_date("Tue, 02 Jan 2024 03:04:05 +0000"), Some(1704164645));
    assert_eq!(parse_date("Tue, 02 Jan 2024 03:04:05 EST"), Some(1704164645 + 5 * 3600));
    assert_eq!(parse_date("2024-01-02"), Some(1704153600));
    assert_eq!(parse_date("soon"), None);
}

#[test]
fn opml_and_discovery() {
    let o = r#"<opml version="2.0"><head/><body><outline text="Tech"><outline text="A" xmlUrl="https://a/feed"/></outline><outline title="B" xmlUrl="https://b/rss"/></body></opml>"#;
    let l = parse_opml(o).unwrap();
    assert_eq!(l.len(), 2);
    assert_eq!(l[0].folder.as_deref(), Some("Tech"));
    let back = parse_opml(&write_opml("Mine", &l)).unwrap();
    assert_eq!(back[1].url, "https://b/rss");
    let html = r#"<head><link rel="stylesheet" href="s.css"><LINK REL="alternate" TYPE="application/atom+xml" title="Posts" href="/atom.xml"/></head>"#;
    let d = discover(html, &Url::parse("https://s.example/blog/").unwrap());
    assert_eq!(d, [("https://s.example/atom.xml".into(), "Posts".into())]);
}
