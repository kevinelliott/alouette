//! Which articles in a group have been read: everything up to a mark, plus some above it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadState {
    /// Every article up to and including this number is read.
    pub upto: u64,
    /// Read articles above `upto`.
    pub more: BTreeSet<u64>,
}

impl ReadState {
    pub fn is_read(&self, n: u64) -> bool {
        n <= self.upto || self.more.contains(&n)
    }

    pub fn mark(&mut self, n: u64, read: bool) {
        if read {
            if n > self.upto {
                self.more.insert(n);
            }
        } else if n <= self.upto {
            // Unread something below the mark: move the mark down and keep the rest.
            for k in n + 1..=self.upto.min(n + 10_000) {
                self.more.insert(k);
            }
            self.upto = n.saturating_sub(1);
        } else {
            self.more.remove(&n);
        }
        self.compact();
    }

    /// Mark everything up to `high` read.
    pub fn catch_up(&mut self, high: u64) {
        self.upto = self.upto.max(high);
        self.compact();
    }

    /// Articles below `low` no longer exist, so count them as read.
    pub fn expire(&mut self, low: u64) {
        if low > 0 && self.upto < low - 1 {
            self.upto = low - 1;
        }
        self.compact();
    }

    /// How many of `low..=high` are unread.
    pub fn unread(&self, low: u64, high: u64) -> u64 {
        if high < low {
            return 0;
        }
        let start = low.max(self.upto + 1);
        if start > high {
            return 0;
        }
        let read_above = self.more.range(start..=high).count() as u64;
        high - start + 1 - read_above
    }

    fn compact(&mut self) {
        while self.more.remove(&(self.upto + 1)) {
            self.upto += 1;
        }
        let upto = self.upto;
        self.more.retain(|&n| n > upto);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks() {
        let mut r = ReadState::default();
        r.expire(10);
        assert_eq!(r.upto, 9);
        r.mark(11, true);
        assert_eq!(r.unread(10, 15), 5);
        r.mark(10, true);
        assert_eq!((r.upto, r.more.len()), (11, 0));
        r.mark(5, false);
        assert!(!r.is_read(5) && r.is_read(6) && r.is_read(11));
        r.catch_up(20);
        assert_eq!(r.unread(1, 20), 0);
        assert!(r.is_read(5));
    }
}
