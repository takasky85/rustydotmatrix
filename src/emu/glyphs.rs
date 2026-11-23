//! Glyph generation for the dot-matrix emulators.
//!
//! * DRAFT  - the hand-built 5x7 font (descenders on pins 8-9), accents composed ahead of time (see tables.rs)
//! * NLQ/LQ - a bundled TTF rasterised onto the printer's fine dot grid and thresholded (via a [`FontRaster`])
//! * box drawing / block elements are drawn procedurally so neighbouring cells join up.

use std::sync::OnceLock;

use super::model::pyround;
use super::tables::{BOX_ARMS, DRAFT as DRAFT_TABLE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecKind {
    Draft,
    Nlq9,
    Lq24,
}

/// Geometry of a glyph cell on the head's dot grid (units of 1/1440 inch = 'U').
#[derive(Clone, Copy, Debug)]
pub struct GridSpec {
    pub kind: SpecKind,
    pub name: &'static str,
    /// Horizontal dot pitch, U (draft: reference value at 10 cpi).
    pub gx: f64,
    /// Vertical dot pitch, U.
    pub gy: f64,
    /// Rows in a cell (box/block characters span all of them).
    pub rows: usize,
    /// Row index that sits on the baseline (bottom of capitals).
    pub base_row: i32,
    /// Capital height in rows.
    pub cap_rows: i32,
    pub pins: i32,
    /// Box line thickness (dots).
    pub thick: i32,
    /// Separation between the two strokes of a double line.
    pub sep: i32,
}

pub const DRAFT: GridSpec = GridSpec { kind: SpecKind::Draft, name: "draft", gx: 24.0, gy: 20.0, rows: 9, base_row: 6, cap_rows: 7, pins: 9, thick: 1, sep: 2 };
pub const NLQ9: GridSpec = GridSpec { kind: SpecKind::Nlq9, name: "nlq", gx: 12.0, gy: 10.0, rows: 24, base_row: 17, cap_rows: 14, pins: 9, thick: 2, sep: 4 };
pub const LQ24: GridSpec = GridSpec { kind: SpecKind::Lq24, name: "lq", gx: 8.0, gy: 8.0, rows: 30, base_row: 22, cap_rows: 18, pins: 24, thick: 3, sep: 6 };

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Roman,
    Sans,
    Courier,
}

impl Face {
    pub fn as_str(self) -> &'static str {
        match self {
            Face::Roman => "roman",
            Face::Sans => "sans",
            Face::Courier => "courier",
        }
    }
}

/// A small 0/1 bitmap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bits {
    pub rows: usize,
    pub cols: usize,
    pub d: Vec<u8>,
}

impl Bits {
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Self { rows, cols, d: vec![0; rows * cols] }
    }
    pub fn get(&self, r: usize, c: usize) -> u8 {
        self.d[r * self.cols + c]
    }
    pub fn set(&mut self, r: usize, c: usize, v: u8) {
        self.d[r * self.cols + c] = v;
    }
    pub fn any(&self) -> bool {
        self.d.iter().any(|&v| v != 0)
    }
    /// Index just past the last used column.
    pub fn used_cols(&self) -> usize {
        let mut best = 0;
        for c in 0..self.cols {
            if (0..self.rows).any(|r| self.get(r, c) != 0) {
                best = c + 1;
            }
        }
        best
    }
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub bits: Bits,
    /// Box/block glyph that fills the whole advance.
    pub full_cell: bool,
    /// Advance in dot columns for NLQ proportional (0 = n/a).
    pub adv_cols: i32,
}

impl Glyph {
    pub fn used_cols(&self) -> usize {
        self.bits.used_cols()
    }
}

// ---------------------------------------------------------------------------------------- TTF hook

/// Rasterises text for the NLQ / LQ modes. The GUI registers an implementation; without one NLQ glyphs are blank.
pub trait FontRaster: Send + Sync {
    /// Rasterise `ch` into a (spec.rows, cols) 0/1 dot pattern.
    fn bits(&self, ch: char, face: Face, spec: &GridSpec, cols: usize, threshold: f32) -> Bits;
    /// Advance width in pixels of `ch` in a font calibrated to `cap_px` capital height.
    fn advance_px(&self, ch: char, face: Face, cap_px: i32) -> f32;
}

static RASTER: OnceLock<Box<dyn FontRaster>> = OnceLock::new();

pub fn set_font_raster(r: Box<dyn FontRaster>) {
    let _ = RASTER.set(r);
}

fn ttf_bits(ch: char, face: Face, spec: &GridSpec, cols: usize, threshold: f32) -> Bits {
    match RASTER.get() {
        Some(r) => r.bits(ch, face, spec, cols, threshold),
        None => Bits::zeros(spec.rows, cols),
    }
}

fn ttf_advance(ch: char, face: Face, cap_px: i32) -> f32 {
    match RASTER.get() {
        Some(r) => r.advance_px(ch, face, cap_px),
        None => cap_px as f32 * 0.6,
    }
}

// ---------------------------------------------------------------------------------------- draft

pub fn draft_glyph(ch: char) -> Glyph {
    let o = ch as u32;
    match DRAFT_TABLE.binary_search_by_key(&o, |e| e.0) {
        Ok(i) => {
            let (_, full, cols, rows) = DRAFT_TABLE[i];
            let cols = cols as usize;
            let mut bits = Bits::zeros(9, cols);
            for (r, m) in rows.iter().enumerate() {
                for c in 0..cols {
                    if m & (1 << c) != 0 {
                        bits.set(r, c, 1);
                    }
                }
            }
            Glyph { bits, full_cell: full, adv_cols: 0 }
        }
        Err(_) => Glyph { bits: Bits::zeros(9, 5), full_cell: false, adv_cols: 0 },
    }
}

/// Fit a descender glyph into `limit_rows` pins (8-pin heads) by dropping its middle row.
pub fn squeeze_descender(bits: &Bits, limit_rows: usize) -> Bits {
    if bits.rows > limit_rows && (limit_rows * bits.cols..bits.d.len()).any(|i| bits.d[i] != 0) {
        let mut out = Bits::zeros(bits.rows, bits.cols);
        let mut dst = 0;
        for r in 0..bits.rows {
            if r == 4 {
                continue;
            }
            for c in 0..bits.cols {
                out.set(dst, c, bits.get(r, c));
            }
            dst += 1;
        }
        return out;
    }
    bits.clone()
}

// ---------------------------------------------------------------------------------------- box drawing

fn box_arms(ch: char) -> Option<(i32, i32, i32, i32)> {
    let o = ch as u32;
    BOX_ARMS
        .binary_search_by_key(&o, |e| e.0)
        .ok()
        .map(|i| (BOX_ARMS[i].1 as i32, BOX_ARMS[i].2 as i32, BOX_ARMS[i].3 as i32, BOX_ARMS[i].4 as i32))
}

/// Inclusive coordinate ranges for a stroke family.
fn strokes(style: i32, centre: i32, thick: i32, sep: i32) -> Vec<(i32, i32)> {
    if style == 1 {
        let s = centre - (thick - 1) / 2;
        vec![(s, s + thick - 1)]
    } else {
        let s1 = centre - sep / 2 - (thick - 1);
        let s2 = centre + (sep + 1) / 2;
        vec![(s1, s1 + thick - 1), (s2, s2 + thick - 1)]
    }
}

/// Draw a box-drawing character filling the whole cell so neighbours join up.
pub fn box_bits(ch: char, cols: usize, spec: &GridSpec) -> Option<Bits> {
    let (l, r, u, d) = box_arms(ch)?;
    let rows = spec.rows as i32;
    let (t, sep) = (spec.thick, spec.sep);
    let (cx, cy) = if spec.kind == SpecKind::Draft { (2, 3) } else { (cols as i32 / 2 - 1, spec.base_row - spec.cap_rows / 3) };
    let hs = [strokes(1, cy, t, sep), strokes(2, cy, t, sep)]; // horizontal strokes (y ranges)
    let vs = [strokes(1, cx, t, sep), strokes(2, cx, t, sep)]; // vertical strokes (x ranges)
    let h = |s: i32| -> &Vec<(i32, i32)> { &hs[(s - 1) as usize] };
    let v = |s: i32| -> &Vec<(i32, i32)> { &vs[(s - 1) as usize] };
    let mut out = Bits::zeros(spec.rows, cols);
    let cols_i = cols as i32;
    let mut fill = |y0: i32, y1: i32, x0: i32, x1: i32| {
        let (y0, y1) = (y0.max(0), y1.min(rows - 1));
        let (x0, x1) = (x0.max(0), x1.min(cols_i - 1));
        if y1 >= y0 && x1 >= x0 {
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    out.set(yy as usize, xx as usize, 1);
                }
            }
        }
    };
    let nz = |s: i32| if s == 0 { 1 } else { s };
    // horizontal arms
    for (arm, style) in [('L', l), ('R', r)] {
        if style == 0 {
            continue;
        }
        for (k, &(y0, y1)) in h(style).iter().enumerate() {
            let side = if style == 1 { 0 } else if k == 0 { -1 } else { 1 };
            let (x0, x1);
            if side == 0 {
                let refv = u.max(d);
                if arm == 'R' {
                    x0 = v(nz(refv))[0].0;
                    x1 = cols_i - 1;
                } else {
                    x0 = 0;
                    x1 = v(nz(refv)).last().unwrap().1;
                }
            } else {
                let p = if side < 0 { u } else { d };
                let o = if side < 0 { d } else { u };
                if arm == 'R' {
                    x0 = if p != 0 {
                        v(p).last().unwrap().0
                    } else if o != 0 {
                        v(o)[0].0
                    } else {
                        v(1)[0].0
                    };
                    x1 = cols_i - 1;
                } else {
                    x0 = 0;
                    x1 = if p != 0 {
                        v(p)[0].1
                    } else if o != 0 {
                        v(o).last().unwrap().1
                    } else {
                        v(1).last().unwrap().1
                    };
                }
            }
            fill(y0, y1, x0, x1);
        }
    }
    // vertical arms
    for (arm, style) in [('U', u), ('D', d)] {
        if style == 0 {
            continue;
        }
        for (k, &(x0, x1)) in v(style).iter().enumerate() {
            let side = if style == 1 { 0 } else if k == 0 { -1 } else { 1 };
            let (y0, y1);
            if side == 0 {
                let refv = l.max(r);
                if arm == 'D' {
                    y0 = h(nz(refv))[0].0;
                    y1 = rows - 1;
                } else {
                    y0 = 0;
                    y1 = h(nz(refv)).last().unwrap().1;
                }
            } else {
                let p = if side < 0 { l } else { r };
                let o = if side < 0 { r } else { l };
                if arm == 'D' {
                    y0 = if p != 0 {
                        h(p).last().unwrap().0
                    } else if o != 0 {
                        h(o)[0].0
                    } else {
                        h(1)[0].0
                    };
                    y1 = rows - 1;
                } else {
                    y0 = 0;
                    y1 = if p != 0 {
                        h(p)[0].1
                    } else if o != 0 {
                        h(o).last().unwrap().1
                    } else {
                        h(1).last().unwrap().1
                    };
                }
            }
            fill(y0, y1, x0, x1);
        }
    }
    Some(out)
}

fn blocks(ch: char) -> Option<&'static [(f64, f64, f64, f64)]> {
    const Q: f64 = 0.5;
    Some(match ch {
        '▀' => &[(0.0, Q, 0.0, 1.0)],
        '▄' => &[(Q, 1.0, 0.0, 1.0)],
        '█' => &[(0.0, 1.0, 0.0, 1.0)],
        '▌' => &[(0.0, 1.0, 0.0, Q)],
        '▐' => &[(0.0, 1.0, Q, 1.0)],
        '▖' => &[(Q, 1.0, 0.0, Q)],
        '▗' => &[(Q, 1.0, Q, 1.0)],
        '▘' => &[(0.0, Q, 0.0, Q)],
        '▝' => &[(0.0, Q, Q, 1.0)],
        '▙' => &[(0.0, Q, 0.0, Q), (Q, 1.0, 0.0, Q), (Q, 1.0, Q, 1.0)],
        '▚' => &[(0.0, Q, 0.0, Q), (Q, 1.0, Q, 1.0)],
        '▛' => &[(0.0, Q, 0.0, Q), (0.0, Q, Q, 1.0), (Q, 1.0, 0.0, Q)],
        '▜' => &[(0.0, Q, 0.0, Q), (0.0, Q, Q, 1.0), (Q, 1.0, Q, 1.0)],
        '▞' => &[(0.0, Q, Q, 1.0), (Q, 1.0, 0.0, Q)],
        '▟' => &[(0.0, Q, Q, 1.0), (Q, 1.0, 0.0, Q), (Q, 1.0, Q, 1.0)],
        '▁' => &[(0.875, 1.0, 0.0, 1.0)],
        '▂' => &[(0.75, 1.0, 0.0, 1.0)],
        '▃' => &[(0.625, 1.0, 0.0, 1.0)],
        '▅' => &[(0.375, 1.0, 0.0, 1.0)],
        '▆' => &[(0.25, 1.0, 0.0, 1.0)],
        '▇' => &[(0.125, 1.0, 0.0, 1.0)],
        '▉' => &[(0.0, 1.0, 0.0, 0.875)],
        '▊' => &[(0.0, 1.0, 0.0, 0.75)],
        '▋' => &[(0.0, 1.0, 0.0, 0.625)],
        '▍' => &[(0.0, 1.0, 0.0, 0.375)],
        '▎' => &[(0.0, 1.0, 0.0, 0.25)],
        '▏' => &[(0.0, 1.0, 0.0, 0.125)],
        '▔' => &[(0.0, 0.125, 0.0, 1.0)],
        '▕' => &[(0.0, 1.0, 0.875, 1.0)],
        _ => return None,
    })
}

pub fn block_bits(ch: char, cols: usize, spec: &GridSpec) -> Option<Bits> {
    let rows = spec.rows;
    if let Some(list) = blocks(ch) {
        let mut out = Bits::zeros(rows, cols);
        for &(r0, r1, c0, c1) in list {
            let ra = pyround(r0 * rows as f64);
            let rb = pyround(r1 * rows as f64).max(ra + 1);
            let ca = pyround(c0 * cols as f64);
            let cb = pyround(c1 * cols as f64).max(ca + 1);
            for r in ra.max(0)..rb.min(rows as i64) {
                for c in ca.max(0)..cb.min(cols as i64) {
                    out.set(r as usize, c as usize, 1);
                }
            }
        }
        return Some(out);
    }
    if matches!(ch, '░' | '▒' | '▓') {
        let mut out = Bits::zeros(rows, cols);
        for yy in 0..rows {
            for xx in 0..cols {
                let v = match ch {
                    '░' => (xx % 4 == 0 && yy % 4 == 0) || (xx % 4 == 2 && yy % 4 == 2),
                    '▒' => (xx + yy) % 2 == 0,
                    _ => !((xx % 4 == 1 && yy % 4 == 1) || (xx % 4 == 3 && yy % 4 == 3)),
                };
                out.set(yy, xx, v as u8);
            }
        }
        return Some(out);
    }
    if matches!(ch, '■' | '▪') {
        let mut out = Bits::zeros(rows, cols);
        let r0 = spec.base_row - (2.max(spec.cap_rows * 2 / 3)) + 1;
        let half = (cols / 4).max(1) as i32;
        let c0 = (cols as i32 / 2 - half).max(0);
        let c1 = cols as i32 / 2 + half;
        for r in r0.max(0)..spec.base_row.min(rows as i32) {
            for c in c0..c1.min(cols as i32) {
                out.set(r as usize, c as usize, 1);
            }
        }
        return Some(out);
    }
    None
}

// ---------------------------------------------------------------------------------------- NLQ

/// Near-letter-quality glyph. For fixed pitch `cols` is the cell width in dot columns.
/// For proportional text `cols` is ignored and the advance comes from the font.
pub fn nlq_glyph(ch: char, face: Face, spec: &GridSpec, cols: usize, prop: bool) -> Glyph {
    if !prop {
        if let Some(b) = box_bits(ch, cols, spec) {
            return Glyph { bits: b, full_cell: true, adv_cols: 0 };
        }
        if let Some(b) = block_bits(ch, cols, spec) {
            return Glyph { bits: b, full_cell: true, adv_cols: 0 };
        }
    }
    if ch == ' ' || ch == '\u{a0}' {
        let mut c = cols as i64;
        if prop {
            c = 3.max(pyround(spec.cap_rows as f64 * spec.gy * 0.5 / spec.gx));
        }
        return Glyph { bits: Bits::zeros(spec.rows, c.max(1) as usize), full_cell: false, adv_cols: c as i32 };
    }
    if prop {
        let sy = 1.max(pyround(spec.gy / 2.0)) as i32;
        let sx = 1.max(pyround(spec.gx / 2.0)) as f64;
        let adv = ttf_advance(ch, face, spec.cap_rows * sy) as f64;
        let c = 2.max(pyround(adv / sx));
        let bits = ttf_bits(ch, face, spec, (c + 1) as usize, 0.5);
        return Glyph { bits, full_cell: false, adv_cols: c as i32 };
    }
    let bits = ttf_bits(ch, face, spec, cols, 0.5);
    Glyph { bits, full_cell: false, adv_cols: cols as i32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_a_has_dots() {
        let g = draft_glyph('A');
        assert_eq!(g.bits.rows, 9);
        assert!(g.bits.any());
        assert!(!draft_glyph(' ').bits.any());
    }

    #[test]
    fn box_light_horizontal_spans_the_cell() {
        let b = box_bits('─', 6, &DRAFT).unwrap();
        // row 3 (cy) is solid
        assert!((0..6).all(|c| b.get(3, c) == 1));
        assert!(!(0..6).all(|c| b.get(2, c) == 1));
    }

    #[test]
    fn blocks_use_python_rounding() {
        // 0.5 * 9 rows = 4.5 -> 4 (half to even), so the upper half block has 4 rows
        let b = block_bits('▀', 6, &DRAFT).unwrap();
        let rows_on = (0..9).filter(|&r| b.get(r, 0) == 1).count();
        assert_eq!(rows_on, 4);
    }

    #[test]
    fn squeeze_drops_middle_row_when_descender_present() {
        let g = draft_glyph('g');
        let s = squeeze_descender(&g.bits, 8);
        assert_eq!(s.rows, 9);
        assert!(!(0..s.cols).any(|c| s.get(8, c) != 0));
    }
}
