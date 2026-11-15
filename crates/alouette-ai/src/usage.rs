//! The cost meter: tokens and estimated dollars per month, kept in a small JSON file, and
//! a price table for the models whose prices are known.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Kind, Reply};

/// Dollars per million input and output tokens.
pub fn price(kind: Kind, model: &str) -> Option<(f64, f64)> {
    if !kind.is_remote() {
        return Some((0.0, 0.0));
    }
    // Anthropic list prices, Claude API, November 2026.
    const TABLE: &[(&str, f64, f64)] = &[
        ("claude-fable-5-2", 8.0, 40.0),
        ("claude-fable-5-1", 10.0, 50.0),
        ("claude-fable-5", 10.0, 50.0),
        ("claude-opus-5-5", 4.0, 20.0),
        ("claude-opus-5", 5.0, 25.0),
        ("claude-opus-4-8", 5.0, 25.0),
        ("claude-opus-4-7", 5.0, 25.0),
        ("claude-opus-4-6", 5.0, 25.0),
        ("claude-sonnet-5-5", 2.0, 10.0),
        ("claude-sonnet-5", 2.0, 10.0),
        ("claude-sonnet-4-6", 3.0, 15.0),
        ("claude-haiku-5-5", 0.10, 0.50),
        ("claude-haiku-4-5", 1.0, 5.0),
    ];
    TABLE.iter().find(|(m, ..)| *m == model).map(|(_, i, o)| (*i, *o))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Month {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    /// Requests whose cost could not be worked out (unknown prices).
    pub unpriced: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip)]
    path: PathBuf,
    /// `2026-11` → totals.
    pub months: BTreeMap<String, Month>,
}

/// The current month as `YYYY-MM`, in UTC.
pub fn this_month() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (now / 86_400) as i64;
    let (y, m, _) = civil(days);
    format!("{y:04}-{m:02}")
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

impl Usage {
    pub fn load(path: &Path) -> Self {
        let mut u: Usage = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        u.path = path.to_path_buf();
        u
    }

    pub fn save(&self) {
        if self.path.as_os_str().is_empty() {
            return;
        }
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(tmp, &self.path);
            }
        }
    }

    pub fn month(&self) -> Month {
        self.months.get(&this_month()).cloned().unwrap_or_default()
    }

    /// Add a reply. `prices` overrides the table. Returns this reply's cost, when known.
    pub fn add(&mut self, kind: Kind, reply: &Reply, prices: Option<(f64, f64)>) -> Option<f64> {
        let cost = reply.cost.or_else(|| {
            prices
                .or_else(|| price(kind, &reply.model))
                .map(|(i, o)| (reply.input_tokens as f64 * i + reply.output_tokens as f64 * o) / 1_000_000.0)
        });
        let m = self.months.entry(this_month()).or_default();
        m.requests += 1;
        m.input_tokens += reply.input_tokens;
        m.output_tokens += reply.output_tokens;
        match cost {
            Some(c) => m.cost += c,
            None => m.unpriced += 1,
        }
        cost
    }

    /// True when the month's spend has reached `budget` (0 means no budget).
    pub fn over_budget(&self, budget: f64) -> bool {
        budget > 0.0 && self.month().cost >= budget
    }
}

/// Such as `$0.0123`, with more digits for small amounts.
pub fn dollars(x: f64) -> String {
    if x == 0.0 {
        "$0".into()
    } else if x < 0.01 {
        format!("${x:.4}")
    } else {
        format!("${x:.2}")
    }
}

/// The cost a request might have, before it is sent: its input plus an answer of about
/// 500 tokens.
pub fn estimate(kind: Kind, model: &str, input_tokens: u64, prices: Option<(f64, f64)>) -> Option<f64> {
    let (i, o) = prices.or_else(|| price(kind, model))?;
    Some((input_tokens as f64 * i + 500.0 * o) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_407), (2025, 11, 15));
        assert_eq!(this_month().len(), 7);
    }

    #[test]
    fn meter() {
        let dir = std::env::temp_dir().join(format!("alouette-ai-usage-{}", std::process::id()));
        let path = dir.join("usage.json");
        let mut u = Usage::load(&path);
        let r = Reply {
            model: "claude-opus-5-5".into(),
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            ..Reply::default()
        };
        assert_eq!(u.add(Kind::Anthropic, &r, None), Some(6.0));
        let local = Reply { model: "llama3".into(), input_tokens: 10, ..Reply::default() };
        assert_eq!(u.add(Kind::Ollama, &local, None), Some(0.0));
        let unknown = Reply { model: "mystery".into(), ..Reply::default() };
        assert_eq!(u.add(Kind::OpenAi, &unknown, None), None);
        u.save();
        let back = Usage::load(&path);
        assert_eq!(back.month().requests, 3);
        assert_eq!(back.month().unpriced, 1);
        assert!(back.over_budget(5.0));
        assert!(!back.over_budget(0.0));
        assert_eq!(dollars(0.00123), "$0.0012");
        let _ = std::fs::remove_dir_all(dir);
    }
}
