//! Core data structures shared by the emulators, renderer, exporters and GUI.

/// Internal unit "U" = 1/1440 inch.
pub const U_PER_INCH: f64 = 1440.0;

/// Python's `round()` (round half to even) so ported arithmetic matches the original bit for bit.
pub fn pyround(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        // exactly half way: pick the even neighbour
        let t = x.trunc();
        if (t as i64) % 2 == 0 {
            t as i64
        } else {
            r as i64
        }
    } else {
        r as i64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Escp,
    Ibm,
    Star,
    ImageWriter,
    Mps,
    Okimate,
}

#[derive(Clone, Debug)]
pub struct PrinterProfile {
    pub key: &'static str,
    pub label: &'static str,
    pub family: Family,
    pub pins: i32,
    /// Nominal printed dot diameter in inches.
    pub dot_diam_in: f64,
    /// Vertical unit of ESC 3 / ESC J (units per inch).
    pub unit_v: i32,
    pub nlq_default: bool,
    pub lq24: bool,
    pub default_codepage: &'static str,
    pub description: &'static str,
}

impl PrinterProfile {
    /// The family name used for "group" decisions (ESC/P, ESC/P-like, ...).
    pub fn is_escp_group(&self) -> bool {
        matches!(self.family, Family::Escp | Family::Ibm | Family::Star)
    }
}

pub static PROFILES: [PrinterProfile; 7] = [
    PrinterProfile {
        key: "epson_fx",
        label: "Epson FX / MX / RX (9-pin ESC/P)",
        family: Family::Escp,
        pins: 9,
        dot_diam_in: 0.0125,
        unit_v: 216,
        nlq_default: false,
        lq24: false,
        default_codepage: "cp437",
        description: "Epson FX-80/85/100, MX-80, RX-80, LX-series and many clones",
    },
    PrinterProfile {
        key: "epson_lq",
        label: "Epson LQ (24-pin ESC/P)",
        family: Family::Escp,
        pins: 24,
        dot_diam_in: 0.0085,
        unit_v: 180,
        nlq_default: true,
        lq24: true,
        default_codepage: "cp437",
        description: "Epson LQ-500/800/1000/1500 and ESC/P2 printers",
    },
    PrinterProfile {
        key: "ibm",
        label: "IBM Proprinter / Graphics Printer",
        family: Family::Ibm,
        pins: 9,
        dot_diam_in: 0.0125,
        unit_v: 216,
        nlq_default: false,
        lq24: false,
        default_codepage: "cp437",
        description: "IBM Graphics Printer, Proprinter, PPDS emulation",
    },
    PrinterProfile {
        key: "star",
        label: "Star NX / NL / Gemini / SG (Epson mode)",
        family: Family::Star,
        pins: 9,
        dot_diam_in: 0.0120,
        unit_v: 216,
        nlq_default: false,
        lq24: false,
        default_codepage: "cp437",
        description: "Star Micronics 9-pin printers in their Epson-compatible mode",
    },
    PrinterProfile {
        key: "imagewriter",
        label: "Apple ImageWriter I / II",
        family: Family::ImageWriter,
        pins: 9,
        dot_diam_in: 0.0135,
        unit_v: 144,
        nlq_default: false,
        lq24: false,
        default_codepage: "mac_roman",
        description: "Apple ImageWriter (Apple II, Macintosh, Basilisk II, SheepShaver)",
    },
    PrinterProfile {
        key: "okimate",
        label: "Okidata Okimate 10 / 20 (colour thermal transfer)",
        family: Family::Okimate,
        pins: 8,
        dot_diam_in: 0.0160,
        unit_v: 216,
        nlq_default: false,
        lq24: false,
        default_codepage: "cp437",
        description: "Okimate 10/20 and Commodore MCS-820: three-pass colour ribbon (GEOS, Print Shop and friends)",
    },
    PrinterProfile {
        key: "mps",
        label: "Commodore MPS-801 / MPS-803 / 1525 (PETSCII)",
        family: Family::Mps,
        pins: 8,
        dot_diam_in: 0.0135,
        unit_v: 72,
        nlq_default: false,
        lq24: false,
        default_codepage: "ascii",
        description: "Commodore 8-bit printers (VICE, C64/C128/VIC-20 software)",
    },
];

pub fn profile(key: &str) -> Option<&'static PrinterProfile> {
    PROFILES.iter().find(|p| p.key == key)
}

/// Short button labels for the always-visible printer buttons.
pub fn short_label(key: &str) -> &'static str {
    match key {
        "epson_fx" => "Epson FX",
        "epson_lq" => "Epson LQ",
        "ibm" => "IBM",
        "star" => "Star",
        "imagewriter" => "ImageWriter",
        "okimate" => "Okimate",
        "mps" => "Commodore MPS",
        _ => "?",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmuOptions {
    /// "auto" or a profile key.
    pub printer: String,
    /// "auto" -> per printer.
    pub codepage: String,
    pub page_length_in: f64,
    pub page_width_in: f64,
    /// None = auto.
    pub cr_adds_lf: Option<bool>,
    pub lf_adds_cr: Option<bool>,
    pub force_nlq: Option<bool>,
    /// auto | upper | lower | ascii
    pub cbm_charset: String,
    /// Distance from the top edge to the first baseline.
    pub tof_offset_in: f64,
    pub honor_page_length: bool,
    pub skip_blank_ff: bool,
    pub auto_wrap: bool,
    pub wide_carriage: Option<bool>,
    /// ESC A n applies at once (true) or only after ESC 2 like a strict Epson (false).
    pub esc_a_immediate: bool,
    /// gap = authentic | fill | close
    pub mps_gap: String,
    pub split_mixed: bool,
    pub fix_capture: bool,
    /// keep | collapse | remove
    pub blank_lines: String,
}

impl Default for EmuOptions {
    fn default() -> Self {
        Self {
            printer: "auto".into(),
            codepage: "auto".into(),
            page_length_in: 11.0,
            page_width_in: 8.5,
            cr_adds_lf: None,
            lf_adds_cr: None,
            force_nlq: None,
            cbm_charset: "auto".into(),
            tof_offset_in: 0.17,
            honor_page_length: true,
            skip_blank_ff: true,
            auto_wrap: true,
            wide_carriage: None,
            esc_a_immediate: true,
            mps_gap: "gap".into(),
            split_mixed: true,
            fix_capture: true,
            blank_lines: "keep".into(),
        }
    }
}

/// All dots printed on one sheet. Coordinates are in U (1/1440 in) from the printable origin.
#[derive(Clone, Debug, Default)]
pub struct DotPage {
    pub index: usize,
    pub width_in: f64,
    pub height_in: f64,
    pub x: Vec<f32>,
    /// From the top edge, includes the top-of-form offset.
    pub y: Vec<f32>,
    pub pin: Vec<i16>,
    /// Head pass (line) number.
    pub pass_no: Vec<i32>,
    /// 0 = left to right, 1 = right to left.
    pub dirn: Vec<i8>,
    /// Which printer produced this sheet (mixed streams); empty = Document.profile.
    pub profile_key: String,
    /// Commands this sheet's data used that the emulator did not understand.
    pub unknown: usize,
    /// Ribbon colour per dot (ESC r n: 0 black 1 magenta 2 cyan 3 violet 4 yellow 5 red 6 green); None = all black.
    pub color: Option<Vec<u8>>,
}

impl DotPage {
    pub fn n_dots(&self) -> usize {
        self.x.len()
    }

    /// (min x, min y, max x, max y) in inches.
    pub fn extent_in(&self) -> (f64, f64, f64, f64) {
        if self.x.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let mut r = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (&x, &y) in self.x.iter().zip(&self.y) {
            r.0 = r.0.min(x);
            r.1 = r.1.min(y);
            r.2 = r.2.max(x);
            r.3 = r.3.max(y);
        }
        (
            r.0 as f64 / U_PER_INCH,
            r.1 as f64 / U_PER_INCH,
            r.2 as f64 / U_PER_INCH,
            r.3 as f64 / U_PER_INCH,
        )
    }
}

#[derive(Clone, Debug)]
pub struct Document {
    pub pages: Vec<DotPage>,
    pub profile: &'static PrinterProfile,
    pub detected: String,
    /// Printable width of the carriage.
    pub carriage_in: f64,
    pub notes: Vec<String>,
    /// Things in the data that look wrong (lost bytes ...).
    pub warnings: Vec<String>,
    /// Repairs applied to the raw capture before emulation.
    pub fixes: Vec<String>,
    pub n_bytes: usize,
    pub n_unknown: usize,
}

impl Document {
    pub fn n_pages(&self) -> usize {
        self.pages.len()
    }

    /// Names of the ribbon colours the data actually prints in (empty for plain black printing).
    pub fn colors_used(&self) -> Vec<&'static str> {
        let mut used = [false; 7];
        for p in &self.pages {
            if let Some(c) = &p.color {
                for &v in c {
                    if (v as usize) < 7 {
                        used[v as usize] = true;
                    }
                }
            }
        }
        const NAMES: [&str; 7] = ["black", "magenta", "cyan", "violet", "yellow", "red", "green"];
        if used.iter().skip(1).any(|&u| u) {
            (0..7).filter(|&i| used[i]).map(|i| NAMES[i]).collect()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyround_is_half_to_even() {
        assert_eq!(pyround(0.5), 0);
        assert_eq!(pyround(1.5), 2);
        assert_eq!(pyround(2.5), 2);
        assert_eq!(pyround(4.5), 4);
        assert_eq!(pyround(-0.5), 0);
        assert_eq!(pyround(-1.5), -2);
        assert_eq!(pyround(3.2), 3);
        assert_eq!(pyround(3.7), 4);
    }
}
