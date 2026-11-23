//! Commodore MPS-801 / MPS-803 / 1525 emulation (PETSCII text, bit-image graphics).
use super::charsets;
use super::engine::{Engine, PendingGap, U};
use super::model::{EmuOptions, PrinterProfile};

const DOT_U: f64 = 24.0; // 60 dots per inch horizontally
const LEAD_TEXT: f64 = U / 6.0;
const LEAD_GFX: f64 = U / 9.0;

pub struct MpsEngine {
    pub e: Engine,
    lower: bool,
    ascii: bool,
    bitimage: bool,
    line_gfx: bool,
    after_cr: bool,
    /// (x of first column, 7-bit columns) for the graphics line being built
    cur_band: Vec<(f64, Vec<u8>)>,
    band_pass: i32,
}

impl MpsEngine {
    pub fn new(profile: &'static PrinterProfile, opts: &EmuOptions) -> Self {
        let mut e = Engine::new(profile, opts);
        e.lead = LEAD_TEXT;
        Self {
            lower: opts.cbm_charset == "lower",
            ascii: opts.cbm_charset == "ascii",
            e,
            bitimage: false,
            line_gfx: false,
            after_cr: false,
            cur_band: Vec::new(),
            band_pass: 0,
        }
    }

    fn newline(&mut self) {
        self.e.end_pass();
        self.e.x = self.e.left_u;
        let fill = self.e.opts.mps_gap == "fill";
        if fill && self.line_gfx && !self.cur_band.is_empty() {
            let y_abs = self.e.tof_u + self.e.y;
            let consecutive = matches!(&self.e.pending_gap, Some(p) if (y_abs - (p.y_abs + LEAD_GFX)).abs() < 1.0);
            if consecutive {
                self.e.flush_gap(Some(&self.cur_band)); // consecutive lines: blend with the line below
            } else {
                self.e.flush_gap(None);
            }
            self.e.pending_gap = Some(PendingGap { band: self.cur_band.clone(), y_abs, pass: self.band_pass });
        } else if !self.line_gfx {
            self.e.flush_gap(None);
        }
        self.cur_band.clear();
        if !self.e.skip_blank_lf() {
            let gfx_lead = if self.e.opts.mps_gap == "close" { 7.0 * U / 72.0 } else { LEAD_GFX };
            self.e.advance_y(if self.line_gfx { gfx_lead } else { LEAD_TEXT });
        }
        self.line_gfx = false;
        self.e.style.reverse = false;
    }

    fn detect_charset(&mut self, data: &[u8]) {
        if self.e.opts.cbm_charset != "auto" {
            return;
        }
        let lower = data.iter().filter(|&&b| (0x61..=0x7A).contains(&b)).count();
        let upper = data.iter().filter(|&&b| (0x41..=0x5A).contains(&b)).count();
        let hi = data.iter().filter(|&&b| b >= 0x80).count();
        if hi == 0 && lower > 20 && lower > 2 * upper {
            self.ascii = true;
        } else if hi == 0 && lower >= 2 && upper as f64 >= 0.85 * (upper + lower) as f64 && upper + lower >= 30 {
            self.lower = true; // PETSCII lower/upper text: lower-case letters are $41-$5A, capitals $61-$7A
        }
    }

    fn detect_density(&mut self, data: &[u8]) {
        // A bit-image line of more than 480 columns cannot be the 60 dpi MPS-801 format (it would be wider than the
        // paper); the MPS-1000 GEOS driver sends 960 columns (each source dot twice) for the same 8 inch line.
        let (mut best, mut run) = (0usize, 0usize);
        for &b in data {
            if b >= 0x80 {
                run += 1;
                best = best.max(run);
            } else if b != 0 {
                run = 0;
            }
        }
        if best > 490 {
            self.e.gfx_dpi = 120.0;
        }
    }

    fn bit_columns(&mut self, cols: Vec<u8>) {
        self.cur_band.push((self.e.x, cols.clone()));
        self.band_pass = self.e.cur_pass + 1000003;
        let dpi = self.e.gfx_dpi;
        self.e.emit_columns(&cols, 1, dpi, 72.0, -120.0, false, false, false, Some(7), true);
        self.line_gfx = true;
    }

    pub fn run(&mut self, data: &[u8]) {
        self.detect_charset(data);
        self.detect_density(data);
        let n = data.len();
        let mut i = 0usize;
        while i < n {
            let b = data[i];
            if self.bitimage {
                if b >= 0x80 {
                    let mut j = i;
                    while j < n && data[j] >= 0x80 {
                        j += 1;
                    }
                    let cols: Vec<u8> = data[i..j].iter().map(|x| x & 0x7F).collect();
                    self.bit_columns(cols);
                    self.after_cr = false;
                    i = j;
                    continue;
                }
                if b == 26 {
                    let cnt = data.get(i + 1).copied().unwrap_or(1) as usize;
                    let val = data.get(i + 2).copied().unwrap_or(0x80);
                    let cnt = if cnt == 0 { 256 } else { cnt };
                    self.bit_columns(vec![val & 0x7F; cnt]);
                    i += 3;
                    continue;
                }
                if b == 13 {
                    // CR keeps bit-image mode (PrintMaster and others send CHR$(8) once)
                    self.newline();
                    self.line_gfx = true;
                    self.after_cr = true;
                    i += 1;
                    continue;
                }
                if b == 0 || (b == 10 && self.after_cr) {
                    i += 1;
                    continue;
                }
                self.bitimage = false;
            }
            if b == 13 {
                self.newline();
                self.after_cr = true;
                i += 1;
                continue;
            }
            if b == 10 {
                if self.after_cr {
                    self.after_cr = false;
                } else {
                    self.newline();
                }
                i += 1;
                continue;
            }
            self.after_cr = false;
            match b {
                8 => {
                    self.bitimage = true;
                    self.line_gfx = true;
                }
                14 => self.e.style.wmul = 2,
                15 => self.e.style.wmul = 1,
                16 => {
                    if i + 2 < n && (48..=57).contains(&data[i + 1]) && (48..=57).contains(&data[i + 2]) {
                        let pos = (data[i + 1] - 48) as f64 * 10.0 + (data[i + 2] - 48) as f64;
                        self.e.x = pos * (U / 10.0);
                        i += 2;
                    }
                }
                17 => self.lower = true,
                145 => self.lower = false,
                18 => self.e.style.reverse = true,
                146 => self.e.style.reverse = false,
                12 => self.e.form_feed(),
                27 => {
                    if i + 1 < n && data[i + 1] == 16 && i + 3 < n {
                        self.e.x = ((((data[i + 2] & 1) as u32) << 8) | data[i + 3] as u32) as f64 * DOT_U;
                        i += 3;
                    } else if i + 2 < n && data[i + 1] == 45 {
                        self.e.style.underline = data[i + 2] & 1 != 0;
                        i += 2;
                    } else {
                        self.e.unknown += 1;
                        i += 1;
                    }
                }
                _ => {
                    let ch = if self.ascii {
                        if (0x20..0x7F).contains(&b) {
                            Some(b as char)
                        } else {
                            None
                        }
                    } else {
                        charsets::petscii_char(b, self.lower)
                    };
                    if let Some(c) = ch {
                        self.e.print_char(c);
                    }
                }
            }
            i += 1;
        }
    }
}
