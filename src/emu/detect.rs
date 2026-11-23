//! Printer auto-detection, mixed-capture splitting and capture repair.
//!
//! The Python original expresses all of this with byte regexes (some with look-ahead); here they are hand-written
//! matchers with the same left-to-right, non-overlapping `findall` semantics.

use super::model::{profile, PROFILES};
use super::okimate::looks_okimate;

// ------------------------------------------------------------------------------------ tiny fixed-length matcher

#[derive(Clone, Copy)]
enum P {
    /// Exact byte.
    L(u8),
    /// Inclusive byte range.
    R(u8, u8),
    /// ASCII digit.
    D,
    /// Any byte.
    A,
    /// One of these bytes.
    S(&'static [u8]),
}

fn pm(e: P, b: u8) -> bool {
    match e {
        P::L(x) => b == x,
        P::R(lo, hi) => (lo..=hi).contains(&b),
        P::D => b.is_ascii_digit(),
        P::A => true,
        P::S(set) => set.contains(&b),
    }
}

/// Does the fixed-length pattern match at `d[i..]` (entirely inside `d[..lim]`)?
fn matches_at(d: &[u8], i: usize, lim: usize, pat: &[P]) -> bool {
    let lim = lim.min(d.len());
    if i + pat.len() > lim {
        return false;
    }
    pat.iter().enumerate().all(|(k, &e)| pm(e, d[i + k]))
}

/// `re.findall(...)` count for matchers returning the match length at a position.
fn count_with(d: &[u8], f: &dyn Fn(&[u8], usize) -> Option<usize>) -> usize {
    let (mut c, mut i) = (0usize, 0usize);
    while i < d.len() {
        match f(d, i) {
            Some(len) if len > 0 => {
                c += 1;
                i += len;
            }
            _ => i += 1,
        }
    }
    c
}

fn count_pat(d: &[u8], pat: &[P]) -> usize {
    count_with(d, &|d, i| if matches_at(d, i, d.len(), pat) { Some(pat.len()) } else { None })
}

use P::{A, D, L, R, S};
const ESC: u8 = 0x1b;

// ------------------------------------------------------------------------------------ helpers

fn digits_at(d: &[u8], i: usize, n: usize) -> bool {
    i + n <= d.len() && d[i..i + n].iter().all(|b| b.is_ascii_digit())
}

fn is_iw_colour(d: &[u8], i: usize) -> bool {
    // \x1bK[0-4](?=[\r\x1b])
    matches_at(d, i, d.len(), &[L(ESC), L(b'K'), R(b'0', b'4')]) && matches!(d.get(i + 3), Some(&0x0d) | Some(&ESC))
}

/// Bytes per column of the 24-pin ESC * modes; every other mode uses 1.
fn bytes_per_column(m: u8) -> usize {
    match m {
        32 | 33 | 38 | 39 | 40 => 3,
        71 | 72 | 73 => 6,
        _ => 1,
    }
}

fn le16(d: &[u8], i: usize) -> usize {
    d[i] as usize | (d[i + 1] as usize) << 8
}

// ------------------------------------------------------------------------------------ PETSCII text

/// Plain Commodore text in the lower/upper character set, as GEOS and many C64 programs send it.
pub fn looks_like_petscii_text(data: &[u8]) -> bool {
    if data.contains(&ESC) || data.len() < 40 {
        return false;
    }
    if data.iter().any(|&b| b >= 0x80) || data.iter().any(|&b| b < 0x20 && !matches!(b, 0x0D | 0x0A | 0x0C | 0x09)) {
        return false;
    }
    let up = data.iter().filter(|&&b| (0x41..=0x5A).contains(&b)).count();
    let lo = data.iter().filter(|&&b| (0x61..=0x7A).contains(&b)).count();
    let letters = up + lo;
    letters >= 30 && lo >= 2 && up as f64 >= 0.85 * letters as f64
}

// ------------------------------------------------------------------------------------ masking

/// Same length as `data`, with the bitmap payload of ESC/P graphics commands zeroed. Bitmap bytes are arbitrary, so they can
/// imitate the control codes of other printers; printer-type scoring must only look at the commands and text around them.
pub fn mask_graphics(data: &[u8]) -> Vec<u8> {
    if !data.contains(&ESC) {
        return data.to_vec();
    }
    let n = data.len();
    let mut out = data.to_vec();
    let mut pos = 0usize;
    loop {
        let Some(off) = data.get(pos..).and_then(|s| s.iter().position(|&b| b == ESC)) else { break };
        let i = pos + off;
        if i + 1 >= n {
            break;
        }
        let c = data[i + 1];
        let (start, end);
        // Apple ImageWriter: ESC G nnnn / ESC S nnnn (nnnn bytes), ESC g nnn (nnn*8 bytes), ASCII-digit counts
        let iw = if (c == b'G' || c == b'S') && digits_at(data, i + 2, 4) {
            Some((i + 6, parse_dec(&data[i + 2..i + 6]), false))
        } else if c == b'g' && digits_at(data, i + 2, 3) {
            // group 2 is `(\d{4}|\d{3})`: a fourth digit is swallowed too
            let nd = if digits_at(data, i + 2, 4) { 4 } else { 3 };
            Some((i + 2 + nd, parse_dec(&data[i + 2..i + 2 + nd]), true))
        } else {
            None
        };
        if let Some((s, cnt, is_g)) = iw {
            start = s;
            end = s + if is_g { cnt * 8 } else { cnt };
        } else if c == 0x4B && is_iw_colour(data, i) {
            // ImageWriter ribbon colour, not Epson ESC K n1 n2
            pos = i + 3;
            continue;
        } else if c == 0x67 && i + 4 < n && data[i + 2] <= 6 {
            // Star SG-10: ESC g m n1 n2
            start = i + 5;
            end = start + le16(data, i + 3) * bytes_per_column(data[i + 2]);
        } else if c == 0x2A && i + 4 < n {
            // ESC * m n1 n2
            start = i + 5;
            end = start + le16(data, i + 3) * bytes_per_column(data[i + 2]);
        } else if matches!(c, 0x4B | 0x4C | 0x59 | 0x5A | 0x7A) && i + 3 < n {
            // ESC K/L/Y/Z n1 n2 (and Star Gemini's ESC z)
            start = i + 4;
            end = start + le16(data, i + 2);
        } else {
            pos = i + 2;
            continue;
        }
        let end = end.min(n);
        if start < end {
            for b in &mut out[start..end] {
                *b = 0;
            }
        }
        pos = end.max(i + 2);
    }
    out
}

fn parse_dec(d: &[u8]) -> usize {
    d.iter().fold(0usize, |a, &b| a * 10 + (b - b'0') as usize)
}

// ------------------------------------------------------------------------------------ scoring

pub type Scores = [(&'static str, f64); 7];

pub fn score_get(s: &Scores, key: &str) -> f64 {
    s.iter().find(|(k, _)| *k == key).map(|x| x.1).unwrap_or(0.0)
}

fn score_set(s: &mut Scores, key: &str, v: f64) {
    for e in s.iter_mut() {
        if e.0 == key {
            e.1 = v;
        }
    }
}

pub fn score_printers(data: &[u8]) -> Scores {
    let mut s: Scores = [("", 0.0); 7];
    for (i, p) in PROFILES.iter().enumerate() {
        s[i] = (p.key, 0.0);
    }
    let cnt = |pat: &[P]| count_pat(data, pat) as f64;

    // --- Apple ImageWriter: ASCII-digit parameters are the giveaway
    let mut iw = 3.0 * cnt(&[L(ESC), L(b'T'), D, D]) + 4.0 * cnt(&[L(ESC), L(b'G'), D, D, D, D]) + 3.0 * cnt(&[L(ESC), L(b'g'), D, D, D])
        + 3.0 * cnt(&[L(ESC), L(b'H'), D, D, D, D]);
    iw += 3.0 * cnt(&[L(ESC), L(b'V'), D, D, D, D]) + 2.0 * cnt(&[L(ESC), L(b'L'), D, D, D]) + 2.0 * cnt(&[L(ESC), L(b'a'), S(b"012")])
        + cnt(&[L(ESC), S(b"ABnNeqQ")]);
    iw += cnt(&[L(ESC), S(b"<>")]) + cnt(&[L(ESC), S(b"!\"")]) + 3.0 * cnt(&[L(ESC), L(b'S'), D, D, D, D]);
    // ribbon colour: ESC K <ASCII digit 0-4> then CR / next command (Epson's ESC K takes raw n1 n2 bytes)
    iw += 3.0 * count_with(data, &|d, i| if is_iw_colour(d, i) { Some(3) } else { None }) as f64;
    score_set(&mut s, "imagewriter", iw);

    // --- Commodore MPS
    let flat = mask_graphics(data); // bitmap bytes of Epson-style graphics must not count as Commodore control codes
    let mcnt = |pat: &[P]| count_pat(&flat, pat) as f64;
    let mut mps = 5.0 * mcnt(&[L(0x08), R(0x80, 0xff), R(0x80, 0xff), R(0x80, 0xff)])
        + 4.0 * mcnt(&[L(ESC), L(0x10), S(&[0, 1])])
        + 3.0 * mcnt(&[L(0x10), D, D]);
    mps += mcnt(&[S(&[0x91, 0x92])])
        + count_with(&flat, &|d, i| {
            // \x0e.{0,80}\x0f  (greedy)
            if d[i] != 0x0e {
                return None;
            }
            (0..=80usize).rev().find(|&k| i + 1 + k < d.len() && d[i + 1 + k] == 0x0f).map(|k| k + 2)
        }) as f64
        + 0.2 * mcnt(&[R(0xc1, 0xda)]);
    if looks_like_petscii_text(data) {
        mps += 12.0;
    }
    score_set(&mut s, "mps", mps);

    // --- Epson / IBM / Star (on the masked stream: bitmap bytes must not count as commands)
    let ecnt = mcnt;
    let k_or_lyz = count_with(&flat, &|d, i| {
        // \x1b(?:K(?![0-4][\r\x1b])|[LYZ])[\x00-\xff]{2}
        if d[i] != ESC || i + 1 >= d.len() {
            return None;
        }
        let ok = match d[i + 1] {
            b'K' => !(matches!(d.get(i + 2), Some(b'0'..=b'4')) && matches!(d.get(i + 3), Some(&0x0d) | Some(&ESC))),
            b'L' | b'Y' | b'Z' => true,
            _ => false,
        };
        if ok && i + 5 <= d.len() {
            Some(5)
        } else {
            None
        }
    }) as f64;
    let attr = count_with(&flat, &|d, i| {
        // \x1b(?:[GS](?!\d{4})|g(?!\d{3})|T(?!\d\d)|[EFHMP45])
        if d[i] != ESC || i + 1 >= d.len() {
            return None;
        }
        let ok = match d[i + 1] {
            b'G' | b'S' => !digits_at(d, i + 2, 4),
            b'g' => !digits_at(d, i + 2, 3),
            b'T' => !digits_at(d, i + 2, 2),
            b'E' | b'F' | b'H' | b'M' | b'P' | b'4' | b'5' => true,
            _ => false,
        };
        if ok {
            Some(2)
        } else {
            None
        }
    }) as f64;
    let mut ep = 4.0 * ecnt(&[L(ESC), L(b'@')]) + 3.0 * k_or_lyz + 3.0 * ecnt(&[L(ESC), L(b'*'), R(0, 7)]);
    ep += 2.0 * ecnt(&[L(ESC), L(b'3'), A]) + 2.0 * ecnt(&[L(ESC), L(b'A'), R(0, 0x20)]) + attr;
    ep += 2.0 * ecnt(&[L(ESC), L(b'x'), S(&[0, 1])]) + ecnt(&[L(ESC), L(b't'), S(&[0, 1])]) + ecnt(&[L(ESC), L(b'R'), R(0, 0x0c)])
        + ecnt(&[L(ESC), L(b'C')]);
    ep += ecnt(&[L(ESC), L(b'-'), S(&[0, 1])]) + 0.2 * ecnt(&[L(0x0f)]) + 0.2 * ecnt(&[L(0x12)]);
    score_set(&mut s, "epson_fx", ep);
    let mut lq = 4.0 * ecnt(&[L(ESC), L(b'*'), S(&[0x20, 0x21, 0x26, 0x27, 0x28, 0x47, 0x48, 0x49])])
        + 3.0 * count_with(&flat, &|d, i| {
            // \x1b\([A-Za-z\^-]
            if matches_at(d, i, d.len(), &[L(ESC), L(b'(')]) && matches!(d.get(i + 2), Some(b'A'..=b'Z') | Some(b'a'..=b'z') | Some(b'^') | Some(b'-')) {
                Some(3)
            } else {
                None
            }
        }) as f64;
    lq += 2.0 * ecnt(&[L(ESC), L(b'X'), A, A]);
    score_set(&mut s, "epson_lq", if lq != 0.0 { ep * 0.9 + lq } else { 0.0 });
    let ibm = 3.0 * ecnt(&[L(ESC), L(b'['), A]) + 2.0 * ecnt(&[L(ESC), L(b'6')]) + 2.0 * ecnt(&[L(ESC), L(b':')]) + ecnt(&[L(ESC), L(b'5')]);
    score_set(&mut s, "ibm", ibm + if ibm != 0.0 { ep * 0.5 } else { 0.0 });
    // Star Gemini / Delta / SG: ESC z n1 n2 = quadruple-density graphics (what Epson spells ESC Z)
    let has_z = (0..flat.len()).any(|i| matches_at(&flat, i, flat.len(), &[L(ESC), L(b'z'), R(0x40, 0xff), R(0, 0x0f)]));
    let mut star = if has_z { 4.0 * count_pat(&flat, &[L(ESC), L(b'z'), A, A]) as f64 } else { 0.0 };
    let sg = count_pat(&flat, &[L(ESC), L(b'g'), R(0, 6), A, A]);
    if sg >= 2 && count_pat(data, &[L(ESC), L(b'g'), D, D, D]) == 0 {
        star += 4.0 * sg as f64;
    }
    score_set(&mut s, "star", star);
    s
}

pub fn detect_printer(data: &[u8]) -> &'static str {
    if looks_okimate(data) {
        // colour-ribbon bit images chained only by carriage returns
        return "okimate";
    }
    let s = score_printers(data);
    let g = |k: &str| score_get(&s, k);
    let mut best = s[0].0;
    for &(k, v) in s.iter() {
        if v > g(best) {
            best = k;
        }
    }
    if g(best) <= 0.0 {
        return "epson_fx";
    }
    // an ImageWriter stream scores on Epson patterns too; prefer it when its giveaways are present
    if g("imagewriter") >= 6.0 && g("imagewriter") >= g("epson_fx") * 0.6 && g("imagewriter") >= g("mps") {
        return "imagewriter";
    }
    // (random graphics bytes can imitate a few Commodore patterns; only believe MPS if it beats every ESC/P family score)
    if g("mps") >= 8.0 && g("mps") > g("epson_fx").max(g("epson_lq")).max(g("ibm")) {
        return "mps";
    }
    if g("star") >= 4.0 {
        return "star";
    }
    if g("epson_lq") != 0.0 && g("epson_lq") >= g("epson_fx") && g("epson_lq") >= g("ibm") {
        return "epson_lq";
    }
    if g("ibm") > g("epson_fx") {
        return "ibm";
    }
    if best != "star" {
        best
    } else {
        "epson_fx"
    }
}

// ------------------------------------------------------------------------------------ mixed captures

fn group_of(key: &str) -> &'static str {
    match key {
        "imagewriter" => "imagewriter",
        "mps" => "mps",
        "okimate" => "okimate",
        _ => "escp",
    }
}

/// First position in `d[lo..hi]` where this family's start signature matches completely inside `hi`.
fn start_sig(group: &str, d: &[u8], lo: usize, hi: usize) -> Option<usize> {
    let hi = hi.min(d.len());
    (lo..hi).find(|&p| {
        let m = |pat: &[P]| matches_at(d, p, hi, pat);
        match group {
            // \x08(?:\x00\r|[\x80-\xff]{2})|\x1b\x10[\x00\x01]
            "mps" => {
                m(&[L(0x08), L(0), L(0x0d)]) || m(&[L(0x08), R(0x80, 0xff), R(0x80, 0xff)]) || m(&[L(ESC), L(0x10), S(&[0, 1])])
            }
            // \x1b[TGHVLgS]\d{2,4}
            "imagewriter" => m(&[L(ESC), S(b"TGHVLgS"), D, D]),
            _ => {
                m(&[L(ESC), L(b'@')])
                    || m(&[L(ESC), L(b'A'), R(0, 0x20)])
                    || m(&[L(ESC), S(b"KLYZz"), A, A])
                    || m(&[L(ESC), L(b'g'), R(0, 6), A, A])
                    || m(&[L(ESC), L(b'*'), R(0, 7)])
                    || m(&[L(ESC), L(b'3'), A])
            }
        }
    })
}

fn window_family(window: &[u8]) -> Option<&'static str> {
    let s = score_printers(window);
    let g = |k: &str| score_get(&s, k);
    let fam = [("escp", g("epson_fx").max(g("epson_lq")).max(g("ibm"))), ("imagewriter", g("imagewriter")), ("mps", g("mps"))];
    let mut best = fam[0];
    for &f in fam.iter() {
        if f.1 > best.1 {
            best = f;
        }
    }
    let mut vals: Vec<f64> = fam.iter().map(|f| f.1).collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let others = vals[vals.len() - 2];
    if best.1 >= 5.0 && best.1 >= 1.5 * others {
        Some(best.0)
    } else {
        None
    }
}

fn printer_for(group: &str, chunk: &[u8]) -> &'static str {
    if group != "escp" {
        return match group {
            "imagewriter" => "imagewriter",
            "mps" => "mps",
            _ => "okimate",
        };
    }
    let key = detect_printer(chunk);
    if group_of(key) == "escp" {
        key
    } else {
        "epson_fx"
    }
}

/// Split a capture in which the printer type changed part-way (VICE appends to one file).
/// Returns `(start, end, printer_key)` covering the whole stream.
pub fn split_segments(data: &[u8]) -> Vec<(usize, usize, &'static str)> {
    let n = data.len();
    if looks_okimate(data) {
        return vec![(0, n, "okimate")];
    }
    let win = 256.max(n / 200);
    if n < 2 * win {
        return vec![(0, n, detect_printer(data))];
    }
    let starts: Vec<usize> = (0..n).step_by(win).collect();
    let masked = mask_graphics(data); // so that bitmap bytes do not look like another printer's codes
    let labels: Vec<Option<&'static str>> = starts.iter().map(|&s| window_family(&masked[s..(s + win).min(n)])).collect();
    let Some(first) = labels.iter().flatten().next().copied() else {
        return vec![(0, n, detect_printer(data))];
    };
    // fill gaps: unknown windows inherit the previous known family (or the next one at the start)
    let mut cur = first;
    let mut filled: Vec<&'static str> = Vec::new();
    for l in &labels {
        cur = l.unwrap_or(cur);
        filled.push(cur);
    }
    // hysteresis: a change needs two consecutive windows of the new family
    let mut stable: Vec<&'static str> = vec![filled[0]];
    for i in 1..filled.len() {
        let nxt = if i + 1 < filled.len() { filled[i + 1] } else { filled[i] };
        let last = *stable.last().unwrap();
        stable.push(if filled[i] == last || filled[i] == nxt { filled[i] } else { last });
    }
    let mut cuts: Vec<(usize, &'static str)> = vec![(0, stable[0])];
    for i in 1..stable.len() {
        if stable[i] != stable[i - 1] {
            let p = starts[i];
            let mut j = i - 1;
            while j > 0 && labels[j] != Some(stable[i - 1]) {
                // last window that clearly looked like the old printer
                j -= 1;
            }
            let (lo, hi) = (starts[j], n.min(p + win));
            let at = start_sig(stable[i], data, lo, hi).unwrap_or(p);
            cuts.push((at, stable[i]));
        }
    }
    let mut segs: Vec<(usize, usize, &'static str)> = Vec::new();
    for (k, &(a, grp)) in cuts.iter().enumerate() {
        let b = if k + 1 < cuts.len() { cuts[k + 1].0 } else { n };
        match segs.last_mut() {
            Some(last) if b.saturating_sub(a) < 64 || last.2 == grp => last.1 = b,
            _ => segs.push((a, b, grp)),
        }
    }
    segs.into_iter().map(|(a, b, g)| (a, b, printer_for(g, &data[a..b]))).collect()
}

// ------------------------------------------------------------------------------------ capture repair

const MIN_DOUBLED: usize = 16;

/// If the whole stream (from byte 0 or byte 1) is made of identical byte pairs, collapse the pairs.
/// Returns `(data, note)`; the note is empty when nothing was changed.
pub fn undouble(data: &[u8]) -> (Vec<u8>, String) {
    for skip in 0..2usize {
        if data.len() < skip {
            continue;
        }
        let body = &data[skip..];
        let n = body.len() / 2;
        if n < MIN_DOUBLED / 2 {
            continue;
        }
        let same = (0..n).filter(|&k| body[2 * k] == body[2 * k + 1]).count();
        // a genuine stream would match about 1 pair in 256; allow a few glitches in a long capture (but not the 120 dpi MPS-1000
        // GEOS stream, where every graphics column is sent twice by design)
        if same == n || (n >= 256 && same as f64 >= 0.999 * n as f64) {
            let mut out: Vec<u8> = (0..n).map(|k| body[2 * k]).collect();
            let tail = &body[2 * n..];
            if tail.len() == 1 {
                out.push(tail[0]);
            }
            let mut note = String::from("every byte was received twice - collapsed to single bytes");
            if skip > 0 {
                note.push_str(&format!(" (ignored a stray leading byte 0x{:02X})", data[0]));
            }
            return (out, note);
        }
    }
    (data.to_vec(), String::new())
}

/// A job of only a few port-probe bytes (e.g. 55 AA 00) that is not print data.
pub fn is_probe(data: &[u8]) -> bool {
    data.len() <= 4 && data.iter().all(|b| matches!(b, 0x00 | 0x04 | 0x55 | 0xAA | 0xFF))
}

/// True if `data` stops inside a bitmap row: a graphics command announced more bytes than have arrived, so the sender is
/// probably only pausing and the job is not over yet.
pub fn unfinished_graphics(data: &[u8]) -> bool {
    let n = data.len();
    let mut pos = 0usize;
    loop {
        let Some(off) = data.get(pos..).and_then(|s| s.iter().position(|&b| b == ESC)) else { return false };
        let i = pos + off;
        if i + 1 >= n {
            return false;
        }
        let c = data[i + 1];
        let mut end: Option<usize> = None;
        if matches!(c, b'G' | b'S' | b'g') {
            let want = if c == b'g' { 3 } else { 4 };
            if digits_at(data, i + 2, want) {
                let cnt = parse_dec(&data[i + 2..i + 2 + want]);
                end = Some(i + 2 + want + if c == b'g' { cnt * 8 } else { cnt });
            } else {
                // \x1b[GSg]\d{0,3}\Z : the header itself is cut off
                let mut j = i + 2;
                while j < n && j < i + 5 && data[j].is_ascii_digit() {
                    j += 1;
                }
                if j == n {
                    return true;
                }
            }
        } else if c == 0x2A && i + 4 < n {
            end = Some(i + 5 + le16(data, i + 3) * bytes_per_column(data[i + 2]));
        } else if matches!(c, b'K' | b'L' | b'Y' | b'Z') && i + 3 < n {
            // (but ImageWriter's ESC K <digit> is a ribbon colour)
            if !(c == b'K' && matches!(data[i + 2], b'0'..=b'4') && matches!(data[i + 3], 0x0d | ESC)) {
                end = Some(i + 4 + le16(data, i + 2));
            }
        }
        match end {
            None => pos = i + 2,
            Some(e) if e > n => return true,
            Some(e) => pos = e,
        }
    }
}

#[allow(dead_code)]
fn _unused() {
    let _ = profile("epson_fx");
}
