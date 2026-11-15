//! URL routing.
//!
//! Minuet opened any URL you clicked in the right built-in tool. Alouette does the same:
//! each module registers the schemes it handles, and everything that opens a URL (a link,
//! the command line, the command palette) goes through [`Router::resolve`].

use std::collections::BTreeMap;

use url::Url;

/// Where a URL should go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub module: String,
    pub url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    #[error("`{0}` is not a URL Alouette understands")]
    Invalid(String),
    #[error("no module handles {0}: URLs yet")]
    NoHandler(String),
}

/// Maps URL schemes to module ids.
#[derive(Debug, Clone, Default)]
pub struct Router {
    schemes: BTreeMap<String, String>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `module` as the handler for `scheme`. A later registration wins.
    pub fn register(&mut self, scheme: &str, module: &str) {
        self.schemes.insert(scheme.to_ascii_lowercase(), module.to_string());
    }

    /// Schemes with a handler, sorted.
    pub fn schemes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.schemes.iter().map(|(s, m)| (s.as_str(), m.as_str()))
    }

    pub fn handles(&self, scheme: &str) -> bool {
        self.schemes.contains_key(&scheme.to_ascii_lowercase())
    }

    /// Turn user input into a URL, then find its module.
    pub fn resolve(&self, input: &str) -> Result<Route, RouteError> {
        let url = normalize(input).ok_or_else(|| RouteError::Invalid(input.trim().to_string()))?;
        match self.schemes.get(url.scheme()) {
            Some(module) => Ok(Route { module: module.clone(), url }),
            None => Err(RouteError::NoHandler(url.scheme().to_string())),
        }
    }
}

/// Turn what a person typed into a URL.
///
/// * Full URLs pass through: `gemini://example.org/`.
/// * `user@example.org` becomes `mailto:user@example.org`.
/// * `@user@host` (a fediverse handle) becomes `acct:user@host`.
/// * `example.org/path` becomes `https://example.org/path`.
/// * `about`, `about:tasks` and similar stay in the `about:` scheme.
pub fn normalize(input: &str) -> Option<Url> {
    let s = input.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return None;
    }
    // A fediverse handle.
    if let Some(rest) = s.strip_prefix('@') {
        if rest.matches('@').count() == 1 && !rest.starts_with('@') && !rest.ends_with('@') {
            return Url::parse(&format!("acct:{rest}")).ok();
        }
        return None;
    }
    if let Some((scheme, _)) = s.split_once(':') {
        let looks_like_scheme = !scheme.is_empty()
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        // `host:port` without a scheme is not a scheme (e.g. `example.org:70`).
        let host_port =
            scheme.contains('.') && s[scheme.len() + 1..].chars().next().is_some_and(|c| c.is_ascii_digit());
        if looks_like_scheme && !host_port {
            return Url::parse(s).ok();
        }
    }
    if s.contains('@') && !s.contains('/') {
        return Url::parse(&format!("mailto:{s}")).ok();
    }
    let host = s.split(['/', ':']).next().unwrap_or("");
    if host.contains('.') || host == "localhost" || host.parse::<std::net::IpAddr>().is_ok() {
        return Url::parse(&format!("https://{s}")).ok();
    }
    if s == "about" {
        return Url::parse("about:alouette").ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        let mut r = Router::new();
        r.register("about", "core");
        r.register("https", "web");
        r.register("mailto", "mail");
        r.register("demo", "demo");
        r
    }

    #[test]
    fn full_urls_route_by_scheme() {
        let r = router();
        assert_eq!(r.resolve("about:tasks").unwrap().module, "core");
        assert_eq!(r.resolve("demo:hello").unwrap().module, "demo");
        assert_eq!(r.resolve("HTTPS://Example.org/x").unwrap().module, "web");
    }

    #[test]
    fn bare_input_is_normalized() {
        assert_eq!(normalize("example.org/a").unwrap().as_str(), "https://example.org/a");
        assert_eq!(normalize("example.org:8080").unwrap().as_str(), "https://example.org:8080/");
        assert_eq!(normalize("kevin@example.org").unwrap().as_str(), "mailto:kevin@example.org");
        assert_eq!(normalize("@alice@social.example").unwrap().as_str(), "acct:alice@social.example");
        assert_eq!(normalize("about").unwrap().as_str(), "about:alouette");
        assert_eq!(normalize("192.0.2.1").unwrap().as_str(), "https://192.0.2.1/");
        assert!(normalize("hello world").is_none());
        assert!(normalize("justaword").is_none());
        assert!(normalize("").is_none());
    }

    #[test]
    fn unknown_scheme_names_the_scheme() {
        let err = router().resolve("gopher://gopher.floodgap.com/").unwrap_err();
        assert_eq!(err, RouteError::NoHandler("gopher".into()));
        assert_eq!(err.to_string(), "no module handles gopher: URLs yet");
    }
}
