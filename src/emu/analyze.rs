//! Spot sheets that did not come out right (blank, a few stray dots, garbage from damaged or truncated data).
//!
//! The test is deliberately conservative and explains itself: every flagged sheet carries a one-line reason, and nothing is
//! ever thrown away - the GUI only hides flagged sheets behind a "pages 10-60 hidden" marker that can be opened again.
use std::collections::{BTreeMap, BTreeSet};

use super::model::{DotPage, Document, U_PER_INCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ok,
    Blank,
    Stray,
    Junk,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ok => "ok",
            Kind::Blank => "blank",
            Kind::Stray => "stray",
            Kind::Junk => "junk",
        }
    }
}

/// A sheet with this few dots is a leftover fragment, not a print.
pub const STRAY_DOTS: usize = 12;
/// This many commands the emulator did not understand on a single sheet = the data is misframed.
pub const UNKNOWN_LIMIT: usize = 24;
/// More than this share of the dots landing outside the paper.
pub const OFF_PAGE_LIMIT: f64 = 0.30;
/// Share of the sheet that is covered in dots: nothing legible ever prints that dense.
pub const INK_LIMIT: f64 = 0.62;

pub const MANUAL: &str = "hidden by you";

#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    pub index: usize,
    pub kind: Kind,
    pub reason: String,
}

impl PageInfo {
    pub fn hidden_by_default(&self) -> bool {
        self.kind != Kind::Ok
    }
}

/// Share of the sheet (1/90 in cells) that holds at least one dot, among the cells that fall on the paper.
fn coverage(page: &DotPage) -> f64 {
    let w = (page.width_in * 90.0) as i64 + 1;
    let h = (page.height_in * 90.0) as i64 + 1;
    let mut cells: BTreeSet<i64> = BTreeSet::new();
    for (&x, &y) in page.x.iter().zip(&page.y) {
        let ix = (x / 16.0) as i64;
        let iy = (y / 16.0) as i64;
        if ix >= 0 && ix < w && iy >= 0 && iy < h {
            cells.insert(iy * w + ix);
        }
    }
    cells.len() as f64 / (w * h) as f64
}

pub fn classify_page(page: &DotPage) -> PageInfo {
    let n = page.n_dots();
    let mk = |kind, reason: String| PageInfo { index: page.index, kind, reason };
    if n == 0 {
        return mk(Kind::Blank, "nothing printed".into());
    }
    if n <= STRAY_DOTS {
        return mk(Kind::Stray, format!("only {n} stray dots"));
    }
    // the Python original compares float32 coordinates with float32-rounded limits
    let lo_x = (-(U_PER_INCH * 0.05)) as f32;
    let hi_x = ((page.width_in + 0.5) * U_PER_INCH) as f32;
    let hi_y = ((page.height_in + 0.3) * U_PER_INCH) as f32;
    let off_n = page.x.iter().zip(&page.y).filter(|&(&x, &y)| x < lo_x || x > hi_x || y < 0.0 || y > hi_y).count();
    let off = off_n as f64 / n as f64;
    if page.unknown >= UNKNOWN_LIMIT {
        return mk(Kind::Junk, format!("{} commands the printer would not understand: data missing or damaged", page.unknown));
    }
    if off > OFF_PAGE_LIMIT {
        return mk(Kind::Junk, format!("{:.0}% of the dots land off the paper: graphics out of step", off * 100.0));
    }
    let cov = coverage(page);
    if cov > INK_LIMIT {
        return mk(Kind::Junk, format!("{:.0}% of the sheet is solid dots: random characters", cov * 100.0));
    }
    mk(Kind::Ok, String::new())
}

pub fn classify(doc: &Document) -> Vec<PageInfo> {
    doc.pages.iter().map(classify_page).collect()
}

/// Group sorted page numbers into inclusive (first, last) runs: [9, 10, 11, 20] -> [(9, 11), (20, 20)].
pub fn runs(indices: &[usize]) -> Vec<(usize, usize)> {
    let mut v: Vec<usize> = indices.to_vec();
    v.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for i in v {
        match out.last_mut() {
            Some(last) if i == last.1 + 1 => last.1 = i,
            _ => out.push((i, i)),
        }
    }
    out
}

/// "Pages 10-60 hidden (garbled)" - 1-based numbers, as the page box shows them.
pub fn describe_run(first: usize, last: usize, infos: &BTreeMap<usize, PageInfo>) -> String {
    let n = last - first + 1;
    let kinds: BTreeSet<&'static str> = (first..=last).filter_map(|i| infos.get(&i)).map(|f| f.kind.as_str()).collect();
    let mut labels: Vec<&str> = Vec::new();
    for (k, w) in [("junk", "garbled"), ("blank", "blank"), ("stray", "nearly empty")] {
        if kinds.contains(k) {
            labels.push(w);
        }
    }
    let label = if labels.is_empty() { "flagged".to_string() } else { labels.join(" / ") };
    let span = if n == 1 { format!("Page {}", first + 1) } else { format!("Pages {}-{}", first + 1, last + 1) };
    format!("{span} hidden ({label})")
}
