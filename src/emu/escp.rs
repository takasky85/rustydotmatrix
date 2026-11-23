//! Epson ESC/P (FX, LQ), IBM Proprinter and Star (Epson mode) emulation.

use super::charsets;
use super::engine::{Chunk, Engine, U};
use super::glyphs::Face;
use super::model::{EmuOptions, Family, PrinterProfile};

/// ESC * modes: (dpi_x, bytes_per_col, no_adjacent)
fn gfx_mode(m: i32) -> Option<(f64, usize, bool)> {
    Some(match m {
        0 => (60.0, 1, false),
        1 => (120.0, 1, false),
        2 => (120.0, 1, true),
        3 => (240.0, 1, true),
        4 => (80.0, 1, false),
        5 => (72.0, 1, false),
        6 => (90.0, 1, false),
        7 => (144.0, 1, false),
        32 => (60.0, 3, false),
        33 => (120.0, 3, false),
        38 => (90.0, 3, false),
        39 => (180.0, 3, false),
        40 => (360.0, 3, true),
        71 => (180.0, 6, false),
        72 => (360.0, 6, true),
        73 => (360.0, 6, true),
        _ => return None,
    })
}

/// Commands whose only job is to swallow their fixed-length parameters.
fn skip_params(c: char) -> Option<usize> {
    Some(match c {
        'U' | 's' | 'i' | 'm' | 'a' | 'I' | 'q' | 'u' | '%' | 'y' | 'z' | 'd' | 'h' => 1,
        'e' | 'f' => 2,
        'X' => 3,
        '=' | '>' | '<' | '#' | '8' | '9' | 'O' | 'c' | 'o' | 'n' | 'v' => 0,
        _ => return None,
    })
}

pub struct EscpEngine {
    pub e: Engine,
    variant: Family,
    /// Graphics mode of ESC K, L, Y, Z.
    kly: [i32; 4],
}

/// `data[i + k]` or 0 past the end (Python helper `u8(k)`).
fn ub(d: &[u8], i: usize) -> u8 {
    d.get(i).copied().unwrap_or(0)
}

/// `data[a:b]` with Python slicing semantics (clamped).
pub fn slice(d: &[u8], a: usize, b: usize) -> &[u8] {
    let a = a.min(d.len());
    let b = b.min(d.len()).max(a);
    &d[a..b]
}

impl EscpEngine {
    pub fn new(profile: &'static PrinterProfile, opts: &EmuOptions) -> Self {
        let mut e = Engine::new(profile, opts);
        if profile.lq24 {
            e.codepage = if opts.codepage != "auto" { opts.codepage.clone() } else { "cp437".into() };
        }
        Self { e, variant: profile.family, kly: [0, 1, 2, 3] }
    }

    fn unit_v(&self) -> f64 {
        self.e.profile.unit_v as f64
    }

    fn cell(&self) -> f64 {
        U / if !self.e.style.prop { self.e.style.eff_cpi() } else { 10.0 }
    }

    /// (dpi_y, top_off) for a graphics column of this many bytes on this printer.
    fn gfx_geometry(&self, bytes_per_col: usize) -> (f64, f64) {
        if self.e.profile.lq24 {
            if bytes_per_col >= 3 {
                (180.0, -18.0 * 8.0)
            } else {
                (60.0, -18.0 * 8.0)
            }
        } else {
            (72.0, -6.0 * 20.0)
        }
    }

    pub fn run(&mut self, data: &[u8]) {
        let n = data.len();
        let mut i = 0usize;
        while i < n {
            let b = data[i];
            if b >= 0x20 && b != 0x7F {
                let cp = self.e.codepage.clone();
                if let Some(ch) = charsets::decode(b, &cp, self.e.national, false) {
                    self.e.print_char(ch);
                }
                i += 1;
                continue;
            }
            if b == 0x1B {
                i = self.escape(data, i + 1);
                continue;
            }
            if b < 0x20 && self.e.ibm_set2 && matches!(b, 1 | 2 | 3 | 4 | 5 | 6 | 0x15 | 0x16 | 0x17 | 0x19 | 0x1A | 0x1C | 0x1D | 0x1E | 0x1F) {
                if let Some(ch) = charsets::cp437_low(b) {
                    self.e.print_char(ch);
                }
                i += 1;
                continue;
            }
            self.control(b);
            i += 1;
        }
    }

    fn control(&mut self, b: u8) {
        match b {
            0x08 => self.e.backspace(),
            0x09 => self.e.x = self.e.next_hstop(),
            0x0A => self.e.lf(false),
            0x0B => self.e.vtab(),
            0x0C => self.e.form_feed(),
            0x0D => self.e.cr(),
            0x0E => self.e.style.wmul_so = true,
            0x0F => self.e.style.condensed = true,
            0x12 => self.e.style.condensed = false,
            0x14 => self.e.style.wmul_so = false,
            _ => {} // BEL, DC1, DC3, CAN, DEL and the rest are ignored
        }
    }

    fn print_cp437(&mut self, b: u8) {
        let mut cc = charsets::decode(b, "cp437", 0, true);
        if cc.is_none() && b < 32 {
            cc = charsets::cp437_low(b);
        }
        if let Some(c) = cc {
            self.e.print_char(c);
        }
    }

    // ---------------------------------------------------------------- ESC
    fn escape(&mut self, data: &[u8], i0: usize) -> usize {
        let n = data.len();
        let mut i = i0;
        if i >= n {
            return i;
        }
        let c = data[i];
        i += 1;
        let ch = c as char;
        let ibm = self.variant == Family::Ibm;
        let u = |k: usize| -> u8 { ub(data, i + k) };
        let flag = |v: u8| -> bool { v & 1 != 0 || v == 49 };

        // ---- reset & line spacing
        match ch {
            '@' => {
                let keep = (self.e.cur_pass, self.e.dirty);
                self.e.reset_state();
                self.e.cur_pass = keep.0;
                self.e.dirty = keep.1;
                return i;
            }
            '0' => {
                self.e.lead = U / 8.0;
                return i;
            }
            '1' => {
                self.e.lead = 7.0 * U / 72.0;
                return i;
            }
            '2' => {
                self.e.lead = match self.e.lead_custom {
                    Some(v) if v != 0.0 => v,
                    _ => U / 6.0,
                };
                return i;
            }
            '3' => {
                self.e.lead = u(0) as f64 * U / self.unit_v();
                return i + 1;
            }
            'A' => {
                let v = u(0) as f64 * U / 72.0;
                self.e.lead_custom = Some(v);
                if self.e.opts.esc_a_immediate {
                    // Star/Okidata/IBM-style; strict Epson waits for ESC 2
                    self.e.lead = v;
                }
                return i + 1;
            }
            'J' => {
                let dy = u(0) as f64 * U / self.unit_v();
                self.e.advance_y(dy);
                return i + 1;
            }
            'j' => {
                self.e.y = (self.e.y - u(0) as f64 * U / self.unit_v()).max(0.0);
                return i + 1;
            }
            '+' => {
                let dy = u(0) as f64 * U / 360.0;
                self.e.advance_y(dy);
                return i + 1;
            }
            _ => {}
        }
        // ---- page
        match ch {
            'C' => {
                if u(0) == 0 {
                    if self.e.opts.honor_page_length && u(1) > 0 {
                        self.e.page_len = u(1) as f64 * U;
                    }
                    return i + 2;
                }
                if self.e.opts.honor_page_length {
                    self.e.page_len = u(0) as f64 * self.e.lead;
                }
                return i + 1;
            }
            'N' => {
                self.e.bottom_margin = u(0) as f64 * self.e.lead;
                return i + 1;
            }
            'O' => {
                self.e.bottom_margin = 0.0;
                return i;
            }
            'l' => {
                self.e.left_u = u(0) as f64 * self.cell();
                self.e.x = self.e.x.max(self.e.left_u);
                return i + 1;
            }
            'Q' => {
                let v = u(0) as f64 * self.cell();
                if v > self.e.left_u {
                    self.e.right_u = v;
                }
                return i + 1;
            }
            // ---- tabs
            'D' => {
                let mut stops = Vec::new();
                let mut k = i;
                let mut last = 0u8;
                while k < n && stops.len() < 32 {
                    let v = data[k];
                    k += 1;
                    if v == 0 || v <= last {
                        break;
                    }
                    stops.push(v as f64 * self.cell());
                    last = v;
                }
                self.e.hstops = stops;
                return k;
            }
            'B' => {
                let mut stops = Vec::new();
                let mut k = i;
                let mut last = 0u8;
                while k < n && stops.len() < 16 {
                    let v = data[k];
                    k += 1;
                    if v == 0 || v <= last {
                        break;
                    }
                    stops.push(v as f64 * self.e.lead);
                    last = v;
                }
                self.e.vstops = stops;
                return k;
            }
            'b' => {
                let mut k = i + 1;
                while k < n && data[k] != 0 {
                    k += 1;
                }
                return k + 1;
            }
            '/' => return i + 1,
            // ---- absolute / relative horizontal position
            '$' => {
                let v = u(0) as i64 + 256 * u(1) as i64;
                self.e.x = v as f64 * U / 60.0;
                return i + 2;
            }
            '\\' => {
                let mut v = u(0) as i64 + 256 * u(1) as i64;
                if v >= 32768 {
                    v -= 65536;
                }
                if ibm {
                    // IBM: ESC \ n1 n2 prints the next n bytes as CP437 graphics
                    let cnt = v;
                    for k in 0..cnt.max(0) as usize {
                        if i + 2 + k < n {
                            self.print_cp437(data[i + 2 + k]);
                        }
                    }
                    return i + 2 + cnt.max(0) as usize;
                }
                let unit = if self.e.profile.lq24 { 8.0 } else { 12.0 };
                self.e.x = (self.e.x + v as f64 * unit).max(0.0);
                return i + 2;
            }
            ' ' => {
                let unit = if self.e.profile.lq24 { 8.0 } else { 12.0 };
                self.e.extra_space_u = u(0) as f64 * unit;
                return i + 1;
            }
            _ => {}
        }
        // ---- attributes
        {
            let st = &mut self.e.style;
            match ch {
                'E' => {
                    st.emph = true;
                    return i;
                }
                'F' => {
                    st.emph = false;
                    return i;
                }
                'G' => {
                    st.dstrike = true;
                    return i;
                }
                'H' => {
                    st.dstrike = false;
                    return i;
                }
                '4' => {
                    if !ibm {
                        st.italic = true;
                    }
                    return i;
                }
                '5' => {
                    if ibm {
                        return i + 1;
                    }
                    st.italic = false;
                    return i;
                }
                '-' => {
                    st.underline = u(0) & 1 != 0 || u(0) == 49;
                    return i + 1;
                }
                '_' => {
                    if ibm {
                        st.underline = u(0) & 1 != 0 || u(0) == 49;
                    } else {
                        st.overscore = u(0) & 1 != 0 || u(0) == 49;
                    }
                    return i + 1;
                }
                'S' => {
                    let v = u(0);
                    st.script = if v == 0 || v == 48 { 1 } else { 2 };
                    return i + 1;
                }
                'T' => {
                    st.script = 0;
                    return i;
                }
                'W' => {
                    st.wmul = if flag(u(0)) { 2 } else { 1 };
                    return i + 1;
                }
                'w' => {
                    st.hmul = if flag(u(0)) { 2 } else { 1 };
                    return i + 1;
                }
                'M' => {
                    st.base_cpi = 12.0;
                    return i;
                }
                'P' => {
                    if ibm {
                        st.prop = flag(u(0));
                        return i + 1;
                    }
                    st.base_cpi = 10.0;
                    st.prop = false;
                    return i;
                }
                ':' => {
                    if ibm {
                        st.base_cpi = 12.0;
                        return i;
                    }
                    return i + 3;
                }
                _ => {}
            }
        }
        if ch == 'g' && self.variant == Family::Star && gfx_mode(u(0) as i32).is_some() && i + 2 < n {
            // Star SG-10/15: ESC g m n1 n2 (as ESC *)
            return self.graphics(data, i + 1, u(0) as i32, 2);
        }
        {
            let lq24 = self.e.profile.lq24;
            let st = &mut self.e.style;
            match ch {
                'g' => {
                    st.base_cpi = 15.0;
                    return i;
                }
                'p' => {
                    st.prop = flag(u(0));
                    if st.prop && !lq24 {
                        st.nlq = true;
                    }
                    return i + 1;
                }
                'x' => {
                    st.nlq = flag(u(0));
                    return i + 1;
                }
                'k' => {
                    st.face = match u(0) & 7 {
                        1 => Face::Sans,
                        2 | 3 => Face::Courier,
                        _ => Face::Roman,
                    };
                    return i + 1;
                }
                '!' => {
                    let v = u(0);
                    st.base_cpi = if v & 1 != 0 { 12.0 } else { 10.0 };
                    st.prop = v & 2 != 0;
                    st.condensed = v & 4 != 0;
                    st.emph = v & 8 != 0;
                    st.dstrike = v & 16 != 0;
                    st.wmul = if v & 32 != 0 { 2 } else { 1 };
                    st.italic = v & 64 != 0;
                    st.underline = v & 128 != 0;
                    return i + 1;
                }
                'R' => {
                    let v = u(0);
                    if v <= 12 {
                        self.e.national = v as i32;
                    }
                    return i + 1;
                }
                't' => return i + 1,
                'r' => {
                    // ESC r n - ribbon colour (Epson / Star colour printers): 0 black ... 6 green
                    let v = u(0);
                    if v <= 6 {
                        self.e.color = v;
                    } else if (48..=54).contains(&v) {
                        self.e.color = v - 48;
                    }
                    return i + 1;
                }
                '6' => {
                    self.e.ibm_set2 = true;
                    return i;
                }
                '7' => {
                    self.e.ibm_set2 = false;
                    return i;
                }
                _ => {}
            }
        }
        if ch == 'I' && ibm {
            let v = u(0);
            self.e.style.nlq = matches!(v, 2 | 3 | 50 | 51);
            return i + 1;
        }
        if c == 0x0E || c == 0x0F {
            if c == 0x0E {
                self.e.style.wmul_so = true;
            } else {
                self.e.style.condensed = true;
            }
            return i;
        }
        // ---- downloaded characters (skipped)
        if ch == '&' {
            let (lo, hi) = (u(1) as i64, u(2) as i64);
            let cnt = (hi - lo + 1).max(0);
            let mut k = i + 3;
            for _ in 0..cnt {
                if k >= n {
                    break;
                }
                let width = 11;
                k += 1 + if !self.e.profile.lq24 { width } else { 33 };
            }
            return k;
        }
        // ---- graphics
        if matches!(ch, 'K' | 'L' | 'Y' | 'Z') {
            let idx = match ch {
                'K' => 0,
                'L' => 1,
                'Y' => 2,
                _ => 3,
            };
            let mode = self.kly[idx];
            return self.graphics(data, i, mode, 2);
        }
        if ch == 'z' && self.variant == Family::Star {
            // Star Gemini / Delta: ESC z n1 n2 = quadruple density (Epson's ESC Z)
            return self.graphics(data, i, 3, 2);
        }
        if ch == '?' {
            let s = u(0) as char;
            let idx = match s {
                'K' => Some(0),
                'L' => Some(1),
                'Y' => Some(2),
                'Z' => Some(3),
                _ => None,
            };
            if let Some(ix) = idx {
                self.kly[ix] = u(1) as i32;
            }
            return i + 2;
        }
        if ch == '*' {
            return self.graphics(data, i + 1, u(0) as i32, 2);
        }
        if ch == '^' {
            if ibm {
                if i < n {
                    self.print_cp437(data[i]);
                }
                return i + 1;
            }
            let m = u(0);
            let cnt = u(1) as usize + 256 * u(2) as usize;
            let body = slice(data, i + 3, i + 3 + cnt * 2).to_vec();
            let dpi = if m != 0 { 120.0 } else { 60.0 };
            let (dpi_y, top) = self.gfx_geometry(1);
            self.e.emit_columns(&body, 2, dpi, dpi_y, top, true, false, true, None, true);
            return i + 3 + cnt * 2;
        }
        if ch == '.' {
            return self.raster(data, i);
        }
        if ch == '(' {
            return self.ext(data, i);
        }
        if ch == '[' {
            // IBM extended command: ESC [ c nL nH data...
            let ln = u(1) as usize + 256 * u(2) as usize;
            return i + 3 + ln;
        }
        if c == 0x19 {
            // ESC EM n
            return i + 1;
        }
        if let Some(k) = skip_params(ch) {
            return i + k;
        }
        self.e.unknown += 1;
        i
    }

    // ---------------------------------------------------------------- graphics
    fn graphics(&mut self, data: &[u8], i: usize, mode: i32, hdr: usize) -> usize {
        let n = data.len();
        let Some((dpi_x, bpc, noadj)) = gfx_mode(mode) else {
            self.e.unknown += 1;
            return i + hdr;
        };
        let n1 = if i < n { data[i] as usize } else { 0 };
        let n2 = if i + 1 < n { data[i + 1] as usize } else { 0 };
        let cols = n1 + 256 * n2;
        let body = slice(data, i + 2, i + 2 + cols * bpc);
        let (dpi_y, top) = self.gfx_geometry(bpc);
        let owned;
        let (body, bpc_eff) = if bpc == 6 {
            // 48-pin modes: print the first 24 pins only
            let m = body.len() / 6;
            let mut v = Vec::with_capacity(m * 3);
            for k in 0..m {
                v.extend_from_slice(&body[k * 6..k * 6 + 3]);
            }
            owned = v;
            (&owned[..], 3)
        } else {
            (body, bpc)
        };
        self.e.emit_columns(body, bpc_eff, dpi_x, dpi_y, top, true, noadj, false, None, true);
        i + 2 + cols * bpc
    }

    /// ESC . c v h m nL nH data  (ESC/P2 raster graphics).
    fn raster(&mut self, data: &[u8], i: usize) -> usize {
        let n = data.len();
        if i + 6 > n {
            return n;
        }
        let (c, v, h, m) = (data[i], data[i + 1], data[i + 2], data[i + 3] as usize);
        let width = data[i + 4] as usize + 256 * data[i + 5] as usize;
        let rb = (width + 7) / 8;
        let mut k = i + 6;
        let total = rb * m;
        let mut buf: Vec<u8>;
        if c == 1 {
            let mut out: Vec<u8> = Vec::new();
            while out.len() < total && k < n {
                let ctr = data[k];
                k += 1;
                if ctr < 128 {
                    out.extend_from_slice(slice(data, k, k + ctr as usize + 1));
                    k += ctr as usize + 1;
                } else if ctr > 128 {
                    if k < n {
                        out.extend(std::iter::repeat(data[k]).take(257 - ctr as usize));
                    }
                    k += 1;
                }
            }
            out.truncate(total);
            buf = out;
        } else {
            buf = slice(data, k, k + total).to_vec();
            k += total;
        }
        if buf.len() < total {
            buf.resize(total, 0);
        }
        let dx = if h != 0 { U * h as f64 / 3600.0 } else { 0.0 };
        let dy = if v != 0 { U * v as f64 / 3600.0 } else { 0.0 };
        if m > 0 && width > 0 && v != 0 && h != 0 {
            let mut ch = Chunk::default();
            for rr in 0..m {
                for cc in 0..width {
                    let byte = buf[rr * rb + cc / 8];
                    if byte & (0x80 >> (cc % 8)) != 0 {
                        ch.x.push((self.e.x + cc as f64 * dx) as f32);
                        ch.y.push((self.e.tof_u + self.e.y + rr as f64 * dy - m as f64 * dy * 0.75) as f32);
                        ch.pin.push((rr % 24) as i16);
                        ch.ps.push(self.e.cur_pass + 1000003);
                        ch.ds.push(0);
                        ch.cs.push(self.e.color);
                    }
                }
            }
            if !ch.x.is_empty() {
                self.e.push_chunk(ch);
                self.e.page_dots += 1;
                self.e.dirty = true;
            }
        }
        self.e.x += width as f64 * dx;
        k
    }

    /// ESC ( c nL nH data  (ESC/P2 extended commands). Mostly skipped.
    fn ext(&mut self, data: &[u8], i: usize) -> usize {
        let n = data.len();
        if i >= n {
            return i;
        }
        let cmd = data[i] as char;
        let ln = (if i + 1 < n { data[i + 1] as usize } else { 0 }) + 256 * (if i + 2 < n { data[i + 2] as usize } else { 0 });
        let body = slice(data, i + 3, i + 3 + ln).to_vec();
        let end = i + 3 + ln;
        if cmd == 'C' && body.len() >= 2 && self.e.opts.honor_page_length {
            let v = body[0] as i64 + 256 * body[1] as i64;
            if v != 0 {
                self.e.page_len = v as f64 * U / 360.0;
            }
        } else if cmd == 'V' && body.len() >= 2 {
            let v = body[0] as i64 + 256 * body[1] as i64;
            self.e.y = v as f64 * U / 360.0;
        } else if cmd == 'v' && body.len() >= 2 {
            let mut v = body[0] as i64 + 256 * body[1] as i64;
            if v >= 32768 {
                v -= 65536;
            }
            if v > 0 {
                self.e.advance_y(v as f64 * U / 360.0);
            }
        } else if cmd == '^' {
            for &b in &body {
                let cp = self.e.codepage.clone();
                let mut ch = charsets::decode(b, &cp, self.e.national, true);
                if ch.is_none() && b < 32 {
                    ch = charsets::cp437_low(b);
                }
                if let Some(c) = ch {
                    self.e.print_char(c);
                }
            }
        } else if cmd == '-' && body.len() >= 3 {
            self.e.style.underline = matches!(body[1], 1 | 2 | 5 | 6) || matches!(body[0], 1 | 2 | 5 | 6);
        }
        end
    }
}
