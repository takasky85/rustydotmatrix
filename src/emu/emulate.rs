//! Entry point: raw printer bytes -> Document (list of dot pages).
use super::commodore::MpsEngine;
use super::detect::{detect_printer, split_segments, undouble};
use super::engine::Engine;
use super::escp::EscpEngine;
use super::imagewriter::ImageWriterEngine;
use super::model::{profile, Document, EmuOptions, Family};
use super::okimate::OkimateEngine;

fn count(d: &[u8], b: u8) -> usize {
    d.iter().filter(|&&x| x == b).count()
}

/// Apply the CR/LF auto-detection of `_emulate_one`.
pub fn configure_newlines(e: &mut Engine, data: &[u8], opts: &EmuOptions) {
    let (n_cr, n_lf) = (count(data, b'\r'), count(data, b'\n'));
    e.cr_adds_lf = opts.cr_adds_lf.unwrap_or(n_cr > 0 && n_lf == 0);
    e.lf_adds_cr = opts.lf_adds_cr.unwrap_or(n_lf > 0 && n_cr == 0);
}

/// Emulate one printer type over the whole stream.
pub fn emulate_one(data: &[u8], key: &str, opts: &EmuOptions) -> Document {
    let prof = profile(key).unwrap_or_else(|| profile("epson_fx").unwrap());
    let mut doc = match prof.family {
        Family::ImageWriter => {
            let mut eng = ImageWriterEngine::new(prof, opts);
            configure_newlines(&mut eng.e, data, opts);
            eng.run(data);
            let mut doc = eng.e.finish(data.len());
            if eng.short_rows > 0 {
                doc.warnings.push(format!(
                    "{} graphics row(s) had fewer bytes than their header announced - the capture probably lost data (those rows end early)",
                    eng.short_rows
                ));
            }
            doc
        }
        Family::Mps => {
            let mut eng = MpsEngine::new(prof, opts);
            configure_newlines(&mut eng.e, data, opts);
            eng.run(data);
            eng.e.finish(data.len())
        }
        Family::Okimate => {
            let mut eng = OkimateEngine::new(prof, opts);
            configure_newlines(&mut eng.e, data, opts);
            eng.run(data);
            eng.finish(data.len())
        }
        _ => {
            let mut eng = EscpEngine::new(prof, opts);
            configure_newlines(&mut eng.e, data, opts);
            eng.run(data);
            eng.e.finish(data.len())
        }
    };
    if opts.wide_carriage == Some(true) {
        doc.carriage_in = 13.6;
    }
    doc
}

/// Full pipeline: capture repair, printer detection (or the chosen printer), mixed-stream splitting.
pub fn emulate(data: &[u8], opts: &EmuOptions) -> Document {
    let mut fixes: Vec<String> = Vec::new();
    let original = data.len();
    let repaired;
    let mut data = data;
    if opts.fix_capture {
        let (d, note) = undouble(data);
        if !note.is_empty() {
            fixes.push(note);
            repaired = d;
            data = &repaired;
        }
    }
    let mut doc = emulate_data(data, opts);
    doc.fixes = fixes;
    doc.n_bytes = original;
    doc
}

fn emulate_data(data: &[u8], opts: &EmuOptions) -> Document {
    let auto = opts.printer == "auto" || profile(&opts.printer).is_none();
    if auto && opts.split_mixed {
        let segs = split_segments(data);
        if segs.len() > 1 {
            let mut pages = Vec::new();
            let mut notes = Vec::new();
            let mut carriage = 0.0f64;
            let mut n_unknown = 0;
            let mut warnings = Vec::new();
            let mut first_profile = None;
            for (a, b, k) in segs {
                let mut d = emulate_one(&data[a..b], k, opts);
                d.detected = k.to_string();
                if first_profile.is_none() {
                    first_profile = Some(d.profile);
                }
                carriage = carriage.max(d.carriage_in);
                n_unknown += d.n_unknown;
                warnings.append(&mut d.warnings);
                let np = d.pages.len();
                for mut p in std::mem::take(&mut d.pages) {
                    p.index = pages.len();
                    if p.profile_key.is_empty() {
                        p.profile_key = k.to_string();
                    }
                    pages.push(p);
                }
                notes.push(format!("{}: bytes {}-{}, {} page(s)", profile(k).unwrap().label, thousands(a), thousands(b), np));
            }
            return Document {
                pages,
                profile: first_profile.unwrap(),
                detected: "mixed".into(),
                carriage_in: carriage,
                notes,
                warnings,
                fixes: Vec::new(),
                n_bytes: data.len(),
                n_unknown,
            };
        }
    }
    let key = if auto { detect_printer(data) } else { opts.printer.as_str() };
    let mut doc = emulate_one(data, key, opts);
    doc.detected = key.to_string();
    doc
}

pub fn thousands(n: usize) -> String {
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
