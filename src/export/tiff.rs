//! A small streaming TIFF writer: one strip per band, optional Deflate, several pages per file.
use super::pixels::{convert_rows, row_bytes, Colour};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{self, Seek, SeekFrom, Write};

pub struct TiffWriter<W: Write + Seek> {
    w: W,
    pos: u64,
    /// Where the next IFD offset must be written (the header, then the previous page's IFD).
    next_ptr: u64,
    page: Option<PageState>,
}

struct PageState {
    width: u32,
    height: u32,
    colour: Colour,
    dpi: u32,
    deflate: bool,
    rows_per_strip: u32,
    rows_done: u32,
    offsets: Vec<u32>,
    counts: Vec<u32>,
}

impl<W: Write + Seek> TiffWriter<W> {
    pub fn new(mut w: W) -> io::Result<Self> {
        w.write_all(&[b'I', b'I', 42, 0, 0, 0, 0, 0])?;
        Ok(TiffWriter { w, pos: 8, next_ptr: 4, page: None })
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn pad_even(&mut self) -> io::Result<()> {
        if self.pos % 2 == 1 {
            self.put(&[0])?;
        }
        Ok(())
    }

    pub fn begin_page(&mut self, width: u32, height: u32, colour: Colour, dpi: u32, deflate: bool, rows_per_strip: u32) {
        self.page = Some(PageState {
            width,
            height,
            colour,
            dpi,
            deflate,
            rows_per_strip: rows_per_strip.max(1),
            rows_done: 0,
            offsets: Vec::new(),
            counts: Vec::new(),
        });
    }

    /// One strip: `rows` rows of RGBA pixels (the strip height must equal `rows_per_strip` except for the last one).
    pub fn add_strip(&mut self, rgba: &[u8], rows: u32) -> io::Result<()> {
        let (width, colour, deflate) = {
            let p = self.page.as_ref().expect("begin_page first");
            (p.width as usize, p.colour, p.deflate)
        };
        let raw = convert_rows(rgba, width, rows as usize, colour, true);
        debug_assert_eq!(raw.len(), row_bytes(width, colour) * rows as usize);
        let data = if deflate {
            let mut z = ZlibEncoder::new(Vec::new(), Compression::new(6));
            z.write_all(&raw)?;
            z.finish()?
        } else {
            raw
        };
        self.pad_even()?;
        let off = self.pos as u32;
        self.put(&data)?;
        let p = self.page.as_mut().unwrap();
        p.offsets.push(off);
        p.counts.push(data.len() as u32);
        p.rows_done += rows;
        Ok(())
    }

    pub fn end_page(&mut self) -> io::Result<()> {
        let p = self.page.take().expect("begin_page first");
        let spp = p.colour.samples() as u32;
        let n = p.offsets.len() as u32;
        // out-of-line values
        self.pad_even()?;
        let bps_off = self.pos as u32;
        if spp == 3 {
            self.put(&[8, 0, 8, 0, 8, 0])?;
        }
        let so_off = self.pos as u32;
        if n > 1 {
            for o in &p.offsets {
                self.put(&o.to_le_bytes())?;
            }
        }
        let sc_off = self.pos as u32;
        if n > 1 {
            for c in &p.counts {
                self.put(&c.to_le_bytes())?;
            }
        }
        let res_off = self.pos as u32;
        self.put(&p.dpi.to_le_bytes())?;
        self.put(&1u32.to_le_bytes())?;
        self.pad_even()?;
        let ifd = self.pos as u32;

        let bits = if p.colour == Colour::Mono { 1u32 } else { 8 };
        let photometric = match p.colour {
            Colour::Rgb => 2u32,
            Colour::Gray => 1,
            Colour::Mono => 0,
        };
        // (tag, type, count, value-or-offset); types: 3 SHORT, 4 LONG, 5 RATIONAL
        let mut e: Vec<(u16, u16, u32, u32)> = vec![
            (256, 4, 1, p.width),
            (257, 4, 1, p.height),
            (258, 3, spp, if spp == 3 { bps_off } else { bits }),
            (259, 3, 1, if p.deflate { 8 } else { 1 }),
            (262, 3, 1, photometric),
            (273, 4, n, if n > 1 { so_off } else { p.offsets[0] }),
            (277, 3, 1, spp),
            (278, 4, 1, p.rows_per_strip),
            (279, 4, n, if n > 1 { sc_off } else { p.counts[0] }),
            (282, 5, 1, res_off),
            (283, 5, 1, res_off),
            (284, 3, 1, 1),
            (296, 3, 1, 2),
        ];
        e.sort_by_key(|t| t.0);
        let mut buf = Vec::with_capacity(2 + e.len() * 12 + 4);
        buf.extend_from_slice(&(e.len() as u16).to_le_bytes());
        for (tag, ty, count, val) in &e {
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&ty.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            // a single SHORT value sits in the first two bytes of the field
            buf.extend_from_slice(&val.to_le_bytes());
        }
        buf.extend_from_slice(&0u32.to_le_bytes());
        self.put(&buf)?;
        // link from the header / previous page
        let here = self.pos;
        self.w.seek(SeekFrom::Start(self.next_ptr))?;
        self.w.write_all(&ifd.to_le_bytes())?;
        self.w.seek(SeekFrom::Start(here))?;
        self.next_ptr = ifd as u64 + 2 + e.len() as u64 * 12;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.w.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn writes_a_valid_two_page_file() {
        let mut t = TiffWriter::new(Cursor::new(Vec::new())).unwrap();
        for _ in 0..2 {
            t.begin_page(4, 4, Colour::Rgb, 300, false, 2);
            t.add_strip(&vec![255u8; 4 * 2 * 4], 2).unwrap();
            t.add_strip(&vec![0u8; 4 * 2 * 4], 2).unwrap();
            t.end_page().unwrap();
        }
        let data = t.finish().unwrap().into_inner();
        assert_eq!(&data[..4], &[b'I', b'I', 42, 0]);
        let ifd1 = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes(data[ifd1..ifd1 + 2].try_into().unwrap()) as usize;
        assert_eq!(n, 13);
        let next = u32::from_le_bytes(data[ifd1 + 2 + n * 12..ifd1 + 6 + n * 12].try_into().unwrap()) as usize;
        assert!(next > ifd1 && next < data.len());
        let n2 = u16::from_le_bytes(data[next..next + 2].try_into().unwrap());
        assert_eq!(n2 as usize, 13);
    }
}
