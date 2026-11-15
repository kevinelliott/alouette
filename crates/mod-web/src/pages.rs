//! Alouette's own pages: the start page, bookmarks and history. They are written as HTML
//! and shown through the same renderer as everything else.

use alouette_ui::window::Cx;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A search form built from the search URL template.
pub fn search_form(template: &str) -> String {
    let Ok(u) = url::Url::parse(&template.replace("{}", "ALOUETTE_Q")) else { return String::new() };
    let mut action = u.clone();
    action.set_query(None);
    let mut fields = String::new();
    let mut found = false;
    for (k, v) in u.query_pairs() {
        if v == "ALOUETTE_Q" {
            fields.push_str(&format!(r#"<input name="{}" placeholder="Search the web"> "#, esc(&k)));
            found = true;
        } else {
            fields.push_str(&format!(r#"<input type="hidden" name="{}" value="{}">"#, esc(&k), esc(&v)));
        }
    }
    if !found {
        return String::new();
    }
    format!(
        r#"<form action="{}" method="get">{fields}<input type="submit" value="Search"></form>"#,
        esc(action.as_str())
    )
}

pub fn internal(url: &str, cx: &Cx) -> String {
    let what = url.trim_start_matches("web:").trim_start_matches("about:");
    let store = cx.store;
    let mut body = String::new();
    let title;
    match what {
        "bookmarks" => {
            title = "Bookmarks";
            let list = store.and_then(|s| s.bookmarks().ok()).unwrap_or_default();
            body.push_str("<h1>Bookmarks</h1>");
            if list.is_empty() {
                body.push_str("<p>No bookmarks yet. Press a on any page to add one.</p>");
            }
            body.push_str("<ul>");
            for b in list {
                body.push_str(&format!(
                    r#"<li><a href="{}">{}</a> <small>{}</small></li>"#,
                    esc(&b.url),
                    esc(&b.title),
                    esc(&b.tags.join(", "))
                ));
            }
            body.push_str("</ul>");
        }
        "history" => {
            title = "History";
            let list = store.and_then(|s| s.recent_history(200).ok()).unwrap_or_default();
            body.push_str("<h1>History</h1><ul>");
            for h in list.iter().filter(|h| h.url.starts_with("http")) {
                let when = h.visited_at.format(&time_fmt()).unwrap_or_default();
                body.push_str(&format!(
                    r#"<li><a href="{}">{}</a> <small>{when}</small></li>"#,
                    esc(&h.url),
                    esc(if h.title.is_empty() { &h.url } else { &h.title })
                ));
            }
            body.push_str("</ul>");
        }
        _ => {
            title = "Alouette Web";
            body.push_str("<h1>Alouette Web</h1>");
            body.push_str(&search_form(&cx.config.web.search));
            body.push_str("<p>Press <b>o</b> to go to an address or search, <b>f</b> to follow a link by its letters, <b>Tab</b> to move between links and fields, <b>b</b> to go back, <b>R</b> for reader mode, <b>/</b> to find, <b>a</b> to bookmark, <b>s</b> to save the page as Markdown and <b>O</b> to open it in your own browser.</p>");
            let marks = store.and_then(|s| s.bookmarks().ok()).unwrap_or_default();
            let marks: Vec<_> = marks.into_iter().filter(|b| b.url.starts_with("http")).take(20).collect();
            if !marks.is_empty() {
                body.push_str(r#"<h2>Bookmarks</h2><ul>"#);
                for b in marks {
                    body.push_str(&format!(r#"<li><a href="{}">{}</a></li>"#, esc(&b.url), esc(&b.title)));
                }
                body.push_str(r#"</ul><p><a href="web:bookmarks">All bookmarks</a></p>"#);
            }
            let recent = store.and_then(|s| s.recent_history(100).ok()).unwrap_or_default();
            let mut seen = std::collections::HashSet::new();
            let recent: Vec<_> = recent
                .into_iter()
                .filter(|h| h.url.starts_with("http") && seen.insert(h.url.clone()))
                .take(10)
                .collect();
            if !recent.is_empty() {
                body.push_str("<h2>Recently</h2><ul>");
                for h in recent {
                    body.push_str(&format!(
                        r#"<li><a href="{}">{}</a></li>"#,
                        esc(&h.url),
                        esc(if h.title.is_empty() { &h.url } else { &h.title })
                    ));
                }
                body.push_str(r#"</ul><p><a href="web:history">All history</a></p>"#);
            }
        }
    }
    format!("<html><head><title>{title}</title></head><body>{body}</body></html>")
}

fn time_fmt() -> Vec<time::format_description::BorrowedFormatItem<'static>> {
    time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]").to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_forms_from_templates() {
        let f = search_form("https://html.duckduckgo.com/html/?q={}");
        let p = alouette_html::Page::parse(&f, "web:start");
        assert_eq!(p.forms[0].action, "https://html.duckduckgo.com/html/");
        let mut p = p;
        let q = p.fields.iter().position(|f| f.name == "q").unwrap();
        p.fields[q].value = "alouette tui".into();
        assert_eq!(p.submit(0, None).unwrap().1, "https://html.duckduckgo.com/html/?q=alouette+tui");
        let f = search_form("https://searx.example/search?format=html&q={}");
        assert!(f.contains(r#"type="hidden" name="format" value="html""#));
        assert_eq!(search_form("https://nothing.example/"), "");
    }
}
