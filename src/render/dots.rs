//! CPU side of the renderer: dot list of one sheet -> per-dot strength and position error -> GPU instance data.
//!
//! The *behaviour* is that of the Python reference (`dot_amplitudes`): strike strength, weak / dropped strikes, ribbon
//! variation from line to line, worn pins, random position error and bidirectional head misalignment. The random numbers
//! come from our own generator, so a given seed gives a different (but equivalent) page than numpy would.
use crate::emu::model::{profile, DotPage, Document, U_PER_INCH};
use crate::emu::settings::{color_code_inks, paper_geometry, RenderSettings};

/// One pin strike as the GPU sees it (matches `DotIn` in render.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuDot {
    /// Inches on the upright sheet (carriage offset included).
    pub x: f32,
    pub y: f32,
    pub amp: f32,
    /// 0 black ribbon, 1 magenta, 2 cyan, 3 yellow.
    pub ink: u32,
}

/// Small, fast, seedable generator (SplitMix64) with uniform and normal draws.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f64()
    }
    /// Normal (Box-Muller).
    pub fn normal(&mut self, mean: f64, sd: f64) -> f64 {
        let u1 = self.f64().max(1e-12);
        let u2 = self.f64();
        mean + sd * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Everything about one sheet that does not depend on the zoom level.
pub struct SheetDots {
    /// Sorted by `y`.
    pub dots: Vec<GpuDot>,
    /// Same order as `dots`: y of every dot (for fast region culling).
    pub ys: Vec<f32>,
    /// More than one ribbon colour is in use (each is its own pass, so they sit slightly out of register).
    pub multi_ink: bool,
    pub paper_w_in: f64,
    pub paper_h_in: f64,
    pub paper_kind: u32,
    pub tractor: bool,
    pub origin_x_in: f64,
    /// Pin-strike diameter of the printer that produced the sheet, in inches (before the dot-size setting).
    pub dot_diam_in: f64,
    pub page_index: usize,
}

pub fn kind_code(kind: &str) -> u32 {
    match kind {
        "cream" => 1,
        "aged" => 2,
        "greenbar" => 3,
        "bluebar" => 4,
        _ => 0,
    }
}

/// What in the settings changes the dots themselves (so the instance buffer can be cached on it).
pub fn dots_key(s: &RenderSettings) -> (i64, i64, i64, i64, i64, bool, bool, String) {
    let q = |v: f64| (v * 10000.0).round() as i64;
    (s.seed, q(s.density), q(s.ribbon_wear), q(s.jitter), q(s.misalign), s.color_ribbon, s.clean, s.paper.clone())
}

/// Per-dot strength and position error. Returns (amp, dx_in, dy_in).
pub fn dot_amplitudes(page: &DotPage, s: &RenderSettings, n_pins: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n = page.n_dots();
    if s.clean {
        return (vec![1.0; n], vec![0.0; n], vec![0.0; n]);
    }
    let mut rng = Rng::new(((s.seed as u64).wrapping_mul(1_000_003)).wrapping_add(page.index as u64 * 7919) & 0x7FFF_FFFF);
    let dens = s.density;
    let wear = s.ribbon_wear;
    let sd = 0.06 + 0.06 * (1.0 - dens);
    let weak_frac = (1.0 - dens) * 0.8;
    let drop_frac = (1.0 - dens) * (1.0 - dens) * 0.25;
    let mut amp: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n {
        let mut a = rng.normal(1.0, sd);
        if rng.f64() < weak_frac {
            a *= rng.uniform(0.2, 0.65);
        }
        if rng.f64() < drop_frac {
            a = 0.0;
        }
        amp.push(a);
    }
    // line-to-line ink variation: one gain per head pass
    let mut pass_ids: Vec<i32> = page.pass_no.clone();
    pass_ids.sort_unstable();
    pass_ids.dedup();
    let pass_gain: Vec<f64> = pass_ids.iter().map(|_| rng.normal(0.0, 0.12 * wear + 0.02).exp()).collect();
    let pass_off: Vec<f64> = pass_ids.iter().map(|_| rng.normal(0.0, 0.0012 * s.misalign)).collect();
    // per-pin wear: some pins hit softer
    let max_pin = page.pin.iter().copied().max().unwrap_or(0).max(0) as usize + 1;
    let np = n_pins.max(max_pin).max(1);
    let mut pin_gain: Vec<f64> = (0..np).map(|_| rng.normal(0.0, 0.10 * wear + 0.02).exp()).collect();
    let weak_p = 0.08 * wear + 0.03 * (1.0 - dens);
    for g in pin_gain.iter_mut() {
        if rng.f64() < weak_p {
            *g *= rng.uniform(0.45, 0.8);
        }
    }
    let jit = 0.0016 * s.jitter;
    let mis = 0.010 * s.misalign;
    let mut out_amp: Vec<f32> = Vec::with_capacity(n);
    let mut dx = Vec::with_capacity(n);
    let mut dy = Vec::with_capacity(n);
    for i in 0..n {
        let pi = pass_ids.binary_search(&page.pass_no[i]).unwrap_or(0);
        let g = pass_gain[pi] * pin_gain[page.pin[i].max(0) as usize];
        out_amp.push((amp[i] * g).max(0.0).min(2.0) as f32);
        let sgn = if page.dirn[i] == 1 { 1.0 } else { -1.0 };
        dx.push((rng.normal(0.0, jit) + sgn * mis + pass_off[pi]) as f32);
        dy.push((rng.normal(0.0, jit * 0.8) + sgn * 0.25 * mis) as f32);
    }
    (out_amp, dx, dy)
}

/// Build the GPU dots of one sheet.
pub fn build_sheet(page: &DotPage, doc: &Document, s: &RenderSettings) -> SheetDots {
    let prof = profile(&page.profile_key).unwrap_or(doc.profile);
    let (pw, ph, kind, tractor, origin_x) = paper_geometry(&s.paper, page.width_in, page.height_in, doc.carriage_in);
    let (amp, dx, dy) = dot_amplitudes(page, s, prof.pins as usize);
    let n = page.n_dots();
    let colours = if s.color_ribbon { page.color.as_ref() } else { None };
    let mut dots: Vec<GpuDot> = Vec::with_capacity(n + n / 16);
    let mut used = [false; 4];
    for i in 0..n {
        let x = (page.x[i] as f64 / U_PER_INCH + origin_x) as f32 + dx[i];
        let y = (page.y[i] as f64 / U_PER_INCH) as f32 + dy[i];
        match colours {
            None => {
                used[0] = true;
                dots.push(GpuDot { x, y, amp: amp[i], ink: 0 });
            }
            Some(c) => {
                for &ink in color_code_inks(c[i]) {
                    let slot = if ink == 4 { 3 } else { ink as u32 };
                    used[slot as usize] = true;
                    dots.push(GpuDot { x, y, amp: amp[i], ink: slot });
                }
            }
        }
    }
    dots.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
    let ys = dots.iter().map(|d| d.y).collect();
    SheetDots {
        dots,
        ys,
        multi_ink: used.iter().filter(|&&u| u).count() > 1,
        paper_w_in: pw,
        paper_h_in: ph,
        paper_kind: kind_code(kind),
        tractor,
        origin_x_in: origin_x,
        dot_diam_in: prof.dot_diam_in,
        page_index: page.index,
    }
}

impl SheetDots {
    /// Index range of the dots whose y lies in [y0, y1] inches.
    pub fn y_range(&self, y0: f32, y1: f32) -> (usize, usize) {
        let lo = self.ys.partition_point(|&y| y < y0);
        let hi = self.ys.partition_point(|&y| y <= y1);
        (lo, hi.max(lo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_normal_ish() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(a.next_u64(), b.next_u64());
        let mut r = Rng::new(1);
        let n = 20000;
        let (mut s, mut s2) = (0.0, 0.0);
        for _ in 0..n {
            let v = r.normal(1.0, 0.1);
            s += v;
            s2 += v * v;
        }
        let m = s / n as f64;
        let sd = (s2 / n as f64 - m * m).sqrt();
        assert!((m - 1.0).abs() < 0.01 && (sd - 0.1).abs() < 0.01, "{m} {sd}");
    }

    #[test]
    fn gpu_dot_is_16_bytes() {
        assert_eq!(std::mem::size_of::<GpuDot>(), 16);
    }
}
