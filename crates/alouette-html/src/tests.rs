use crate::*;

fn texts(p: &Page, w: usize) -> Vec<String> {
    layout(p, w, &Default::default()).iter().map(|l| l.text().trim_end().to_string()).collect()
}

#[test]
fn paragraphs_headings_and_links() {
    let p = Page::parse(
        r#"<html lang="en"><head><title> A  page </title><base href="https://example.org/dir/">
        <link rel="alternate" type="application/rss+xml" href="/feed.xml"></head>
        <body><h1>Hello</h1><p>Some <b>bold</b> and <a href="other.html">a   link</a>.</p>
        <script>var x = 1;</script><p hidden>secret</p><p style="display: none">gone</p></body></html>"#,
        "https://example.org/start",
    );
    assert_eq!(p.title, "A page");
    assert_eq!(p.lang.as_deref(), Some("en"));
    assert_eq!(p.feeds, [("application/rss+xml".to_string(), "https://example.org/feed.xml".to_string())]);
    assert_eq!(p.links[0].url, "https://example.org/dir/other.html");
    assert_eq!(texts(&p, 80), ["Hello", "━━━━━", "", "Some bold and a link."]);
    let para = layout(&p, 80, &Default::default()).pop().unwrap();
    let link = para.segs.iter().find(|s| s.link == Some(0)).unwrap();
    assert_eq!(link.text, "a link");
    assert!(link.fx.has(Fx::UNDERLINE));
}

#[test]
fn wraps_words_and_keeps_styles() {
    let p = Page::parse("<p>one two three <i>four five</i> six</p>", "about:blank");
    assert_eq!(texts(&p, 10), ["one two", "three four", "five six"]);
    let lines = layout(&p, 10, &Default::default());
    assert!(lines[1].segs.iter().any(|s| s.text == "four" && s.fx.has(Fx::ITALIC)));
    // Very long words are broken.
    let p = Page::parse("<p>abcdefghijklmnop</p>", "about:blank");
    assert_eq!(texts(&p, 10), ["abcdefghij", "klmnop"]);
}

#[test]
fn lists_quotes_pre_and_breaks() {
    let p = Page::parse(
        "<ul><li>a</li><li>b<ol start=3><li>c</li><li>d</li></ol></li></ul><blockquote><p>q1</p><p>q2</p></blockquote>\
         <pre>\n  x = 1\n  y = 2\n</pre><p>line<br>break</p><hr>",
        "about:blank",
    );
    assert_eq!(
        texts(&p, 40),
        [
            "• a",
            "• b",
            "  3. c",
            "  4. d",
            "",
            "│ q1",
            "│",
            "│ q2",
            "",
            "  x = 1",
            "  y = 2",
            "",
            "line",
            "break",
            "",
            &"─".repeat(40)
        ]
    );
}

#[test]
fn tables_line_up() {
    let p = Page::parse(
        "<table><tr><th>Name</th><th>Size</th></tr><tr><td>alouette</td><td>3 MB</td></tr><tr><td>x</td><td>12 KB</td></tr></table>",
        "about:blank",
    );
    assert_eq!(texts(&p, 40), ["Name     │ Size", "─────────┼──────", "alouette │ 3 MB", "x        │ 12 KB"]);
    // Layout tables are read as plain blocks.
    let p = Page::parse("<table><tr><td><table><tr><td>inner</td></tr></table></td></tr></table>", "about:blank");
    assert_eq!(texts(&p, 40), ["inner"]);
}

#[test]
fn forms_submit() {
    let mut p = Page::parse(
        r#"<form action="/search" method="get"><input name="q" placeholder="Search"><input type=hidden name=t value=web>
        <input type=checkbox name=safe checked><input type=radio name=r value=a checked><input type=radio name=r value=b>
        <select name=lang><option value=en>English</option><option value=fr selected>Français</option></select>
        <input type=submit name=go value="Go"></form>
        <form method=post action="https://example.org/login"><input name=user><input type=password name=pw><button>Sign in</button></form>"#,
        "https://example.org/",
    );
    assert_eq!(p.forms.len(), 2);
    let q = p.fields.iter().position(|f| f.name == "q").unwrap();
    p.fields[q].value = "rust tui".into();
    let b = p.fields.iter().position(|f| f.name == "r" && f.value == "b").unwrap();
    p.check_radio(b);
    let go = p.fields.iter().position(|f| f.name == "go").unwrap();
    let (m, url, body, _) = p.submit(0, Some(go)).unwrap();
    assert_eq!(m, "GET");
    assert_eq!(url, "https://example.org/search?q=rust+tui&t=web&safe=on&r=b&lang=fr&go=Go");
    assert!(body.is_none());
    let user = p.fields.iter().position(|f| f.name == "user").unwrap();
    p.fields[user].value = "me".into();
    let (m, url, body, ct) = p.submit(1, None).unwrap();
    assert_eq!((m.as_str(), url.as_str()), ("POST", "https://example.org/login"));
    assert_eq!(body.as_deref(), Some("user=me&pw="));
    assert_eq!(ct, "application/x-www-form-urlencoded");
    let shown = texts(&p, 80).join("\n");
    assert!(shown.contains("[rust tui________]"), "{shown}");
    assert!(shown.contains("[x]") && shown.contains("[x] ( ) (•)"), "{shown}");
    assert!(shown.contains("[x]") && shown.contains("[Français ▾]") && shown.contains("[ Go ]"));
    assert!(shown.contains("[ Sign in ]"), "{shown}");
}

#[test]
fn images_and_refresh() {
    let p = Page::parse(
        r#"<head><meta http-equiv="refresh" content="0; url=/next"></head><p><img src="a.png" alt="A cat"></p>
        <p>text <img src="i.png" alt="icon"> more</p><img src="t.gif" width=1 height=1><a href="/x"><img src="logo.png" alt="Logo"></a>"#,
        "https://example.org/",
    );
    assert_eq!(p.refresh, Some((0, "https://example.org/next".to_string())));
    assert_eq!(p.images.len(), 3);
    assert_eq!(texts(&p, 80), ["[image: A cat]", "", "text [icon] more", "", "[Logo]"]);
}

#[test]
fn plain_text_for_mail() {
    let html = "<p>Hi <a href='https://example.org/'>there</a>!</p><p>Bye <a href='https://b.org/'>https://b.org/</a></p><img src=x.gif>";
    assert_eq!(
        to_text(html, 72, LinkStyle::Numbered),
        "Hi there[1]!\n\nBye https://b.org/[2]\n\n[1] https://example.org/\n[2] https://b.org/"
    );
    assert_eq!(to_text(html, 72, LinkStyle::Inline), "Hi there <https://example.org/>!\n\nBye https://b.org/");
    assert_eq!(to_text(html, 72, LinkStyle::Hidden), "Hi there!\n\nBye https://b.org/");
}

#[test]
fn markdown_round_trip() {
    let p = Page::parse(
        "<title>T</title><h2>Sub</h2><p>A <b>b</b> <code>c</code> <a href='https://x.org/'>link</a>.</p><ul><li>one</li><li>two</li></ul><pre>code</pre>\
         <table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>",
        "https://x.org/page",
    );
    let md = markdown::to_markdown(&p);
    assert_eq!(
        md,
        "# T\n\n## Sub\n\nA **b** `c` [link](https://x.org/).\n\n- one\n- two\n\n```\ncode\n```\n\n| **a** | **b** |\n| --- | --- |\n| 1 | 2 |\n\n---\nSource: <https://x.org/page>\n"
    );
}

#[test]
fn reader_mode_finds_the_article() {
    let filler = "This sentence is part of the article, and it goes on for a while, with commas. ".repeat(4);
    let html = format!(
        r#"<html><head><title>Big news | The Paper</title></head><body>
        <div class="nav"><a href="/">Home</a> <a href="/world">World</a> <a href="/sport">Sport</a></div>
        <div id="sidebar"><p>Popular: <a href="/1">one</a>, <a href="/2">two</a>, and <a href="/3">three</a> things.</p></div>
        <div class="article-body"><h1>Big news</h1><p>{filler}</p><p>{filler}</p><p>{filler}</p></div>
        <div class="comments"><p>First! This comment is long enough to count as a paragraph, maybe.</p></div>
        <footer>© The Paper</footer></body></html>"#
    );
    let full = Page::parse(&html, "https://paper.example/");
    assert!(full.plain_text().contains("Popular"));
    let r = Page::parse_reader(&html, "https://paper.example/");
    let t = r.plain_text();
    assert_eq!(r.title, "Big news");
    assert!(t.starts_with("Big news"), "{t}");
    assert!(t.contains("part of the article"));
    assert!(!t.contains("Popular") && !t.contains("Home") && !t.contains("First!") && !t.contains("©"), "{t}");
    // Short pages stay whole.
    let r = Page::parse_reader("<p>Just this.</p>", "about:blank");
    assert_eq!(r.plain_text(), "Just this.");
}

#[test]
fn script_only_pages_are_flagged() {
    let p = Page::parse("<div id=root></div><script src=app.js></script>", "about:blank");
    assert!(p.needs_script);
    let p = Page::parse("<noscript><p>Please enable JavaScript.</p></noscript><script></script>", "about:blank");
    assert_eq!(p.plain_text(), "Please enable JavaScript.");
}
