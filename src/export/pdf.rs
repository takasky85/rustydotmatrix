//! A small PDF writer: one page per sheet, each page one image (Flate with PNG predictor, or JPEG).
use super::pixels::{convert_rows, Colour};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Flate,
    Jpeg(u8),
}

pub struct PdfWriter {
    out: Vec<u8>,
    offsets: Vec<usize>,
    pages: Vec<usize>,
    cur: Option<Cur>,
    title: String,
}

struct Cur {
    width: u32,
    height: u32,
    colour: Colour,
    dpi: u32,
    codec: Codec,
    z: Option<ZlibEncoder<Vec<u8>>>,
    raw: Vec<u8>,
    prev_row: Vec<u8>,
}

impl PdfWriter {
    pub fn new(title: &str) -> Self {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
        // object ids 1 (catalog) and 2 (pages) are reserved; written at the end
        PdfWriter { out, offsets: vec![0, 0], pages: Vec::new(), cur: None, title: title.to_string() }
    }

    fn new_obj(&mut self) -> usize {
        self.offsets.push(self.out.len());
        self.offsets.len()
    }

    pub fn begin_page(&mut self, width: u32, height: u32, colour: Colour, dpi: u32, codec: Codec) {
        // Mono data is packed 1 bit per pixel, which the JPEG codec cannot carry
        let codec = if colour == Colour::Mono { Codec::Flate } else { codec };
        self.cur = Some(Cur {
            width,
            height,
            colour,
            dpi,
            codec,
            z: match codec {
                Codec::Flate => Some(ZlibEncoder::new(Vec::new(), Compression::new(6))),
                Codec::Jpeg(_) => None,
            },
            raw: Vec::new(),
            prev_row: Vec::new(),
        });
    }

    pub fn add_rows(&mut self, rgba: &[u8], rows: u32) -> io::Result<()> {
        let c = self.cur.as_mut().expect("begin_page first");
        let w = c.width as usize;
        let data = convert_rows(rgba, w, rows as usize, c.colour, false);
        match c.codec {
            Codec::Jpeg(_) => c.raw.extend_from_slice(&data),
            Codec::Flate => {
                let rb = data.len() / rows.max(1) as usize;
                let bpp = c.colour.samples();
                let z = c.z.as_mut().unwrap();
                if c.colour == Colour::Mono {
                    z.write_all(&data)?;
                } else {
                    // PNG "Sub" predictor (filter byte 1) on every row: much better compression of smooth paper
                    let mut line = vec![0u8; rb + 1];
                    line[0] = 1;
                    for r in 0..rows as usize {
                        let row = &data[r * rb..(r + 1) * rb];
                        for i in 0..rb {
                            let left = if i >= bpp { row[i - bpp] } else { 0 };
                            line[i + 1] = row[i].wrapping_sub(left);
                        }
                        z.write_all(&line)?;
                    }
                }
                c.prev_row.clear();
            }
        }
        Ok(())
    }

    pub fn end_page(&mut self) -> io::Result<()> {
        let c = self.cur.take().expect("begin_page first");
        let (stream, filter): (Vec<u8>, String) = match c.codec {
            Codec::Flate => {
                let data = c.z.unwrap().finish()?;
                let parms = if c.colour == Colour::Mono {
                    String::new()
                } else {
                    format!("/DecodeParms << /Predictor 15 /Colors {} /BitsPerComponent 8 /Columns {} >>", c.colour.samples(), c.width)
                };
                (data, format!("/Filter /FlateDecode {}", parms))
            }
            Codec::Jpeg(q) => {
                let mut jpg = Vec::new();
                let enc = jpeg_encoder::Encoder::new(&mut jpg, q);
                let ct = if c.colour == Colour::Rgb { jpeg_encoder::ColorType::Rgb } else { jpeg_encoder::ColorType::Luma };
                enc.encode(&c.raw, c.width as u16, c.height as u16, ct).map_err(io::Error::other)?;
                (jpg, "/Filter /DCTDecode".to_string())
            }
        };
        let (cs, bpc) = match c.colour {
            Colour::Rgb => ("/DeviceRGB", 8),
            Colour::Gray => ("/DeviceGray", 8),
            Colour::Mono => ("/DeviceGray", 1),
        };
        // image
        let img_id = self.new_obj();
        let head = format!(
            "{} 0 obj\n<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {} /BitsPerComponent {} {} /Length {} >>\nstream\n",
            img_id, c.width, c.height, cs, bpc, filter, stream.len()
        );
        self.out.extend_from_slice(head.as_bytes());
        self.out.extend_from_slice(&stream);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
        // content
        let pw = c.width as f64 * 72.0 / c.dpi as f64;
        let ph = c.height as f64 * 72.0 / c.dpi as f64;
        let content = format!("q {:.3} 0 0 {:.3} 0 0 cm /Im0 Do Q\n", pw, ph);
        let cont_id = self.new_obj();
        let s = format!("{} 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n", cont_id, content.len(), content);
        self.out.extend_from_slice(s.as_bytes());
        // page
        let page_id = self.new_obj();
        let s = format!(
            "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.3} {:.3}] /Resources << /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>\nendobj\n",
            page_id, pw, ph, img_id, cont_id
        );
        self.out.extend_from_slice(s.as_bytes());
        self.pages.push(page_id);
        Ok(())
    }

    pub fn finish(mut self) -> Vec<u8> {
        let kids: Vec<String> = self.pages.iter().map(|p| format!("{} 0 R", p)).collect();
        self.offsets[1] = self.out.len();
        let s = format!("2 0 obj\n<< /Type /Pages /Count {} /Kids [{}] >>\nendobj\n", self.pages.len(), kids.join(" "));
        self.out.extend_from_slice(s.as_bytes());
        self.offsets[0] = self.out.len();
        self.out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let info_id = self.new_obj();
        let title: String = self.title.chars().filter(|c| c.is_ascii() && !"()\\".contains(*c)).collect();
        let s = format!("{} 0 obj\n<< /Title ({}) /Producer (RetroImpact) >>\nendobj\n", info_id, title);
        self.out.extend_from_slice(s.as_bytes());
        let xref = self.out.len();
        let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len() + 1);
        for o in &self.offsets {
            x.push_str(&format!("{:010} 00000 n \n", o));
        }
        x.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R /Info {} 0 R >>\nstartxref\n{}\n%%EOF\n", self.offsets.len() + 1, info_id, xref));
        self.out.extend_from_slice(x.as_bytes());
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_pdf_structure() {
        let mut p = PdfWriter::new("test");
        for _ in 0..2 {
            p.begin_page(8, 4, Colour::Gray, 144, Codec::Flate);
            p.add_rows(&vec![200u8; 8 * 4 * 4], 4).unwrap();
            p.end_page().unwrap();
        }
        let d = p.finish();
        let s = String::from_utf8_lossy(&d);
        assert!(s.starts_with("%PDF-1.4"));
        assert!(s.contains("/Count 2"));
        assert!(s.trim_end().ends_with("%%EOF"));
        // every xref offset points at "N 0 obj"
        let xr = s.rfind("startxref\n").unwrap();
        let xoff: usize = s[xr + 10..].lines().next().unwrap().parse().unwrap();
        assert!(d[xoff..].starts_with(b"xref"));
    }
}
