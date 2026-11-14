//! The UniKey typing engine: a port of `ukengine.cpp` (UkEngine) and the glue in
//! `unikey.cpp`. Control flow is kept close to the original on purpose, so the
//! two can be compared key for key (see `tests/`).
//!
//! The engine keeps a buffer describing the word being typed. For each key it
//! reports whether it converted anything; if so, the caller must delete `backs`
//! characters before the cursor and type `text` instead. If not, the key is
//! passed through untouched.

use std::path::Path;

use crate::charset::{OFFSET, std_to_char, std_to_lower, std_to_upper};
use crate::input::*;
use crate::keymap;
use crate::macros::MacroTable;
use crate::tables::*;

const MAX_UK_ENGINE: usize = 128;
const ENTER_CHAR: u32 = 13;

// VnWordForm
const VNW_NON_VN: i32 = 0;
const VNW_EMPTY: i32 = 1;
const VNW_C: i32 = 2;
const VNW_V: i32 = 3;
const VNW_CV: i32 = 4;
const VNW_VC: i32 = 5;
const VNW_CVC: i32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    /// `text` is characters to type.
    Char,
    /// `text` is the original key strokes (after a restore).
    Key,
}

/// Result of feeding one key to the engine.
#[derive(Debug, Clone)]
pub struct Output {
    /// The engine changed the text: apply `backs` + `text`. If false, pass the key on.
    pub handled: bool,
    /// Characters to delete before the cursor first.
    pub backs: usize,
    pub text: String,
    pub kind: OutputKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub free_marking: bool,
    pub modern_style: bool,
    pub macro_enabled: bool,
    pub spell_check_enabled: bool,
    pub auto_non_vn_restore: bool,
    /// Macro text follows the case of the typed abbreviation (vn → việt nam,
    /// VN → VIỆT NAM). Off: always as written in the macro list (UniKey 4.3).
    pub macro_auto_case: bool,
}

impl Default for Options {
    /// Defaults of ibus-unikey (free marking, old tone style, spell check and
    /// auto-restore on).
    fn default() -> Self {
        Options {
            free_marking: true,
            modern_style: false,
            macro_enabled: false,
            spell_check_enabled: true,
            auto_non_vn_restore: true,
            macro_auto_case: true,
        }
    }
}

#[derive(Clone, Copy)]
struct WordInfo {
    form: i32,
    c1_offset: i32,
    v_offset: i32,
    c2_offset: i32,
    /// Union in the original: a vowel sequence (`vseq`) or consonant sequence (`cseq`).
    seq: i32,
    caps: i32,
    tone: i32,
    vn_sym: i32,
    key_code: i32,
}

impl WordInfo {
    const ZERO: WordInfo = WordInfo {
        form: VNW_NON_VN,
        c1_offset: 0,
        v_offset: 0,
        c2_offset: 0,
        seq: 0,
        caps: 0,
        tone: 0,
        vn_sym: 0,
        key_code: 0,
    };
    /// What reading before the start of the buffer yields. The original reads
    /// out of bounds there; this is the harmless value.
    const BEFORE_START: WordInfo = WordInfo { form: VNW_EMPTY, vn_sym: vnl_nonVnChar, ..Self::ZERO };
}

#[derive(Clone, Copy)]
struct KeyBufEntry {
    ev: KeyEvent,
    converted: bool,
}

const KEY_ZERO: KeyBufEntry = KeyBufEntry {
    ev: KeyEvent { ev_type: 0, ch_type: 0, vn_sym: 0, key_code: 0, tone: 0 },
    converted: false,
};

pub struct Engine {
    viet_key: bool,
    options: Options,
    input: InputProcessor,
    macros: MacroTable,
    user_map: Option<[i32; 256]>,
    shift_pressed: bool,
    caps_lock_on: bool,

    change_pos: i32,
    backs: i32,
    current: i32,
    single_mode: bool,
    key_strokes: [KeyBufEntry; MAX_UK_ENGINE],
    key_current: i32,
    to_escape: bool,
    out: String,
    output_written: bool,
    reverted: bool,
    key_restored: bool,
    key_restoring: bool,
    out_type: OutputKind,
    buffer: [WordInfo; MAX_UK_ENGINE],
    /// `static bool usedAsMapChar` in processTelexW: survives reset().
    used_as_map_char: bool,
}

// ---- lexicon helpers ------------------------------------------------------

fn is_odd(x: i32) -> bool {
    x & 1 == 1
}

fn vn_to_lower(x: i32) -> i32 {
    if x == vnl_nonVnChar || is_odd(x) { x } else { x + 1 }
}

fn change_case(x: i32) -> i32 {
    if x == vnl_nonVnChar {
        x
    } else if !is_odd(x) {
        x + 1
    } else {
        x - 1
    }
}

fn is_vn_vowel(sym: i32) -> bool {
    sym >= 0 && tables().is_vn_vowel[sym as usize]
}

fn no_tone(x: i32) -> i32 {
    STD_VN_NO_TONE[x as usize]
}

fn lookup_vseq(v1: i32, v2: i32, v3: i32) -> i32 {
    VSEQ_LIST.iter().position(|s| s.v == [v1, v2, v3]).map_or(vs_nil, |i| i as i32)
}

fn lookup_cseq(c1: i32, c2: i32, c3: i32) -> i32 {
    CSEQ_LIST.iter().position(|s| s.c == [c1, c2, c3]).map_or(cs_nil, |i| i as i32)
}

static NO_VSEQ: VowelSeqInfo = VowelSeqInfo {
    len: 0, complete: false, con_suffix: false, v: [0; 3], sub: [0; 3],
    roof_pos: 0, with_roof: 0, hook_pos: 0, with_hook: 0,
};
static NO_CSEQ: ConSeqInfo = ConSeqInfo { len: 0, c: [0; 3], suffix: false };

// The original indexes its tables with -1 ("no sequence") in a few paths, which
// reads whatever precedes the array (zeros in practice). Return that explicitly.
fn vseq(i: i32) -> &'static VowelSeqInfo {
    VSEQ_LIST.get(i as usize).unwrap_or(&NO_VSEQ)
}

fn cseq(i: i32) -> &'static ConSeqInfo {
    CSEQ_LIST.get(i as usize).unwrap_or(&NO_CSEQ)
}

fn is_alpha(code: i32) -> bool {
    (0..128).contains(&code) && (code as u8).is_ascii_alphabetic()
}

fn is_valid_cv(c: i32, v: i32) -> bool {
    if c == cs_nil || v == vs_nil {
        return true;
    }
    let info = vseq(v);
    // gi doesn't go with i; qu doesn't go with u, u+; q alone goes with no vowel
    // (the last two from fcitx5-unikey, after the 0.6.1 sources)
    if (c == cs_gi && info.v[0] == vnl_i)
        || (c == cs_qu && (info.v[0] == vnl_u || info.v[0] == vnl_uh))
        || c == cs_q
    {
        return false;
    }
    if c == cs_k {
        // k can only go with these vowel sequences
        return [vs_e, vs_i, vs_y, vs_er, vs_eo, vs_eu, vs_eru, vs_ia, vs_ie, vs_ier, vs_ieu, vs_ieru]
            .contains(&v);
    }
    true
}

fn is_valid_vc(v: i32, c: i32) -> bool {
    if v == vs_nil || c == cs_nil {
        return true;
    }
    if !vseq(v).con_suffix || !cseq(c).suffix {
        return false;
    }
    VC_PAIRS.contains(&(v, c))
}

fn is_valid_cvc(c1: i32, v: i32, c2: i32) -> bool {
    if v == vs_nil {
        return c1 == cs_nil || c2 != cs_nil;
    }
    if c1 == cs_nil {
        return is_valid_vc(v, c2);
    }
    if c2 == cs_nil {
        return is_valid_cv(c1, v);
    }
    let ok_cv = is_valid_cv(c1, v);
    let ok_vc = is_valid_vc(v, c2);
    if ok_cv && ok_vc {
        return true;
    }
    if !ok_vc {
        // quyn, quynh
        if c1 == cs_qu && v == vs_y && (c2 == cs_n || c2 == cs_nh) {
            return true;
        }
        // gieng, gie^ng
        if c1 == cs_gi && (v == vs_e || v == vs_er) && (c2 == cs_n || c2 == cs_ng) {
            return true;
        }
    }
    false
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            viet_key: true,
            options: Options::default(),
            input: InputProcessor::new(),
            macros: MacroTable::default(),
            user_map: None,
            shift_pressed: false,
            caps_lock_on: false,
            change_pos: 0,
            backs: 0,
            current: -1,
            single_mode: false,
            key_strokes: [KEY_ZERO; MAX_UK_ENGINE],
            key_current: -1,
            to_escape: false,
            out: String::new(),
            output_written: false,
            reverted: false,
            key_restored: false,
            key_restoring: false,
            out_type: OutputKind::Char,
            buffer: [WordInfo::ZERO; MAX_UK_ENGINE],
            used_as_map_char: false,
        }
    }

    // ---- configuration (unikey.cpp) --------------------------------------

    pub fn set_options(&mut self, options: Options) {
        self.options = options;
    }

    pub fn options(&self) -> Options {
        self.options
    }

    /// Turn Vietnamese conversion on or off; the buffer is reset either way.
    pub fn set_viet_key(&mut self, on: bool) {
        self.viet_key = on;
        self.reset();
    }

    pub fn viet_key(&self) -> bool {
        self.viet_key
    }

    pub fn input_method(&self) -> InputMethod {
        self.input.im()
    }

    /// Switch input method (resets the buffer). `UsrIM` needs a layout loaded
    /// with `load_user_key_map` first, otherwise it is ignored.
    pub fn set_input_method(&mut self, im: InputMethod) {
        if im == InputMethod::UsrIM {
            if let Some(map) = &self.user_map {
                self.input.set_user_map(map);
            } else {
                return;
            }
        } else {
            self.input.set_im(im);
        }
        self.reset();
    }

    pub fn load_macro_table(&mut self, path: &Path) -> std::io::Result<()> {
        self.macros.load_from_file(path)
    }

    pub fn macro_count(&self) -> usize {
        self.macros.len()
    }

    pub fn load_user_key_map(&mut self, path: &Path) -> bool {
        match keymap::load_key_map(path) {
            Some(map) => {
                self.user_map = Some(map);
                true
            }
            None => false,
        }
    }

    /// Tell the engine the shift / caps lock state of the key about to be processed.
    pub fn set_caps_state(&mut self, shift_pressed: bool, caps_lock_on: bool) {
        self.shift_pressed = shift_pressed;
        self.caps_lock_on = caps_lock_on;
    }

    // ---- state ----------------------------------------------------------

    fn w(&self, i: i32) -> &WordInfo {
        if i < 0 { &WordInfo::BEFORE_START } else { &self.buffer[i as usize] }
    }

    fn b(&mut self, i: i32) -> &mut WordInfo {
        &mut self.buffer[i as usize]
    }

    pub fn reset(&mut self) {
        self.current = -1;
        self.key_current = -1;
        self.single_mode = false;
        self.to_escape = false;
    }

    pub fn set_single_mode(&mut self) {
        self.single_mode = true;
    }

    pub fn at_word_beginning(&self) -> bool {
        self.current < 0 || self.w(self.current).form == VNW_EMPTY
    }

    // ---- key processing ---------------------------------------------------

    fn get_tone_position(&self, vs: i32, terminated: bool) -> i32 {
        let info = vseq(vs);
        if info.len == 1 {
            return 0;
        }
        if info.roof_pos != -1 {
            return info.roof_pos;
        }
        if info.hook_pos != -1 {
            if vs == vs_uhoh || vs == vs_uhohi || vs == vs_uhohu {
                return 1; // u+o+, u+o+u, u+o+i
            }
            return info.hook_pos;
        }
        if info.len == 3 {
            return 1;
        }
        if self.options.modern_style && (vs == vs_oa || vs == vs_oe || vs == vs_uy) {
            return 1;
        }
        if terminated { 0 } else { 1 }
    }

    /// Vowel-sequence symbols at `[start, start + len)` looked up as a sequence.
    fn vseq_at(&self, start: i32, len: i32) -> i32 {
        let s = |k: i32| self.w(start + k).vn_sym;
        match len {
            3 => lookup_vseq(s(0), s(1), s(2)),
            2 => lookup_vseq(s(0), s(1), vnl_nonVnChar),
            _ => lookup_vseq(s(0), vnl_nonVnChar, vnl_nonVnChar),
        }
    }

    /// (c1, c2) consonant sequences around the current vowel group.
    fn surrounding_cseqs(&self) -> (i32, i32) {
        let cur = self.w(self.current);
        let c1 = if cur.c1_offset != -1 { self.w(self.current - cur.c1_offset).seq } else { cs_nil };
        let c2 = if cur.c2_offset != -1 { self.w(self.current - cur.c2_offset).seq } else { cs_nil };
        (c1, c2)
    }

    fn process_roof(&mut self, ev: &mut KeyEvent) -> i32 {
        if !self.viet_key || self.current < 0 || self.w(self.current).v_offset < 0 {
            return self.process_append(ev);
        }

        let target = match ev.ev_type {
            vneRoof_a => vnl_ar,
            vneRoof_e => vnl_er,
            vneRoof_o => vnl_or,
            _ => vnl_nonVnChar,
        };

        let mut roof_removed = false;
        let v_end = self.current - self.w(self.current).v_offset;
        let vs = self.w(v_end).seq;
        let v_start = v_end - (vseq(vs).len - 1);
        let cur_tone_pos = v_start + self.get_tone_position(vs, v_end == self.current);
        let tone = self.w(cur_tone_pos).tone;

        let mut double_change_uo = false;
        let mut new_vs;
        if vs == vs_uho || vs == vs_uhoh || vs == vs_uhoi || vs == vs_uhohi {
            // special cases: u+o+ -> uo^, u+o -> uo^, u+o+i -> uo^i, u+oi -> uo^i
            new_vs = lookup_vseq(vnl_u, vnl_or, vseq(vs).v[2]);
            double_change_uo = true;
        } else {
            new_vs = vseq(vs).with_roof;
        }

        let info: &VowelSeqInfo;
        if new_vs == vs_nil {
            if vseq(vs).roof_pos == -1 {
                return self.process_append(ev); // roof is not applicable
            }
            // a roof already exists -> undo roof
            let cur_ch = self.w(v_start + vseq(vs).roof_pos).vn_sym;
            if target != vnl_nonVnChar && cur_ch != target {
                return self.process_append(ev); // specific roof and the roof character don't match
            }
            let new_ch = if cur_ch == vnl_ar {
                vnl_a
            } else if cur_ch == vnl_er {
                vnl_e
            } else {
                vnl_o
            };
            let change_pos = v_start + vseq(vs).roof_pos;
            if !self.options.free_marking && change_pos != self.current {
                return self.process_append(ev);
            }
            self.mark_change(change_pos);
            self.b(change_pos).vn_sym = new_ch;
            new_vs = self.vseq_at(v_start, vseq(vs).len);
            info = vseq(new_vs);
            roof_removed = true;
        } else {
            info = vseq(new_vs);
            if target != vnl_nonVnChar && info.v[info.roof_pos as usize] != target {
                return self.process_append(ev);
            }
            // check validity of new VC and CV
            let (c1, c2) = self.surrounding_cseqs();
            if !is_valid_cvc(c1, new_vs, c2) {
                return self.process_append(ev);
            }
            let change_pos = if double_change_uo { v_start } else { v_start + info.roof_pos };
            if !self.options.free_marking && change_pos != self.current {
                return self.process_append(ev);
            }
            self.mark_change(change_pos);
            if double_change_uo {
                self.b(v_start).vn_sym = vnl_u;
                self.b(v_start + 1).vn_sym = vnl_or;
            } else {
                self.b(change_pos).vn_sym = info.v[info.roof_pos as usize];
            }
        }

        for i in 0..info.len {
            self.b(v_start + i).seq = info.sub[i as usize]; // update sub-sequences
        }

        // check if tone re-position is needed
        let new_tone_pos = v_start + self.get_tone_position(new_vs, v_end == self.current);
        if cur_tone_pos != new_tone_pos && tone != 0 {
            self.mark_change(new_tone_pos);
            self.b(new_tone_pos).tone = tone;
            self.mark_change(cur_tone_pos);
            self.b(cur_tone_pos).tone = 0;
        }

        if roof_removed {
            self.single_mode = false;
            self.process_append(ev);
            self.reverted = true;
        }
        1
    }

    /// Only called from process_hook.
    fn process_hook_with_uo(&mut self, ev: &mut KeyEvent) -> i32 {
        let mut hook_removed = false;

        if !self.options.free_marking && self.w(self.current).v_offset != 0 {
            return self.process_append(ev);
        }

        let v_end = self.current - self.w(self.current).v_offset;
        let vs = self.w(v_end).seq;
        let v_start = v_end - (vseq(vs).len - 1);
        let v = vseq(vs).v;
        let cur_tone_pos = v_start + self.get_tone_position(vs, v_end == self.current);
        let tone = self.w(cur_tone_pos).tone;
        let new_vs;

        match ev.ev_type {
            vneHook_u => {
                if v[0] == vnl_u {
                    new_vs = vseq(vs).with_hook;
                    self.mark_change(v_start);
                    self.b(v_start).vn_sym = vnl_uh;
                } else {
                    // v[0] = vnl_uh, -> uo
                    new_vs = lookup_vseq(vnl_u, vnl_o, v[2]);
                    self.mark_change(v_start);
                    self.b(v_start).vn_sym = vnl_u;
                    self.b(v_start + 1).vn_sym = vnl_o;
                    hook_removed = true;
                }
            }
            vneHook_o => {
                if v[1] == vnl_o || v[1] == vnl_or {
                    if v_end == self.current
                        && vseq(vs).len == 2
                        && self.w(self.current).form == VNW_CV
                        && self.w(self.current - 2).seq == cs_th
                    {
                        // o|o^ -> o+
                        new_vs = vseq(vs).with_hook;
                        self.mark_change(v_start + 1);
                        self.b(v_start + 1).vn_sym = vnl_oh;
                    } else {
                        new_vs = lookup_vseq(vnl_uh, vnl_oh, v[2]);
                        if v[0] == vnl_u {
                            self.mark_change(v_start);
                            self.b(v_start).vn_sym = vnl_uh;
                            self.b(v_start + 1).vn_sym = vnl_oh;
                        } else {
                            self.mark_change(v_start + 1);
                            self.b(v_start + 1).vn_sym = vnl_oh;
                        }
                    }
                } else {
                    // v[1] = vnl_oh, -> uo
                    new_vs = lookup_vseq(vnl_u, vnl_o, v[2]);
                    if v[0] == vnl_uh {
                        self.mark_change(v_start);
                        self.b(v_start).vn_sym = vnl_u;
                        self.b(v_start + 1).vn_sym = vnl_o;
                    } else {
                        self.mark_change(v_start + 1);
                        self.b(v_start + 1).vn_sym = vnl_o;
                    }
                    hook_removed = true;
                }
            }
            _ => {
                // vneHookAll, vneHook_uo
                if v[0] == vnl_u {
                    if v[1] == vnl_o || v[1] == vnl_or {
                        // uo -> uo+ if prefixed by "h", "kh", "th", or standing alone
                        // ("h", "kh" and alone from fcitx5-unikey #25: huơ, khuơ, uơ)
                        let cur_form = self.w(self.current).form;
                        let c1 = self.w(self.current - 2).seq;
                        if (vs == vs_uo || vs == vs_uor)
                            && v_end == self.current
                            && ((cur_form == VNW_CV && (c1 == cs_h || c1 == cs_kh || c1 == cs_th))
                                || cur_form == VNW_V)
                        {
                            new_vs = vs_uoh;
                            self.mark_change(v_start + 1);
                            self.b(v_start + 1).vn_sym = vnl_oh;
                        } else {
                            // uo -> u+o+
                            let first = vseq(vs).with_hook;
                            self.mark_change(v_start);
                            self.b(v_start).vn_sym = vnl_uh;
                            new_vs = vseq(first).with_hook;
                            self.b(v_start + 1).vn_sym = vnl_oh;
                        }
                    } else {
                        // uo+ -> u+o+
                        new_vs = vseq(vs).with_hook;
                        self.mark_change(v_start);
                        self.b(v_start).vn_sym = vnl_uh;
                    }
                } else if v[1] == vnl_o {
                    // u+o -> u+o+
                    new_vs = vseq(vs).with_hook;
                    self.mark_change(v_start + 1);
                    self.b(v_start + 1).vn_sym = vnl_oh;
                } else {
                    // v[1] == vnl_oh, u+o+ -> uo
                    new_vs = lookup_vseq(vnl_u, vnl_o, v[2]);
                    self.mark_change(v_start);
                    self.b(v_start).vn_sym = vnl_u;
                    self.b(v_start + 1).vn_sym = vnl_o;
                    hook_removed = true;
                }
            }
        }

        let p = vseq(new_vs);
        for i in 0..p.len {
            self.b(v_start + i).seq = p.sub[i as usize];
        }

        // check if tone re-position is needed
        let new_tone_pos = v_start + self.get_tone_position(new_vs, v_end == self.current);
        if cur_tone_pos != new_tone_pos && tone != 0 {
            self.mark_change(new_tone_pos);
            self.b(new_tone_pos).tone = tone;
            self.mark_change(cur_tone_pos);
            self.b(cur_tone_pos).tone = 0;
        }

        if hook_removed {
            self.single_mode = false;
            self.process_append(ev);
            self.reverted = true;
        }
        1
    }

    fn process_hook(&mut self, ev: &mut KeyEvent) -> i32 {
        if !self.viet_key || self.current < 0 || self.w(self.current).v_offset < 0 {
            return self.process_append(ev);
        }

        let mut hook_removed = false;
        let v_end = self.current - self.w(self.current).v_offset;
        let vs = self.w(v_end).seq;
        let v = vseq(vs).v;

        if vseq(vs).len > 1
            && ev.ev_type != vneBowl
            && (v[0] == vnl_u || v[0] == vnl_uh)
            && (v[1] == vnl_o || v[1] == vnl_oh || v[1] == vnl_or)
        {
            return self.process_hook_with_uo(ev);
        }

        let v_start = v_end - (vseq(vs).len - 1);
        let cur_tone_pos = v_start + self.get_tone_position(vs, v_end == self.current);
        let tone = self.w(cur_tone_pos).tone;

        let mut new_vs = vseq(vs).with_hook;
        let info: &VowelSeqInfo;
        if new_vs == vs_nil {
            if vseq(vs).hook_pos == -1 {
                return self.process_append(ev); // hook is not applicable
            }
            // a hook already exists -> undo hook
            let cur_ch = self.w(v_start + vseq(vs).hook_pos).vn_sym;
            let new_ch = if cur_ch == vnl_ab {
                vnl_a
            } else if cur_ch == vnl_uh {
                vnl_u
            } else {
                vnl_o
            };
            let change_pos = v_start + vseq(vs).hook_pos;
            if !self.options.free_marking && change_pos != self.current {
                return self.process_append(ev);
            }
            match ev.ev_type {
                vneHook_u => {
                    if cur_ch != vnl_uh {
                        return self.process_append(ev);
                    }
                }
                vneHook_o => {
                    if cur_ch != vnl_oh {
                        return self.process_append(ev);
                    }
                }
                vneBowl => {
                    if cur_ch != vnl_ab {
                        return self.process_append(ev);
                    }
                }
                _ => {
                    if ev.ev_type == vneHook_uo && cur_ch == vnl_ab {
                        return self.process_append(ev);
                    }
                }
            }

            self.mark_change(change_pos);
            self.b(change_pos).vn_sym = new_ch;
            new_vs = self.vseq_at(v_start, vseq(vs).len);
            info = vseq(new_vs);
            hook_removed = true;
        } else {
            info = vseq(new_vs);
            let hv = info.v[info.hook_pos as usize];
            match ev.ev_type {
                vneHook_u => {
                    if hv != vnl_uh {
                        return self.process_append(ev);
                    }
                }
                vneHook_o => {
                    if hv != vnl_oh {
                        return self.process_append(ev);
                    }
                }
                vneBowl => {
                    if hv != vnl_ab {
                        return self.process_append(ev);
                    }
                }
                _ => {
                    if ev.ev_type == vneHook_uo && hv == vnl_ab {
                        return self.process_append(ev);
                    }
                }
            }

            // check validity of new VC and CV
            let (c1, c2) = self.surrounding_cseqs();
            if !is_valid_cvc(c1, new_vs, c2) {
                return self.process_append(ev);
            }
            let change_pos = v_start + info.hook_pos;
            if !self.options.free_marking && change_pos != self.current {
                return self.process_append(ev);
            }
            self.mark_change(change_pos);
            self.b(change_pos).vn_sym = hv;
        }

        for i in 0..info.len {
            self.b(v_start + i).seq = info.sub[i as usize]; // update sub-sequences
        }

        // check if tone re-position is needed
        let new_tone_pos = v_start + self.get_tone_position(new_vs, v_end == self.current);
        if cur_tone_pos != new_tone_pos && tone != 0 {
            self.mark_change(new_tone_pos);
            self.b(new_tone_pos).tone = tone;
            self.mark_change(cur_tone_pos);
            self.b(cur_tone_pos).tone = 0;
        }

        if hook_removed {
            self.single_mode = false;
            self.process_append(ev);
            self.reverted = true;
        }
        1
    }

    fn process_tone(&mut self, ev: &mut KeyEvent) -> i32 {
        if self.current < 0 || !self.viet_key {
            return self.process_append(ev);
        }

        let cur = *self.w(self.current);
        if cur.form == VNW_C && (cur.seq == cs_gi || cur.seq == cs_gin) {
            let p = if cur.seq == cs_gi { self.current } else { self.current - 1 };
            if self.w(p).tone == 0 && ev.tone == 0 {
                return self.process_append(ev);
            }
            self.mark_change(p);
            if self.w(p).tone == ev.tone {
                self.b(p).tone = 0;
                self.single_mode = false;
                self.process_append(ev);
                self.reverted = true;
                return 1;
            }
            self.b(p).tone = ev.tone;
            return 1;
        }

        if cur.v_offset < 0 {
            return self.process_append(ev);
        }

        let v_end = self.current - cur.v_offset;
        let vs = self.w(v_end).seq;
        let info = vseq(vs);
        if self.options.spell_check_enabled && !self.options.free_marking && !info.complete {
            return self.process_append(ev);
        }

        if cur.form == VNW_VC || cur.form == VNW_CVC {
            let cs = cur.seq;
            if (cs == cs_c || cs == cs_ch || cs == cs_p || cs == cs_t)
                && (ev.tone == 2 || ev.tone == 3 || ev.tone == 4)
            {
                return self.process_append(ev); // c, ch, p, t suffixes don't allow ` ? ~
            }
        }

        let tone_offset = self.get_tone_position(vs, v_end == self.current);
        let tone_pos = v_end - (info.len - 1) + tone_offset;
        if self.w(tone_pos).tone == 0 && ev.tone == 0 {
            return self.process_append(ev);
        }

        if self.w(tone_pos).tone == ev.tone {
            self.mark_change(tone_pos);
            self.b(tone_pos).tone = 0;
            self.single_mode = false;
            self.process_append(ev);
            self.reverted = true;
            return 1;
        }

        self.mark_change(tone_pos);
        self.b(tone_pos).tone = ev.tone;
        1
    }

    fn process_dd(&mut self, ev: &mut KeyEvent) -> i32 {
        if !self.viet_key || self.current < 0 {
            return self.process_append(ev);
        }

        // dd is allowed even in a non-vn sequence (used a lot in abbreviations),
        // as long as the preceding character is not a vowel
        let cur = *self.w(self.current);
        let prev_sym = self.w(self.current - 1).vn_sym;
        if cur.form == VNW_NON_VN
            && cur.vn_sym == vnl_d
            && (prev_sym == vnl_nonVnChar || !is_vn_vowel(prev_sym))
        {
            self.single_mode = true;
            let pos = self.current;
            self.mark_change(pos);
            let e = self.b(pos);
            e.seq = cs_dd;
            e.vn_sym = vnl_dd;
            e.form = VNW_C;
            e.c1_offset = 0;
            e.c2_offset = -1;
            e.v_offset = -1;
            return 1;
        }

        if cur.c1_offset < 0 {
            return self.process_append(ev);
        }

        let pos = self.current - cur.c1_offset;
        if !self.options.free_marking && pos != self.current {
            return self.process_append(ev);
        }

        if self.w(pos).seq == cs_d {
            self.mark_change(pos);
            let e = self.b(pos);
            e.seq = cs_dd;
            e.vn_sym = vnl_dd;
            // never spellcheck a word which starts with dd (abbreviations)
            self.single_mode = true;
            return 1;
        }

        if self.w(pos).seq == cs_dd {
            // undo dd
            self.mark_change(pos);
            let e = self.b(pos);
            e.seq = cs_d;
            e.vn_sym = vnl_d;
            self.single_mode = false;
            self.process_append(ev);
            self.reverted = true;
            return 1;
        }

        self.process_append(ev)
    }

    fn process_map_char(&mut self, ev: &mut KeyEvent) -> i32 {
        if self.caps_lock_on {
            ev.vn_sym = change_case(ev.vn_sym);
        }

        let mut ret = self.process_append(ev);
        if !self.viet_key {
            return ret;
        }

        let cf = self.w(self.current).form;
        if self.current >= 0 && cf != VNW_EMPTY && cf != VNW_NON_VN {
            return 1;
        }

        if self.current < 0 {
            return 0;
        }

        // mapChar doesn't apply
        self.current -= 1;
        let entry = *self.w(self.current);

        let mut undo = false;
        // test if undo is needed
        if entry.form != VNW_EMPTY && entry.form != VNW_NON_VN {
            let mut prev_sym = entry.vn_sym;
            if entry.caps != 0 {
                prev_sym -= 1;
            }
            // Unlike the original, a letter key undoes in either case: Shift+W then
            // w undoes "Ư" too, and the letter keeps the first key's case ("Ww" ->
            // "W"). Symbol keys stay exact: "]" and "}" are different keys.
            let letter = ev.key_code < 128 && (ev.key_code as u8).is_ascii_alphabetic();
            if prev_sym == ev.vn_sym || (letter && prev_sym == change_case(ev.vn_sym)) {
                if letter {
                    let k = ev.key_code as u8;
                    ev.key_code = if entry.caps != 0 { k.to_ascii_uppercase() } else { k.to_ascii_lowercase() } as _;
                }
                if entry.form != VNW_C {
                    let v_end = self.current - entry.v_offset;
                    let vs = self.w(v_end).seq;
                    let v_start = v_end - vseq(vs).len + 1;
                    let cur_tone_pos = v_start + self.get_tone_position(vs, v_end == self.current);
                    let tone = self.w(cur_tone_pos).tone;
                    self.mark_change(self.current);
                    self.current -= 1;

                    // check if tone position is needed
                    let f = self.w(self.current).form;
                    if tone != 0 && self.current >= 0 && (f == VNW_V || f == VNW_CV) {
                        let new_vs = self.w(self.current).seq;
                        let new_tone_pos = v_start + self.get_tone_position(new_vs, true);
                        if new_tone_pos != cur_tone_pos {
                            self.mark_change(new_tone_pos);
                            self.b(new_tone_pos).tone = tone;
                            self.mark_change(cur_tone_pos);
                            self.b(cur_tone_pos).tone = 0;
                        }
                    }
                } else {
                    self.mark_change(self.current);
                    self.current -= 1;
                }
                undo = true;
            }
        }

        ev.ev_type = vneNormal;
        ev.ch_type = self.input.char_type(ev.key_code);
        ev.vn_sym = iso_to_vn_lexi(ev.key_code);
        ret = self.process_append(ev);
        if undo {
            self.single_mode = false;
            self.reverted = true;
            return 1;
        }
        ret
    }

    fn process_telex_w(&mut self, ev: &mut KeyEvent) -> i32 {
        if !self.viet_key {
            return self.process_append(ev);
        }

        let upper = (ev.key_code as u8).is_ascii_uppercase() && ev.key_code < 128;
        let uh = if upper { vnl_Uh } else { vnl_uh };

        if self.used_as_map_char {
            ev.ev_type = vneMapChar;
            ev.vn_sym = if self.caps_lock_on { change_case(uh) } else { uh };
            ev.ch_type = UKC_VN;
            let ret = self.process_map_char(ev);
            if ret == 0 {
                if self.current >= 0 {
                    self.current -= 1;
                }
                self.used_as_map_char = false;
                ev.ev_type = vneHookAll;
                return self.process_hook(ev);
            }
            return ret;
        }

        ev.ev_type = vneHookAll;
        self.used_as_map_char = false;
        let ret = self.process_hook(ev);
        if ret == 0 {
            if self.current >= 0 {
                self.current -= 1;
            }
            ev.ev_type = vneMapChar;
            ev.vn_sym = if self.caps_lock_on { change_case(uh) } else { uh };
            ev.ch_type = UKC_VN;
            self.used_as_map_char = true;
            return self.process_map_char(ev);
        }
        ret
    }

    fn process_esc_char(&mut self, ev: &mut KeyEvent) -> i32 {
        let cf = self.w(self.current).form;
        if self.viet_key && self.current >= 0 && cf != VNW_EMPTY && cf != VNW_NON_VN {
            self.to_escape = true;
        }
        self.process_append(ev)
    }

    fn process_append(&mut self, ev: &mut KeyEvent) -> i32 {
        match ev.ch_type {
            UKC_RESET => {
                if ev.key_code == ENTER_CHAR {
                    // (Windows only in the original: macros on Enter.)
                }
                self.reset();
                0
            }
            UKC_WORD_BREAK => {
                self.single_mode = false;
                self.process_word_end(ev)
            }
            UKC_NON_VN => {
                self.current += 1;
                let sym = vn_to_lower(ev.vn_sym);
                let cur = self.current;
                let e = self.b(cur);
                e.form = VNW_NON_VN;
                e.c1_offset = -1;
                e.c2_offset = -1;
                e.v_offset = -1;
                e.key_code = ev.key_code as i32;
                e.vn_sym = sym;
                e.tone = 0;
                e.caps = (sym != ev.vn_sym) as i32;
                0
            }
            UKC_VN => {
                if is_vn_vowel(ev.vn_sym) {
                    let v = no_tone(vn_to_lower(ev.vn_sym));
                    let cur = *self.w(self.current);
                    if self.current >= 0
                        && cur.form == VNW_C
                        && ((cur.seq == cs_q && v == vnl_u) || (cur.seq == cs_g && v == vnl_i))
                    {
                        return self.append_consonant(ev); // u after q, i after g are consonants
                    }
                    return self.append_vowel(ev);
                }
                self.append_consonant(ev)
            }
            _ => 0,
        }
    }

    fn append_vowel(&mut self, ev: &mut KeyEvent) -> i32 {
        let mut complex_event = false;
        self.current += 1;
        let cur = self.current;

        let lower_sym = vn_to_lower(ev.vn_sym);
        let can_sym = no_tone(lower_sym);

        {
            let e = self.b(cur);
            e.vn_sym = can_sym;
            e.caps = (lower_sym != ev.vn_sym) as i32;
            e.tone = (lower_sym - can_sym) / 2;
            e.key_code = ev.key_code as i32;
        }

        if cur == 0 || !self.viet_key {
            let e = self.b(cur);
            e.form = VNW_V;
            e.c1_offset = -1;
            e.c2_offset = -1;
            e.v_offset = 0;
            e.seq = lookup_vseq(can_sym, vnl_nonVnChar, vnl_nonVnChar);
            if !self.viet_key || is_alpha(self.buffer[cur as usize].key_code) {
                return 0;
            }
            self.mark_change(cur);
            return 1;
        }

        let prev = *self.w(cur - 1);
        let mut new_vs = vs_nil;

        match prev.form {
            VNW_EMPTY => {
                new_vs = lookup_vseq(can_sym, vnl_nonVnChar, vnl_nonVnChar);
                let e = self.b(cur);
                e.form = VNW_V;
                e.c1_offset = -1;
                e.c2_offset = -1;
                e.v_offset = 0;
                e.seq = new_vs;
            }
            VNW_NON_VN | VNW_CVC | VNW_VC => {
                let e = self.b(cur);
                e.form = VNW_NON_VN;
                e.c1_offset = -1;
                e.c2_offset = -1;
                e.v_offset = -1;
            }
            VNW_V | VNW_CV => {
                let mut vs = prev.seq;
                let prev_tone_pos = (cur - 1) - (vseq(vs).len - 1) + self.get_tone_position(vs, true);
                let mut tone = self.w(prev_tone_pos).tone;

                // u+o / uo+ followed by u or i -> u+o+ u/i: hươu, người (fcitx5-unikey #25)
                if (vs == vs_uoh || vs == vs_uho) && (lower_sym == vnl_i || lower_sym == vnl_u) {
                    if vs == vs_uho {
                        self.mark_change(cur - 1);
                        let p = self.b(cur - 1);
                        p.vn_sym = vnl_oh;
                        p.seq = vs_uhoh;
                    } else {
                        self.mark_change(cur - 2);
                        let p2 = self.b(cur - 2);
                        p2.vn_sym = vnl_uh;
                        p2.seq = vs_uh;
                    }
                    vs = vs_uhoh;
                    complex_event = true;
                }

                if lower_sym != can_sym && tone != 0 {
                    // new sym has a tone, but there is already a preceding tone
                    new_vs = vs_nil;
                } else if vseq(vs).len == 3 {
                    new_vs = vs_nil;
                } else if vseq(vs).len == 2 {
                    new_vs = lookup_vseq(vseq(vs).v[0], vseq(vs).v[1], can_sym);
                } else {
                    new_vs = lookup_vseq(vseq(vs).v[0], can_sym, vnl_nonVnChar);
                }

                if new_vs != vs_nil && prev.form == VNW_CV {
                    let cs = self.w(cur - 1 - prev.c1_offset).seq;
                    if !is_valid_cv(cs, new_vs) {
                        new_vs = vs_nil;
                    }
                }

                if new_vs == vs_nil {
                    let e = self.b(cur);
                    e.form = VNW_NON_VN;
                    e.c1_offset = -1;
                    e.c2_offset = -1;
                    e.v_offset = -1;
                } else {
                    {
                        let e = self.b(cur);
                        e.form = prev.form;
                        e.c1_offset = if prev.form == VNW_CV { prev.c1_offset + 1 } else { -1 };
                        e.c2_offset = -1;
                        e.v_offset = 0;
                        e.seq = new_vs;
                        e.tone = 0;
                    }

                    let new_tone = (lower_sym - can_sym) / 2;
                    if tone == 0 {
                        if new_tone != 0 {
                            tone = new_tone;
                            let tone_pos =
                                self.get_tone_position(new_vs, true) + ((cur - 1) - vseq(vs).len + 1);
                            self.mark_change(tone_pos);
                            self.b(tone_pos).tone = tone;
                            return 1;
                        }
                    } else {
                        let new_tone_pos =
                            self.get_tone_position(new_vs, true) + ((cur - 1) - vseq(vs).len + 1);
                        if new_tone_pos != prev_tone_pos {
                            self.mark_change(prev_tone_pos);
                            self.b(prev_tone_pos).tone = 0;
                            self.mark_change(new_tone_pos);
                            if new_tone != 0 {
                                tone = new_tone;
                            }
                            self.b(new_tone_pos).tone = tone;
                            return 1;
                        }
                        if new_tone != 0 && new_tone != tone {
                            tone = new_tone;
                            self.mark_change(prev_tone_pos);
                            self.b(prev_tone_pos).tone = tone;
                            return 1;
                        }
                    }
                }
            }
            VNW_C => {
                new_vs = lookup_vseq(can_sym, vnl_nonVnChar, vnl_nonVnChar);
                let cs = prev.seq;
                if !is_valid_cv(cs, new_vs) {
                    let e = self.b(cur);
                    e.form = VNW_NON_VN;
                    e.c1_offset = -1;
                    e.c2_offset = -1;
                    e.v_offset = -1;
                } else {
                    {
                        let e = self.b(cur);
                        e.form = VNW_CV;
                        e.c1_offset = 1;
                        e.c2_offset = -1;
                        e.v_offset = 0;
                        e.seq = new_vs;
                    }
                    if cs == cs_gi && prev.tone != 0 {
                        if self.w(cur).tone == 0 {
                            self.b(cur).tone = prev.tone;
                        }
                        self.mark_change(cur - 1);
                        self.b(cur - 1).tone = 0;
                        return 1;
                    }
                }
            }
            _ => {}
        }
        let _ = new_vs;

        if complex_event {
            return 1;
        }

        if is_alpha(self.w(cur).key_code) {
            return 0;
        }

        self.mark_change(cur);
        1
    }

    fn append_consonant(&mut self, ev: &mut KeyEvent) -> i32 {
        let mut complex_event = false;
        self.current += 1;
        let cur = self.current;

        let lower_sym = vn_to_lower(ev.vn_sym);
        {
            let e = self.b(cur);
            e.vn_sym = lower_sym;
            e.caps = (lower_sym != ev.vn_sym) as i32;
            e.key_code = ev.key_code as i32;
            e.tone = 0;
        }

        if cur == 0 || !self.viet_key {
            let e = self.b(cur);
            e.form = VNW_C;
            e.c1_offset = 0;
            e.c2_offset = -1;
            e.v_offset = -1;
            e.seq = lookup_cseq(lower_sym, vnl_nonVnChar, vnl_nonVnChar);
            return 0;
        }

        let prev = *self.w(cur - 1);

        match prev.form {
            VNW_NON_VN => {
                let e = self.b(cur);
                e.form = VNW_NON_VN;
                e.c1_offset = -1;
                e.c2_offset = -1;
                e.v_offset = -1;
                0
            }
            VNW_EMPTY => {
                let e = self.b(cur);
                e.form = VNW_C;
                e.c1_offset = 0;
                e.c2_offset = -1;
                e.v_offset = -1;
                e.seq = lookup_cseq(lower_sym, vnl_nonVnChar, vnl_nonVnChar);
                0
            }
            VNW_V | VNW_CV => {
                let vs = prev.seq;
                let mut new_vs = vs;
                if vs == vs_uoh || vs == vs_uho {
                    new_vs = vs_uhoh;
                }

                let c1 = if prev.c1_offset != -1 { self.w(cur - 1 - prev.c1_offset).seq } else { cs_nil };
                let new_cs = lookup_cseq(lower_sym, vnl_nonVnChar, vnl_nonVnChar);
                let is_valid = is_valid_cvc(c1, new_vs, new_cs);

                if is_valid {
                    // check u+o -> u+o+
                    if vs == vs_uho {
                        self.mark_change(cur - 1);
                        let p = self.b(cur - 1);
                        p.vn_sym = vnl_oh;
                        p.seq = vs_uhoh;
                        complex_event = true;
                    } else if vs == vs_uoh {
                        self.mark_change(cur - 2);
                        let p2 = self.b(cur - 2);
                        p2.vn_sym = vnl_uh;
                        p2.seq = vs_uh;
                        self.b(cur - 1).seq = vs_uhoh;
                        complex_event = true;
                    }

                    {
                        let e = self.b(cur);
                        if prev.form == VNW_V {
                            e.form = VNW_VC;
                            e.c1_offset = -1;
                        } else {
                            e.form = VNW_CVC;
                            e.c1_offset = prev.c1_offset + 1;
                        }
                        e.c2_offset = 0;
                        e.v_offset = 1;
                        e.seq = new_cs;
                    }

                    // reposition tone if needed
                    let old_idx = (cur - 1) - (vseq(vs).len - 1) + self.get_tone_position(vs, true);
                    if self.w(old_idx).tone != 0 {
                        let new_idx = (cur - 1) - (vseq(new_vs).len - 1) + self.get_tone_position(new_vs, false);
                        if new_idx != old_idx {
                            self.mark_change(new_idx);
                            let t = self.w(old_idx).tone;
                            self.b(new_idx).tone = t;
                            self.mark_change(old_idx);
                            self.b(old_idx).tone = 0;
                            return 1;
                        }
                    }
                } else {
                    let e = self.b(cur);
                    e.form = VNW_NON_VN;
                    e.c1_offset = -1;
                    e.c2_offset = -1;
                    e.v_offset = -1;
                }

                if complex_event {
                    return 1;
                }
                0
            }
            VNW_C | VNW_VC | VNW_CVC => {
                let cs = prev.seq;
                let mut new_cs = if cseq(cs).len == 3 {
                    cs_nil
                } else if cseq(cs).len == 2 {
                    lookup_cseq(cseq(cs).c[0], cseq(cs).c[1], lower_sym)
                } else {
                    lookup_cseq(cseq(cs).c[0], lower_sym, vnl_nonVnChar)
                };

                if new_cs != cs_nil && (prev.form == VNW_VC || prev.form == VNW_CVC) {
                    // Check CVC combination
                    let c1 = if prev.c1_offset != -1 { self.w(cur - 1 - prev.c1_offset).seq } else { cs_nil };
                    let v_idx = (cur - 1) - prev.v_offset;
                    let vs = self.w(v_idx).seq;
                    if !is_valid_cvc(c1, vs, new_cs) {
                        new_cs = cs_nil;
                    }
                }

                let e = self.b(cur);
                if new_cs == cs_nil {
                    e.form = VNW_NON_VN;
                    e.c1_offset = -1;
                    e.c2_offset = -1;
                    e.v_offset = -1;
                } else {
                    if prev.form == VNW_C {
                        e.form = VNW_C;
                        e.c1_offset = 0;
                        e.c2_offset = -1;
                        e.v_offset = -1;
                    } else if prev.form == VNW_VC {
                        e.form = VNW_VC;
                        e.c1_offset = -1;
                        e.c2_offset = 0;
                        e.v_offset = prev.v_offset + 1;
                    } else {
                        e.form = VNW_CVC;
                        e.c1_offset = prev.c1_offset + 1;
                        e.c2_offset = 0;
                        e.v_offset = prev.v_offset + 1;
                    }
                    e.seq = new_cs;
                }
                0
            }
            _ => 0,
        }
    }

    pub fn pass(&mut self, key_code: u32) {
        let mut ev = self.input.key_code_to_event(key_code);
        self.process_append(&mut ev);
    }

    /// Called only after the other processing: the new event is already in the buffer.
    fn process_no_spell_check(&mut self, ev: &KeyEvent) -> i32 {
        let cur = self.current;
        let sym = self.w(cur).vn_sym;
        if is_vn_vowel(sym) {
            let e = self.b(cur);
            e.form = VNW_V;
            e.v_offset = 0;
            e.seq = lookup_vseq(sym, vnl_nonVnChar, vnl_nonVnChar);
            e.c1_offset = -1;
            e.c2_offset = -1;
        } else {
            let e = self.b(cur);
            e.form = VNW_C;
            e.c1_offset = 0;
            e.c2_offset = -1;
            e.v_offset = -1;
            e.seq = lookup_cseq(sym, vnl_nonVnChar, vnl_nonVnChar);
        }

        let k = self.w(cur).key_code;
        if ev.ev_type == vneNormal && is_alpha(k) {
            return 0;
        }
        self.mark_change(cur);
        1
    }

    /// Feed one key (ASCII for normal typing). See [`Output`].
    pub fn process(&mut self, key_code: u32) -> Output {
        self.prepare_buffer();
        self.backs = 0;
        self.change_pos = self.current + 1;
        self.out.clear();
        self.output_written = false;
        self.reverted = false;
        self.key_restored = false;
        self.key_restoring = false;
        self.out_type = OutputKind::Char;

        let mut ev = self.input.key_code_to_event(key_code);

        let mut ret;
        if !self.to_escape {
            ret = match ev.ev_type {
                vneRoofAll | vneRoof_a | vneRoof_e | vneRoof_o => self.process_roof(&mut ev),
                vneHookAll | vneHook_uo | vneHook_u | vneHook_o | vneBowl => self.process_hook(&mut ev),
                vneDd => self.process_dd(&mut ev),
                vneTone0..=vneTone5 => self.process_tone(&mut ev),
                vne_telex_w => self.process_telex_w(&mut ev),
                vneMapChar => self.process_map_char(&mut ev),
                vneEscChar => self.process_esc_char(&mut ev),
                _ => self.process_append(&mut ev),
            };
        } else {
            self.to_escape = false;
            if self.current < 0 || ev.ev_type == vneNormal || ev.ev_type == vneEscChar {
                ret = self.process_append(&mut ev);
            } else {
                self.current -= 1;
                self.process_append(&mut ev);
                self.mark_change(self.current); // this will assign backs to 1 and mark the character for output
                ret = 1;
            }
        }

        if self.viet_key
            && self.current >= 0
            && self.w(self.current).form == VNW_NON_VN
            && ev.ch_type == UKC_VN
            && (!self.options.spell_check_enabled || self.single_mode)
        {
            // The spell check has failed, but because we are in non-spellcheck
            // mode, the new character starts a new word.
            ret = self.process_no_spell_check(&ev);
        }

        // add the key to the key buffer only if it did not cause a reset
        if self.current >= 0 {
            ev.ch_type = self.input.char_type(ev.key_code);
            self.key_current += 1;
            self.key_strokes[self.key_current as usize] =
                KeyBufEntry { ev, converted: ret != 0 && !self.key_restored };
        }

        if ret == 0 {
            return Output { handled: false, backs: 0, text: String::new(), kind: self.out_type };
        }

        if !self.output_written {
            self.write_output();
        }
        Output {
            handled: true,
            backs: self.backs as usize,
            text: std::mem::take(&mut self.out),
            kind: self.out_type,
        }
    }

    /// The standard char of buffer entry `i`.
    fn std_char(&self, i: i32) -> u32 {
        let e = self.w(i);
        if e.vn_sym != vnl_nonVnChar {
            let mut c = (e.vn_sym as u32) + OFFSET;
            if e.caps != 0 {
                c -= 1;
            }
            if e.tone != 0 {
                c += (e.tone * 2) as u32;
            }
            c
        } else if (e.key_code as u32) < 256 {
            tables().iso_std[e.key_code as usize]
        } else {
            e.key_code as u32
        }
    }

    fn write_output(&mut self) {
        for i in self.change_pos..=self.current {
            if let Some(c) = std_to_char(self.std_char(i)) {
                self.out.push(c);
            }
        }
    }

    fn mark_change(&mut self, pos: i32) {
        if pos < self.change_pos {
            // Output is Unicode, so one buffer entry is one character to delete.
            self.backs += self.change_pos - pos;
            self.change_pos = pos;
        }
    }

    /// Keep the key stroke buffer in step with the character buffer.
    fn synch_key_stroke_buffer(&mut self) {
        if self.key_current >= 0 {
            self.key_current -= 1;
        }
        if self.current >= 0 && self.w(self.current).form == VNW_EMPTY {
            // we reached a word break: move the key pointer back to the matching word break
            while self.key_current >= 0
                && self.key_strokes[self.key_current as usize].ev.ch_type != UKC_WORD_BREAK
            {
                self.key_current -= 1;
            }
        }
    }

    /// Backspace pressed. If `handled`, replace the key by `backs` backspaces and
    /// `text`; otherwise let the backspace through (the buffer is updated either way).
    pub fn process_backspace(&mut self) -> Output {
        let none = |this: &Self| Output { handled: false, backs: 0, text: String::new(), kind: this.out_type };
        self.out_type = OutputKind::Char;
        if !self.viet_key || self.current < 0 {
            return none(self);
        }

        self.out.clear();
        self.backs = 0;
        self.change_pos = self.current + 1;
        self.mark_change(self.current);

        let cur = *self.w(self.current);
        let prev = *self.w(self.current - 1);
        if self.current == 0
            || cur.form == VNW_EMPTY
            || cur.form == VNW_NON_VN
            || cur.form == VNW_C
            || prev.form == VNW_C
            || prev.form == VNW_CVC
            || prev.form == VNW_VC
        {
            self.current -= 1;
            let backs = self.backs;
            self.synch_key_stroke_buffer();
            return Output { handled: backs > 1, backs: backs as usize, text: String::new(), kind: self.out_type };
        }

        let v_end = self.current - cur.v_offset;
        let vs = self.w(v_end).seq;
        let v_start = v_end - vseq(vs).len + 1;
        let new_vs = prev.seq;
        let cur_tone_pos = v_start + self.get_tone_position(vs, v_end == self.current);
        let new_tone_pos = v_start + self.get_tone_position(new_vs, true);
        let tone = self.w(cur_tone_pos).tone;

        if tone == 0
            || cur_tone_pos == new_tone_pos
            || (cur_tone_pos == self.current && cur.tone != 0)
        {
            self.current -= 1;
            let backs = self.backs;
            self.synch_key_stroke_buffer();
            return Output { handled: backs > 1, backs: backs as usize, text: String::new(), kind: self.out_type };
        }

        self.mark_change(new_tone_pos);
        self.b(new_tone_pos).tone = tone;
        self.mark_change(cur_tone_pos);
        self.b(cur_tone_pos).tone = 0;
        self.current -= 1;
        self.synch_key_stroke_buffer();
        self.write_output();
        Output { handled: true, backs: self.backs as usize, text: std::mem::take(&mut self.out), kind: self.out_type }
    }

    /// Make room in the buffers (at least 10 free entries).
    fn prepare_buffer(&mut self) {
        let size = MAX_UK_ENGINE as i32;
        if self.current >= 0 && self.current + 10 >= size {
            // Get rid of at least half of the entries, but not from the middle of a word.
            let mut rid = self.current / 2;
            while self.w(rid).form != VNW_EMPTY && rid < self.current {
                rid += 1;
            }
            if rid == self.current {
                self.current = -1;
            } else {
                rid += 1;
                let n = (self.current - rid + 1) as usize;
                self.buffer.copy_within(rid as usize..rid as usize + n, 0);
                self.current -= rid;
            }
        }

        if self.key_current > 0 && self.key_current + 1 >= size {
            let rid = self.key_current / 2;
            let n = (self.key_current - rid + 1) as usize;
            self.key_strokes.copy_within(rid as usize..rid as usize + n, 0);
            self.key_current -= rid;
        }
    }

    // ---- macros ---------------------------------------------------------

    fn macro_match(&mut self, ev: &KeyEvent) -> i32 {
        if self.shift_pressed && (ev.key_code == b' ' as u32 || ev.key_code == ENTER_CHAR) {
            return 0;
        }

        let mut i = self.current;
        let mut key: Vec<u32> = Vec::new();
        let mut found: Option<Vec<u32>> = None;
        let mut start = 0usize; // where the matched key starts within `key`

        while i >= 0 && (self.current - i + 1) < 16 {
            while i >= 0 && self.w(i).form != VNW_EMPTY && (self.current - i + 1) < 16 {
                i -= 1;
            }
            if i >= 0 && self.w(i).form != VNW_EMPTY {
                return 0;
            }

            key.clear();
            key.push(if i >= 0 { self.std_char(i) } else { 0 });
            for j in (i + 1)..=self.current {
                key.push(self.std_char(j));
            }

            // search the macro table: without the leading word break, then with it
            if let Some(t) = self.macros.lookup(&key[1..]) {
                found = Some(t.to_vec());
                i += 1; // mark the position where change is needed
                start = 1;
                break;
            }
            if i >= 0 {
                if let Some(t) = self.macros.lookup(&key) {
                    found = Some(t.to_vec());
                    start = 0;
                    break;
                }
            }
            i -= 1;
        }

        let Some(mac_text) = found else { return 0 };
        let key = &key[start..];

        self.mark_change(i);

        // case of the replacement follows the typed key: ALL CAPS, or unchanged
        #[derive(PartialEq)]
        enum Case {
            NoChange,
            AllCapital,
            AllSmall,
        }
        let is_lower = |c: u32| crate::charset::is_std_vn_alpha(c) && c & 1 == 1;
        let is_upper = |c: u32| crate::charset::is_std_vn_alpha(c) && c & 1 == 0;
        let case = if !self.options.macro_auto_case {
            Case::NoChange
        } else if is_lower(key[0]) {
            Case::AllSmall
        } else if is_upper(key[0]) {
            if key[1..].iter().any(|&c| is_lower(c)) { Case::NoChange } else { Case::AllCapital }
        } else {
            Case::NoChange
        };

        for c in &mac_text {
            let c = match case {
                Case::AllCapital => std_to_upper(*c),
                Case::AllSmall => std_to_lower(*c),
                Case::NoChange => *c,
            };
            if let Some(ch) = std_to_char(c) {
                self.out.push(ch);
            }
        }

        // write the last input character
        let last = if ev.vn_sym != vnl_nonVnChar { ev.vn_sym as u32 + OFFSET } else { ev.key_code };
        if let Some(ch) = std_to_char(last) {
            self.out.push(ch);
        }

        let backs = self.backs;
        self.reset();
        self.output_written = true;
        self.backs = backs;
        1
    }

    // ---- restoring key strokes -----------------------------------------------

    /// Undo the conversions of the last word and give back the keys as typed.
    pub fn restore_key_strokes(&mut self) -> Output {
        self.out.clear();
        self.out_type = OutputKind::Key;
        if self.restore_key_strokes_inner() {
            Output { handled: true, backs: self.backs as usize, text: std::mem::take(&mut self.out), kind: OutputKind::Key }
        } else {
            Output { handled: false, backs: 0, text: String::new(), kind: OutputKind::Key }
        }
    }

    fn restore_key_strokes_inner(&mut self) -> bool {
        if !self.last_word_has_vn_mark() {
            return false;
        }

        self.backs = 0;
        self.change_pos = self.current + 1;

        let mut key_start = self.key_current;
        let mut converted = false;
        while key_start >= 0 && self.key_strokes[key_start as usize].ev.ch_type != UKC_WORD_BREAK {
            if self.key_strokes[key_start as usize].converted {
                converted = true;
            }
            key_start -= 1;
        }
        key_start += 1;

        if !converted {
            // no key stroke has been converted, so restoring makes no sense
            return false;
        }

        while self.current >= 0 && self.w(self.current).form != VNW_EMPTY {
            self.current -= 1;
        }
        self.mark_change(self.current + 1);

        self.key_restoring = true;
        for i in key_start..=self.key_current {
            let code = self.key_strokes[i as usize].ev.key_code;
            if let Some(c) = char::from_u32(code) {
                self.out.push(c);
            }
            let mut ev = self.input.key_code_to_symbol(code);
            self.key_strokes[i as usize].converted = false;
            self.process_append(&mut ev);
        }
        self.key_restoring = false;
        true
    }

    fn process_word_end(&mut self, ev: &KeyEvent) -> i32 {
        if self.options.macro_enabled && self.macro_match(ev) != 0 {
            return 1;
        }

        let push_empty = |this: &mut Self| {
            this.current += 1;
            let sym = vn_to_lower(ev.vn_sym);
            let cur = this.current;
            let e = this.b(cur);
            e.form = VNW_EMPTY;
            e.c1_offset = -1;
            e.c2_offset = -1;
            e.v_offset = -1;
            e.key_code = ev.key_code as i32;
            e.vn_sym = sym;
            e.caps = (sym != ev.vn_sym) as i32;
        };

        if !self.options.spell_check_enabled || self.single_mode || self.current < 0 || self.key_restoring {
            push_empty(self);
            return 0;
        }

        if self.options.auto_non_vn_restore && self.last_word_is_non_vn() {
            self.out.clear();
            // A failed restore must not lose the pending backspaces (the C++
            // original zeroes them here and corrupts the text).
            let backs = self.backs;
            if self.restore_key_strokes_inner() {
                self.key_restored = true;
                self.output_written = true;
                self.out_type = OutputKind::Key;
            } else {
                self.backs = backs;
            }
        }

        push_empty(self);

        if self.key_restored {
            if let Some(c) = char::from_u32(ev.key_code) {
                self.out.push(c);
            }
            return 1;
        }
        0
    }

    /// Is the last word not Vietnamese, so key strokes may be restored?
    fn last_word_is_non_vn(&self) -> bool {
        if self.current < 0 {
            return false;
        }
        let cur = *self.w(self.current);
        match cur.form {
            VNW_NON_VN => true,
            VNW_EMPTY | VNW_C => false,
            VNW_V | VNW_CV => !vseq(cur.seq).complete,
            VNW_VC | VNW_CVC => {
                let v_index = self.current - cur.v_offset;
                let vs = self.w(v_index).seq;
                if !vseq(vs).complete {
                    return true;
                }
                let cs = cur.seq;
                let c1 = if cur.c1_offset != -1 { self.w(self.current - cur.c1_offset).seq } else { cs_nil };
                if !is_valid_cvc(c1, vs, cs) {
                    return true;
                }
                let tone_pos = (v_index - vseq(vs).len + 1) + self.get_tone_position(vs, false);
                let tone = self.w(tone_pos).tone;
                (cs == cs_c || cs == cs_ch || cs == cs_p || cs == cs_t) && (tone == 2 || tone == 3 || tone == 4)
            }
            _ => false,
        }
    }

    /// Does the last word carry a Vietnamese mark (tone or decoration)?
    fn last_word_has_vn_mark(&self) -> bool {
        let mut i = self.current;
        while i >= 0 && self.w(i).form != VNW_EMPTY {
            let sym = self.w(i).vn_sym;
            if sym != vnl_nonVnChar {
                if is_vn_vowel(sym) && self.w(i).tone != 0 {
                    return true;
                }
                if sym != STD_VN_ROOT_CHAR[sym as usize] {
                    return true;
                }
            }
            i -= 1;
        }
        false
    }
}
