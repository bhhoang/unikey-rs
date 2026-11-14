//! UniKey's internal "standard" Vietnamese character codes and their Unicode
//! mapping. Codes below `OFFSET` are plain code points; `OFFSET + n` is entry
//! `n` of the lexicon (a letter with its mark and tone), see `tables.rs`.

use crate::tables::{UNICODE_TABLE, vnl_lastChar};

pub const OFFSET: u32 = 0x10000;
pub const TOTAL_VNCHARS: usize = 213;
pub const TOTAL_ALPHA_VNCHARS: u32 = 186;

/// Western symbols (0x80..0x9F block) that UniKey keeps as separate lexicon
/// entries after the alphabet.
const SPECIAL_WESTERN_CHARS: [u8; 27] = [
    0x80, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8E, 0x91, 0x92,
    0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9E, 0x9F,
];

pub fn is_std_vn_alpha(x: u32) -> bool {
    x >= OFFSET && x < OFFSET + TOTAL_ALPHA_VNCHARS
}

pub fn std_to_upper(ch: u32) -> u32 {
    if is_std_vn_alpha(ch) && ch & 1 == 1 {
        ch - 1
    } else {
        ch
    }
}

pub fn std_to_lower(ch: u32) -> u32 {
    if is_std_vn_alpha(ch) && ch & 1 == 0 {
        ch + 1
    } else {
        ch
    }
}

/// Code point -> standard char (the original's `UnicodeCharset::nextInput`).
pub fn unicode_to_std(c: u32) -> u32 {
    match UNICODE_TABLE.iter().position(|&u| u == c) {
        Some(i) => OFFSET + i as u32,
        None => c,
    }
}

/// Standard char -> Unicode char (the original's UTF-8 `putChar`).
/// `None` for codes the original would read out of bounds.
pub fn std_to_char(std: u32) -> Option<char> {
    if std < OFFSET {
        char::from_u32(std)
    } else {
        UNICODE_TABLE
            .get((std - OFFSET) as usize)
            .and_then(|&u| char::from_u32(u))
    }
}

/// Latin-1 byte (as typed) -> standard char (`IsoStdVnCharMap` before the
/// lexicon override that `input::Tables` applies).
pub fn iso_base_map(code: u32) -> u32 {
    match SPECIAL_WESTERN_CHARS.iter().position(|&b| b as u32 == code) {
        Some(i) => OFFSET + vnl_lastChar as u32 + i as u32,
        None => code,
    }
}
