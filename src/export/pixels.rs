//! Pixel format conversion shared by the exporters.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Rgb,
    Gray,
    /// 1 bit per pixel (threshold); black ink = 1 in the packed data of the TIFF writer (WhiteIsZero).
    Mono,
}

impl Colour {
    pub fn samples(self) -> usize {
        match self {
            Colour::Rgb => 3,
            _ => 1,
        }
    }
}

#[inline]
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((299 * r as u32 + 587 * g as u32 + 114 * b as u32 + 500) / 1000) as u8
}

/// RGBA rows -> packed rows of the target colour mode. `invert_mono`: black = 1 (TIFF WhiteIsZero) instead of black = 0.
pub fn convert_rows(rgba: &[u8], w: usize, rows: usize, c: Colour, invert_mono: bool) -> Vec<u8> {
    match c {
        Colour::Rgb => {
            let mut out = Vec::with_capacity(w * rows * 3);
            for p in rgba.chunks_exact(4).take(w * rows) {
                out.extend_from_slice(&p[..3]);
            }
            out
        }
        Colour::Gray => rgba.chunks_exact(4).take(w * rows).map(|p| luma(p[0], p[1], p[2])).collect(),
        Colour::Mono => {
            let stride = (w + 7) / 8;
            let mut out = vec![0u8; stride * rows];
            for y in 0..rows {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    let dark = luma(rgba[i], rgba[i + 1], rgba[i + 2]) < 128;
                    let bit = dark == invert_mono;
                    if bit {
                        out[y * stride + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            out
        }
    }
}

pub fn row_bytes(w: usize, c: Colour) -> usize {
    match c {
        Colour::Rgb => w * 3,
        Colour::Gray => w,
        Colour::Mono => (w + 7) / 8,
    }
}
