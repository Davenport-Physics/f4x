//! Matching the functions of one build (old) to another (new).
//!
//! Evidence, strongest first:
//! 1. seeds given by the caller (e.g. signature hits we verified);
//! 2. the same set of referenced strings, unique in both builds;
//! 3. rare strings / vtables referenced by exactly one function in each build (votes);
//! 4. call-graph propagation: the callees of a matched pair, aligned in code order
//!    (LCS), and the sole callers of matched callees;
//! 5. globals: the `.data` references of matched pairs, aligned the same way.
//!
//! Every match records how it was found, so a result can be checked by hand.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::index::{Binary, Func};

#[derive(Clone, Debug, Serialize)]
pub struct Match {
    pub old: u32,
    pub new: u32,
    pub how: String,
}

pub struct Matcher<'b, 'a> {
    pub old: &'b Binary<'a>,
    pub new: &'b Binary<'a>,
    fwd: HashMap<usize, usize>,
    back: HashMap<usize, usize>,
    pub how: HashMap<usize, String>,
    /// Globals: old RVA -> (new RVA, how).
    pub globals: HashMap<u32, (u32, String)>,
    globals_back: HashMap<u32, u32>,
    pub conflicts: usize,
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    let a: HashSet<&String> = a.iter().collect();
    let b: HashSet<&String> = b.iter().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    a.intersection(&b).count() as f64 / a.union(&b).count() as f64
}

impl<'b, 'a> Matcher<'b, 'a> {
    pub fn new(old: &'b Binary<'a>, new: &'b Binary<'a>) -> Self {
        Matcher {
            old,
            new,
            fwd: HashMap::new(),
            back: HashMap::new(),
            how: HashMap::new(),
            globals: HashMap::new(),
            globals_back: HashMap::new(),
            conflicts: 0,
        }
    }

    pub fn get(&self, old_index: usize) -> Option<usize> {
        self.fwd.get(&old_index).copied()
    }

    pub fn matches(&self) -> Vec<Match> {
        let mut out: Vec<Match> = self
            .fwd
            .iter()
            .map(|(&o, &n)| Match { old: self.old.funcs[o].start, new: self.new.funcs[n].start, how: self.how[&o].clone() })
            .collect();
        out.sort_by_key(|m| m.old);
        out
    }

    fn add(&mut self, o: usize, n: usize, how: impl Into<String>) -> bool {
        match (self.fwd.get(&o), self.back.get(&n)) {
            (None, None) => {
                self.fwd.insert(o, n);
                self.back.insert(n, o);
                self.how.insert(o, how.into());
                true
            }
            (Some(&x), _) if x == n => false,
            _ => {
                self.conflicts += 1;
                false
            }
        }
    }

    fn add_global(&mut self, o: u32, n: u32, how: String) {
        if self.globals.contains_key(&o) || self.globals_back.contains_key(&n) {
            return;
        }
        self.globals.insert(o, (n, how));
        self.globals_back.insert(n, o);
    }

    /// Size and string similarity good enough to accept a structural match.
    fn compatible(&self, a: &Func, b: &Func) -> bool {
        let ratio = (b.size.max(1) as f64) / (a.size.max(1) as f64);
        if !(0.33..=3.0).contains(&ratio) {
            return false;
        }
        if !a.strings.is_empty() || !b.strings.is_empty() {
            return jaccard(&a.strings, &b.strings) >= 0.5;
        }
        true
    }

    pub fn seed(&mut self, old_start: u32, new_start: u32, how: &str) -> bool {
        match (self.old.index_of(old_start), self.new.index_of(new_start)) {
            (Some(o), Some(n)) => self.add(o, n, how.to_owned()),
            _ => false,
        }
    }

    /// Unique string sets, then rare-string and rare-vtable votes.
    pub fn anchors(&mut self) {
        let key = |f: &Func| {
            let mut s: Vec<&str> = f.strings.iter().map(String::as_str).collect();
            s.sort_unstable();
            s.dedup();
            s.join("\u{1}")
        };
        let mut old_keys: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in self.old.funcs.iter().enumerate().filter(|(_, f)| !f.strings.is_empty()) {
            old_keys.entry(key(f)).or_default().push(i);
        }
        let mut new_keys: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in self.new.funcs.iter().enumerate().filter(|(_, f)| !f.strings.is_empty()) {
            new_keys.entry(key(f)).or_default().push(i);
        }
        for (k, olds) in &old_keys {
            if let (true, Some(news)) = (olds.len() == 1, new_keys.get(k)) {
                if news.len() == 1 && self.compatible(&self.old.funcs[olds[0]], &self.new.funcs[news[0]]) {
                    self.add(olds[0], news[0], "strings");
                }
            }
        }

        let mut votes: HashMap<usize, HashMap<usize, usize>> = HashMap::new();
        let old_users = self.old.string_users();
        let new_users = self.new.string_users();
        for (s, olds) in &old_users {
            if let (1, Some(news)) = (olds.len(), new_users.get(s)) {
                if news.len() == 1 {
                    *votes.entry(olds[0]).or_default().entry(news[0]).or_default() += 1;
                }
            }
        }
        let vt_users = |b: &Binary| {
            let mut m: HashMap<String, Vec<usize>> = HashMap::new();
            for (i, f) in b.funcs.iter().enumerate() {
                for c in f.vtables.iter().collect::<HashSet<_>>() {
                    m.entry(c.clone()).or_default().push(i);
                }
            }
            m
        };
        let (old_vt, new_vt) = (vt_users(self.old), vt_users(self.new));
        for (c, olds) in &old_vt {
            if let (1, Some(news)) = (olds.len(), new_vt.get(c)) {
                if news.len() == 1 {
                    *votes.entry(olds[0]).or_default().entry(news[0]).or_default() += 1;
                }
            }
        }
        let mut ordered: Vec<_> = votes.into_iter().collect();
        ordered.sort_by_key(|(o, _)| *o);
        for (o, candidates) in ordered {
            let mut best: Vec<_> = candidates.into_iter().collect();
            best.sort_by(|a, b| b.1.cmp(&a.1));
            let unambiguous = best.len() == 1 || best[0].1 > best[1].1;
            if unambiguous && self.compatible(&self.old.funcs[o], &self.new.funcs[best[0].0]) {
                self.add(o, best[0].0, format!("rare refs ({} votes)", best[0].1));
            }
        }
    }

    /// Propagate through callees, sole callers and globals until nothing changes.
    pub fn propagate(&mut self) {
        loop {
            let before = self.fwd.len() + self.globals.len();
            let pairs: Vec<(usize, usize)> = {
                let mut v: Vec<_> = self.fwd.iter().map(|(&o, &n)| (o, n)).collect();
                v.sort_unstable();
                v
            };
            for (o, n) in pairs {
                self.align_calls(o, n);
                self.align_globals(o, n);
            }
            // Sole callers.
            let pairs: Vec<(usize, usize)> = self.fwd.iter().map(|(&o, &n)| (o, n)).collect();
            for (o, n) in pairs {
                let (co, cn) = (&self.old.callers[o], &self.new.callers[n]);
                if co.len() == 1 && cn.len() == 1 && !self.fwd.contains_key(&co[0]) && !self.back.contains_key(&cn[0]) {
                    let (a, b) = (co[0], cn[0]);
                    if self.compatible(&self.old.funcs[a], &self.new.funcs[b]) {
                        let how = format!("sole caller of {:#x}", self.old.funcs[o].start);
                        self.add(a, b, how);
                    }
                }
            }
            if self.fwd.len() + self.globals.len() == before {
                break;
            }
        }
    }

    fn align_calls(&mut self, o: usize, n: usize) {
        let dedup = |b: &Binary, calls: &[u32]| -> Vec<usize> {
            let mut out: Vec<usize> = calls.iter().filter_map(|&c| b.index_of(c)).collect();
            out.dedup();
            out
        };
        let a = dedup(self.old, &self.old.funcs[o].calls);
        let b = dedup(self.new, &self.new.funcs[n].calls);
        let same = |s: &Self, x: usize, y: usize| match (s.fwd.get(&x), s.back.get(&y)) {
            (Some(&m), _) => m == y,
            (None, None) => s.compatible(&s.old.funcs[x], &s.new.funcs[y]),
            _ => false,
        };
        let pairs = lcs(&a, &b, |x, y| same(self, x, y));
        let count = |v: &[usize], x: usize| v.iter().filter(|&&e| e == x).count();
        let from = self.old.funcs[o].start;
        for (x, y) in pairs {
            if !self.fwd.contains_key(&x) && count(&a, x) == 1 && count(&b, y) == 1 {
                self.add(x, y, format!("callee of {from:#x}"));
            }
        }
    }

    fn align_globals(&mut self, o: usize, n: usize) {
        let a = &self.old.funcs[o].data;
        let b = &self.new.funcs[n].data;
        let same = |s: &Self, x: u32, y: u32| match (s.globals.get(&x), s.globals_back.get(&y)) {
            (Some(&(m, _)), _) => m == y,
            (None, None) => true,
            _ => false,
        };
        let pairs = lcs(a, b, |x, y| same(self, x, y));
        let from = self.old.funcs[o].start;
        if a.len() == b.len() || pairs.len() * 10 >= a.len().max(b.len()) * 8 {
            for (x, y) in pairs {
                self.add_global(x, y, format!("data ref of {from:#x}"));
            }
        }
    }
}

/// Longest common subsequence under `eq`; returns aligned element pairs.
fn lcs<T: Copy>(a: &[T], b: &[T], eq: impl Fn(T, T) -> bool) -> Vec<(T, T)> {
    if a.is_empty() || b.is_empty() || a.len() * b.len() > 4_000_000 {
        return Vec::new();
    }
    let (n, m) = (a.len(), b.len());
    let mut table = vec![0u16; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * (m + 1) + j] = if eq(a[i], b[j]) {
                table[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if eq(a[i], b[j]) && table[i * (m + 1) + j] == table[(i + 1) * (m + 1) + j + 1] + 1 {
            out.push((a[i], b[j]));
            i += 1;
            j += 1;
        } else if table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}
