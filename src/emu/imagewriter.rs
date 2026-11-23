//! Apple ImageWriter I/II emulation (Apple II, Macintosh drivers, Basilisk II / SheepShaver dumps).
use super::charsets;
use super::engine::{Engine, U};
use super::glyphs::Face;
use super::model::{EmuOptions, PrinterProfile};

/// ESC K n: ImageWriter II ribbon panel -> ink (0 black, 1 yellow, 2 red = magenta ink, 3 blue = cyan ink)
fn ribbon_color(c: i64) -> u8 {
    match c {
        1 => 4,
        2 => 1,
        3 => 2,
        _ => 0,
    }
}

/// ESC letter -> (characters per inch, graphics dots per inch, proportional)
fn pitch(c: char) -> Option<(f64, f64, bool)> {
    Some(match c {
        'n' => (9.0, 72.0, false),
        'N' => (10.0, 80.0, false),
        'E' => (12.0, 96.0, false),
        'e' => (13.4, 107.2, false),
        'q' => (15.0, 120.0, false),
        'Q' => (17.16, 136.0, false),
        'p' => (10.0, 144.0, true),
        'P' => (10.0, 160.0, true),
        _ => return None,
    })
}

/// Where a graphics row that is shorter than announced really ends: the next ribbon-colour / header / line-end commands
/// (`\r\x1bK[0-4]`, `\r\n\r\x1b`, `\x1b>\x1bP\x1bG\d`). Bitmap bytes essentially never contain these sequences.
/// Returns the start of the leftmost match lying fully inside `d[lo..hi]`.
fn resync_search(d: &[u8], lo: usize, hi: usize) -> Option<usize> {
    let hi = hi.min(d.len());
    let mut p = lo;
    while p < hi {
        if d[p] == b'\r' {
            if p + 4 <= hi && d[p + 1] == 0x1b && d[p + 2] == b'K' && (b'0'..=b'4').contains(&d[p + 3]) {
                return Some(p);
            }
            if p + 4 <= hi && d[p + 1] == b'\n' && d[p + 2] == b'\r' && d[p + 3] == 0x1b {
                return Some(p);
            }
        } else if d[p] == 0x1b
            && p + 7 <= hi
            && d[p + 1] == b'>'
            && d[p + 2] == 0x1b
            && d[p + 3] == b'P'
            && d[p + 4] == 0x1b
            && d[p + 5] == b'G'
            && d[p + 6].is_ascii_digit()
        {
            return Some(p);
        }
        p += 1;
    }
    None
}

pub struct ImageWriterEngine {
    pub e: Engine,
    gdpi: f64,
    /// 0 standard, 1 draft, 2 NLQ
    quality: u8,
    bold: bool,
    pending_cr: bool,
    /// Graphics rows that ended before the announced byte count.
    pub short_rows: usize,
}

impl ImageWriterEngine {
    pub fn new(profile: &'static PrinterProfile, opts: &EmuOptions) -> Self {
        let mut s = Self { e: Engine::new(profile, opts), gdpi: 80.0, quality: 0, bold: false, pending_cr: false, short_rows: 0 };
        s.e.style.face = Face::Sans;
        s.apply_style();
        s
    }

    fn reset_iw(&mut self) {
        self.e.reset_state();
        self.gdpi = 80.0;
        self.quality = 0;
        self.bold = false;
    }

    fn apply_style(&mut self) {
        let st = &mut self.e.style;
        st.nlq = self.quality == 2;
        match self.quality {
            2 => {
                st.emph = self.bold;
                st.dstrike = false;
            }
            0 => {
                st.emph = true;
                st.dstrike = self.bold;
            }
            _ => {
                st.emph = self.bold;
                st.dstrike = false;
            }
        }
    }

    fn digits(&self, d: &[u8], i: usize, count: usize) -> (i64, usize) {
        let mut v = 0i64;
        for k in 0..count {
            let c = d.get(i + k).copied().unwrap_or(48);
            v = v * 10 + if (48..=57).contains(&c) { (c - 48) as i64 } else { 0 };
        }
        (v, i + count)
    }

    pub fn run(&mut self, data: &[u8]) {
        let n = data.len();
        let mut i = 0usize;
        while i < n {
            let b = data[i];
            if b >= 0x20 && b != 0x7F {
                let cp = self.e.codepage.clone();
                if let Some(ch) = charsets::decode(b, &cp, 0, false) {
                    self.e.print_char(ch);
                }
                i += 1;
            } else if b == 0x1B {
                i = self.escape(data, i + 1);
            } else if b == 0x09 {
                // Apple II parallel card firmware command: Ctrl-I [number] letter (e.g. Ctrl-I Z); not for the printer
                let mut j = i + 1;
                while j < n && (48..=57).contains(&data[j]) {
                    j += 1;
                }
                i = if j < n && (65..=90).contains(&data[j]) { j + 1 } else { i + 1 };
            } else {
                match b {
                    0x08 => self.e.backspace(),
                    0x0A => self.e.lf(false),
                    0x0C => self.e.form_feed(),
                    0x0D => self.e.cr(),
                    _ => {}
                }
                i += 1;
            }
        }
    }

    fn escape(&mut self, data: &[u8], i0: usize) -> usize {
        let n = data.len();
        let mut i = i0;
        if i >= n {
            return i;
        }
        let ch = data[i] as char;
        i += 1;
        if self.pending_cr {
            // see take(): a lost carriage return, put back before the next command
            self.pending_cr = false;
            self.e.cr();
        }
        if let Some((cpi, gdpi, prop)) = pitch(ch) {
            self.e.style.base_cpi = cpi;
            self.e.style.prop = prop;
            self.gdpi = gdpi;
            self.e.style.condensed = false;
            return i;
        }
        match ch {
            'A' => {
                self.e.lead = U / 6.0;
                return i;
            }
            'B' => {
                self.e.lead = U / 8.0;
                return i;
            }
            'T' => {
                let (v, j) = self.digits(data, i, 2);
                self.e.lead = v as f64 * U / 144.0;
                return j;
            }
            'L' => {
                let (v, j) = self.digits(data, i, 3);
                let cpi = if !self.e.style.prop { self.e.style.eff_cpi() } else { 10.0 };
                self.e.left_u = v as f64 * (U / cpi);
                self.e.x = self.e.x.max(self.e.left_u);
                return j;
            }
            'H' => {
                let (v, j) = self.digits(data, i, 4);
                if self.e.opts.honor_page_length && v > 0 {
                    self.e.page_len = if v >= 200 { v as f64 * U / 144.0 } else { v as f64 * self.e.lead };
                }
                return j;
            }
            'f' => {
                let l = self.e.lead;
                self.e.advance_y(l);
                return i;
            }
            'r' => {
                self.e.y = (self.e.y - self.e.lead).max(0.0);
                return i;
            }
            'v' => return i,
            'c' => {
                let keep = (self.e.cur_pass, self.e.dirty);
                self.reset_iw();
                self.e.cur_pass = keep.0;
                self.e.dirty = keep.1;
                self.apply_style();
                return i;
            }
            '<' | '>' => return i,
            '!' => {
                self.bold = true;
                self.apply_style();
                return i;
            }
            '"' => {
                self.bold = false;
                self.apply_style();
                return i;
            }
            'X' => {
                self.e.style.underline = true;
                return i;
            }
            'Y' => {
                self.e.style.underline = false;
                return i;
            }
            'x' => {
                self.e.style.script = 1;
                return i;
            }
            'y' => {
                self.e.style.script = 2;
                return i;
            }
            'z' => {
                self.e.style.script = 0;
                return i;
            }
            'w' => {
                self.e.style.vscale = 0.55;
                return i;
            }
            'W' => {
                self.e.style.vscale = 1.0;
                return i;
            }
            'a' => {
                let c = data.get(i).copied().unwrap_or(48);
                self.quality = match c {
                    49 => 1,
                    50 => 2,
                    _ => 0,
                };
                self.apply_style();
                return i + 1;
            }
            'K' => {
                // colour ribbon select, as an ASCII digit (a raw 0-4 is accepted too)
                let mut c = data.get(i).copied().unwrap_or(48) as i64;
                if (48..=57).contains(&c) {
                    c -= 48;
                }
                self.e.color = ribbon_color(c);
                return i + 1;
            }
            's' => return i + 1,
            'G' | 'S' => {
                let (cnt, j) = self.digits(data, i, 4);
                let (body, end) = self.take(data, j, cnt as usize);
                self.gfx(&body);
                return end;
            }
            'g' => {
                let (cnt, j) = self.digits(data, i, 3);
                let (body, end) = self.take(data, j, cnt as usize * 8);
                self.gfx(&body);
                return end;
            }
            'V' => {
                let (cnt, j) = self.digits(data, i, 4);
                let val = data.get(j).copied().unwrap_or(0);
                self.gfx(&vec![val; cnt as usize]);
                return j + 1;
            }
            'F' => {
                let (v, j) = self.digits(data, i, 4);
                self.e.x = v as f64 * U / self.gdpi;
                return j;
            }
            'R' => {
                let (cnt, j) = self.digits(data, i, 3);
                let c = data.get(j).copied().unwrap_or(32);
                let cp = self.e.codepage.clone();
                if let Some(chx) = charsets::decode(c, &cp, 0, false) {
                    for _ in 0..cnt {
                        self.e.print_char(chx);
                    }
                }
                return j + 1;
            }
            _ => {}
        }
        self.e.unknown += 1;
        i
    }

    /// The graphics bytes announced by a header: (bytes, index after them). If the next commands show up before `cnt`
    /// bytes have gone by, the capture lost data: the row ends there and parsing carries on at those commands.
    fn take(&mut self, data: &[u8], j: usize, cnt: usize) -> (Vec<u8>, usize) {
        let mut end = (j + cnt).min(data.len());
        if let Some(m) = resync_search(data, j, end + 6) {
            if m < j + cnt {
                self.short_rows += 1;
                end = m;
                // the row was cut short, and the carriage return that ends it was usually lost with the missing bytes:
                // without it the next colour pass would start where this one stopped, out to the right of the page
                self.pending_cr = data.get(end) != Some(&b'\r');
            }
        }
        let j = j.min(data.len());
        (data[j..end.max(j)].to_vec(), end)
    }

    fn gfx(&mut self, body: &[u8]) {
        if !body.is_empty() {
            self.e.emit_columns(body, 1, self.gdpi, 72.0, -6.0 * 20.0, false, false, false, Some(8), true);
        }
    }
}
