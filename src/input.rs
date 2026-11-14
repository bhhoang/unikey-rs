//! Key -> event classification and the built-in input methods (`inputproc.cpp`).

use std::sync::OnceLock;

use crate::charset::{OFFSET, iso_base_map};
use crate::tables::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputMethod {
    Telex,
    Vni,
    Viqr,
    MsVi,
    UsrIM,
    SimpleTelex,
    SimpleTelex2,
}

// UkCharType
pub const UKC_VN: i32 = 0;
pub const UKC_WORD_BREAK: i32 = 1;
pub const UKC_NON_VN: i32 = 2;
pub const UKC_RESET: i32 = 3;

#[derive(Clone, Copy)]
pub struct KeyEvent {
    pub ev_type: i32,
    pub ch_type: i32,
    pub vn_sym: i32,
    pub key_code: u32,
    pub tone: i32,
}

// Excludes ~ ` ^ on purpose (they are VIQR marks), as the original does.
const WORD_BREAK_SYMS: &[u8] = &[
    b',', b';', b':', b'.', b'"', b'\'', b'!', b'?', b' ', b'<', b'>', b'=', b'+', b'-', b'*',
    b'/', b'\\', b'_', b'@', b'#', b'$', b'%', b'&', b'(', b')', b'{', b'}', b'[', b']', b'|',
];

pub struct Tables {
    ukc_map: [i32; 256],
    iso_vn_lexi: [i32; 256],
    pub iso_std: [u32; 256],
    pub is_vn_vowel: [bool; vnl_lastChar as usize],
    pub az_lower: [i32; 26],
    pub az_upper: [i32; 26],
}

const AZ_LOWER: [i32; 26] = [
    vnl_a, vnl_b, vnl_c, vnl_d, vnl_e, vnl_f, vnl_g, vnl_h, vnl_i, vnl_j, vnl_k, vnl_l, vnl_m,
    vnl_n, vnl_o, vnl_p, vnl_q, vnl_r, vnl_s, vnl_t, vnl_u, vnl_v, vnl_w, vnl_x, vnl_y, vnl_z,
];
const AZ_UPPER: [i32; 26] = [
    vnl_A, vnl_B, vnl_C, vnl_D, vnl_E, vnl_F, vnl_G, vnl_H, vnl_I, vnl_J, vnl_K, vnl_L, vnl_M,
    vnl_N, vnl_O, vnl_P, vnl_Q, vnl_R, vnl_S, vnl_T, vnl_U, vnl_V, vnl_W, vnl_X, vnl_Y, vnl_Z,
];

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut ukc = [UKC_NON_VN; 256];
        for c in 0..=32 {
            ukc[c] = UKC_RESET;
        }
        for c in b'a'..=b'z' {
            ukc[c as usize] = UKC_VN;
            ukc[c.to_ascii_uppercase() as usize] = UKC_VN;
        }
        for &(asc, _) in ASC_VN_LEXI.iter() {
            ukc[asc as usize] = UKC_VN;
        }
        for c in *b"jJfFwW" {
            ukc[c as usize] = UKC_NON_VN;
        }
        for &c in WORD_BREAK_SYMS {
            ukc[c as usize] = UKC_WORD_BREAK;
        }

        let mut lexi = [vnl_nonVnChar; 256];
        for &(asc, l) in ASC_VN_LEXI.iter() {
            lexi[asc as usize] = l; // later duplicates win, as in the original
        }
        for i in 0..26 {
            lexi[b'a' as usize + i] = AZ_LOWER[i];
            lexi[b'A' as usize + i] = AZ_UPPER[i];
        }

        let mut iso_std = [0u32; 256];
        for i in 0..256 {
            iso_std[i] = iso_base_map(i as u32);
            if lexi[i] != vnl_nonVnChar {
                iso_std[i] = lexi[i] as u32 + OFFSET;
            }
        }

        let mut is_vowel = [true; vnl_lastChar as usize];
        for ch in b'a'..=b'z' {
            if !b"aeiouy".contains(&ch) {
                is_vowel[AZ_LOWER[(ch - b'a') as usize] as usize] = false;
                is_vowel[AZ_UPPER[(ch - b'a') as usize] as usize] = false;
            }
        }
        is_vowel[vnl_dd as usize] = false;
        is_vowel[vnl_DD as usize] = false;

        Tables {
            ukc_map: ukc,
            iso_vn_lexi: lexi,
            iso_std,
            is_vn_vowel: is_vowel,
            az_lower: AZ_LOWER,
            az_upper: AZ_UPPER,
        }
    })
}

pub fn iso_to_vn_lexi(key_code: u32) -> i32 {
    if key_code >= 256 {
        vnl_nonVnChar
    } else {
        tables().iso_vn_lexi[key_code as usize]
    }
}

pub struct InputProcessor {
    im: InputMethod,
    key_map: [i32; 256],
}

impl InputProcessor {
    pub fn new() -> Self {
        let mut p = InputProcessor { im: InputMethod::Telex, key_map: [VNE_NORMAL; 256] };
        p.set_im(InputMethod::Telex);
        p
    }

    pub fn im(&self) -> InputMethod {
        self.im
    }

    pub fn set_im(&mut self, im: InputMethod) {
        let map: &[KeyMapping] = match im {
            InputMethod::Telex => &TELEX_MAP,
            InputMethod::SimpleTelex => &SIMPLE_TELEX_MAP,
            InputMethod::SimpleTelex2 => &SIMPLE_TELEX2_MAP,
            InputMethod::Vni => &VNI_MAP,
            InputMethod::Viqr => &VIQR_MAP,
            InputMethod::MsVi => &MSVI_MAP,
            InputMethod::UsrIM => {
                // A user map is installed with `set_user_map`; fall back to Telex.
                self.im = InputMethod::Telex;
                return self.set_im(InputMethod::Telex);
            }
        };
        self.im = im;
        self.key_map = [VNE_NORMAL; 256];
        for &(key, action) in map {
            self.key_map[key as usize] = action;
            if action < VNE_COUNT {
                if key.is_ascii_lowercase() {
                    self.key_map[key.to_ascii_uppercase() as usize] = action;
                } else if key.is_ascii_uppercase() {
                    self.key_map[key.to_ascii_lowercase() as usize] = action;
                }
            }
        }
    }

    pub fn set_user_map(&mut self, map: &[i32; 256]) {
        self.im = InputMethod::UsrIM;
        self.key_map = *map;
    }

    pub fn key_code_to_event(&self, key_code: u32) -> KeyEvent {
        let mut ev = KeyEvent { ev_type: 0, ch_type: 0, vn_sym: 0, key_code, tone: 0 };
        if key_code > 255 {
            ev.ev_type = VNE_NORMAL;
            ev.vn_sym = iso_to_vn_lexi(key_code);
            ev.ch_type = if ev.vn_sym == vnl_nonVnChar { UKC_NON_VN } else { UKC_VN };
        } else {
            ev.ch_type = tables().ukc_map[key_code as usize];
            ev.ev_type = self.key_map[key_code as usize];
            if (VNE_TONE0..=VNE_TONE5).contains(&ev.ev_type) {
                ev.tone = ev.ev_type - VNE_TONE0;
            }
            if ev.ev_type >= VNE_COUNT {
                ev.ch_type = UKC_VN;
                ev.vn_sym = ev.ev_type - VNE_COUNT;
                ev.ev_type = vneMapChar;
            } else {
                ev.vn_sym = iso_to_vn_lexi(key_code);
            }
        }
        ev
    }

    /// A key stroke taken as plain character input, not as an action key.
    pub fn key_code_to_symbol(&self, key_code: u32) -> KeyEvent {
        let vn_sym = iso_to_vn_lexi(key_code);
        let ch_type = if key_code > 255 {
            if vn_sym == vnl_nonVnChar { UKC_NON_VN } else { UKC_VN }
        } else {
            tables().ukc_map[key_code as usize]
        };
        KeyEvent { ev_type: VNE_NORMAL, ch_type, vn_sym, key_code, tone: 0 }
    }

    pub fn char_type(&self, key_code: u32) -> i32 {
        if key_code > 255 {
            if iso_to_vn_lexi(key_code) == vnl_nonVnChar { UKC_NON_VN } else { UKC_VN }
        } else {
            tables().ukc_map[key_code as usize]
        }
    }
}

pub const VNE_NORMAL: i32 = vneNormal;
pub const VNE_TONE0: i32 = vneTone0;
pub const VNE_TONE5: i32 = vneTone5;
