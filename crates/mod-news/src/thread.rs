//! Threading articles by their References, newest threads first.

use std::collections::HashMap;

use proto_nntp::Overview;

/// One row of a threaded list: which article, and how deep in its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub idx: usize,
    pub depth: usize,
}

/// Order `over` as threads. Each thread follows its newest article's position, so a
/// thread with a new reply comes to the top.
pub fn thread(over: &[Overview]) -> Vec<Row> {
    let by_id: HashMap<&str, usize> = over.iter().enumerate().map(|(i, o)| (o.message_id.as_str(), i)).collect();
    let mut children: Vec<Vec<usize>> = vec![vec![]; over.len()];
    let mut roots = vec![];
    for (i, o) in over.iter().enumerate() {
        // The nearest ancestor we have, so threads with missing articles still join up.
        let parent = o.parents().iter().rev().find_map(|p| by_id.get(p).copied()).filter(|&p| p != i);
        match parent {
            Some(p) => children[p].push(i),
            None => roots.push(i),
        }
    }
    // Newest article number in each subtree.
    fn newest(i: usize, over: &[Overview], ch: &[Vec<usize>], depth: usize) -> u64 {
        if depth > 200 {
            return over[i].number;
        }
        ch[i].iter().map(|&c| newest(c, over, ch, depth + 1)).fold(over[i].number, u64::max)
    }
    roots.sort_by_key(|&r| std::cmp::Reverse(newest(r, over, &children, 0)));
    for c in &mut children {
        c.sort_by_key(|&i| over[i].number);
    }
    let mut out = Vec::with_capacity(over.len());
    let mut seen = vec![false; over.len()];
    fn walk(i: usize, depth: usize, ch: &[Vec<usize>], seen: &mut [bool], out: &mut Vec<Row>) {
        if seen[i] {
            return;
        }
        seen[i] = true;
        out.push(Row { idx: i, depth });
        for &c in &ch[i] {
            walk(c, depth + 1, ch, seen, out);
        }
    }
    for r in roots {
        walk(r, 0, &children, &mut seen, &mut out);
    }
    // Anything caught in a reference loop.
    for i in 0..over.len() {
        if !seen[i] {
            walk(i, 0, &children, &mut seen, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(n: u64, id: &str, refs: &str) -> Overview {
        Overview { number: n, message_id: id.into(), references: refs.into(), ..Default::default() }
    }

    #[test]
    fn threads() {
        let over = vec![
            o(1, "<a>", ""),
            o(2, "<b>", ""),
            o(3, "<c>", "<a>"),
            o(4, "<d>", "<a> <c>"),
            o(5, "<e>", "<gone> <b>"),
            o(6, "<f>", "<missing>"),
            o(7, "<g>", "<g>"),
        ];
        let rows: Vec<(u64, usize)> = thread(&over).iter().map(|r| (over[r.idx].number, r.depth)).collect();
        assert_eq!(rows, [(7, 0), (6, 0), (2, 0), (5, 1), (1, 0), (3, 1), (4, 2)]);
    }
}
