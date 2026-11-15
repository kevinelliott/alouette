//! Wrapping blocks into lines of styled segments for a given width.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{Block, Fx, Page, Seg};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Add `[n]` after each link, for plain text output.
    pub link_numbers: bool,
}

/// One screen line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub segs: Vec<Seg>,
    /// The top-level block this line came from.
    pub block: usize,
}

impl Line {
    pub fn text(&self) -> String {
        self.segs.iter().map(|s| s.text.as_str()).collect()
    }
    pub fn width(&self) -> usize {
        self.segs.iter().map(|s| s.text.width()).sum()
    }
}

/// Lay out a page at `width` columns.
pub fn layout(page: &Page, width: usize, opts: &Options) -> Vec<Line> {
    let width = width.max(10);
    let mut out = vec![];
    let fresh: Vec<Block> =
        if page.fields.is_empty() { vec![] } else { page.blocks.iter().map(|b| refresh(page, b)).collect() };
    let bs = if page.fields.is_empty() { &page.blocks } else { &fresh };
    for (i, b) in bs.iter().enumerate() {
        let mut lines = vec![];
        block(page, b, width, opts, 0, &mut lines);
        if lines.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Line { segs: vec![], block: i });
        }
        for mut l in lines {
            l.block = i;
            out.push(l);
        }
    }
    out
}

/// Show form fields as they are now, not as the page first had them.
fn refresh(page: &Page, b: &Block) -> Block {
    let segs = |v: &Vec<Seg>| -> Vec<Seg> {
        v.iter()
            .map(|s| match s.field.and_then(|f| page.fields.get(f)) {
                Some(f) => Seg { text: f.shown(), ..s.clone() },
                None => s.clone(),
            })
            .collect()
    };
    let blocks = |v: &Vec<Block>| v.iter().map(|b| refresh(page, b)).collect::<Vec<_>>();
    match b {
        Block::Heading(l, s) => Block::Heading(*l, segs(s)),
        Block::Para(s) => Block::Para(segs(s)),
        Block::Quote(v) => Block::Quote(blocks(v)),
        Block::List { ordered, start, items } => {
            Block::List { ordered: *ordered, start: *start, items: items.iter().map(blocks).collect() }
        }
        Block::Table(rows) => Block::Table(rows.iter().map(|(h, r)| (*h, r.iter().map(segs).collect())).collect()),
        Block::Def(t, d) => Block::Def(segs(t), blocks(d)),
        other => other.clone(),
    }
}

fn dim(text: impl Into<String>) -> Seg {
    Seg { text: text.into(), fx: Fx::DIM, ..Default::default() }
}

fn with_prefix(lines: Vec<Line>, first: &str, rest: &str) -> Vec<Line> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, mut l)| {
            let p = if i == 0 { first } else { rest };
            if !p.is_empty() {
                l.segs.insert(0, dim(p));
            }
            l
        })
        .collect()
}

fn block(page: &Page, b: &Block, width: usize, opts: &Options, depth: usize, out: &mut Vec<Line>) {
    match b {
        Block::Heading(level, segs) => {
            let segs: Vec<Seg> =
                segs.iter().map(|s| Seg { fx: s.fx.with(Fx::BOLD).with(Fx::HEADING), ..s.clone() }).collect();
            let lines = wrap(&segs, width, opts);
            let w = lines.iter().map(Line::width).max().unwrap_or(0);
            out.extend(lines);
            match level {
                1 => out.push(Line { segs: vec![dim("━".repeat(w))], block: 0 }),
                2 => out.push(Line { segs: vec![dim("─".repeat(w))], block: 0 }),
                _ => {}
            }
        }
        Block::Para(segs) => out.extend(wrap(segs, width, opts)),
        Block::Pre(text) => {
            for l in text.lines() {
                let mut rest = l;
                loop {
                    let (head, tail) = split_at_width(rest, width);
                    out.push(Line {
                        segs: vec![Seg { text: head.to_string(), fx: Fx::CODE, ..Default::default() }],
                        block: 0,
                    });
                    if tail.is_empty() {
                        break;
                    }
                    rest = tail;
                }
            }
        }
        Block::Quote(inner) => {
            let mut lines = vec![];
            blocks(page, inner, width.saturating_sub(2).max(8), opts, depth, &mut lines);
            out.extend(with_prefix(lines, "│ ", "│ "));
        }
        Block::List { ordered, start, items } => {
            let last = *start as usize + items.len().saturating_sub(1);
            let num_w = last.to_string().len();
            let bullet = ["•", "◦", "▪"][depth % 3];
            let loose = items.iter().any(|i| i.iter().filter(|b| !matches!(b, Block::List { .. })).count() > 1);
            for (n, item) in items.iter().enumerate() {
                let marker = if *ordered { format!("{:>num_w$}. ", *start as usize + n) } else { format!("{bullet} ") };
                let mw = marker.width();
                let mut lines = vec![];
                blocks(page, item, width.saturating_sub(mw).max(8), opts, depth + 1, &mut lines);
                if lines.is_empty() {
                    lines.push(Line::default());
                }
                if loose && n > 0 {
                    out.push(Line::default());
                }
                out.extend(with_prefix(lines, &marker, &" ".repeat(mw)));
            }
        }
        Block::Table(rows) => table(rows, width, opts, out),
        Block::Rule => out.push(Line { segs: vec![dim("─".repeat(width.min(72)))], block: 0 }),
        Block::Image(i) => {
            let Some(img) = page.images.get(*i) else { return };
            let label = if img.alt.is_empty() { crate::convert::short_url(&img.src) } else { img.alt.clone() };
            let seg = Seg {
                text: format!("[image: {label}]"),
                fx: Fx::DIM.with(Fx::ITALIC),
                link: img.link,
                image: Some(*i),
                ..Default::default()
            };
            out.extend(wrap(&[seg], width, opts));
        }
        Block::Def(term, def) => {
            if !term.is_empty() {
                out.extend(wrap(term, width, opts));
            }
            let mut lines = vec![];
            blocks(page, def, width.saturating_sub(4).max(8), opts, depth, &mut lines);
            out.extend(with_prefix(lines, "    ", "    "));
        }
    }
}

fn blocks(page: &Page, bs: &[Block], width: usize, opts: &Options, depth: usize, out: &mut Vec<Line>) {
    for (i, b) in bs.iter().enumerate() {
        let before = out.len();
        if i > 0 && !matches!(b, Block::List { .. }) {
            out.push(Line::default());
        }
        let mark = out.len();
        block(page, b, width, opts, depth, out);
        if out.len() == mark {
            out.truncate(before);
        }
    }
}

fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    let mut w = 0;
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width {
            return (&s[..i], &s[i..]);
        }
        w += cw;
    }
    (s, "")
}

/// Pieces of text that wrap as one: a word, possibly in several styles.
enum Piece {
    Word(Vec<Seg>),
    Space,
    Break,
}

fn pieces(segs: &[Seg], opts: &Options) -> Vec<Piece> {
    let mut out: Vec<Piece> = vec![];
    let mut word: Vec<Seg> = vec![];
    let push_word = |word: &mut Vec<Seg>, out: &mut Vec<Piece>| {
        if !word.is_empty() {
            out.push(Piece::Word(std::mem::take(word)));
        }
    };
    for (si, s) in segs.iter().enumerate() {
        if s.field.is_some() {
            push_word(&mut word, &mut out);
            out.push(Piece::Word(vec![s.clone()]));
            continue;
        }
        let mut cur = String::new();
        for c in s.text.chars() {
            if c == '\n' || c == ' ' {
                if !cur.is_empty() {
                    word.push(Seg { text: std::mem::take(&mut cur), ..s.clone() });
                }
                push_word(&mut word, &mut out);
                out.push(if c == '\n' { Piece::Break } else { Piece::Space });
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            word.push(Seg { text: cur, ..s.clone() });
        }
        // `[n]` after the last part of a link.
        if opts.link_numbers
            && let Some(l) = s.link
            && segs.get(si + 1).is_none_or(|n| n.link != Some(l))
        {
            word.push(dim(format!("[{}]", l + 1)));
        }
    }
    push_word(&mut word, &mut out);
    out
}

/// Word-wrap styled text.
pub fn wrap(segs: &[Seg], width: usize, opts: &Options) -> Vec<Line> {
    let mut lines = vec![];
    let mut line: Vec<Seg> = vec![];
    let mut w = 0usize;
    let mut pending_space: Option<Seg> = None;
    let push_seg = |line: &mut Vec<Seg>, s: Seg| {
        if let Some(last) = line.last_mut()
            && last.fx == s.fx
            && last.link == s.link
            && last.field.is_none()
            && s.field.is_none()
            && last.image == s.image
        {
            last.text.push_str(&s.text);
        } else {
            line.push(s);
        }
    };
    let mut prev_word: Option<Seg> = None;
    for p in pieces(segs, opts) {
        match p {
            Piece::Break => {
                lines.push(Line { segs: std::mem::take(&mut line), block: 0 });
                w = 0;
                pending_space = None;
            }
            Piece::Space => {
                if w > 0 {
                    // A space inside a link keeps the link's look.
                    let style = prev_word.clone().unwrap_or_default();
                    pending_space = Some(Seg { text: " ".into(), ..style });
                }
            }
            Piece::Word(parts) => {
                let ww: usize = parts.iter().map(|s| s.text.width()).sum();
                let sp = usize::from(pending_space.is_some());
                if w > 0 && w + sp + ww > width {
                    lines.push(Line { segs: std::mem::take(&mut line), block: 0 });
                    w = 0;
                    pending_space = None;
                }
                if let Some(s) = pending_space.take() {
                    // Only keep the link style when the next word is in the same link.
                    let same = parts.first().is_some_and(|p| p.link == s.link && s.link.is_some());
                    let s = if same { s } else { Seg::plain(" ") };
                    push_seg(&mut line, s);
                    w += 1;
                }
                prev_word = parts.last().cloned().map(|mut s| {
                    s.field = None;
                    s
                });
                for part in parts {
                    if part.text.width() > width && part.field.is_none() {
                        // Break long words such as URLs.
                        let mut rest = part.text.as_str();
                        while !rest.is_empty() {
                            let room = width.saturating_sub(w).max(1);
                            let (head, tail) = split_at_width(rest, room);
                            let head =
                                if head.is_empty() { &rest[..rest.chars().next().unwrap().len_utf8()] } else { head };
                            push_seg(&mut line, Seg { text: head.to_string(), ..part.clone() });
                            w += head.width();
                            rest = &rest[head.len()..];
                            if !rest.is_empty() {
                                lines.push(Line { segs: std::mem::take(&mut line), block: 0 });
                                w = 0;
                            }
                            let _ = tail;
                        }
                    } else {
                        w += part.text.width();
                        push_seg(&mut line, part);
                    }
                }
            }
        }
    }
    if !line.is_empty() {
        lines.push(Line { segs: line, block: 0 });
    }
    lines
}

fn table(rows: &[(bool, Vec<Vec<Seg>>)], width: usize, opts: &Options, out: &mut Vec<Line>) {
    let cols = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    let text_w = |c: &Vec<Seg>| c.iter().map(|s| s.text.width()).sum::<usize>();
    let word_w = |c: &Vec<Seg>| c.iter().flat_map(|s| s.text.split_whitespace()).map(|w| w.width()).max().unwrap_or(0);
    let mut natural = vec![0usize; cols];
    let mut minimum = vec![1usize; cols];
    for (_, r) in rows {
        for (i, c) in r.iter().enumerate() {
            natural[i] = natural[i].max(text_w(c).min(60));
            minimum[i] = minimum[i].max(word_w(c).min(16));
        }
    }
    let sep = 3; // " │ "
    let avail = width.saturating_sub(sep * (cols - 1));
    let total: usize = natural.iter().sum();
    let widths: Vec<usize> = if total <= avail {
        natural.clone()
    } else {
        let min_total: usize = minimum.iter().sum();
        if min_total >= avail {
            // Too narrow: give each column an even share.
            vec![(avail / cols).max(3); cols]
        } else {
            let extra = avail - min_total;
            let want: usize = natural.iter().zip(&minimum).map(|(n, m)| n.saturating_sub(*m)).sum::<usize>().max(1);
            natural.iter().zip(&minimum).map(|(n, m)| m + n.saturating_sub(*m) * extra / want).collect()
        }
    };
    for (ri, (head, r)) in rows.iter().enumerate() {
        let wrapped: Vec<Vec<Line>> =
            (0..cols).map(|i| r.get(i).map(|c| wrap(c, widths[i].max(1), opts)).unwrap_or_default()).collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for li in 0..height {
            let mut segs = vec![];
            for (ci, cell) in wrapped.iter().enumerate() {
                if ci > 0 {
                    segs.push(dim(" │ "));
                }
                let line = cell.get(li).cloned().unwrap_or_default();
                let w = line.width();
                segs.extend(line.segs);
                if ci + 1 < cols && w < widths[ci] {
                    segs.push(Seg::plain(" ".repeat(widths[ci] - w)));
                }
            }
            out.push(Line { segs, block: 0 });
        }
        if *head && rows.get(ri + 1).is_some_and(|n| !n.0) {
            let s = widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─");
            out.push(Line { segs: vec![dim(s)], block: 0 });
        }
    }
}
