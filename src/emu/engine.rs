//! Shared printer-emulation engine: paging, text-to-dots, graphics columns.
//!
//! Everything is expressed in U (1/1440 inch). The emulators (escp / imagewriter / commodore / okimate) parse
//! their byte streams and call into this engine, which produces `DotPage`s holding the raw list of struck dots.
//! Nothing here knows about pixels or ribbons.

use std::collections::HashMap;

use super::glyphs::{self as g, Bits, Face, GridSpec, SpecKind};
use super::model::{pyround, Document, DotPage, EmuOptions, Family, PrinterProfile};

pub const U: f64 = 1440.0;

#[derive(Clone, Debug)]
pub struct TextStyle {
    pub nlq: bool,
    pub face: Face,
    pub base_cpi: f64,
    pub condensed: bool,
    pub prop: bool,
    pub wmul: i32,
    /// One-line double width (SO).
    pub wmul_so: bool,
    pub hmul: i32,
    pub vscale: f64,
    pub emph: bool,
    pub dstrike: bool,
    pub italic: bool,
    /// 0 none, 1 super, 2 sub
    pub script: i32,
    pub underline: bool,
    pub overscore: bool,
    pub reverse: bool,
}

impl TextStyle {
    pub fn new(nlq: bool) -> Self {
        Self {
            nlq,
            face: Face::Roman,
            base_cpi: 10.0,
            condensed: false,
            prop: false,
            wmul: 1,
            wmul_so: false,
            hmul: 1,
            vscale: 1.0,
            emph: false,
            dstrike: false,
            italic: false,
            script: 0,
            underline: false,
            overscore: false,
            reverse: false,
        }
    }

    pub fn eff_cpi(&self) -> f64 {
        let mut c = self.base_cpi;
        if self.condensed && c < 14.0 {
            c = c * 12.0 / 7.0;
        }
        c
    }
}

/// One group of dots produced in one go (a stamped glyph expansion, a graphics line ...).
#[derive(Clone, Debug, Default)]
pub struct Chunk {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub pin: Vec<i16>,
    pub ps: Vec<i32>,
    pub ds: Vec<i8>,
    pub cs: Vec<u8>,
}

impl Chunk {
    fn len(&self) -> usize {
        self.x.len()
    }
}

struct Brush {
    ox: Vec<f64>,
    oy: Vec<f64>,
    pin: Vec<i16>,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct BrushKey {
    ch: char,
    spec: u8,
    face: Face,
    cpi: i64,
    prop: bool,
    wm: i32,
    hm: i32,
    vscale: i64,
    emph: bool,
    dstrike: bool,
    italic: bool,
    script: i32,
    underline: bool,
    overscore: bool,
    reverse: bool,
    family: u8,
    extra_space: i64,
}

struct Stamp {
    brush: usize,
    x: f64,
    y: f64,
    pass: i32,
    color: u8,
}

/// MPS "fill" mode: a graphics band waiting for its neighbour below.
pub struct PendingGap {
    pub band: Vec<(f64, Vec<u8>)>,
    pub y_abs: f64,
    pub pass: i32,
}

pub struct Engine {
    pub profile: &'static PrinterProfile,
    pub opts: EmuOptions,
    pub family: Family,
    pub pages: Vec<DotPage>,
    pub unknown: usize,
    pub notes: Vec<String>,
    pub codepage: String,
    pub tof_u: f64,
    pub page_idx: usize,
    pub cr_adds_lf: bool,
    pub lf_adds_cr: bool,
    pub extra_space_u: f64,

    brush_ids: HashMap<BrushKey, usize>,
    brushes: Vec<Brush>,
    brush_adv: Vec<f64>,

    pub style: TextStyle,
    pub x: f64,
    pub y: f64,
    pub lead: f64,
    pub lead_custom: Option<f64>,
    pub left_u: f64,
    pub right_u: f64,
    pub page_len: f64,
    pub bottom_margin: f64,
    pub national: i32,
    pub ibm_set2: bool,
    pub hstops: Vec<f64>,
    pub vstops: Vec<f64>,
    pub cur_pass: i32,
    /// ESC r n ribbon colour.
    pub color: u8,
    pub dirty: bool,
    pub line_ink: bool,
    pub blank_run: i32,
    pub last_adv: f64,

    stamps: Vec<Stamp>,
    chunks: Vec<Chunk>,
    page_len_at_start: f64,
    pub page_dots: usize,
    unk_at_start: usize,
    carry: Option<Chunk>,

    // Commodore "fill" mode support (inert for other printers)
    pub gfx_dpi: f64,
    pub pending_gap: Option<PendingGap>,
}

impl Engine {
    pub fn new(profile: &'static PrinterProfile, opts: &EmuOptions) -> Self {
        let codepage = if opts.codepage != "auto" { opts.codepage.clone() } else { profile.default_codepage.to_string() };
        let mut e = Self {
            profile,
            opts: opts.clone(),
            family: profile.family,
            pages: Vec::new(),
            unknown: 0,
            notes: Vec::new(),
            codepage,
            tof_u: opts.tof_offset_in * U,
            page_idx: 0,
            cr_adds_lf: false,
            lf_adds_cr: false,
            extra_space_u: 0.0,
            brush_ids: HashMap::new(),
            brushes: Vec::new(),
            brush_adv: Vec::new(),
            style: TextStyle::new(profile.nlq_default),
            x: 0.0,
            y: 0.0,
            lead: U / 6.0,
            lead_custom: None,
            left_u: 0.0,
            right_u: 8.0 * U,
            page_len: opts.page_length_in * U,
            bottom_margin: 0.0,
            national: 0,
            ibm_set2: false,
            hstops: Vec::new(),
            vstops: Vec::new(),
            cur_pass: 0,
            color: 0,
            dirty: false,
            line_ink: false,
            blank_run: 0,
            last_adv: U / 10.0,
            stamps: Vec::new(),
            chunks: Vec::new(),
            page_len_at_start: 0.0,
            page_dots: 0,
            unk_at_start: 0,
            carry: None,
            gfx_dpi: 60.0,
            pending_gap: None,
        };
        e.reset_state();
        e.start_page();
        e
    }

    // ------------------------------------------------------------------ state
    pub fn reset_state(&mut self) {
        self.style = TextStyle::new(self.profile.nlq_default);
        self.x = 0.0;
        self.y = 0.0;
        self.lead = U / 6.0;
        self.lead_custom = None;
        self.left_u = 0.0;
        self.right_u = if self.opts.wide_carriage == Some(true) { 13.6 } else { 8.0 } * U;
        self.page_len = self.opts.page_length_in * U;
        self.bottom_margin = 0.0;
        self.extra_space_u = 0.0;
        self.national = 0;
        self.ibm_set2 = false;
        self.hstops.clear();
        self.vstops.clear();
        self.cur_pass = 0;
        self.color = 0;
        self.dirty = false;
        self.line_ink = false;
        self.blank_run = 0;
        self.last_adv = U / 10.0;
    }

    // ------------------------------------------------------------------ pages
    fn start_page(&mut self) {
        self.stamps.clear();
        self.chunks.clear();
        self.page_len_at_start = self.page_len;
        self.page_dots = 0;
        self.unk_at_start = self.unknown;
        if let Some(c) = self.carry.take() {
            // dots that went over the perforation of the previous sheet land on top of this one
            self.chunks.push(c);
            self.page_dots = 1;
        }
    }

    fn expand_stamps(&mut self) {
        if self.stamps.is_empty() {
            return;
        }
        let nb = self.brushes.len();
        let mut by_brush: Vec<Vec<usize>> = vec![Vec::new(); nb];
        for (i, s) in self.stamps.iter().enumerate() {
            by_brush[s.brush].push(i);
        }
        for (bid, sel) in by_brush.iter().enumerate() {
            if sel.is_empty() {
                continue;
            }
            let b = &self.brushes[bid];
            let k = b.ox.len();
            if k == 0 {
                continue;
            }
            let mut c = Chunk::default();
            let total = sel.len() * k;
            c.x.reserve(total);
            c.y.reserve(total);
            c.pin.reserve(total);
            c.ps.reserve(total);
            c.ds.reserve(total);
            c.cs.reserve(total);
            for &si in sel {
                let s = &self.stamps[si];
                for j in 0..k {
                    c.x.push((s.x + b.ox[j]) as f32);
                    c.y.push((s.y + b.oy[j]) as f32);
                    c.pin.push(b.pin[j]);
                    c.ps.push(s.pass);
                    c.ds.push((s.pass & 1) as i8);
                    c.cs.push(s.color);
                }
            }
            self.chunks.push(c);
        }
        self.stamps.clear();
    }

    fn finish_page(&mut self, keep_empty: bool) -> bool {
        self.expand_stamps();
        let mut all = Chunk::default();
        if self.chunks.is_empty() {
            if !keep_empty {
                return false;
            }
        } else {
            for c in self.chunks.drain(..) {
                all.x.extend(c.x);
                all.y.extend(c.y);
                all.pin.extend(c.pin);
                all.ps.extend(c.ps);
                all.ds.extend(c.ds);
                all.cs.extend(c.cs);
            }
        }
        // continuous paper: ink that falls below the bottom edge of this sheet is printed across the perforation,
        // i.e. at the top of the next sheet (banners from Print Shop / PrintMaster cross it all the time)
        self.carry = None;
        let limit = self.page_len_at_start as f32;
        if all.y.iter().any(|&y| y > limit) {
            let mut over = Chunk::default();
            let mut keep = Chunk::default();
            for i in 0..all.len() {
                let dst = if all.y[i] > limit { &mut over } else { &mut keep };
                let yy = if all.y[i] > limit { all.y[i] - limit } else { all.y[i] };
                dst.x.push(all.x[i]);
                dst.y.push(yy);
                dst.pin.push(all.pin[i]);
                dst.ps.push(all.ps[i]);
                dst.ds.push(all.ds[i]);
                dst.cs.push(all.cs[i]);
            }
            self.carry = Some(over);
            all = keep;
        }
        let any_color = all.cs.iter().any(|&c| c != 0);
        let unknown = self.unknown - self.unk_at_start;
        self.pages.push(DotPage {
            index: self.pages.len(),
            width_in: self.opts.page_width_in,
            height_in: self.page_len_at_start / U,
            x: all.x,
            y: all.y,
            pin: all.pin,
            pass_no: all.ps,
            dirn: all.ds,
            profile_key: self.profile.key.to_string(),
            unknown,
            color: if any_color { Some(all.cs) } else { None },
        });
        true
    }

    pub fn new_page(&mut self, from_ff: bool) {
        self.flush_gap(None);
        if from_ff && self.opts.skip_blank_ff && self.page_dots == 0 && self.y < 1.0 {
            return;
        }
        self.end_pass();
        self.finish_page(true);
        self.start_page();
        self.line_ink = false;
        self.blank_run = 0;
        self.y = 0.0;
        self.page_idx += 1;
    }

    // ------------------------------------------------------------------ motion
    pub fn end_pass(&mut self) {
        if self.dirty {
            self.cur_pass += 1;
            self.dirty = false;
        }
    }

    pub fn cr(&mut self) {
        self.end_pass();
        self.x = self.left_u;
        if self.cr_adds_lf {
            self.lf(true);
        }
    }

    /// True if this line feed should be swallowed under the 'blank lines' option.
    pub fn skip_blank_lf(&mut self) -> bool {
        if self.line_ink {
            self.line_ink = false;
            self.blank_run = 0;
            return false;
        }
        self.blank_run += 1;
        let mode = self.opts.blank_lines.as_str();
        mode == "remove" || (mode == "collapse" && self.blank_run > 1)
    }

    pub fn lf(&mut self, keep_x: bool) {
        self.end_pass();
        self.style.wmul_so = false;
        if !self.skip_blank_lf() {
            let lead = self.lead;
            self.advance_y(lead);
        }
        if self.lf_adds_cr && !keep_x {
            self.x = self.left_u;
        }
    }

    pub fn advance_y(&mut self, dy: f64) {
        self.end_pass();
        self.y += dy;
        let limit = self.page_len - self.bottom_margin;
        if self.y >= limit - 1e-3 && dy > 0.0 {
            let over = self.y - limit;
            self.new_page(false);
            self.y = if self.bottom_margin == 0.0 { over } else { 0.0 };
        }
    }

    pub fn form_feed(&mut self) {
        self.new_page(true);
        if self.lf_adds_cr {
            self.x = self.left_u;
        }
    }

    pub fn next_hstop(&self) -> f64 {
        let x = self.x;
        for &s in &self.hstops {
            if s > x + 0.5 {
                return s;
            }
        }
        if !self.hstops.is_empty() {
            return x;
        }
        let cell = U / if !self.style.prop { self.style.eff_cpi() } else { 10.0 };
        let step = 8.0 * cell;
        ((x / step + 1e-6).trunc() + 1.0) * step
    }

    pub fn vtab(&mut self) {
        if !self.vstops.is_empty() {
            let y = self.y;
            let stop = self.vstops.iter().copied().find(|&s| s > y + 0.5);
            if let Some(s) = stop {
                self.advance_y(s - y);
                return;
            }
        }
        let lead = self.lead;
        self.advance_y(lead);
    }

    // ------------------------------------------------------------------ text
    fn mode_spec(&self) -> GridSpec {
        let nlq = self.opts.force_nlq.unwrap_or(self.style.nlq);
        if !nlq {
            g::DRAFT
        } else if self.profile.lq24 {
            g::LQ24
        } else {
            g::NLQ9
        }
    }

    pub fn wm(&self) -> i32 {
        if self.style.wmul_so {
            2 * self.style.wmul
        } else {
            self.style.wmul
        }
    }

    pub fn cell_u(&self) -> f64 {
        let st = &self.style;
        U / if !st.prop { st.eff_cpi() } else { 10.0 } * self.wm() as f64
    }

    fn family_id(&self) -> u8 {
        match self.family {
            Family::Escp => 0,
            Family::Ibm => 1,
            Family::Star => 2,
            Family::ImageWriter => 3,
            Family::Mps => 4,
            Family::Okimate => 5,
        }
    }

    fn get_brush(&mut self, ch: char) -> (usize, f64) {
        let st = &self.style;
        let spec = self.mode_spec();
        let wm = self.wm();
        let key = BrushKey {
            ch,
            spec: spec.kind as u8,
            face: st.face,
            cpi: pyround(st.eff_cpi() * 1000.0),
            prop: st.prop,
            wm,
            hm: st.hmul,
            vscale: pyround(st.vscale * 1000.0),
            emph: st.emph,
            dstrike: st.dstrike,
            italic: st.italic,
            script: st.script,
            underline: st.underline,
            overscore: st.overscore,
            reverse: st.reverse,
            family: self.family_id(),
            extra_space: pyround(self.extra_space_u * 1000.0),
        };
        if let Some(&bid) = self.brush_ids.get(&key) {
            return (bid, self.brush_adv[bid]);
        }
        let (bid, adv) = self.build_brush(ch, &spec, wm);
        self.brush_ids.insert(key, bid);
        (bid, adv)
    }

    fn build_brush(&mut self, ch: char, spec: &GridSpec, wm: i32) -> (usize, f64) {
        let st = self.style.clone();
        let hm = st.hmul;
        let cpi = if !st.prop { st.eff_cpi() } else { 10.0 };
        let bits: Bits;
        let gx: f64;
        let gy: f64;
        let mut adv: f64;
        let pin_div: i32;
        let emph_dx: f64;
        let ds_dy: f64;
        let under_pin: i32;
        let under_rows: i32;
        if spec.kind == SpecKind::Draft {
            let glyph = g::draft_glyph(ch);
            let mut b = glyph.bits.clone();
            if self.family == Family::Mps {
                b = g::squeeze_descender(&b, 8);
            }
            if st.reverse {
                let mut rev = Bits { rows: 9, cols: 6, d: vec![1; 54] };
                for r in 7..9 {
                    for c in 0..6 {
                        rev.set(r, c, 0);
                    }
                }
                for r in 0..b.rows.min(9) {
                    for c in 0..b.cols.min(6) {
                        let v = rev.get(r, c) ^ b.get(r, c);
                        rev.set(r, c, v);
                    }
                }
                for r in 7..9 {
                    for c in 0..6 {
                        rev.set(r, c, 0);
                    }
                }
                b = rev;
            }
            gx = 240.0 / cpi;
            gy = spec.gy;
            if st.prop {
                let adv_cols = if ch == ' ' || ch == '\u{a0}' {
                    3
                } else if glyph.full_cell {
                    b.cols as i64
                } else {
                    2.max(glyph.used_cols() as i64 + 1 + if glyph.used_cols() >= 5 { 1 } else { 0 })
                };
                adv = adv_cols as f64 * gx * wm as f64;
            } else {
                adv = U / cpi * wm as f64;
            }
            bits = b;
            pin_div = 1;
            emph_dx = 12.0;
            ds_dy = 20.0 / 3.0;
            under_pin = 8;
            under_rows = 2;
        } else {
            let cols_cell = 2.max(pyround((U / cpi) / spec.gx)) as usize;
            let glyph = g::nlq_glyph(ch, st.face, spec, cols_cell, st.prop);
            bits = glyph.bits.clone();
            gx = spec.gx;
            gy = spec.gy;
            let adv_cols = if st.prop { glyph.adv_cols as f64 } else { cols_cell as f64 };
            adv = adv_cols * gx * wm as f64;
            pin_div = if spec.kind == SpecKind::Nlq9 { 2 } else { 1 };
            emph_dx = gx * 0.5;
            ds_dy = gy * 0.5;
            under_pin = if spec.kind == SpecKind::Nlq9 { 8 } else { 23 };
            under_rows = if spec.kind == SpecKind::Nlq9 { 4 } else { 5 };
        }
        adv += self.extra_space_u;

        let mut ox: Vec<f64> = Vec::new();
        let mut oy: Vec<f64> = Vec::new();
        let mut pins: Vec<i16> = Vec::new();
        for r in 0..bits.rows {
            for c in 0..bits.cols {
                if bits.get(r, c) == 0 {
                    continue;
                }
                for w in 0..wm {
                    let cc3 = c as i32 * wm + w;
                    for h in 0..hm {
                        let rel = (r as i32 - spec.base_row) * hm - (hm - 1) + h;
                        ox.push(cc3 as f64 * gx);
                        oy.push(rel as f64 * gy * st.vscale);
                        pins.push((r as i32 / pin_div) as i16);
                    }
                }
            }
        }
        if st.italic {
            for i in 0..ox.len() {
                ox[i] -= 0.25 * oy[i];
            }
        }
        if st.script != 0 {
            let shift = if st.script == 1 { -50.0 } else { 40.0 };
            for v in oy.iter_mut() {
                *v = *v * 0.7 + shift;
            }
        }
        let mut copies: Vec<(f64, f64)> = vec![(0.0, 0.0)];
        if st.emph {
            copies.push((emph_dx, 0.0));
        }
        if st.dstrike {
            let more: Vec<(f64, f64)> = copies.iter().map(|&(a, b)| (a, b + ds_dy)).collect();
            copies.extend(more);
        }
        if copies.len() > 1 && !ox.is_empty() {
            let (bx, by, bp) = (ox.clone(), oy.clone(), pins.clone());
            ox.clear();
            oy.clear();
            pins.clear();
            for &(a, b) in &copies {
                ox.extend(bx.iter().map(|v| v + a));
                oy.extend(by.iter().map(|v| v + b));
                pins.extend(bp.iter().copied());
            }
        }
        let step = if spec.kind != SpecKind::Draft { gx } else { 12.0 };
        let arange = |stop: f64| -> Vec<f64> {
            let n = (stop / step).ceil().max(0.0) as usize;
            (0..n).map(|i| i as f64 * step).collect()
        };
        if st.underline {
            for xv in arange(adv + step * 0.5) {
                ox.push(xv);
                oy.push(under_rows as f64 * gy);
                pins.push(under_pin as i16);
            }
        }
        if st.overscore {
            for xv in arange(adv + step * 0.5) {
                ox.push(xv);
                oy.push(-(spec.cap_rows as f64 + 1.5) * gy);
                pins.push(0);
            }
        }
        let bid = self.brushes.len();
        self.brushes.push(Brush { ox, oy, pin: pins });
        self.brush_adv.push(adv);
        (bid, adv)
    }

    pub fn print_char(&mut self, ch: char) {
        self.flush_gap(None);
        let (bid, adv) = self.get_brush(ch);
        if self.opts.auto_wrap && self.x + adv > self.right_u + 0.5 && self.x > self.left_u + 0.5 {
            self.cr();
            if !self.cr_adds_lf {
                self.lf(true);
            }
        }
        if !self.brushes[bid].ox.is_empty() {
            self.stamps.push(Stamp { brush: bid, x: self.x, y: self.tof_u + self.y, pass: self.cur_pass, color: self.color });
            self.dirty = true;
            self.line_ink = true;
            self.page_dots += 1;
        }
        self.x += adv;
        self.last_adv = adv;
    }

    pub fn backspace(&mut self) {
        self.x = self.left_u.max(self.x - self.last_adv);
    }

    // ------------------------------------------------------------------ graphics
    /// Print `data` as graphics columns of `bytes_per_col` bytes each.
    #[allow(clippy::too_many_arguments)]
    pub fn emit_columns(
        &mut self,
        data: &[u8],
        bytes_per_col: usize,
        dpi_x: f64,
        dpi_y: f64,
        top_off: f64,
        msb_top: bool,
        no_adjacent: bool,
        nine_pin: bool,
        nbits: Option<usize>,
        advance: bool,
    ) {
        let n = data.len() / bytes_per_col.max(1);
        if n == 0 {
            return;
        }
        // build the bit matrix: n columns x nb rows
        let mut nb = bytes_per_col * 8;
        let mut cols_bits: Vec<Vec<u8>> = Vec::with_capacity(n);
        for ci in 0..n {
            let col = &data[ci * bytes_per_col..(ci + 1) * bytes_per_col];
            let mut bitsv: Vec<u8> = Vec::with_capacity(nb);
            for &byte in col {
                for k in 0..8 {
                    let bit = if msb_top { (byte >> (7 - k)) & 1 } else { (byte >> k) & 1 };
                    bitsv.push(bit);
                }
            }
            if nine_pin && bytes_per_col == 2 {
                bitsv.truncate(9);
                // keep bits 0..8 then bit 8 (index 8) -> first 9 entries; entries 9.. are dropped
            }
            cols_bits.push(bitsv);
        }
        if nine_pin && bytes_per_col == 2 {
            nb = 9;
        }
        if let Some(limit) = nbits {
            for c in cols_bits.iter_mut() {
                c.truncate(limit);
            }
            nb = nb.min(limit);
        }
        if no_adjacent {
            for k in 0..nb {
                let mut run0: i64 = -1;
                let mut prev = 0u8;
                for (ci, col) in cols_bits.iter_mut().enumerate() {
                    let cur = col[k];
                    if cur == 1 && prev == 0 {
                        run0 = ci as i64;
                    }
                    let keep = cur == 1 && (ci as i64 - run0) % 2 == 0;
                    prev = cur;
                    col[k] = keep as u8;
                }
            }
        }
        let dx = U / dpi_x;
        let dy = U / dpi_y;
        let mut c = Chunk::default();
        for (ci, col) in cols_bits.iter().enumerate() {
            for (k, &b) in col.iter().enumerate().take(nb) {
                if b != 0 {
                    c.x.push((self.x + ci as f64 * dx) as f32);
                    c.y.push((self.tof_u + self.y + top_off + k as f64 * dy) as f32);
                    c.pin.push(k as i16);
                    c.ps.push(self.cur_pass + 1000003); // graphics passes are distinct
                    c.ds.push(0);
                    c.cs.push(self.color);
                }
            }
        }
        if c.len() > 0 {
            self.chunks.push(c);
            self.page_dots += 1;
            self.dirty = true;
            self.line_ink = true;
        }
        if advance {
            self.x += n as f64 * dx;
        }
    }

    /// Convenience wrapper with the common defaults.
    pub fn columns(&mut self, data: &[u8], bpc: usize, dpi_x: f64, dpi_y: f64, top_off: f64, msb_top: bool, no_adjacent: bool) {
        self.emit_columns(data, bpc, dpi_x, dpi_y, top_off, msb_top, no_adjacent, false, None, true);
    }

    /// Push a ready-made chunk (raster graphics, gap fill).
    pub fn push_chunk(&mut self, c: Chunk) {
        if c.len() > 0 {
            self.chunks.push(c);
        }
    }

    // ------------------------------------------------------------------ Commodore gap fill
    fn band_rows(&self, band: &[(f64, Vec<u8>)]) -> (Vec<f64>, Vec<bool>, Vec<bool>) {
        let mut xs = Vec::new();
        let mut top = Vec::new();
        let mut bottom = Vec::new();
        for (x0, a) in band {
            for (i, &v) in a.iter().enumerate() {
                xs.push(x0 + i as f64 * (U / self.gfx_dpi));
                top.push(v & 1 != 0);
                bottom.push((v >> 6) & 1 != 0);
            }
        }
        (xs, top, bottom)
    }

    /// Fill in the one-dot gap row between two consecutive Commodore graphics lines.
    pub fn flush_gap(&mut self, nxt: Option<&[(f64, Vec<u8>)]>) {
        let Some(p) = self.pending_gap.take() else { return };
        let Some(nxt) = nxt else { return };
        if nxt.is_empty() {
            return; // nothing below this line: there is no picture row to blend with
        }
        let (xs, _top, bottom) = self.band_rows(&p.band);
        let (xn, top_n, _b) = self.band_rows(nxt);
        let step = U / self.gfx_dpi;
        let key = |x: f64| -> i64 { (x / step).round_ties_even() as i64 };
        let below_map: HashMap<i64, bool> = xn.iter().zip(&top_n).map(|(&x, &t)| (key(x), t)).collect();
        let mut c = Chunk::default();
        for (i, &x) in xs.iter().enumerate() {
            let k = key(x);
            let fill = if k % 2 == 0 { bottom[i] } else { *below_map.get(&k).unwrap_or(&false) };
            if fill {
                c.x.push(x as f32);
                c.y.push((p.y_abs + U / 72.0) as f32);
                c.pin.push(6);
                c.ps.push(p.pass);
                c.ds.push(0);
                c.cs.push(0);
            }
        }
        if c.len() > 0 {
            self.chunks.push(c);
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------------ result
    pub fn finish(&mut self, n_bytes: usize) -> Document {
        self.flush_gap(None);
        self.end_pass();
        self.expand_stamps();
        if !self.chunks.is_empty() {
            self.finish_page(false);
        }
        let mut max_x: f64 = 0.0;
        for p in &self.pages {
            if let Some(m) = p.x.iter().copied().reduce(f32::max) {
                max_x = max_x.max(m as f64 / U);
            }
        }
        let carriage = if max_x <= 8.2 { 8.0 } else { 13.6 };
        Document {
            pages: std::mem::take(&mut self.pages),
            profile: self.profile,
            detected: String::new(),
            carriage_in: carriage,
            notes: std::mem::take(&mut self.notes),
            warnings: Vec::new(),
            fixes: Vec::new(),
            n_bytes,
            n_unknown: self.unknown,
        }
    }
}
