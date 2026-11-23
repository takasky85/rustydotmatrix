//! Character sets: byte -> character used to pick a glyph.

use super::tables::{CP437_HIGH, CP437_LOW, CP850_HIGH, LATIN1_HIGH, MAC_ROMAN_HIGH};

pub const NAT_POS: [u32; 12] = [0x23, 0x24, 0x40, 0x5B, 0x5C, 0x5D, 0x5E, 0x60, 0x7B, 0x7C, 0x7D, 0x7E];

/// Epson national character sets, ESC R n. Positions: # $ @ [ \ ] ^ ` { | } ~
pub static NATIONAL: [&str; 13] = [
    "#$@[\\]^`{|}~",
    "#$à°ç§^`éùè¨",
    "#$§ÄÖÜ^`äöüß",
    "£$@[\\]^`{|}~",
    "#$@ÆØÅ^`æøå~",
    "#¤ÉÄÖÅÜéäöåü",
    "#$@°\\é^ùàòèì",
    "₧$@¡Ñ¿^`¨ñ}~",
    "#$@[¥]^`{|}~",
    "#¤ÉÆØÅÜéæøåü",
    "#$ÉÆØÅÜéæøåü",
    "#$á¡Ñ¿é`íñóú",
    "#$á¡Ñ¿éüíñóú",
];

pub static NATIONAL_NAMES: [&str; 13] = [
    "USA",
    "France",
    "Germany",
    "UK",
    "Denmark I",
    "Sweden",
    "Italy",
    "Spain I",
    "Japan",
    "Norway",
    "Denmark II",
    "Spain II",
    "Latin America",
];

pub static CODEPAGES: [(&str, &str); 5] = [
    ("cp437", "IBM PC (CP437)"),
    ("cp850", "IBM PC Multilingual (CP850)"),
    ("latin1", "ISO Latin-1"),
    ("mac_roman", "Mac Roman (ImageWriter)"),
    ("ascii", "ASCII only"),
];

fn ch(u: u32) -> char {
    char::from_u32(u).unwrap_or('?')
}

pub fn cp437_low(b: u8) -> Option<char> {
    CP437_LOW.get(b as usize).map(|&u| ch(u))
}

pub fn apply_national(c: char, national: i32) -> char {
    if national <= 0 || national as usize >= NATIONAL.len() {
        return c;
    }
    let o = c as u32;
    if let Some(p) = NAT_POS.iter().position(|&v| v == o) {
        return NATIONAL[national as usize].chars().nth(p).unwrap_or(c);
    }
    c
}

/// Character for printable byte `b` (None if not printable).
pub fn decode(b: u8, codepage: &str, national: i32, ibm_graphics: bool) -> Option<char> {
    if b < 0x20 {
        if ibm_graphics && !matches!(b, 0x07 | 0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x0E | 0x0F | 0x11 | 0x12 | 0x13 | 0x14 | 0x18 | 0x1B) {
            return cp437_low(b);
        }
        return None;
    }
    if b < 0x7F {
        return Some(apply_national(b as char, national));
    }
    if b == 0x7F {
        return None;
    }
    let i = (b - 0x80) as usize;
    Some(match codepage {
        "ascii" => '?',
        "cp437" => ch(CP437_HIGH[i]),
        "cp850" => ch(CP850_HIGH[i]),
        "mac_roman" => ch(MAC_ROMAN_HIGH[i]),
        _ => ch(LATIN1_HIGH[i]),
    })
}

// PETSCII (Commodore). Graphics symbols are mapped to the nearest Unicode glyph.
const PET_A0: [char; 32] = [
    ' ', '▌', '▄', '▔', '▁', '▏', '▒', '▕', '◤', '◥', '▕', '├', '▗', '└', '┐', '▂', '┌', '┴', '┬', '┤', '▎', '▍', '▌', '▔', '▀',
    '▃', '✓', '▖', '▝', '┘', '▘', '▚',
];
const PET_C0: [char; 32] = [
    '─', '♠', '│', '─', '─', '─', '─', '│', '│', '╮', '╰', '╯', '▗', '╲', '╱', '▘', '▝', '●', '▔', '♥', '▎', '╭', '╳', '○', '♣',
    '▏', '♦', '┼', '▚', '│', 'π', '◥',
];

/// Map a PETSCII byte to a displayable character (None = no glyph / control).
pub fn petscii_char(b: u8, lower: bool) -> Option<char> {
    if b < 0x20 || (0x80..0xA0).contains(&b) {
        return None;
    }
    if (0x20..=0x3F).contains(&b) {
        return Some(b as char);
    }
    if b == 0x40 {
        return Some('@');
    }
    if (0x41..=0x5A).contains(&b) {
        return Some(if lower { (b + 32) as char } else { b as char });
    }
    match b {
        0x5B => return Some('['),
        0x5C => return Some('£'),
        0x5D => return Some(']'),
        0x5E => return Some('↑'),
        0x5F => return Some('←'),
        _ => {}
    }
    let mut b = b as u32;
    if (0x60..=0x7F).contains(&b) {
        b = b - 0x60 + 0xC0;
    }
    if (0xE0..=0xFE).contains(&b) {
        b = b - 0xE0 + 0xA0;
    }
    if b == 0xFF {
        b = 0xDE;
    }
    if (0xA0..=0xBF).contains(&b) {
        return Some(PET_A0[(b - 0xA0) as usize]);
    }
    if (0xC0..=0xDF).contains(&b) {
        if lower && (0xC1..=0xDA).contains(&b) {
            return Some((b - 0xC1 + 0x41) as u8 as char);
        }
        return Some(PET_C0[(b - 0xC0) as usize]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn national_sets_have_twelve_chars() {
        for s in NATIONAL.iter() {
            assert_eq!(s.chars().count(), 12, "{s}");
        }
    }

    #[test]
    fn decode_basics() {
        assert_eq!(decode(b'A', "cp437", 0, false), Some('A'));
        assert_eq!(decode(0x80, "cp437", 0, false), Some('Ç'));
        assert_eq!(decode(0x80, "ascii", 0, false), Some('?'));
        assert_eq!(decode(0x23, "cp437", 3, false), Some('£'));
        assert_eq!(decode(0x7F, "cp437", 0, false), None);
        assert_eq!(decode(0x01, "cp437", 0, true), Some('☺'));
    }

    #[test]
    fn petscii() {
        assert_eq!(petscii_char(0x41, false), Some('A'));
        assert_eq!(petscii_char(0x41, true), Some('a'));
        assert_eq!(petscii_char(0xC1, true), Some('A'));
        assert_eq!(petscii_char(0x0D, true), None);
    }
}
