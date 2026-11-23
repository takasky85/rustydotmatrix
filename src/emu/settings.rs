//! Render settings, look presets, ribbon colours and paper presets.
use super::model::pyround;

pub type Rgb = (u8, u8, u8);

/// key -> (label, RGB of the ink as it lands on white paper)
pub static RIBBONS: [(&str, &str, Rgb); 7] = [
    ("black", "Black", (22, 22, 28)),
    ("blue_black", "Blue-black", (18, 24, 62)),
    ("blue", "Blue", (22, 40, 140)),
    ("purple", "Purple", (74, 28, 110)),
    ("red", "Red", (170, 26, 30)),
    ("green", "Green", (20, 96, 52)),
    ("brown", "Brown", (88, 52, 30)),
];

/// Ink sets for the colour ribbon: (key, label, [magenta, cyan, yellow] as they land on white paper).
pub static INK_SETS: [(&str, &str, [Rgb; 3]); 5] = [
    ("ribbon", "Standard colour ribbon", [(206, 34, 124), (22, 138, 210), (250, 222, 34)]),
    ("vivid", "Vivid (fresh, saturated)", [(226, 16, 128), (0, 156, 228), (255, 232, 0)]),
    ("process", "Process inks (print-shop CMY)", [(236, 0, 140), (0, 174, 239), (255, 242, 0)]),
    ("faded", "Faded (old ribbon)", [(186, 72, 122), (74, 134, 180), (226, 206, 96)]),
    ("thermal", "Thermal transfer (Okimate waxy)", [(214, 46, 110), (36, 120, 196), (246, 210, 48)]),
];

/// ESC r n -> the inks it prints with (3/5/6 are two inks struck on top of each other). Ink codes: 0 black, 1 magenta, 2 cyan, 4 yellow.
pub fn color_code_inks(code: u8) -> &'static [u8] {
    match code {
        0 => &[0],
        1 => &[1],
        2 => &[2],
        3 => &[1, 2],
        4 => &[4],
        5 => &[1, 4],
        6 => &[2, 4],
        _ => &[0],
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Paper {
    pub key: &'static str,
    pub label: &'static str,
    /// None = auto
    pub width_in: Option<f64>,
    pub height_in: Option<f64>,
    pub kind: &'static str,
    pub tractor: bool,
}

pub static PAPERS: [Paper; 9] = [
    Paper { key: "auto", label: "Auto (continuous 8.5 in)", width_in: None, height_in: None, kind: "white", tractor: false },
    Paper { key: "letter", label: "US Letter, white", width_in: Some(8.5), height_in: Some(11.0), kind: "white", tractor: false },
    Paper { key: "a4", label: "A4, white", width_in: Some(8.27), height_in: Some(11.69), kind: "white", tractor: false },
    Paper { key: "tractor", label: "Tractor feed 9½ × 11, white", width_in: Some(9.5), height_in: Some(11.0), kind: "white", tractor: true },
    Paper { key: "tractor_cream", label: "Tractor feed 9½ × 11, cream", width_in: Some(9.5), height_in: Some(11.0), kind: "cream", tractor: true },
    Paper { key: "tractor_aged", label: "Tractor feed 9½ × 11, aged", width_in: Some(9.5), height_in: Some(11.0), kind: "aged", tractor: true },
    Paper { key: "greenbar", label: "Greenbar 9½ × 11", width_in: Some(9.5), height_in: Some(11.0), kind: "greenbar", tractor: true },
    Paper { key: "greenbar_wide", label: "Greenbar 14⅞ × 11", width_in: Some(14.875), height_in: Some(11.0), kind: "greenbar", tractor: true },
    Paper { key: "bluebar", label: "Blue-bar 9½ × 11", width_in: Some(9.5), height_in: Some(11.0), kind: "bluebar", tractor: true },
];

pub fn paper(key: &str) -> &'static Paper {
    PAPERS.iter().find(|p| p.key == key).unwrap_or(&PAPERS[3])
}

/// The footprint of one pin strike: key, label, description.
pub static PIN_SHAPES: [(&str, &str, &str); 5] = [
    ("round", "Round (new pin)", "A clean round dot, like a fresh head."),
    ("oval", "Oval (head moving)", "Slightly wider than tall: the head keeps moving while the pin strikes. Typical of 9-pin printers."),
    ("rough", "Rough (worn pin)", "Ragged, uneven edges where the pin tip is worn and the ribbon fabric breaks the ink up."),
    ("square", "Square (blunt pin)", "Rounded-square dots from a flat, blunted pin tip."),
    ("ring", "Ring (heavy strike)", "Darker rim with a pale centre: a hard strike squeezes the ink to the edge."),
];

pub const DEFAULT_CUSTOM_INKS: &str = "#ce227c,#168ad2,#fade22";

/// '#rrggbb,#rrggbb,#rrggbb' -> three colours, or None if it is not three valid colours.
pub fn parse_inks(text: &str) -> Option<[Rgb; 3]> {
    let parts: Vec<&str> = text.split(',').map(|p| p.trim().trim_start_matches('#')).collect();
    if parts.len() != 3 || parts.iter().any(|p| p.len() != 6) {
        return None;
    }
    let mut out = [(0u8, 0u8, 0u8); 3];
    for (k, p) in parts.iter().enumerate() {
        let v = |i: usize| u8::from_str_radix(&p[i..i + 2], 16).ok();
        out[k] = (v(0)?, v(2)?, v(4)?);
    }
    Some(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderSettings {
    pub dpi: i32,
    /// pin diameter multiplier (0.3 .. 2.0)
    pub dot_size: f64,
    /// 1 = every dot strikes fully, lower = weak/missing strikes
    pub density: f64,
    /// ribbon ink load (darkness / colour richness)
    pub saturation: f64,
    pub ribbon: String,
    pub paper: String,
    /// streaks, line-to-line ink variation, weak pins
    pub ribbon_wear: f64,
    pub pin_shape: String,
    /// bidirectional head misalignment
    pub misalign: f64,
    pub paper_texture: f64,
    /// emboss / dent shading around strikes
    pub impact: f64,
    /// random dot position error
    pub jitter: f64,
    pub show_holes: bool,
    /// key of INK_SETS, or "custom"
    pub ink_set: String,
    pub custom_inks: String,
    /// honour ESC r colour changes (off = everything prints in the black ribbon)
    pub color_ribbon: bool,
    /// view/export rotation in degrees clockwise: 0, 90, 180, 270
    pub rotation: i32,
    pub seed: i64,
    /// Show the dots on bare white paper with no ribbon/paper simulation ("clean dots" look).
    pub clean: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            dpi: 144,
            dot_size: 1.0,
            density: 0.88,
            saturation: 0.90,
            ribbon: "black".into(),
            paper: "tractor".into(),
            ribbon_wear: 0.35,
            pin_shape: "round".into(),
            misalign: 0.35,
            paper_texture: 0.5,
            impact: 0.5,
            jitter: 0.4,
            show_holes: true,
            ink_set: "ribbon".into(),
            custom_inks: DEFAULT_CUSTOM_INKS.into(),
            color_ribbon: true,
            rotation: 0,
            seed: 1984,
            clean: false,
        }
    }
}

fn clamp01(v: f64) -> f64 {
    v.max(0.0).min(1.0)
}

impl RenderSettings {
    pub fn clamp(&mut self) -> &mut Self {
        self.dpi = self.dpi.max(36).min(2400);
        self.dot_size = self.dot_size.max(0.3).min(2.0);
        self.density = clamp01(self.density);
        self.ribbon_wear = clamp01(self.ribbon_wear);
        self.misalign = clamp01(self.misalign);
        self.paper_texture = clamp01(self.paper_texture);
        self.impact = clamp01(self.impact);
        self.jitter = clamp01(self.jitter);
        self.saturation = self.saturation.max(0.05).min(1.5);
        self.rotation = (pyround(self.rotation as f64 / 90.0) * 90).rem_euclid(360) as i32;
        if !PIN_SHAPES.iter().any(|p| p.0 == self.pin_shape) {
            self.pin_shape = "round".into();
        }
        if self.ink_set != "custom" && !INK_SETS.iter().any(|p| p.0 == self.ink_set) {
            self.ink_set = "ribbon".into();
        }
        if self.ink_set == "custom" && parse_inks(&self.custom_inks).is_none() {
            self.custom_inks = DEFAULT_CUSTOM_INKS.into();
        }
        if !RIBBONS.iter().any(|r| r.0 == self.ribbon) {
            self.ribbon = "black".into();
        }
        if !PAPERS.iter().any(|p| p.key == self.paper) {
            self.paper = "tractor".into();
        }
        self
    }

    /// RGB of ribbon ink 1 (magenta), 2 (cyan) or 4 (yellow) as it lands on white paper.
    pub fn ink_rgb(&self, ink: u8) -> Rgb {
        let idx = match ink {
            1 => 0,
            2 => 1,
            _ => 2,
        };
        if self.ink_set == "custom" {
            if let Some(t) = parse_inks(&self.custom_inks) {
                return t[idx];
            }
        }
        INK_SETS.iter().find(|s| s.0 == self.ink_set).unwrap_or(&INK_SETS[0]).2[idx]
    }

    pub fn ribbon_rgb(&self) -> Rgb {
        RIBBONS.iter().find(|r| r.0 == self.ribbon).unwrap_or(&RIBBONS[0]).2
    }

    // ---- persistence (plain `key=value` lines, no external crates)
    pub fn to_kv(&self) -> String {
        let mut s = String::new();
        let mut put = |k: &str, v: String| {
            s.push_str(k);
            s.push('=');
            s.push_str(&v);
            s.push('\n');
        };
        put("dpi", self.dpi.to_string());
        put("dot_size", self.dot_size.to_string());
        put("density", self.density.to_string());
        put("saturation", self.saturation.to_string());
        put("ribbon", self.ribbon.clone());
        put("paper", self.paper.clone());
        put("ribbon_wear", self.ribbon_wear.to_string());
        put("pin_shape", self.pin_shape.clone());
        put("misalign", self.misalign.to_string());
        put("paper_texture", self.paper_texture.to_string());
        put("impact", self.impact.to_string());
        put("jitter", self.jitter.to_string());
        put("show_holes", self.show_holes.to_string());
        put("ink_set", self.ink_set.clone());
        put("custom_inks", self.custom_inks.clone());
        put("color_ribbon", self.color_ribbon.to_string());
        put("rotation", self.rotation.to_string());
        put("seed", self.seed.to_string());
        put("clean", self.clean.to_string());
        s
    }

    pub fn set_kv(&mut self, k: &str, v: &str) {
        let f = |v: &str| v.parse::<f64>().ok();
        let b = |v: &str| v == "true";
        match k {
            "dpi" => self.dpi = v.parse().unwrap_or(self.dpi),
            "dot_size" => self.dot_size = f(v).unwrap_or(self.dot_size),
            "density" => self.density = f(v).unwrap_or(self.density),
            "saturation" => self.saturation = f(v).unwrap_or(self.saturation),
            "ribbon" => self.ribbon = v.into(),
            "paper" => self.paper = v.into(),
            "ribbon_wear" => self.ribbon_wear = f(v).unwrap_or(self.ribbon_wear),
            "pin_shape" => self.pin_shape = v.into(),
            "misalign" => self.misalign = f(v).unwrap_or(self.misalign),
            "paper_texture" => self.paper_texture = f(v).unwrap_or(self.paper_texture),
            "impact" => self.impact = f(v).unwrap_or(self.impact),
            "jitter" => self.jitter = f(v).unwrap_or(self.jitter),
            "show_holes" => self.show_holes = b(v),
            "ink_set" => self.ink_set = v.into(),
            "custom_inks" => self.custom_inks = v.into(),
            "color_ribbon" => self.color_ribbon = b(v),
            "rotation" => self.rotation = v.parse().unwrap_or(0),
            "seed" => self.seed = v.parse().unwrap_or(self.seed),
            "clean" => self.clean = b(v),
            _ => {}
        }
    }

    pub fn from_kv(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                s.set_kv(k.trim(), v.trim());
            }
        }
        s.clamp();
        s
    }
}

/// (paper_w_in, paper_h_in, kind, tractor, origin_x_in) for a sheet.
pub fn paper_geometry(key: &str, page_w_in: f64, page_h_in: f64, carriage_in: f64) -> (f64, f64, &'static str, bool, f64) {
    let p = paper(key);
    let (mut pw, ph) = match p.width_in {
        Some(w) => (w, p.height_in.unwrap_or(page_h_in)),
        None => (if carriage_in > 9.0 { 14.875 } else { page_w_in }, page_h_in),
    };
    if carriage_in > pw - if p.tractor { 1.0 } else { 0.0 } {
        pw = pw.max(carriage_in + if p.tractor { 1.0 } else { 0.25 });
    }
    (pw, ph, p.kind, p.tractor, (pw - carriage_in) / 2.0)
}

/// A named "look": the settings it changes. `pin_shape: None` leaves the current pin shape alone.
pub struct LookPreset {
    pub name: &'static str,
    pub pin_shape: Option<&'static str>,
    pub dot_size: f64,
    pub density: f64,
    pub saturation: f64,
    pub ribbon_wear: f64,
    pub misalign: f64,
    pub jitter: f64,
    pub paper: &'static str,
    pub impact: f64,
    pub paper_texture: f64,
}

pub static LOOK_PRESETS: [LookPreset; 6] = [
    LookPreset { name: "Fresh ribbon", pin_shape: None, dot_size: 1.0, density: 0.97, saturation: 1.1, ribbon_wear: 0.1, misalign: 0.15, jitter: 0.2, paper: "tractor", impact: 0.5, paper_texture: 0.35 },
    LookPreset { name: "Office workhorse", pin_shape: Some("oval"), dot_size: 1.0, density: 0.88, saturation: 0.8, ribbon_wear: 0.35, misalign: 0.35, jitter: 0.4, paper: "tractor", impact: 0.5, paper_texture: 0.5 },
    LookPreset { name: "Worn ribbon", pin_shape: Some("rough"), dot_size: 0.95, density: 0.7, saturation: 0.5, ribbon_wear: 0.65, misalign: 0.5, jitter: 0.55, paper: "tractor_cream", impact: 0.6, paper_texture: 0.6 },
    LookPreset { name: "Dying ribbon", pin_shape: Some("rough"), dot_size: 0.9, density: 0.5, saturation: 0.28, ribbon_wear: 0.9, misalign: 0.7, jitter: 0.7, paper: "tractor_aged", impact: 0.7, paper_texture: 0.75 },
    LookPreset { name: "Greenbar mainframe", pin_shape: Some("oval"), dot_size: 1.05, density: 0.85, saturation: 0.9, ribbon_wear: 0.45, misalign: 0.4, jitter: 0.4, paper: "greenbar", impact: 0.5, paper_texture: 0.5 },
    LookPreset { name: "Clean (for PDF)", pin_shape: None, dot_size: 1.0, density: 1.0, saturation: 1.2, ribbon_wear: 0.0, misalign: 0.0, jitter: 0.0, paper: "letter", impact: 0.2, paper_texture: 0.0 },
];

impl RenderSettings {
    pub fn apply_look(&mut self, p: &LookPreset) {
        if let Some(s) = p.pin_shape {
            self.pin_shape = s.into();
        }
        self.dot_size = p.dot_size;
        self.density = p.density;
        self.saturation = p.saturation;
        self.ribbon_wear = p.ribbon_wear;
        self.misalign = p.misalign;
        self.jitter = p.jitter;
        self.paper = p.paper.into();
        self.impact = p.impact;
        self.paper_texture = p.paper_texture;
        self.clamp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geometry_matches_python_examples() {
        let (pw, ph, kind, tractor, ox) = paper_geometry("tractor", 8.5, 11.0, 8.0);
        assert_eq!((pw, ph, kind, tractor), (9.5, 11.0, "white", true));
        assert!((ox - 0.75).abs() < 1e-12);
        let (pw, _, _, _, _) = paper_geometry("auto", 8.5, 11.0, 13.6);
        assert!((pw - 14.875).abs() < 1e-12);
    }
    #[test]
    fn clamp_and_kv_roundtrip() {
        let mut s = RenderSettings::default();
        s.rotation = 450;
        s.dpi = 5000;
        s.clamp();
        assert_eq!((s.rotation, s.dpi), (90, 2400));
        let t = RenderSettings::from_kv(&s.to_kv());
        assert_eq!(s, t);
    }
    #[test]
    fn inks() {
        assert_eq!(parse_inks("#ce227c,#168ad2,#fade22").unwrap()[0], (206, 34, 124));
        assert!(parse_inks("#ce227c,#168ad2").is_none());
    }
}
