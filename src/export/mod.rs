//! Export sheets as PNG, PDF or TIFF. Pages are rendered band by band on the GPU and streamed into the encoders, so a
//! 1200 dpi page does not need a gigabyte of RAM.
pub mod pdf;
pub mod pixels;
pub mod tiff;

use crate::emu::settings::RenderSettings;
use crate::render::{displayed_size, Renderer, SheetBuf};
pub use pdf::Codec;
pub use pixels::Colour;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Pdf,
    Tiff,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Pdf => "pdf",
            Format::Tiff => "tif",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub format: Format,
    pub dpi: u32,
    pub colour: Colour,
    /// PDF: JPEG quality (1..=100) or lossless Flate when None.
    pub pdf_jpeg: Option<u8>,
    /// TIFF: Deflate compression.
    pub tiff_deflate: bool,
    /// PDF and TIFF: all sheets in one file (otherwise one file per sheet). PNG is always one file per sheet.
    pub single_file: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions { format: Format::Pdf, dpi: 300, colour: Colour::Rgb, pdf_jpeg: None, tiff_deflate: true, single_file: true }
    }
}

fn err<E: std::error::Error + Send + Sync + 'static>(e: E) -> io::Error {
    io::Error::other(e)
}

/// `name.ext` -> `name_p003.ext`
pub fn page_path(base: &Path, ext: &str, page: usize) -> PathBuf {
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("page");
    base.with_file_name(format!("{}_p{:03}.{}", stem, page + 1, ext))
}

const BAND_PIXELS: u64 = 4_000_000;

/// Export `sheets` (already uploaded to the GPU). `progress(done, total)` is called after every band; returning false from
/// `keep_going` cancels. Returns the files written.
pub fn export_sheets(
    gpu: &mut Renderer,
    sheets: &[&SheetBuf],
    s: &RenderSettings,
    opts: &ExportOptions,
    base: &Path,
    mut progress: impl FnMut(usize, usize),
    keep_going: impl Fn() -> bool,
) -> io::Result<Vec<PathBuf>> {
    let ppi = opts.dpi as f32;
    let total = sheets.len();
    let mut written = Vec::new();
    let title = base.file_stem().and_then(|s| s.to_str()).unwrap_or("RetroImpact").to_string();
    let single = opts.single_file && opts.format != Format::Png;

    let mut pdf = if opts.format == Format::Pdf && single { Some(pdf::PdfWriter::new(&title)) } else { None };
    let mut tiff = if opts.format == Format::Tiff && single {
        Some(tiff::TiffWriter::new(BufWriter::new(File::create(base)?))?)
    } else {
        None
    };

    for (i, sb) in sheets.iter().enumerate() {
        if !keep_going() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let (w, h) = displayed_size(&sb.sheet, s.rotation, ppi);
        let band_h = ((BAND_PIXELS / w.max(1) as u64) as u32).clamp(64, 2048);
        let path = if single { base.to_path_buf() } else { page_path(base, opts.format.ext(), i) };
        match opts.format {
            Format::Png => {
                let mut enc = png::Encoder::new(BufWriter::new(File::create(&path)?), w, h);
                let (ct, depth) = match opts.colour {
                    Colour::Rgb => (png::ColorType::Rgb, png::BitDepth::Eight),
                    Colour::Gray => (png::ColorType::Grayscale, png::BitDepth::Eight),
                    Colour::Mono => (png::ColorType::Grayscale, png::BitDepth::One),
                };
                enc.set_color(ct);
                enc.set_depth(depth);
                let ppm = (opts.dpi as f64 / 0.0254).round() as u32;
                enc.set_pixel_dims(Some(png::PixelDimensions { xppu: ppm, yppu: ppm, unit: png::Unit::Meter }));
                let mut writer = enc.write_header().map_err(err)?;
                let mut sw = writer.stream_writer().map_err(err)?;
                let mut res = Ok(());
                gpu.render_bands(sb, s, ppi, band_h, |_y, rows, rgba| {
                    if res.is_ok() {
                        // PNG: 1 = white for 1-bit grayscale, which is what Mono (black = 0) gives
                        let data = pixels::convert_rows(rgba, w as usize, rows as usize, opts.colour, false);
                        res = sw.write_all(&data);
                    }
                    progress(i, total);
                });
                res?;
                sw.finish().map_err(err)?;
                written.push(path);
            }
            Format::Tiff => {
                let mut owned = if single { None } else { Some(tiff::TiffWriter::new(BufWriter::new(File::create(&path)?))?) };
                let t = if single { tiff.as_mut().unwrap() } else { owned.as_mut().unwrap() };
                t.begin_page(w, h, opts.colour, opts.dpi, opts.tiff_deflate, band_h);
                let mut res = Ok(());
                gpu.render_bands(sb, s, ppi, band_h, |_y, rows, rgba| {
                    if res.is_ok() {
                        res = t.add_strip(rgba, rows);
                    }
                    progress(i, total);
                });
                res?;
                t.end_page()?;
                if let Some(o) = owned {
                    o.finish()?.flush()?;
                    written.push(path);
                }
            }
            Format::Pdf => {
                let mut owned = if single { None } else { Some(pdf::PdfWriter::new(&title)) };
                let p = if single { pdf.as_mut().unwrap() } else { owned.as_mut().unwrap() };
                let codec = match opts.pdf_jpeg {
                    Some(q) => Codec::Jpeg(q),
                    None => Codec::Flate,
                };
                p.begin_page(w, h, opts.colour, opts.dpi, codec);
                let mut res = Ok(());
                gpu.render_bands(sb, s, ppi, band_h, |_y, rows, rgba| {
                    if res.is_ok() {
                        res = p.add_rows(rgba, rows);
                    }
                    progress(i, total);
                });
                res?;
                p.end_page()?;
                if let Some(o) = owned {
                    std::fs::write(&path, o.finish())?;
                    written.push(path);
                }
            }
        }
    }
    if let Some(p) = pdf {
        std::fs::write(base, p.finish())?;
        written.push(base.to_path_buf());
    }
    if let Some(t) = tiff {
        t.finish()?.flush()?;
        written.push(base.to_path_buf());
    }
    Ok(written)
}
