//! mIRC text formatting: bold, italic, underline, strike, monospace, reverse and colours.

/// The style of a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub reverse: bool,
    /// mIRC colour numbers, 0 to 98.
    pub fg: Option<u8>,
    pub bg: Option<u8>,
    /// A `\x04RRGGBB` hex colour.
    pub fg_rgb: Option<(u8, u8, u8)>,
}

/// Split text into styled runs. Control codes are dropped.
pub fn spans(s: &str) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = vec![];
    let mut cur = Style::default();
    let mut buf = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let flush = |buf: &mut String, out: &mut Vec<(String, Style)>, st: Style| {
        if !buf.is_empty() {
            out.push((std::mem::take(buf), st));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let before = cur;
        match c {
            '\x02' => cur.bold = !cur.bold,
            '\x1d' => cur.italic = !cur.italic,
            '\x1f' => cur.underline = !cur.underline,
            '\x1e' => cur.strike = !cur.strike,
            '\x11' => cur.mono = !cur.mono,
            '\x16' => cur.reverse = !cur.reverse,
            '\x0f' => cur = Style::default(),
            '\x03' => {
                let (fg, n) = digits(&chars[i + 1..]);
                i += n;
                match fg {
                    None => {
                        cur.fg = None;
                        cur.bg = None;
                        cur.fg_rgb = None;
                    }
                    Some(f) => {
                        cur.fg = Some(f);
                        cur.fg_rgb = None;
                        if chars.get(i + 1) == Some(&',')
                            && let (Some(b), m @ 1..) = digits(&chars[i + 2..])
                        {
                            cur.bg = Some(b);
                            i += 1 + m;
                        }
                    }
                }
            }
            '\x04' => {
                let hex: String = chars[i + 1..].iter().take(6).collect();
                if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    let v = u32::from_str_radix(&hex, 16).unwrap_or(0);
                    cur.fg_rgb = Some(((v >> 16) as u8, (v >> 8) as u8, v as u8));
                    i += 6;
                    // A background after a comma is read and ignored.
                    if chars.get(i + 1) == Some(&',') {
                        let bg: String = chars[i + 2..].iter().take(6).collect();
                        if bg.len() == 6 && bg.chars().all(|c| c.is_ascii_hexdigit()) {
                            i += 7;
                        }
                    }
                } else {
                    cur.fg_rgb = None;
                }
            }
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => {
                buf.push(c);
                i += 1;
                continue;
            }
        }
        if before != cur || c == '\x03' || c == '\x04' {
            flush(&mut buf, &mut out, before);
        }
        i += 1;
    }
    flush(&mut buf, &mut out, cur);
    // Merge neighbours with the same style.
    let mut merged: Vec<(String, Style)> = vec![];
    for (t, st) in out {
        match merged.last_mut() {
            Some((lt, ls)) if *ls == st => lt.push_str(&t),
            _ => merged.push((t, st)),
        }
    }
    merged
}

/// One or two digits.
fn digits(c: &[char]) -> (Option<u8>, usize) {
    let n = c.iter().take(2).take_while(|c| c.is_ascii_digit()).count();
    if n == 0 {
        return (None, 0);
    }
    let s: String = c[..n].iter().collect();
    (s.parse().ok(), n)
}

/// The text without any formatting.
pub fn strip(s: &str) -> String {
    spans(s).into_iter().map(|(t, _)| t).collect()
}

/// The RGB value of a mIRC colour number, for terminals with true colour.
pub fn rgb(n: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (255, 255, 255),
        (0, 0, 0),
        (0, 0, 127),
        (0, 147, 0),
        (255, 0, 0),
        (127, 0, 0),
        (156, 0, 156),
        (252, 127, 0),
        (255, 255, 0),
        (0, 252, 0),
        (0, 147, 147),
        (0, 255, 255),
        (0, 0, 252),
        (255, 0, 255),
        (127, 127, 127),
        (210, 210, 210),
    ];
    if (n as usize) < BASE.len() {
        return BASE[n as usize];
    }
    // 16 to 98: the extended palette is a 12-hue ramp at 7 levels, then greys.
    if n >= 88 {
        let g = ((n - 88) as u16 * 255 / 10) as u8;
        return (g, g, g);
    }
    let k = (n - 16) as usize;
    let (level, hue) = (k / 12, k % 12);
    let v = [71u16, 116, 255, 255, 255, 255, 255][level.min(6)];
    let w = [0u16, 0, 0, 89, 140, 180, 220][level.min(6)];
    let h = hue as f32 / 12.0 * 6.0;
    let x = 1.0 - ((h % 2.0) - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let f = |c: f32| ((c * (v - w) as f32) as u16 + w).min(255) as u8;
    (f(r), f(g), f(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_and_colours() {
        let s = spans("plain \x02bold\x02 \x0304,01red\x03 back \x1ditalic\x0f done");
        let texts: Vec<&str> = s.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["plain ", "bold", " ", "red", " back ", "italic", " done"]);
        assert!(s[1].1.bold);
        assert_eq!((s[3].1.fg, s[3].1.bg), (Some(4), Some(1)));
        assert_eq!(s[4].1.fg, None);
        assert!(s[5].1.italic);
        assert_eq!(s[6].1, Style::default());
        assert_eq!(strip("\x0312,4blue\x03 and \x04ff0000hex"), "blue and hex");
        // A comma after a colour that isn't followed by digits is text.
        assert_eq!(strip("\x034,hi"), ",hi");
        assert_eq!(rgb(4), (255, 0, 0));
        let _ = rgb(52);
        let _ = rgb(98);
    }
}
