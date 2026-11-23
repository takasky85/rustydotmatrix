//! Okidata Okimate 10 / 20 (and Commodore MCS-820) emulation.
//!
//! The Okimate prints colour graphics with a three-colour thermal-transfer ribbon (yellow, magenta, cyan panels).
//! Every printed line is three passes, one per ribbon colour, and a carriage return steps the ribbon on to the next
//! colour; the carriage return after the cyan pass also advances the paper. GEOS sends
//! `ESC K n1 n2 <yellow> CR ESC K n1 n2 <magenta> CR ESC K n1 n2 <cyan> CR`, or its own "colour band" format
//! (`ESC EM 03 ... 03 02 8D` three times, then `ESC LF 16`).
use super::charsets;
use super::engine::{Engine, U};
use super::escp::slice;
use super::model::{Document, EmuOptions, PrinterProfile};

/// Ribbon panel order -> ink: yellow, magenta, cyan (ink codes as in DotPage.color).
const PASS_INK: [u8; 3] = [4, 1, 2];

fn graphics_dpi(c: u8) -> Option<f64> {
    match c {
        0x4B => Some(60.0),
        0x4C | 0x59 => Some(120.0),
        0x5A => Some(240.0),
        _ => None,
    }
}

const BAND_DPI_X: f64 = 80.0; // estimated
const BAND_DPI_Y: f64 = 72.0; // one pixel row = 2 pins = 1/72 in

/// (start, end) of every ESC K / L / Y / Z bit-image block (header + payload).
fn walk_blocks(data: &[u8]) -> Vec<(usize, usize)> {
    let n = data.len();
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        let Some(off) = data.get(pos..).and_then(|s| s.iter().position(|&b| b == 0x1b)) else { return out };
        let i = pos + off;
        if i + 4 > n {
            return out;
        }
        if graphics_dpi(data[i + 1]).is_some() {
            let end = i + 4 + (data[i + 2] as usize | (data[i + 3] as usize) << 8);
            if end > n {
                return out;
            }
            out.push((i, end));
            pos = end;
        } else {
            pos = i + 2;
        }
    }
}

fn count_sub(d: &[u8], pat: &[u8]) -> usize {
    let (mut c, mut i) = (0, 0);
    while i + pat.len() <= d.len() {
        if &d[i..i + pat.len()] == pat {
            c += 1;
            i += pat.len();
        } else {
            i += 1;
        }
    }
    c
}

/// Colour-ribbon graphics: bit-image blocks chained only by bare carriage returns (no line feeds between passes),
/// or GEOS colour bands (ESC EM 3 ... 03 02 8D).
pub fn looks_okimate(data: &[u8]) -> bool {
    if count_sub(data, b"\x03\x02\x8d") >= 6 && (count_sub(data, b"\x1b\x19\x03") >= 2 || data.starts_with(b"\x1c\x1bP")) {
        return true;
    }
    let ranges = walk_blocks(data);
    let blocks = ranges.len();
    let mut chained = 0usize;
    for &(_a, end) in &ranges {
        if data.get(end) == Some(&b'\r') {
            let mut j = end;
            while data.get(j) == Some(&b'\r') {
                j += 1;
            }
            let nxt = slice(data, j, j + 2);
            if matches!(nxt, b"\x1bK" | b"\x1bL" | b"\x1bY" | b"\x1bZ") || j - end >= 2 {
                chained += 1;
            }
        }
    }
    if !(blocks >= 6 && chained as f64 >= 0.8 * blocks as f64) {
        return false;
    }
    // Commodore-mode Star / Riteman / Epson streams chain blocks with bare CRs too (CR = new line there). What sets the
    // Okimate apart is that the blocks of one line are three passes over the same picture, so neighbours look alike.
    let mut sims: Vec<f64> = Vec::new();
    let lim = ranges.len().min(80);
    for k in 0..lim.min(ranges.len().saturating_sub(1)) {
        let (a1, e1) = ranges[k];
        let (a2, e2) = ranges[k + 1];
        let x = &data[a1 + 4..e1];
        let y = &data[a2 + 4..e2];
        let n = x.len().min(y.len());
        let either: u32 = (0..n).map(|i| (x[i] | y[i]).count_ones()).sum();
        if either > 0 {
            let both: u32 = (0..n).map(|i| (x[i] & y[i]).count_ones()).sum();
            sims.push(both as f64 / either as f64);
        }
    }
    if sims.is_empty() {
        return false;
    }
    let mut s = 0.0;
    for v in &sims {
        s += v;
    }
    s / sims.len() as f64 >= 0.55
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub struct OkimateEngine {
    pub e: Engine,
    colour_ribbon: bool,
    pass_mode: bool,
    skipped_bytes: usize,
    pub bands: usize,
    phase: usize,
    pending_lead: f64,
}

impl OkimateEngine {
    pub fn new(profile: &'static PrinterProfile, opts: &EmuOptions) -> Self {
        let mut s = Self {
            e: Engine::new(profile, opts),
            colour_ribbon: false,
            pass_mode: false,
            skipped_bytes: 0,
            bands: 0,
            phase: 0,
            pending_lead: 0.0,
        };
        s.reset_ok();
        s
    }

    fn reset_ok(&mut self) {
        self.e.reset_state();
        self.phase = 0;
        self.e.lead = U / 6.0;
        self.pending_lead = 0.0;
    }

    pub fn run(&mut self, data: &[u8]) {
        self.colour_ribbon = looks_okimate(data);
        self.pass_mode = count_sub(data, b"\x03\x02\x8d") >= 6;
        if self.pass_mode {
            self.e.lead = U * 16.0 / 144.0; // one band (12 picture rows) per line feed
        }
        self.e.color = if self.colour_ribbon { PASS_INK[0] } else { 0 };
        let n = data.len();
        let mut i = 0usize;
        while i < n {
            let b = data[i];
            if b == 0x1B {
                i = self.escape(data, i + 1);
            } else if b == 0x03 && self.pass_mode {
                // start of a GEOS pass (black ribbon: no ESC EM in front of it)
                i = self.pass(data, i, 0);
            } else if (0x20..0x80).contains(&b) && b != 0x7F {
                let cp = self.e.codepage.clone();
                if let Some(ch) = charsets::decode(b, &cp, 0, false) {
                    self.e.print_char(ch);
                }
                i += 1;
            } else {
                match b {
                    0x0D => self.carriage_return(),
                    0x0A => self.e.lf(false),
                    0x0C => self.e.form_feed(),
                    0x08 => self.e.backspace(),
                    0x10 => i += 1, // tab to character column: DLE n
                    _ => {}
                }
                i += 1;
            }
        }
    }

    fn carriage_return(&mut self) {
        if !self.colour_ribbon {
            self.e.cr();
            return;
        }
        self.e.end_pass();
        self.e.x = self.e.left_u;
        self.phase += 1;
        if self.phase == 3 {
            // after the cyan pass the paper moves on
            self.phase = 0;
            let l = self.e.lead;
            self.e.advance_y(l);
        }
        self.e.color = PASS_INK[self.phase];
    }

    fn escape(&mut self, data: &[u8], i: usize) -> usize {
        let n = data.len();
        if i >= n {
            return i;
        }
        let c = data[i];
        let b = |k: usize| -> usize { data[i + k] as usize };
        if let Some(dpi) = graphics_dpi(c) {
            let cnt = if i + 2 < n { b(1) | b(2) << 8 } else { 0 };
            let body = slice(data, i + 3, i + 3 + cnt).to_vec();
            self.e.emit_columns(&body, 1, dpi, 72.0, -6.0 * 20.0, true, false, false, Some(8), true);
            return i + 3 + cnt;
        }
        match c {
            0x19 => {
                // ESC EM: align the ribbon, back to yellow
                self.phase = 0;
                if self.colour_ribbon {
                    self.e.color = PASS_INK[0];
                }
                if i + 1 < n && data[i + 1] == 0x03 {
                    // GEOS colour band: three passes of doubled 24-dot columns
                    return self.band(data, i + 1);
                }
                i + 1
            }
            0x41 => {
                // ESC A n: n/72 in line spacing
                if i + 1 < n {
                    self.pending_lead = b(1) as f64 * U / 72.0;
                    if self.e.opts.esc_a_immediate {
                        self.e.lead = self.pending_lead;
                    }
                }
                i + 2
            }
            0x33 => {
                if i + 1 < n {
                    self.e.lead = b(1) as f64 * U / 216.0;
                }
                i + 2
            }
            0x4A => {
                if i + 1 < n {
                    self.e.advance_y(b(1) as f64 * U / 216.0);
                }
                i + 2
            }
            0x0A => {
                // ESC LF n: fine line feed, n/144 in (0 = 256)
                if i + 1 < n {
                    let v = if b(1) == 0 { 256 } else { b(1) };
                    self.e.advance_y(v as f64 * U / 144.0);
                }
                i + 2
            }
            0x10 => {
                // ESC DLE n1 n2: absolute dot column
                if i + 2 < n {
                    self.e.x = self.e.left_u.max((b(1) * 256 + b(2)) as f64 * U / 60.0);
                }
                i + 3
            }
            0x25 => {
                // ESC % O n1 n2: 24-dot graphics (density not verified): skipped
                let cnt = if i + 3 < n { (b(2) | b(3) << 8) * 3 } else { 0 };
                self.skipped_bytes += cnt;
                self.e.unknown += 1;
                i + 4 + cnt
            }
            0x32 => {
                if self.pending_lead != 0.0 {
                    self.e.lead = self.pending_lead;
                }
                i + 1
            }
            0x42 | 0x50 | 0x40 => {
                if c == 0x40 {
                    self.reset_ok();
                    if self.colour_ribbon {
                        self.e.color = PASS_INK[0];
                    }
                }
                i + 1
            }
            _ => {
                self.e.unknown += 1;
                i + 1
            }
        }
    }

    /// One pass: 3-byte columns (bits doubled), 03 02, 0x8D. Returns the index after the 0x8D.
    fn pass(&mut self, data: &[u8], i: usize, ink: u8) -> usize {
        let n = data.len();
        let end = data.get(i..).and_then(|s| s.iter().position(|&b| b == 0x8d)).map(|p| p + i).unwrap_or(n);
        let mut body: Vec<u8> = slice(data, i + 1, end).to_vec(); // the 03 that opens a pass is a marker, not data
        if body.ends_with(&[0x03, 0x02]) {
            body.truncate(body.len() - 2);
        }
        // a data byte of 03 is sent twice
        let mut b2: Vec<u8> = Vec::with_capacity(body.len());
        let mut k = 0;
        while k < body.len() {
            if body[k] == 3 && k + 1 < body.len() && body[k + 1] == 3 {
                b2.push(3);
                k += 2;
            } else {
                b2.push(body[k]);
                k += 1;
            }
        }
        let cols = b2.len() / 3;
        if cols > 0 {
            // 24 bits per column, bit 0 first; every second bit is a picture row (12 rows, top first)
            let mut packed: Vec<u8> = Vec::with_capacity(cols * 2);
            for c in 0..cols {
                let bytes = &b2[c * 3..c * 3 + 3];
                let mut rows = [0u8; 16];
                for r in 0..12 {
                    let bit = r * 2; // index within the 24-bit little-endian stream
                    rows[r] = (bytes[bit / 8] >> (bit % 8)) & 1;
                }
                let mut hi = 0u8;
                let mut lo = 0u8;
                for r in 0..8 {
                    hi |= rows[r] << (7 - r);
                }
                for r in 8..16 {
                    lo |= rows[r] << (15 - r);
                }
                packed.push(hi);
                packed.push(lo);
            }
            self.e.color = ink;
            self.e.emit_columns(&packed, 2, BAND_DPI_X, BAND_DPI_Y, 0.0, true, false, false, Some(12), true);
        }
        self.e.end_pass();
        self.e.x = self.e.left_u;
        (end + 1).min(n)
    }

    /// Colour band: three passes (yellow, magenta, cyan).
    fn band(&mut self, data: &[u8], i0: usize) -> usize {
        let mut i = i0;
        for ink in PASS_INK {
            i = self.pass(data, i, ink);
        }
        self.bands += 1;
        self.phase = 0;
        self.e.color = PASS_INK[0];
        i
    }

    pub fn finish(&mut self, n_bytes: usize) -> Document {
        let mut doc = self.e.finish(n_bytes);
        while doc.pages.last().map_or(false, |p| p.n_dots() == 0) {
            doc.pages.pop(); // a blank sheet the form feeds left behind
        }
        if self.skipped_bytes > 0 {
            doc.warnings.push(format!(
                "{} byte(s) of Okimate data in a mode this emulator does not decode (Commodore-style colour data behind ESC EM 3) were skipped",
                thousands(self.skipped_bytes)
            ));
        }
        doc
    }
}
