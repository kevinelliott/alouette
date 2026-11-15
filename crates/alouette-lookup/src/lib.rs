//! Lookups that Inquire and NetTools share. Each returns a [`Section`]: a titled block of
//! styled lines, some of them links, ready to show.

pub mod query;
pub mod reference;
pub mod sources;
pub mod tls;

pub use query::{Query, QueryKind};

/// How a line should look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Normal,
    /// A `key: value` pair; the renderer may align it.
    Field,
    Good,
    Bad,
    Dim,
    Heading,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    pub style: Style,
    pub text: String,
    pub link: Option<String>,
}

impl Out {
    pub fn new(style: Style, text: impl Into<String>) -> Self {
        Self { style, text: text.into(), link: None }
    }
    pub fn field(key: &str, value: impl std::fmt::Display) -> Self {
        Self::new(Style::Field, format!("{key:<14} {value}"))
    }
    pub fn link(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self { style: Style::Normal, text: text.into(), link: Some(url.into()) }
    }
}

/// One source's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The source, such as `RDAP` or `Veronica-2`.
    pub source: String,
    pub title: String,
    pub lines: Vec<Out>,
    /// True when the source had nothing to say (shown collapsed).
    pub empty: bool,
}

impl Section {
    pub fn new(source: impl Into<String>, title: impl Into<String>) -> Self {
        Self { source: source.into(), title: title.into(), lines: vec![], empty: false }
    }
    pub fn push(&mut self, o: Out) -> &mut Self {
        self.lines.push(o);
        self
    }
    pub fn text(&mut self, style: Style, t: impl Into<String>) -> &mut Self {
        self.push(Out::new(style, t))
    }
    pub fn field(&mut self, k: &str, v: impl std::fmt::Display) -> &mut Self {
        self.push(Out::field(k, v))
    }
    pub fn error(source: &str, title: &str, e: impl std::fmt::Display) -> Self {
        let mut s = Self::new(source, title);
        s.text(Style::Bad, e.to_string());
        s
    }
    pub fn nothing(source: &str, title: &str, why: &str) -> Self {
        let mut s = Self::new(source, title);
        s.text(Style::Dim, why);
        s.empty = true;
        s
    }
    /// Add each line of a block of text.
    pub fn block(&mut self, text: &str, max_lines: usize) -> &mut Self {
        let lines: Vec<&str> = text.lines().collect();
        for l in lines.iter().take(max_lines) {
            self.text(Style::Normal, l.trim_end().replace('\t', "    "));
        }
        if lines.len() > max_lines {
            self.text(Style::Dim, format!("… {} more lines", lines.len() - max_lines));
        }
        self
    }
}
