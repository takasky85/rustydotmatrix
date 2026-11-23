//! TrueType rasteriser for the near-letter-quality (NLQ / LQ) glyphs: a port of `ttf_bits` from the Python app.
//!
//! The bundled font is scaled so that a capital H is exactly the cap height of the printer's NLQ grid, drawn with its advance
//! centred in the cell, squeezed if it is wider than the cell, area-averaged down to the dot grid and thresholded.

use std::collections::HashMap;
use std::sync::Mutex;

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};

use crate::emu::glyphs::{Bits, Face, FontRaster, GridSpec};
use crate::emu::model::pyround;

static ROMAN: &[u8] = include_bytes!("../assets/fonts/LiberationSerif-Regular.ttf");
static SANS: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Regular.ttf");
static MONO: &[u8] = include_bytes!("../assets/fonts/LiberationMono-Regular.ttf");
static SYMBOLS: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Which {
    Roman,
    Sans,
    Mono,
    Symbols,
}

struct Loaded {
    font: FontRef<'static>,
    /// capital H height as a share of the em
    cap_ratio: f32,
    upem: f32,
}

pub struct AbRaster {
    fonts: HashMap<Which, Loaded>,
    /// (face, cap_px) -> PxScale of the calibrated font
    sizes: Mutex<HashMap<(Which, i32), f32>>,
}

fn px_scale(l: &Loaded, ppem: f32) -> PxScale {
    // ab_glyph's PxScale is the height of ascent+descent; PIL's size is pixels per em
    PxScale::from(ppem * l.font.height_unscaled() / l.upem)
}

impl AbRaster {
    pub fn new() -> Self {
        let mut fonts = HashMap::new();
        for (w, data) in [(Which::Roman, ROMAN), (Which::Sans, SANS), (Which::Mono, MONO), (Which::Symbols, SYMBOLS)] {
            let font = FontRef::try_from_slice(data).expect("bundled font");
            let upem = font.units_per_em().unwrap_or(1000.0);
            let mut l = Loaded { font, cap_ratio: 0.7, upem };
            // PIL: probe = truetype(path, 200); bb = probe.getbbox("H", anchor="ls"); cap_ratio = (bb[3]-bb[1]) / 200
            let sc = px_scale(&l, 200.0);
            let h = l.font.glyph_id('H').with_scale_and_position(sc, point(0.0, 0.0));
            if let Some(o) = l.font.outline_glyph(h) {
                let b = o.px_bounds();
                l.cap_ratio = (b.max.y.ceil() - b.min.y.floor()) / 200.0;
            }
            fonts.insert(w, l);
        }
        Self { fonts, sizes: Mutex::new(HashMap::new()) }
    }

    fn which(ch: char, face: Face) -> Which {
        if (ch as u32) > 0x2FF {
            return Which::Symbols;
        }
        match face {
            Face::Roman => Which::Roman,
            Face::Sans => Which::Sans,
            Face::Courier => Which::Mono,
        }
    }

    /// PIL: size = max(4, int(round(cap_px / cap_ratio)))
    fn ppem(&self, w: Which, cap_px: i32) -> f32 {
        let mut m = self.sizes.lock().unwrap();
        *m.entry((w, cap_px)).or_insert_with(|| {
            let l = &self.fonts[&w];
            (4i64.max(pyround(cap_px as f64 / l.cap_ratio as f64))) as f32
        })
    }

    fn advance(&self, w: Which, ch: char, cap_px: i32) -> f32 {
        let l = &self.fonts[&w];
        let sc = px_scale(l, self.ppem(w, cap_px));
        l.font.as_scaled(sc).h_advance(l.font.glyph_id(ch))
    }
}

impl Default for AbRaster {
    fn default() -> Self {
        Self::new()
    }
}

/// PIL-style Lanczos (a = 3) horizontal resize of a w x h float image to `nw` x h.
fn lanczos_resize_w(src: &[f32], w: usize, h: usize, nw: usize) -> Vec<f32> {
    fn lz(x: f32) -> f32 {
        if x == 0.0 {
            1.0
        } else if x.abs() >= 3.0 {
            0.0
        } else {
            let px = std::f32::consts::PI * x;
            3.0 * px.sin() * (px / 3.0).sin() / (px * px)
        }
    }
    let scale = w as f32 / nw as f32;
    let filterscale = scale.max(1.0);
    let support = 3.0 * filterscale;
    let mut out = vec![0.0f32; nw * h];
    for x in 0..nw {
        let center = (x as f32 + 0.5) * scale;
        let xmin = ((center - support + 0.5).floor() as i64).max(0) as usize;
        let xmax = (((center + support + 0.5).floor()) as i64).min(w as i64) as usize;
        let mut ws: Vec<f32> = (xmin..xmax).map(|xx| lz((xx as f32 - center + 0.5) / filterscale)).collect();
        let sum: f32 = ws.iter().sum();
        if sum != 0.0 {
            for v in ws.iter_mut() {
                *v /= sum;
            }
        }
        for y in 0..h {
            let mut acc = 0.0;
            for (k, wgt) in ws.iter().enumerate() {
                acc += src[y * w + xmin + k] * wgt;
            }
            out[y * nw + x] = acc.clamp(0.0, 1.0);
        }
    }
    out
}

impl FontRaster for AbRaster {
    fn bits(&self, ch: char, face: Face, spec: &GridSpec, cols: usize, threshold: f32) -> Bits {
        let w = Self::which(ch, face);
        let sx = 1.max(pyround(spec.gx / 2.0)) as usize;
        let sy = 1.max(pyround(spec.gy / 2.0)) as usize;
        let (wp, hp) = (cols * sx, spec.rows * sy);
        let cap_px = spec.cap_rows * sy as i32;
        let l = &self.fonts[&w];
        let sc = px_scale(l, self.ppem(w, cap_px));
        let base_y = ((spec.base_row + 1) as usize * sy) as f32;
        let adv = l.font.as_scaled(sc).h_advance(l.font.glyph_id(ch));
        let pad = adv as usize + 8;
        let (cw, chh) = (wp + 2 * pad, hp + 2 * sy);
        let mut canvas = vec![0.0f32; cw * chh];
        // draw with its advance centred on the cell centre; baseline at base_y + sy
        let ox = pad as f32 + wp as f32 / 2.0 - adv / 2.0;
        let g = l.font.glyph_id(ch).with_scale_and_position(sc, point(ox, base_y + sy as f32));
        if let Some(o) = l.font.outline_glyph(g) {
            let b = o.px_bounds();
            o.draw(|x, y, c| {
                let (px, py) = (b.min.x as i64 + x as i64, b.min.y as i64 + y as i64);
                if px >= 0 && py >= 0 && (px as usize) < cw && (py as usize) < chh {
                    let i = py as usize * cw + px as usize;
                    canvas[i] = (canvas[i] + c).min(1.0);
                }
            });
        }
        // squeeze horizontally if the ink is wider than the cell
        let (mut x0, mut x1) = (usize::MAX, 0usize);
        let (mut y0, mut y1) = (usize::MAX, 0usize);
        for y in 0..chh {
            for x in 0..cw {
                if canvas[y * cw + x] > 0.0 {
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                    y0 = y0.min(y);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x1 > x0 {
            let ink_w = x1 - x0;
            if ink_w as f32 > wp as f32 * 0.98 {
                let scale = wp as f32 * 0.98 / ink_w as f32;
                let nw = 1.max(pyround(ink_w as f64 * scale as f64)) as usize;
                // crop the bbox, resize its width, paste back centred on the cell
                let mut crop = vec![0.0f32; ink_w * (y1 - y0)];
                for y in y0..y1 {
                    crop[(y - y0) * ink_w..(y - y0 + 1) * ink_w].copy_from_slice(&canvas[y * cw + x0..y * cw + x1]);
                }
                let res = lanczos_resize_w(&crop, ink_w, y1 - y0, nw);
                canvas.iter_mut().for_each(|v| *v = 0.0);
                let cx = pad as f32 + wp as f32 / 2.0;
                let left = pyround((cx - nw as f32 / 2.0) as f64);
                for y in y0..y1 {
                    for x in 0..nw {
                        let tx = left + x as i64;
                        if tx >= 0 && (tx as usize) < cw {
                            canvas[y * cw + tx as usize] = res[(y - y0) * nw + x];
                        }
                    }
                }
            }
        }
        // crop (pad, sy, pad+W, sy+H) and area-average each dot cell
        let mut bits = Bits::zeros(spec.rows, cols);
        let area = (sx * sy) as f32;
        for r in 0..spec.rows {
            for c in 0..cols {
                let mut acc = 0.0f32;
                for yy in 0..sy {
                    for xx in 0..sx {
                        acc += canvas[(sy + r * sy + yy) * cw + pad + c * sx + xx];
                    }
                }
                if acc / area >= threshold {
                    bits.d[r * cols + c] = 1;
                }
            }
        }
        bits
    }

    fn advance_px(&self, ch: char, face: Face, cap_px: i32) -> f32 {
        self.advance(Self::which(ch, face), ch, cap_px)
    }
}
