mod tables;

use crate::unicode::tables::*;
use alloc::string::String;

const CONT_MASK: u8 = 0b0011_1111;

#[inline(always)]
fn utf8_acc_cont_byte(ch: u32, byte: u8) -> u32 {
    (ch << 6) | (byte & CONT_MASK) as u32
}

/// Big-endian UTF-16, as the `name` table stores it. Font data is untrusted: unpaired surrogates
/// decode to U+FFFD, and a trailing odd byte is dropped.
pub fn decode_utf16(bytes: &[u8]) -> String {
    let units = bytes.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
    core::char::decode_utf16(units).map(|c| c.unwrap_or(core::char::REPLACEMENT_CHARACTER)).collect()
}

/// Returns (length, character). Cannot be run at the end of the string.
pub fn read_utf8(bytes: &[u8], byte_offset: &mut usize) -> char {
    let x = bytes[*byte_offset];
    *byte_offset += 1;
    if x < 128 {
        return unsafe { core::char::from_u32_unchecked(x as u32) };
    }
    let init = (x & (0x7F >> 2)) as u32;
    let y = bytes[*byte_offset];
    *byte_offset += 1;
    let mut ch = utf8_acc_cont_byte(init, y);
    if x >= 0xE0 {
        let z = bytes[*byte_offset];
        *byte_offset += 1;
        let y_z = utf8_acc_cont_byte((y & CONT_MASK) as u32, z);
        ch = init << 12 | y_z;
        if x >= 0xF0 {
            let w = bytes[*byte_offset];
            *byte_offset += 1;
            ch = (init & 7) << 18 | utf8_acc_cont_byte(y_z, w);
        }
    }
    unsafe { core::char::from_u32_unchecked(ch) }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
/// Ordering is based on linebreak priority. Ordering is Hard > Soft > None.
pub struct LinebreakData {
    bits: u8,
}

pub const LINEBREAK_NONE: LinebreakData = LinebreakData::new(0b0000_0000);
pub const LINEBREAK_SOFT: LinebreakData = LinebreakData::new(0b0000_0001);
pub const LINEBREAK_HARD: LinebreakData = LinebreakData::new(0b0000_0010);

impl LinebreakData {
    const NONE: u8 = 0b0000_0000;
    const SOFT: u8 = 0b0000_0001;
    const HARD: u8 = 0b0000_0010;

    const fn new(bits: u8) -> LinebreakData {
        LinebreakData {
            bits,
        }
    }

    pub fn from_mask(wrap_soft_breaks: bool, wrap_hard_breaks: bool, has_width: bool) -> LinebreakData {
        let mut mask = 0;
        if wrap_hard_breaks {
            mask |= LinebreakData::HARD;
        }
        if wrap_soft_breaks && has_width {
            mask |= LinebreakData::SOFT;
        }
        LinebreakData {
            bits: mask,
        }
    }

    pub fn is_hard(&self) -> bool {
        self.bits == LinebreakData::HARD
    }

    pub fn is_soft(&self) -> bool {
        self.bits == LinebreakData::SOFT
    }

    pub fn mask(&self, other: LinebreakData) -> LinebreakData {
        Self::new(self.bits & other.bits)
    }
}

#[derive(Debug, Copy, Clone)]
pub struct Linebreaker {
    state: u8,
}

impl Linebreaker {
    pub fn new() -> Linebreaker {
        Linebreaker {
            state: 0,
        }
    }

    pub fn reset(&mut self) {
        self.state = 0;
    }

    // [See license/xi-editor/xi-unicode] Copyright 2016 The xi-editor Authors
    pub fn next(&mut self, codepoint: char) -> LinebreakData {
        let cp = codepoint as usize;
        let lb = if cp < 0x800 {
            LINEBREAK_1_2[cp]
        } else if cp < 0x10000 {
            let child = LINEBREAK_3_ROOT[cp >> 6];
            LINEBREAK_3_CHILD[(child as usize) * 0x40 + (cp & 0x3f)]
        } else {
            let mid = LINEBREAK_4_ROOT[cp >> 12];
            let leaf = LINEBREAK_4_MID[(mid as usize) * 0x40 + ((cp >> 6) & 0x3f)];
            LINEBREAK_4_LEAVES[(leaf as usize) * 0x40 + (cp & 0x3f)]
        };
        let i = (self.state as usize) * N_LINEBREAK_CATEGORIES + (lb as usize);
        let new = LINEBREAK_STATE_MACHINE[i];
        if (new as i8) < 0 {
            self.state = new & 0x3f;
            if new >= 0xc0 {
                LINEBREAK_HARD
            } else {
                LINEBREAK_SOFT
            }
        } else {
            self.state = new;
            LINEBREAK_NONE
        }
    }
}

/// Miscellaneous metadata associated with a character to assist in layout.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct CharacterData {
    bits: u8,
}

impl CharacterData {
    const WHITESPACE: u8 = 0b0000_0001;
    const CONTROL: u8 = 0b0000_0010;
    const MISSING: u8 = 0b0000_0100;

    /// Classifies a character given its index in the font.
    pub fn classify(c: char, index: u16) -> CharacterData {
        let mut class = 0;
        if index == 0 {
            class |= CharacterData::MISSING;
        }
        match c {
            '\t' | '\n' | '\x0C' | '\r' | ' ' => class |= CharacterData::WHITESPACE,
            _ => {}
        }
        match c {
            '\0'..='\x1F' | '\x7F' => class |= CharacterData::CONTROL,
            _ => {}
        }
        CharacterData {
            bits: class,
        }
    }

    /// A heuristic for if the glpyh this was classified from should be rasterized. Missing glyphs,
    /// whitespace, and control characters will return false.
    pub fn rasterize(&self) -> bool {
        self.bits == 0
    }

    /// Marks if the character is an ASCII whitespace character.
    pub fn is_whitespace(&self) -> bool {
        self.bits & CharacterData::WHITESPACE != 0
    }

    /// Marks if the character is an ASCII control character.
    pub fn is_control(&self) -> bool {
        self.bits & CharacterData::CONTROL != 0
    }

    /// Marks if the character is missing from its associated font.
    pub fn is_missing(&self) -> bool {
        self.bits & CharacterData::MISSING != 0
    }
}

#[cfg(test)]
mod tests {
    use super::decode_utf16;
    use alloc::string::String;

    /// Font names are untrusted: unpaired surrogates and an odd length must decode, not produce an
    /// invalid `char` or panic.
    #[test]
    fn decode_utf16_survives_malformed_names() {
        let be = |units: &[u16]| units.iter().flat_map(|u| u.to_be_bytes()).collect::<alloc::vec::Vec<u8>>();
        assert_eq!(decode_utf16(&be(&[0x0041, 0xD83D, 0xDE00, 0x0042])), "A\u{1F600}B");
        assert_eq!(decode_utf16(&be(&[0xDC00, 0x0041])), "\u{FFFD}A");
        assert_eq!(decode_utf16(&be(&[0xD800, 0x0041])), "\u{FFFD}A");
        assert_eq!(decode_utf16(&be(&[0xDBFF])), "\u{FFFD}");
        assert_eq!(decode_utf16(&[0x00, 0x41, 0x00]), String::from("A"));
    }
}
