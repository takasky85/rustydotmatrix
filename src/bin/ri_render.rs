//! Command line renderer (also the GPU test used by CI): printer data in, PNG / PDF / TIFF out.
//!
//!   ri-render [INPUT] [-o OUT] [--dpi N] [--look N] [--clean] [--rotate DEG] [--paper KEY] [--selftest]
//!
//! Without INPUT a built-in ESC/P sample is printed. `--selftest` renders the first sheet in one piece and again in small
//! tiles and fails if they differ (seams).
use retroimpact::emu::emulate::emulate;
use retroimpact::emu::model::EmuOptions;
use retroimpact::emu::settings::{RenderSettings, LOOK_PRESETS};
use retroimpact::export::{export_sheets, Colour, ExportOptions, Format};
use retroimpact::fontraster::AbRaster;
use retroimpact::render::{build_sheet, displayed_size, Region, Renderer};
use std::path::PathBuf;

fn sample() -> Vec<u8> {
    let mut d: Vec<u8> = Vec::new();
    d.extend_from_slice(b"\x1b@");
    d.extend_from_slice(b"RetroImpact GPU renderer test page\r\n\r\n");
    d.extend_from_slice(b"The quick brown fox jumps over the lazy dog 0123456789\r\n");
    d.extend_from_slice(b"\x1bE Emphasised text \x1bF \x1b4Italic text\x1b5 \x1b-1Underlined\x1b-0\r\n");
    d.extend_from_slice(b"\x0fCondensed: the quick brown fox jumps over the lazy dog 0123456789 !\"#$%&'()*+,-./\x12\r\n");
    d.extend_from_slice(b"\x1bW1Wide\x1bW0 and normal\r\n");
    d.extend_from_slice(b"\x1bx1\x1bk\x00Near letter quality: the quick brown fox jumps over the lazy dog\x1bx0\r\n\r\n");
    // a bit-image row: a triangle
    d.extend_from_slice(b"\x1b*\x00\x40\x01");
    for i in 0..320u32 {
        d.push(0xFFu8 << (7 - (i * 8 / 320) as u8).min(7));
    }
    d.extend_from_slice(b"\r\n\r\nPage two follows\x0c");
    d.extend_from_slice(b"Second sheet\r\n");
    d
}

fn main() {
    let mut input: Option<PathBuf> = None;
    let mut out = PathBuf::from("retroimpact_out.png");
    let mut dpi = 144u32;
    let mut look: Option<usize> = None;
    let mut clean = false;
    let mut rotate = 0;
    let mut paper: Option<String> = None;
    let mut selftest = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => out = PathBuf::from(args.next().unwrap_or_default()),
            "--dpi" => dpi = args.next().and_then(|v| v.parse().ok()).unwrap_or(144),
            "--look" => look = args.next().and_then(|v| v.parse().ok()),
            "--clean" => clean = true,
            "--rotate" => rotate = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--paper" => paper = args.next(),
            "--selftest" => selftest = true,
            _ => input = Some(PathBuf::from(a)),
        }
    }
    retroimpact::emu::glyphs::set_font_raster(Box::new(AbRaster::new()));
    let data = match &input {
        Some(p) => std::fs::read(p).unwrap_or_else(|e| {
            eprintln!("cannot read {}: {e}", p.display());
            std::process::exit(2)
        }),
        None => sample(),
    };
    let doc = emulate(&data, &EmuOptions::default());
    println!("printer: {} ({} page(s), {} unknown commands)", doc.detected, doc.pages.len(), doc.n_unknown);

    let mut s = RenderSettings::default();
    if let Some(i) = look {
        s.apply_look(&LOOK_PRESETS[i.min(LOOK_PRESETS.len() - 1)]);
    }
    s.clean = clean;
    s.rotation = rotate;
    if let Some(p) = paper {
        s.paper = p;
    }
    s.dpi = dpi as i32;
    s.clamp();

    let mut gpu = match Renderer::headless() {
        Some(g) => g,
        None => {
            eprintln!("no GPU adapter available");
            std::process::exit(3);
        }
    };
    println!("adapter ready, max texture {}", gpu.max_dim());
    let sheets: Vec<_> = doc.pages.iter().map(|p| gpu.upload(build_sheet(p, &doc, &s))).collect();
    if sheets.is_empty() {
        eprintln!("nothing to print");
        std::process::exit(1);
    }

    if selftest {
        let ppi = dpi as f32;
        let (w, h) = displayed_size(&sheets[0].sheet, s.rotation, ppi);
        let (w, h) = (w.min(1200), h.min(900));
        let (_, _, whole) = gpu.render_rgba(&sheets[0], &s, ppi, Region { x: 0, y: 0, w, h });
        let mut tiled = vec![0u8; whole.len()];
        let (mut y, tile) = (0u32, 192u32);
        while y < h {
            let mut x = 0u32;
            while x < w {
                let r = Region { x: x as i32, y: y as i32, w: tile.min(w - x), h: tile.min(h - y) };
                let (rw, rh, px) = gpu.render_rgba(&sheets[0], &s, ppi, r);
                for row in 0..rh as usize {
                    let d = ((y as usize + row) * w as usize + x as usize) * 4;
                    tiled[d..d + rw as usize * 4].copy_from_slice(&px[row * rw as usize * 4..(row + 1) * rw as usize * 4]);
                }
                x += tile;
            }
            y += tile;
        }
        let (mut max, mut bad) = (0i32, 0usize);
        for (a, b) in whole.iter().zip(&tiled) {
            let d = (*a as i32 - *b as i32).abs();
            max = max.max(d);
            if d > 3 {
                bad += 1;
            }
        }
        println!("tile seam check: max difference {max}, {bad} bytes differ by more than 3");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&out).unwrap()), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&whole).unwrap();
        if bad > 0 {
            std::process::exit(4);
        }
        return;
    }

    let format = match out.extension().and_then(|e| e.to_str()).unwrap_or("png") {
        "pdf" => Format::Pdf,
        "tif" | "tiff" => Format::Tiff,
        _ => Format::Png,
    };
    let opts = ExportOptions { format, dpi, colour: Colour::Rgb, pdf_jpeg: None, tiff_deflate: true, single_file: true };
    let refs: Vec<_> = sheets.iter().collect();
    match export_sheets(&mut gpu, &refs, &s, &opts, &out, |_, _| {}, || true) {
        Ok(files) => {
            for f in files {
                println!("wrote {}", f.display());
            }
        }
        Err(e) => {
            eprintln!("export failed: {e}");
            std::process::exit(5);
        }
    }
}
