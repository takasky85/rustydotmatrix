//! A small Epson ESC/P (9-pin) interpreter.
//!
//! Raw printer bytes go in, pages full of individual dots come out. This is a
//! proof-of-concept subset: enough to render plain text, the common text
//! attributes and the ESC K/L/Y/Z/* bit-image modes.
//!
//! Coordinates:
//! * `x` is in 1/240 inch (the finest horizontal resolution of the 9-pin heads)
//! * `y` is in 1/216 inch (the ESC/P vertical unit; one pin pitch is 1/72" = 3)

use crate::font_data::{FIRST, GLYPHS, GLYPH_COLS, LAST};

pub const PAGE_W: i32 = 2040; // 8.5"
pub const PAGE_H: i32 = 2376; // 11"
pub const X_PER_INCH: f32 = 240.0;
pub const Y_PER_INCH: f32 = 216.0;
pub const PAGE_W_IN: f32 = PAGE_W as f32 / X_PER_INCH;
pub const PAGE_H_IN: f32 = PAGE_H as f32 / Y_PER_INCH;

/// Distance between two printhead pins, 1/72".
pub const PIN_PITCH: i32 = 3;
/// Rows in one text glyph (one per pin of the head).
const GLYPH_ROWS: i32 = 9;

const LEFT_MARGIN: i32 = 60; // 0.25"
const RIGHT_MARGIN: i32 = 60;
const TOP_MARGIN: i32 = 54; // 0.25"
const BOTTOM_MARGIN: i32 = 54;
const DEFAULT_LINE_SPACING: i32 = 36; // 1/6"

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dot {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub dots: Vec<Dot>,
}

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub pages: Vec<Page>,
}

impl Document {
    pub fn dot_count(&self) -> usize {
        self.pages.iter().map(|p| p.dots.len()).sum()
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Treat LF as CR+LF. Most files from Unix-like systems need this.
    pub auto_cr_on_lf: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { auto_cr_on_lf: true }
    }
}

struct Interp {
    opts: Options,
    pages: Vec<Page>,
    cur: Page,

    x: i32,
    y: i32,
    left: i32,
    line_spacing: i32,
    esc_a_spacing: i32,
    /// Width of one character cell at the base pitch, in 1/240".
    base_cell: i32,
    condensed: bool,
    double_width: bool,
    double_width_line: bool,
    emphasized: bool,
    double_strike: bool,
    underline: bool,
}

/// Interpret a raw ESC/P byte stream.
pub fn interpret(data: &[u8], opts: &Options) -> Document {
    let mut it = Interp::new(opts.clone());
    it.run(data);
    it.finish()
}

impl Interp {
    fn new(opts: Options) -> Self {
        Self {
            opts,
            pages: Vec::new(),
            cur: Page::default(),
            x: LEFT_MARGIN,
            y: TOP_MARGIN,
            left: LEFT_MARGIN,
            line_spacing: DEFAULT_LINE_SPACING,
            esc_a_spacing: DEFAULT_LINE_SPACING,
            base_cell: 24,
            condensed: false,
            double_width: false,
            double_width_line: false,
            emphasized: false,
            double_strike: false,
            underline: false,
        }
    }

    /// ESC @
    fn reset(&mut self) {
        self.left = LEFT_MARGIN;
        self.x = self.left;
        self.line_spacing = DEFAULT_LINE_SPACING;
        self.esc_a_spacing = DEFAULT_LINE_SPACING;
        self.base_cell = 24;
        self.condensed = false;
        self.double_width = false;
        self.double_width_line = false;
        self.emphasized = false;
        self.double_strike = false;
        self.underline = false;
    }

    fn finish(mut self) -> Document {
        if !self.cur.dots.is_empty() || self.pages.is_empty() {
            self.pages.push(std::mem::take(&mut self.cur));
        }
        Document { pages: self.pages }
    }

    fn cell(&self) -> i32 {
        let mut c = self.base_cell;
        if self.condensed {
            c = c * 7 / 12;
        }
        if self.double_width || self.double_width_line {
            c *= 2;
        }
        c.max(2)
    }

    fn put(&mut self, x: i32, y: i32) {
        if (0..PAGE_W).contains(&x) && (0..PAGE_H).contains(&y) {
            self.cur.dots.push(Dot { x, y });
        }
    }

    fn new_page(&mut self) {
        self.pages.push(std::mem::take(&mut self.cur));
        self.y = TOP_MARGIN;
        self.x = self.left;
    }

    fn cr(&mut self) {
        self.x = self.left;
    }

    fn lf(&mut self) {
        self.double_width_line = false;
        self.y += self.line_spacing;
        if self.opts.auto_cr_on_lf {
            self.cr();
        }
        if self.y + GLYPH_ROWS * PIN_PITCH > PAGE_H - BOTTOM_MARGIN {
            self.new_page();
        }
    }

    fn ff(&mut self) {
        self.double_width_line = false;
        self.new_page();
    }

    /// Print one strike of a dot, plus the extra strikes for the
    /// emphasized / double-strike modes.
    fn strike(&mut self, x: i32, y: i32) {
        self.put(x, y);
        if self.emphasized {
            self.put(x + 2, y); // shifted by 1/120"
        }
        if self.double_strike {
            self.put(x, y + 1); // shifted by 1/216"
            if self.emphasized {
                self.put(x + 2, y + 1);
            }
        }
    }

    fn print_char(&mut self, code: u8) {
        let cell = self.cell();
        if self.x + cell > PAGE_W - RIGHT_MARGIN {
            self.cr();
            self.lf();
            if !self.opts.auto_cr_on_lf {
                self.cr();
            }
        }
        // In double-width modes every printed column is struck twice, side by
        // side, so glyphs stay solid instead of falling apart into sparse dots.
        let sub: i32 = if self.double_width || self.double_width_line { 2 } else { 1 };
        let steps = GLYPH_COLS as i32 * sub;
        if (FIRST..=LAST).contains(&code) {
            let glyph = &GLYPHS[(code - FIRST) as usize];
            for (c, &mask) in glyph.iter().enumerate().take(GLYPH_COLS) {
                for k in 0..sub {
                    let gx = self.x + (c as i32 * sub + k) * cell / steps;
                    for r in 0..GLYPH_ROWS {
                        if mask & (1 << r) != 0 {
                            self.strike(gx, self.y + r * PIN_PITCH);
                        }
                    }
                }
            }
        }
        if self.underline {
            for c in 0..steps {
                let gx = self.x + c * cell / steps;
                self.strike(gx, self.y + (GLYPH_ROWS - 1) * PIN_PITCH + 2);
            }
        }
        self.x += cell;
    }

    fn run(&mut self, data: &[u8]) {
        let mut i = 0usize;
        while i < data.len() {
            let b = data[i];
            i += 1;
            match b {
                0x08 => {
                    // BS
                    let cell = self.cell();
                    self.x = (self.x - cell).max(self.left);
                }
                0x09 => {
                    // HT: next multiple of 8 columns
                    let cell = self.cell();
                    let col = (self.x - self.left) / cell;
                    self.x = self.left + (col / 8 + 1) * 8 * cell;
                }
                0x0A => self.lf(),
                0x0B => {} // VT: ignored
                0x0C => self.ff(),
                0x0D => self.cr(),
                0x0E => self.double_width_line = true, // SO
                0x0F => self.condensed = true,         // SI
                0x12 => self.condensed = false,        // DC2
                0x14 => self.double_width_line = false, // DC4
                0x1B => self.escape(data, &mut i),
                0x20..=0x7E => self.print_char(b),
                0x80..=0xFF => {
                    // Upper half of the code page is not mapped yet: blank cell.
                    let cell = self.cell();
                    self.x += cell;
                }
                _ => {} // other control codes are ignored
            }
        }
    }

    fn escape(&mut self, d: &[u8], i: &mut usize) {
        let Some(cmd) = take(d, i) else { return };
        match cmd {
            b'@' => self.reset(),
            b'E' => self.emphasized = true,
            b'F' => self.emphasized = false,
            b'G' => self.double_strike = true,
            b'H' => self.double_strike = false,
            b'-' => {
                let n = take(d, i).unwrap_or(0);
                self.underline = n == 1 || n == b'1';
            }
            b'W' => {
                let n = take(d, i).unwrap_or(0);
                self.double_width = n == 1 || n == b'1';
            }
            b'P' => self.base_cell = 24, // 10 cpi
            b'M' => self.base_cell = 20, // 12 cpi
            b'g' => self.base_cell = 16, // 15 cpi
            b'0' => self.line_spacing = 27, // 1/8"
            b'1' => self.line_spacing = 21, // 7/72"
            b'2' => self.line_spacing = self.esc_a_spacing,
            b'3' => {
                let n = take(d, i).unwrap_or(0) as i32;
                self.line_spacing = n.max(1);
            }
            b'A' => {
                let n = take(d, i).unwrap_or(0) as i32;
                self.esc_a_spacing = (n * PIN_PITCH).max(1);
            }
            b'J' => {
                let n = take(d, i).unwrap_or(0) as i32;
                self.y += n;
                if self.y + GLYPH_ROWS * PIN_PITCH > PAGE_H - BOTTOM_MARGIN {
                    self.new_page();
                }
            }
            b'$' => {
                // absolute horizontal position in 1/60" from the left margin
                let n = take_u16(d, i) as i32;
                self.x = self.left + n * 4;
            }
            b'l' => {
                let n = take(d, i).unwrap_or(0) as i32;
                self.left = LEFT_MARGIN + n * self.cell();
                self.x = self.left;
            }
            b'K' => self.bit_image(d, i, 4, 1, false),
            b'L' => self.bit_image(d, i, 2, 1, false),
            b'Y' => self.bit_image(d, i, 2, 1, false),
            b'Z' => self.bit_image(d, i, 1, 1, false),
            b'*' => {
                let m = take(d, i).unwrap_or(0);
                // (x step numerator, denominator, 24-pin?)
                let (num, den, wide) = match m {
                    0 => (4, 1, false),  // 60 dpi
                    1 | 2 => (2, 1, false), // 120 dpi
                    3 => (1, 1, false),  // 240 dpi
                    4 => (3, 1, false),  // 80 dpi
                    6 => (8, 3, false),  // 90 dpi
                    32 => (4, 1, true),  // 60 dpi, 24-pin
                    33 => (2, 1, true),  // 120 dpi, 24-pin
                    38 => (4, 3, true),  // 180 dpi, 24-pin
                    39 => (2, 3, true),  // 360 dpi, 24-pin
                    _ => (2, 1, false),
                };
                self.bit_image(d, i, num, den, wide);
            }
            b'(' => {
                // ESC ( <cmd> nL nH <data...>: skip the payload
                let _cmd = take(d, i);
                let n = take_u16(d, i) as usize;
                *i = (*i + n).min(d.len());
            }
            b'B' | b'D' | b'b' => {
                // tab / VFU lists terminated by NUL
                while let Some(v) = take(d, i) {
                    if v == 0 {
                        break;
                    }
                }
            }
            // Commands with one argument byte that we accept but ignore.
            b'!' | b'Q' | b'N' | b'a' | b'S' | b'w' | b'x' | b't' | b'R' | b'U' | b'i'
            | b'm' | b'p' | b'r' | b's' | b'k' | b'j' | b'/' | b'%' | b'C' => {
                let n = take(d, i);
                if cmd == b'C' && n == Some(0) {
                    let _ = take(d, i); // ESC C NUL n (page length in inches)
                }
            }
            b'e' | b'f' | b'?' => {
                let _ = take(d, i);
                let _ = take(d, i);
            }
            _ => {} // anything else: no arguments assumed
        }
    }

    /// Print `n` columns of bit-image data.
    ///
    /// * 8-pin modes: one byte per column, MSB = top pin, 1/72" pin pitch.
    /// * 24-pin modes: three bytes per column, 1/180" pin pitch.
    fn bit_image(&mut self, d: &[u8], i: &mut usize, num: i32, den: i32, wide: bool) {
        let n = take_u16(d, i) as usize;
        let bytes_per_col = if wide { 3 } else { 1 };
        let start_x = self.x;
        let mut col = 0usize;
        while col < n && *i < d.len() {
            let end = (*i + bytes_per_col).min(d.len());
            let chunk = &d[*i..end];
            *i = end;
            let gx = start_x + (col as i32) * num / den;
            for (bi, &byte) in chunk.iter().enumerate() {
                for bit in 0..8i32 {
                    if byte & (0x80 >> bit) != 0 {
                        let row = bi as i32 * 8 + bit;
                        let gy = if wide {
                            self.y + row * 6 / 5
                        } else {
                            self.y + row * PIN_PITCH
                        };
                        self.strike(gx, gy);
                    }
                }
            }
            col += 1;
        }
        self.x = start_x + (n as i32) * num / den;
    }
}

fn take(d: &[u8], i: &mut usize) -> Option<u8> {
    let v = d.get(*i).copied();
    if v.is_some() {
        *i += 1;
    }
    v
}

fn take_u16(d: &[u8], i: &mut usize) -> u16 {
    let lo = take(d, i).unwrap_or(0) as u16;
    let hi = take(d, i).unwrap_or(0) as u16;
    lo | (hi << 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(bytes: &[u8]) -> Document {
        interpret(bytes, &Options::default())
    }

    #[test]
    fn empty_input_gives_one_blank_page() {
        let doc = run(b"");
        assert_eq!(doc.pages.len(), 1);
        assert_eq!(doc.dot_count(), 0);
    }

    #[test]
    fn text_produces_dots_inside_the_page() {
        let doc = run(b"Hello, world\r\n");
        assert!(doc.dot_count() > 50);
        for d in &doc.pages[0].dots {
            assert!((0..PAGE_W).contains(&d.x) && (0..PAGE_H).contains(&d.y));
        }
    }

    #[test]
    fn form_feed_starts_a_new_page_and_trailing_ff_adds_none() {
        let doc = run(b"A\x0cB");
        assert_eq!(doc.pages.len(), 2);
        let doc = run(b"A\x0c");
        assert_eq!(doc.pages.len(), 1);
    }

    #[test]
    fn long_text_flows_onto_more_pages() {
        let mut data = Vec::new();
        for _ in 0..200 {
            data.extend_from_slice(b"line\r\n");
        }
        assert!(run(&data).pages.len() >= 3);
    }

    #[test]
    fn esc_k_bit_image_places_one_dot_per_set_bit() {
        // 3 columns: 0x80 (top pin), 0x01 (bottom pin of 8), 0xFF (all 8)
        let doc = run(&[0x1B, b'K', 3, 0, 0x80, 0x01, 0xFF]);
        assert_eq!(doc.dot_count(), 1 + 1 + 8);
    }

    #[test]
    fn truncated_graphics_does_not_panic() {
        let doc = run(&[0x1B, b'K', 0xFF, 0xFF, 0xFF]);
        assert_eq!(doc.dot_count(), 8);
        let _ = run(&[0x1B]);
        let _ = run(&[0x1B, b'*']);
        let _ = run(&[0x1B, b'(', b'U', 0xFF, 0xFF]);
    }

    #[test]
    fn emphasized_prints_more_dots() {
        let plain = run(b"A").dot_count();
        let bold = run(b"\x1bEA").dot_count();
        assert!(bold > plain);
    }

    #[test]
    fn reset_clears_attributes() {
        let plain = run(b"A").dot_count();
        let after_reset = run(b"\x1bE\x1bGq\x1b@A").dot_count() - run(b"\x1bE\x1bGq").dot_count();
        assert_eq!(after_reset, plain);
    }

    #[test]
    fn all_bytes_never_panic() {
        let all: Vec<u8> = (0..=255u8).collect();
        let _ = run(&all);
        let mut stress = Vec::new();
        for b in 0..=255u8 {
            stress.extend_from_slice(&[0x1B, b, 1, 2, 3]);
        }
        let _ = run(&stress);
    }

    #[test]
    fn lf_without_auto_cr_staircases() {
        let opts = Options { auto_cr_on_lf: false };
        let a = interpret(b"A\nA", &opts);
        let b = interpret(b"A\nA", &Options::default());
        let max_x = |d: &Document| d.pages[0].dots.iter().map(|p| p.x).max().unwrap();
        assert!(max_x(&a) > max_x(&b));
    }
}
