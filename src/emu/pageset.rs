//! Which sheets of a document are shown: deleted by the user, hidden because they look wrong, or revealed again.
use std::collections::{BTreeMap, BTreeSet};

use super::analyze::{Kind, PageInfo, MANUAL};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Page(usize),
    /// A run of hidden sheets (first, last).
    Gap(usize, usize),
}

#[derive(Clone, Debug, Default)]
pub struct PageSet {
    pub n: usize,
    /// removed by the user: not shown, not exported
    pub deleted: BTreeSet<usize>,
    /// sheets that look wrong (automatic) or were hidden by hand
    pub flags: BTreeMap<usize, PageInfo>,
    /// flagged sheets that were opened up again
    pub revealed: BTreeSet<usize>,
    /// off unless the user asks for flagged sheets to be folded away
    pub hide_flagged: bool,
}

impl PageSet {
    pub fn new(n: usize, flags: BTreeMap<usize, PageInfo>) -> Self {
        Self { n, flags, ..Default::default() }
    }

    pub fn is_hidden(&self, i: usize) -> bool {
        match self.flags.get(&i) {
            Some(f) => !self.revealed.contains(&i) && (self.hide_flagged || f.reason == MANUAL),
            None => false,
        }
    }

    pub fn live(&self) -> Vec<usize> {
        (0..self.n).filter(|i| !self.deleted.contains(i)).collect()
    }

    pub fn visible(&self) -> Vec<usize> {
        self.live().into_iter().filter(|&i| !self.is_hidden(i)).collect()
    }

    pub fn hidden(&self) -> Vec<usize> {
        self.live().into_iter().filter(|&i| self.is_hidden(i)).collect()
    }

    /// `Page(i)` for a shown sheet, `Gap(first, last)` for a run of hidden ones, in page order.
    pub fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        let mut run: Option<(usize, usize)> = None;
        for i in self.live() {
            if self.is_hidden(i) {
                run = Some((run.map_or(i, |r| r.0), i));
            } else {
                if let Some(r) = run.take() {
                    out.push(Item::Gap(r.0, r.1));
                }
                out.push(Item::Page(i));
            }
        }
        if let Some(r) = run {
            out.push(Item::Gap(r.0, r.1));
        }
        out
    }

    pub fn delete(&mut self, indices: &[usize]) {
        for &i in indices {
            if i < self.n {
                self.deleted.insert(i);
            }
        }
    }

    pub fn hide(&mut self, indices: &[usize]) {
        for &i in indices {
            if i < self.n && !self.deleted.contains(&i) {
                self.flags.insert(i, PageInfo { index: i, kind: Kind::Junk, reason: MANUAL.into() });
                self.revealed.remove(&i);
            }
        }
    }

    pub fn reveal(&mut self, first: usize, last: usize) {
        for i in first..=last {
            if self.flags.contains_key(&i) {
                self.revealed.insert(i);
            }
        }
    }

    /// Fold the automatically flagged sheets away (true) or show them (false). Sheets hidden by hand stay hidden.
    pub fn set_hide_flagged(&mut self, on: bool) {
        self.hide_flagged = on;
        if on {
            self.revealed.clear();
        }
    }

    pub fn reveal_all(&mut self) {
        let keys: Vec<usize> = self.flags.keys().copied().collect();
        self.revealed.extend(keys);
    }

    /// (deleted, manually hidden, revealed) for the session file.
    pub fn state(&self) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
        (
            self.deleted.iter().copied().collect(),
            self.flags.iter().filter(|(_, f)| f.reason == MANUAL).map(|(&i, _)| i).collect(),
            self.revealed.iter().copied().collect(),
        )
    }

    /// Take saved state back; page numbers that no longer exist are ignored.
    pub fn apply(&mut self, deleted: &[usize], hidden: &[usize], revealed: &[usize]) {
        let ok = |v: &[usize], n: usize| -> BTreeSet<usize> { v.iter().copied().filter(|&i| i < n).collect() };
        self.deleted = ok(deleted, self.n);
        for i in ok(hidden, self.n) {
            self.flags.insert(i, PageInfo { index: i, kind: Kind::Junk, reason: MANUAL.into() });
        }
        self.revealed = ok(revealed, self.n);
    }
}
