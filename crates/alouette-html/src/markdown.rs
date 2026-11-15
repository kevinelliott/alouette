//! Writing a page as Markdown, for "save as Markdown".

use crate::{Block, Fx, Page, Seg};

pub fn to_markdown(page: &Page) -> String {
    let mut out = String::new();
    if !page.title.is_empty() && !matches!(page.blocks.first(), Some(Block::Heading(1, _))) {
        out.push_str(&format!("# {}\n\n", page.title));
    }
    blocks(page, &page.blocks, "", &mut out);
    let mut out = out.trim().to_string();
    out.push('\n');
    if !page.url.is_empty() && page.url != "about:blank" {
        out.push_str(&format!("\n---\nSource: <{}>\n", page.url));
    }
    out
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

fn wrap_fx(text: &str, fx: Fx) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    // Keep surrounding spaces outside the markers.
    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];
    let core = text.trim();
    let mut s = if fx.has(Fx::CODE) { format!("`{core}`") } else { escape(core) };
    if fx.has(Fx::STRIKE) {
        s = format!("~~{s}~~");
    }
    if fx.has(Fx::ITALIC) {
        s = format!("*{s}*");
    }
    if fx.has(Fx::BOLD) && !fx.has(Fx::HEADING) {
        s = format!("**{s}**");
    }
    format!("{lead}{s}{trail}")
}

pub(crate) fn inline(page: &Page, segs: &[Seg]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < segs.len() {
        let s = &segs[i];
        if let Some(f) = s.field {
            if let Some(field) = page.fields.get(f) {
                out.push_str(&escape(&field.shown()));
            }
            i += 1;
            continue;
        }
        if let Some(img) = s.image.and_then(|n| page.images.get(n)) {
            out.push_str(&format!("![{}]({})", escape(&img.alt), img.src));
            i += 1;
            continue;
        }
        match s.link {
            Some(l) => {
                let mut text = String::new();
                while i < segs.len() && segs[i].link == Some(l) {
                    text.push_str(&wrap_fx(&segs[i].text, segs[i].fx));
                    i += 1;
                }
                let url = page.links.get(l).map(|l| l.url.as_str()).unwrap_or("");
                let lead = &text[..text.len() - text.trim_start().len()];
                let trail = &text[text.trim_end().len()..];
                out.push_str(&format!("{lead}[{}]({url}){trail}", text.trim()));
            }
            None => {
                out.push_str(&wrap_fx(&s.text, s.fx).replace('\n', "  \n"));
                i += 1;
            }
        }
    }
    out
}

fn prefixed(text: &str, first: &str, rest: &str) -> String {
    let mut o = String::new();
    for (i, l) in text.lines().enumerate() {
        o.push_str(if i == 0 { first } else { rest });
        o.push_str(l);
        o.push('\n');
    }
    o
}

fn blocks(page: &Page, bs: &[Block], indent: &str, out: &mut String) {
    for b in bs {
        let mut s = String::new();
        block(page, b, &mut s);
        out.push_str(&prefixed(s.trim_end(), indent, indent));
        out.push_str(indent.trim_end());
        out.push('\n');
    }
}

fn block(page: &Page, b: &Block, out: &mut String) {
    match b {
        Block::Heading(l, segs) => out.push_str(&format!("{} {}\n", "#".repeat(*l as usize), inline(page, segs))),
        Block::Para(segs) => {
            out.push_str(&inline(page, segs));
            out.push('\n');
        }
        Block::Pre(t) => {
            let fence = if t.contains("```") { "~~~~" } else { "```" };
            out.push_str(&format!("{fence}\n{t}\n{fence}\n"));
        }
        Block::Quote(inner) => {
            let mut s = String::new();
            blocks(page, inner, "", &mut s);
            out.push_str(&prefixed(s.trim_end(), "> ", "> "));
        }
        Block::List { ordered, start, items } => {
            for (n, item) in items.iter().enumerate() {
                let marker = if *ordered { format!("{}. ", *start as usize + n) } else { "- ".into() };
                let mut s = String::new();
                for (i, b) in item.iter().enumerate() {
                    if i > 0 {
                        s.push('\n');
                    }
                    block(page, b, &mut s);
                }
                out.push_str(&prefixed(s.trim_end(), &marker, &" ".repeat(marker.len())));
            }
        }
        Block::Table(rows) => {
            let cols = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
            for (i, (_, r)) in rows.iter().enumerate() {
                let cells: Vec<String> = (0..cols)
                    .map(|c| r.get(c).map(|s| inline(page, s).replace('\n', " ")).unwrap_or_default())
                    .collect();
                out.push_str(&format!("| {} |\n", cells.join(" | ")));
                if i == 0 {
                    out.push_str(&format!("|{}\n", " --- |".repeat(cols)));
                }
            }
        }
        Block::Rule => out.push_str("---\n"),
        Block::Image(i) => {
            if let Some(img) = page.images.get(*i) {
                let md = format!("![{}]({})", escape(&img.alt), img.src);
                match img.link.and_then(|l| page.links.get(l)) {
                    Some(l) => out.push_str(&format!("[{md}]({})\n", l.url)),
                    None => out.push_str(&format!("{md}\n")),
                }
            }
        }
        Block::Def(term, def) => {
            if !term.is_empty() {
                out.push_str(&format!("**{}**\n", inline(page, term).trim_matches('*')));
            }
            let mut s = String::new();
            blocks(page, def, "", &mut s);
            out.push_str(&prefixed(s.trim_end(), ": ", "  "));
        }
    }
}
