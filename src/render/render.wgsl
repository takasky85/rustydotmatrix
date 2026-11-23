// RetroImpact GPU renderer.
//
//  1. fs_cov    : every dot is an instanced quad; the fragment shader evaluates the soft pin-strike kernel analytically and
//                 adds it into a coverage texture (one channel per ink: black ribbon, magenta, cyan, yellow).
//  2. fs_ink    : coverage -> ink transfer  a = 1 - exp(-gain * coverage * fibre texture), averaged over the supersamples.
//  3. fs_blur_h / fs_blur_v : Gaussian blurs of the ink (two widths) for the impact (dent) shading.
//  4. fs_final  : procedural paper (stock, mottling, greenbar, tractor holes) * inks * impact shading.
//
// All geometry is in physical units (inches), so the page looks the same at every zoom level.

struct Params {
    reg: vec4<f32>,      // extended region (displayed px): x0, y0, w, h
    out_reg: vec4<f32>,  // output region (displayed px):   x0, y0, w, h
    pg: vec4<f32>,       // paper_w_in, paper_h_in, ppi, ss
    pgpx: vec4<f32>,     // upright page size in px (w, h), rotation quarter-turns, multi-ink flag
    kern: vec4<f32>,     // kernel radius (cov px), softness (cov px), rim, shape (0 round 1 oval 2 square 3 ring)
    inkp: vec4<f32>,     // gain, ribbon_wear, impact, blur sigma 1 (px)
    paperp: vec4<f32>,   // texture amount, blur sigma 2 (px), misregistration (inches), clean (1 = no paper / ribbon simulation)
    flags: vec4<u32>,    // paper kind, tractor, holes, rough
    seeds_a: vec4<u32>,  // ink-pass noise seeds (fibre, cloud, grain, -)
    seeds_b: vec4<u32>,  // paper noise seeds (mottle, cloud, fibre, -)
    tone0: vec4<f32>,    // black ribbon rgb
    tone1: vec4<f32>,    // magenta
    tone2: vec4<f32>,    // cyan
    tone3: vec4<f32>,    // yellow
};

@group(0) @binding(0) var<uniform> P: Params;

// ------------------------------------------------------------------------------------------------ helpers

fn rot_q() -> i32 { return i32(P.pgpx.z + 0.5); }

// upright page px (U, V) -> displayed page px
fn up_to_disp(uv: vec2<f32>) -> vec2<f32> {
    let q = rot_q();
    if (q == 1) { return vec2<f32>(P.pgpx.y - uv.y, uv.x); }
    if (q == 2) { return vec2<f32>(P.pgpx.x - uv.x, P.pgpx.y - uv.y); }
    if (q == 3) { return vec2<f32>(uv.y, P.pgpx.x - uv.x); }
    return uv;
}

// displayed page px -> upright page px
fn disp_to_up(d: vec2<f32>) -> vec2<f32> {
    let q = rot_q();
    if (q == 1) { return vec2<f32>(d.y, P.pgpx.y - d.x); }
    if (q == 2) { return vec2<f32>(P.pgpx.x - d.x, P.pgpx.y - d.y); }
    if (q == 3) { return vec2<f32>(P.pgpx.x - d.y, d.x); }
    return d;
}

// a displayed-space vector -> upright-space vector (no translation)
fn vec_to_up(v: vec2<f32>) -> vec2<f32> {
    let q = rot_q();
    if (q == 1) { return vec2<f32>(v.y, -v.x); }
    if (q == 2) { return vec2<f32>(-v.x, -v.y); }
    if (q == 3) { return vec2<f32>(-v.y, v.x); }
    return v;
}

fn hash_u(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn hash3(ix: i32, iy: i32, seed: u32) -> u32 {
    return hash_u(hash_u(bitcast<u32>(ix) ^ hash_u(bitcast<u32>(iy) + seed)) ^ seed);
}

fn rnd01(h: u32) -> f32 { return f32(h >> 8u) * (1.0 / 16777216.0); }

// approximately normal (Irwin-Hall of three uniforms), unit variance
fn gauss_hash(ix: i32, iy: i32, seed: u32) -> f32 {
    let h = hash3(ix, iy, seed);
    let a = rnd01(h);
    let b = rnd01(hash_u(h ^ 0x9e3779b9u));
    let c = rnd01(hash_u(h + 0x85ebca6bu));
    return (a + b + c - 1.5) * 2.0;
}

// Smooth noise with unit variance and a correlation length of about `sigma` inches: Gaussian-weighted sum of random values on
// a lattice, normalised by the weights so that the variance is exactly 1 everywhere.
fn snoise(p: vec2<f32>, sigma: f32, seed: u32) -> f32 {
    let h = sigma * 1.2;
    let q = p / h;
    let c0 = floor(q);
    var sum = 0.0;
    var ws = 0.0;
    let inv2s = 1.0 / (2.0 * sigma * sigma);
    for (var j = -1; j <= 2; j++) {
        for (var i = -1; i <= 2; i++) {
            let c = c0 + vec2<f32>(f32(i), f32(j));
            let d = (c - q) * h;
            let w = exp(-dot(d, d) * inv2s);
            sum += w * gauss_hash(i32(c.x), i32(c.y), seed);
            ws += w * w;
        }
    }
    return sum * inverseSqrt(ws);
}

fn full_tri(vi: u32) -> vec4<f32> {
    let x = f32(i32(vi & 1u) * 4 - 1);
    let y = f32(i32(vi >> 1u) * 4 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

// ------------------------------------------------------------------------------------------------ 1. coverage

struct DotIn {
    @location(0) pos: vec2<f32>,   // inches on the upright sheet
    @location(1) amp: f32,
    @location(2) ink: u32,
};

struct CovV {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) centre: vec2<f32>,   // dot centre in coverage px
    @location(1) @interpolate(flat) amp: f32,
    @location(2) @interpolate(flat) ink: u32,
};

fn kernel_half() -> f32 { return ceil(max(P.kern.x, 0.6) * 1.9) + 1.0; }

fn ink_offset(ink: u32) -> vec2<f32> {
    // each ribbon colour is its own pass, so they sit a hair out of register with each other (inches, upright sheet)
    if (P.pgpx.w < 0.5) { return vec2<f32>(0.0, 0.0); }
    if (ink == 1u) { return vec2<f32>(1.0, -0.5) * P.paperp.z; }
    if (ink == 2u) { return vec2<f32>(-1.0, 0.6) * P.paperp.z; }
    if (ink == 3u) { return vec2<f32>(0.4, 1.0) * P.paperp.z; }
    return vec2<f32>(0.0, 0.0);
}

@vertex
fn vs_cov(@builtin(vertex_index) vi: u32, d: DotIn) -> CovV {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0));
    let ppi = P.pg.z;
    let ss = P.pg.w;
    let up_px = (d.pos + ink_offset(d.ink)) * ppi;
    let disp = up_to_disp(up_px);
    let centre = (disp - P.reg.xy) * ss;             // coverage px
    let hw = kernel_half() + 1.0;
    let pos = centre + corners[vi] * hw;
    var o: CovV;
    o.clip = vec4<f32>(pos.x / (P.reg.z * ss) * 2.0 - 1.0, 1.0 - pos.y / (P.reg.w * ss) * 2.0, 0.0, 1.0);
    o.centre = centre;
    o.amp = d.amp;
    o.ink = d.ink;
    return o;
}

// unit-peak ink coverage of one pin strike at upright offset (du, dv) px from the dot centre
fn pin_kernel(du: f32, dv: f32) -> f32 {
    let r = max(P.kern.x, 0.6);
    let hw = kernel_half();
    if (abs(du) > hw + 0.5 || abs(dv) > hw + 0.5) { return 0.0; }
    let shape = i32(P.kern.w + 0.5);
    var d = sqrt(du * du + dv * dv);
    if (shape == 1) {
        d = sqrt((du / 1.14) * (du / 1.14) + (dv / 0.88) * (dv / 0.88));
    } else if (shape == 2) {
        d = pow(pow(abs(du), 4.0) + pow(abs(dv), 4.0), 0.25) * 1.08;
    }
    var rim = P.kern.z;
    if (shape == 3) { rim = max(rim, 0.55); }
    let soft = max(0.35, P.kern.y);
    let prof = clamp((r + soft * 0.5 - d) / soft, 0.0, 1.0);
    var ring = 1.0 + rim * pow(clamp(d / r, 0.0, 1.0), 2.0) - rim * 0.5;
    if (shape == 3) { ring = ring * (1.0 - 0.7 * exp(-pow(d / (r * 0.45), 2.0))); }
    var k = prof * ring;
    // faint outer bleed (ink wicking into paper fibres)
    let bleed = exp(-pow(d / (r * 1.25), 2.0)) * 0.10;
    if (d > r * 0.5 && P.paperp.w < 0.5) { k = k + bleed; }
    return clamp(k, 0.0, 1.4);
}

@fragment
fn fs_cov(v: CovV) -> @location(0) vec4<f32> {
    let off = v.clip.xy - v.centre;        // displayed-space offset, coverage px
    let up = vec_to_up(off);
    let rr = P.kern.x / 0.6;
    let k = pin_kernel(up.x, up.y) * v.amp * min(1.0, rr * rr);
    var o = vec4<f32>(0.0);
    if (v.ink == 0u) { o.x = k; }
    else if (v.ink == 1u) { o.y = k; }
    else if (v.ink == 2u) { o.z = k; }
    else { o.w = k; }
    return o;
}

// ------------------------------------------------------------------------------------------------ 2. ink transfer

@group(1) @binding(0) var cov_tex: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> { return full_tri(vi); }

@fragment
fn fs_ink(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let ss = i32(P.pg.w + 0.5);
    let ppi = P.pg.z;
    let ip = vec2<i32>(floor(pos.xy));
    var acc = vec4<f32>(0.0);
    var cloud_done = false;
    var cloud = 0.0;
    let wear = P.inkp.y;
    let rough = (P.flags.w & 1u) != 0u;
    let clean = P.paperp.w > 0.5;
    for (var sy = 0; sy < ss; sy++) {
        for (var sx = 0; sx < ss; sx++) {
            let cov = max(textureLoad(cov_tex, vec2<i32>(ip.x * ss + sx, ip.y * ss + sy), 0), vec4<f32>(0.0));
            if (cov.x + cov.y + cov.z + cov.w <= 0.0) { continue; }
            let dsp = vec2<f32>(P.reg.xy) + (vec2<f32>(f32(ip.x), f32(ip.y)) + (vec2<f32>(f32(sx), f32(sy)) + 0.5) / f32(ss));
            let inch = disp_to_up(dsp) / ppi;
            // fibre / ribbon-weave modulation of the ink transfer
            var tex = 1.0;
            var gain = P.inkp.x;
            if (clean) {
                acc += vec4<f32>(1.0) - exp(-gain * cov);
                continue;
            }
            let fib = snoise(inch, 0.006, P.seeds_a.x);
            if (!cloud_done) { cloud = snoise(inch, 0.05, P.seeds_a.y); cloud_done = true; }
            tex = clamp(1.0 + 0.22 * fib * (0.4 + 0.6 * wear) + 0.10 * cloud * wear, 0.2, 2.0);
            if (rough) {
                let grain = snoise(inch, 0.0012, P.seeds_a.z);
                tex = clamp(tex * (1.0 + 0.42 * grain), 0.05, 3.0);
                gain = gain * 1.25;
            }
            acc += vec4<f32>(1.0) - exp(-gain * cov * tex);
        }
    }
    return acc / f32(ss * ss);
}

// ------------------------------------------------------------------------------------------------ 3. blurs

@group(1) @binding(1) var src_tex: texture_2d<f32>;

fn load_clamped(p: vec2<i32>) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(src_tex));
    return textureLoad(src_tex, clamp(p, vec2<i32>(0), dims - vec2<i32>(1)), 0);
}

fn ink_max(c: vec4<f32>) -> f32 { return max(max(c.x, c.y), max(c.z, c.w)); }

fn blur_dir(ip: vec2<i32>, dir: vec2<i32>, from_ink: bool) -> vec2<f32> {
    let s1 = P.inkp.w;
    let s2 = P.paperp.y;
    let r1 = i32(ceil(3.0 * s1));
    let r2 = i32(ceil(3.0 * s2));
    var a1 = 0.0; var w1 = 0.0;
    var a2 = 0.0; var w2 = 0.0;
    let rmax = max(r1, r2);
    for (var k = -rmax; k <= rmax; k++) {
        let c = load_clamped(ip + dir * k);
        var v1: f32; var v2: f32;
        if (from_ink) { v1 = ink_max(c); v2 = v1; } else { v1 = c.x; v2 = c.y; }
        let fk = f32(k);
        if (abs(k) <= r1) { let w = exp(-fk * fk / (2.0 * s1 * s1)); a1 += w * v1; w1 += w; }
        if (abs(k) <= r2) { let w = exp(-fk * fk / (2.0 * s2 * s2)); a2 += w * v2; w2 += w; }
    }
    return vec2<f32>(a1 / w1, a2 / w2);
}

@fragment
fn fs_blur_h(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let b = blur_dir(vec2<i32>(floor(pos.xy)), vec2<i32>(1, 0), true);
    return vec4<f32>(b, 0.0, 0.0);
}

@fragment
fn fs_blur_v(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let b = blur_dir(vec2<i32>(floor(pos.xy)), vec2<i32>(0, 1), false);
    return vec4<f32>(b, 0.0, 0.0);
}

// ------------------------------------------------------------------------------------------------ 4. final composite

@group(1) @binding(2) var ink_tex: texture_2d<f32>;
@group(1) @binding(3) var blur_tex: texture_2d<f32>;

fn stock(kind: u32) -> vec3<f32> {
    if (kind == 1u) { return vec3<f32>(240.0, 232.0, 207.0) / 255.0; }
    if (kind == 2u) { return vec3<f32>(226.0, 208.0, 166.0) / 255.0; }
    if (kind == 3u) { return vec3<f32>(247.0, 246.0, 240.0) / 255.0; }
    if (kind == 4u) { return vec3<f32>(247.0, 246.0, 242.0) / 255.0; }
    return vec3<f32>(246.0, 245.0, 240.0) / 255.0;
}

fn round_half_up(x: f32) -> f32 { return floor(x + 0.5); }

fn fmod_pos(a: f32, b: f32) -> f32 { return a - b * floor(a / b); }

// procedural paper at the upright pixel (iu, iv)
fn paper_colour(iu: f32, iv: f32) -> vec3<f32> {
    let res = P.pg.z;
    let w_in = P.pg.x;
    let h_in = P.pg.y;
    let kind = P.flags.x;
    var img = stock(kind);
    let xx = (iu + 0.5) / res;
    let yy = (iv + 0.5) / res;
    if (kind >= 3u) {
        var band = vec3<f32>(206.0, 232.0, 206.0) / 255.0;
        if (kind == 4u) { band = vec3<f32>(206.0, 220.0, 238.0) / 255.0; }
        let lines = floor((yy - 0.25) / 0.5);
        let on = (fmod_pos(lines, 2.0) < 0.5) && (yy > 0.25);
        let m = fmod_pos(yy - 0.25, 0.5);
        let edge = clamp(min(abs(m), abs(0.5 - m)) * res / 1.2, 0.0, 1.0);
        var k = 0.0;
        if (on) { k = 0.35 + 0.65 * edge; }
        img = img * (1.0 - k) + band * k;
        // fine rules at the band edges
        let n = max(0.0, round_half_up((yy - 0.25) / 0.5));
        let t = 0.25 + n * 0.5;
        if (t < h_in && iv == round_half_up(t * res)) { img = img * (1.0 - 0.06); }
    }
    let tx = P.paperp.x;
    if (tx > 0.0) {
        let p = vec2<f32>(xx, yy);
        let m1 = snoise(p, 0.5, P.seeds_b.x);
        let m2 = snoise(p, 0.06, P.seeds_b.y);
        let m3 = snoise(p, 0.012, P.seeds_b.z);
        let lum = 1.0 + tx * (0.012 * m1 + 0.010 * m2 + 0.016 * m3);
        img = img * lum;
        if (kind == 1u || kind == 2u) { img.z = img.z * (1.0 + tx * 0.012 * m1); }
        let edge = min(xx, w_in - xx);
        img = img * (1.0 - tx * 0.05 * exp(-edge / 0.18));
    }
    if (P.flags.y == 1u) {
        let strip = 0.5;
        let hole_d = 0.156;
        let pitch = 0.5;
        let ys = iv / res;
        let width = max(1.0, round_half_up(0.012 * res));
        let dash = select(0.0, 1.0, fmod_pos(ys, 0.08) < 0.04);
        for (var s = 0; s < 2; s++) {
            let side = select(strip, w_in - strip, s == 1);
            let c = round_half_up(side * res);
            if (iu >= c - width && iu < c + width) {
                img = img * (1.0 - 0.18 * dash);
                img = img * 0.97;
            }
        }
        if (P.flags.z == 1u) {
            let r = hole_d / 2.0 * res;
            let nk = ceil((h_in - pitch / 2.0) / pitch);
            let ky = clamp(round_half_up((iv + 0.5 - pitch / 2.0 * res) / (pitch * res)), 0.0, nk - 1.0);
            let cy_px = (pitch / 2.0 + ky * pitch) * res;
            for (var s = 0; s < 2; s++) {
                let cx = select(strip / 2.0, w_in - strip / 2.0, s == 1);
                let cx_px = cx * res;
                let x0 = trunc(cx_px - r - 3.0);
                let x1 = trunc(cx_px + r + 3.0);
                let y0 = trunc(cy_px - r - 3.0);
                let y1 = trunc(cy_px + r + 3.0);
                if (iu >= x0 && iu < x1 && iv >= y0 && iv < y1) {
                    let gx = iu + 0.5 - cx_px;
                    let gy = iv + 0.5 - cy_px;
                    let d = sqrt(gx * gx + gy * gy);
                    let inside = clamp(r - d + 0.5, 0.0, 1.0);
                    let rim = clamp(1.0 - abs(d - r - 0.6) / 1.5, 0.0, 1.0) * (1.0 - inside) * 0.10;
                    let shade = 0.30 - 0.10 * clamp(gy / max(r, 1.0), -1.0, 1.0);
                    img = img * (1.0 - rim);
                    img = img * (1.0 - inside) + vec3<f32>(inside * shade) * vec3<f32>(1.0, 1.0, 1.05);
                }
            }
        }
    }
    return clamp(img, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_final(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let ip = vec2<i32>(floor(pos.xy));
    // displayed page px of this output pixel, and where it sits in the extended-region textures
    let dsp = P.out_reg.xy + vec2<f32>(f32(ip.x), f32(ip.y));
    let tp = vec2<i32>(dsp - P.reg.xy);
    let up = disp_to_up(dsp + vec2<f32>(0.5)) - vec2<f32>(0.5);
    var out = vec3<f32>(1.0);
    if (P.paperp.w < 0.5) { out = paper_colour(round(up.x), round(up.y)); }
    let a = textureLoad(ink_tex, tp, 0);
    out = out * (vec3<f32>(1.0) - a.x * (vec3<f32>(1.0) - P.tone0.xyz));
    out = out * (vec3<f32>(1.0) - a.y * (vec3<f32>(1.0) - P.tone1.xyz));
    out = out * (vec3<f32>(1.0) - a.z * (vec3<f32>(1.0) - P.tone2.xyz));
    out = out * (vec3<f32>(1.0) - a.w * (vec3<f32>(1.0) - P.tone3.xyz));
    let impact = P.inkp.z;
    if (impact > 0.0 && P.paperp.w < 0.5) {
        // ink gently dents the paper: light on one side, dark on the other (a directional gradient lit from the top left of
        // the sheet; scaled by the resolution so the shading looks the same at every zoom)
        let dims = vec2<i32>(textureDimensions(blur_tex));
        var b: array<f32, 9>;
        for (var j = -1; j <= 1; j++) {
            for (var i = -1; i <= 1; i++) {
                let q = clamp(tp + vec2<i32>(i, j), vec2<i32>(0), dims - vec2<i32>(1));
                b[(j + 1) * 3 + (i + 1)] = textureLoad(blur_tex, q, 0).x;
            }
        }
        let gx = ((b[2] + 2.0 * b[5] + b[8]) - (b[0] + 2.0 * b[3] + b[6])) / 8.0;
        let gy = ((b[6] + 2.0 * b[7] + b[8]) - (b[0] + 2.0 * b[1] + b[2])) / 8.0;
        let g = vec_to_up(vec2<f32>(gx, gy));
        let lit = -(g.x + g.y) * 0.7071 * (P.pg.z / 150.0);
        let halo = textureLoad(blur_tex, clamp(tp, vec2<i32>(0), dims - vec2<i32>(1)), 0).y;
        out = out * (1.0 + 0.55 * impact * lit - 0.10 * impact * halo);
    }
    return vec4<f32>(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
