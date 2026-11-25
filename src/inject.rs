//! Typing corrections as key presses through a dedicated virtual keyboard: the
//! method used by fcitx5-lotus (https://github.com/LotusInputMethod/fcitx5-lotus).
//!
//! For each correction we upload a tiny keymap that puts the replacement
//! characters (ê, ơ, ...) on ordinary letter keys, then tap the Backspaces and
//! those keys back to back. Everything the application receives is a key event,
//! in order, in one burst: no commit/key reordering to wait out, so it can
//! process the whole correction before drawing. It works the same for Wayland
//! clients, terminals and XWayland windows.
//!
//! The user's own keys keep going through the other virtual keyboard, which has
//! their real layout.

use std::{io::Write, os::fd::AsFd};

use wayland_client::protocol::wl_keyboard::KeymapFormat;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

const KEY_BACKSPACE: u32 = 14;
const KEY_LEFTSHIFT: u32 = 42;

/// evdev codes of printable keys, used to carry the replacement characters.
/// Printable on purpose: Chromium may fall back to a key's physical meaning.
/// Never Escape, Backspace, Tab, Enter or modifiers.
const TEXT_KEYS: [u32; 48] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 30, 31,
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 57,
];

/// The evdev key that types `c` without Shift on a US layout. Chromium tells
/// web pages which physical key was pressed (`code`), and pages act on it: a
/// space sent on the "1" key makes YouTube seek to 10% instead of pausing.
fn us_key(c: char) -> Option<u32> {
    const ROWS: [(&str, u32); 4] = [("1234567890-=", 2), ("qwertyuiop[]", 16), ("asdfghjkl;'`", 30), ("zxcvbnm,./", 44)];
    match c {
        ' ' => Some(57),
        '\\' => Some(43),
        _ => ROWS.iter().find_map(|(row, first)| row.find(c).map(|i| first + i as u32)),
    }
}

/// Assign a key to each distinct character of `chars` (at most
/// `TEXT_KEYS.len()`): its own key when it has one, else a spare one. Returns
/// the keymap entries and the keys to tap, in order.
fn layout(chars: &[char]) -> (Vec<(u32, char)>, Vec<u32>) {
    let mut keys: Vec<(u32, char)> = Vec::new();
    // Own keys first, so a spare never takes a key another character needs.
    for &c in chars {
        if let Some(key) = us_key(c).filter(|_| !keys.iter().any(|k| k.1 == c)) {
            keys.push((key, c));
        }
    }
    for &c in chars {
        if !keys.iter().any(|k| k.1 == c) {
            let key = TEXT_KEYS.into_iter().find(|t| !keys.iter().any(|k| k.0 == *t)).expect("a free key");
            keys.push((key, c));
        }
    }
    let taps = chars.iter().map(|&c| keys.iter().find(|k| k.1 == c).unwrap().0).collect();
    (keys, taps)
}

pub struct Injector {
    vk: ZwpVirtualKeyboardV1,
    time: u32,
}

/// XKB keymap with BackSpace and `chars` on `TEXT_KEYS`, one level each (so a
/// held Shift or Caps Lock cannot change them).
///
/// It also maps a Shift key (never pressed): GTK3 ignores BackSpace from a
/// keymap that has no modifier keys at all.
pub fn keymap(keys: &[(u32, char)]) -> String {
    let mut codes = format!("    <BKSP> = {};\n    <LFSH> = {};\n", KEY_BACKSPACE + 8, KEY_LEFTSHIFT + 8);
    let mut syms =
        String::from("    key <BKSP> { [ BackSpace ] };\n    key <LFSH> { [ Shift_L ] };\n    modifier_map Shift { <LFSH> };\n");
    for (i, (key, c)) in keys.iter().enumerate() {
        codes += &format!("    <K{i:02}> = {};\n", key + 8);
        syms += &format!("    key <K{i:02}> {{ [ U{:04X} ] }};\n", *c as u32);
    }
    format!(
        "xkb_keymap {{\n\
         xkb_keycodes \"unikey\" {{\n    minimum = 8;\n    maximum = 255;\n{codes}}};\n\
         xkb_types \"complete\" {{ include \"complete\" }};\n\
         xkb_compatibility \"complete\" {{ include \"complete\" }};\n\
         xkb_symbols \"unikey\" {{\n{syms}}};\n\
         }};\n"
    )
}

impl Injector {
    pub fn new(vk: ZwpVirtualKeyboardV1) -> Self {
        Injector { vk, time: 0 }
    }

    fn tap(&mut self, key: u32) {
        self.vk.key(self.time, key, 1);
        self.vk.key(self.time + 1, key, 0);
        self.time = self.time.wrapping_add(2);
    }

    fn upload(&self, keys: &[(u32, char)]) -> bool {
        let text = keymap(keys);
        let fd = unsafe { libc::memfd_create(c"unikey-keymap".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return false;
        }
        let mut file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        if file.write_all(text.as_bytes()).and_then(|_| file.write_all(&[0])).is_err() {
            return false;
        }
        self.vk.keymap(KeymapFormat::XkbV1.into(), file.as_fd(), text.len() as u32 + 1);
        self.vk.modifiers(0, 0, 0, 0); // a new keymap resets the client's modifier state
        true
    }

    /// Delete `backs` characters before the cursor and type `text`.
    /// Returns the evdev keys sent, in order (for telling our own keys apart).
    pub fn emit(&mut self, backs: usize, text: &str) -> Vec<u32> {
        let chars: Vec<char> = text.chars().collect();
        let mut sent = Vec::new();
        let chunks: Vec<&[char]> = if chars.is_empty() { vec![&[]] } else { chars.chunks(TEXT_KEYS.len()).collect() };
        for (n, chunk) in chunks.into_iter().enumerate() {
            let (keys, taps) = layout(chunk);
            if !self.upload(&keys) {
                eprintln!("unikey: cannot create a keymap for the correction");
                return sent;
            }
            if n == 0 {
                for _ in 0..backs {
                    self.tap(KEY_BACKSPACE);
                    sent.push(KEY_BACKSPACE);
                }
            }
            for key in taps {
                self.tap(key);
                sent.push(key);
            }
        }
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xkbcommon::xkb;

    #[test]
    fn keymap_compiles_and_maps_the_characters() {
        let chars = ['ê', 'ơ', 'Đ', 'a', ' ', 'ê', 'A', '1'];
        let (keys, taps) = layout(&chars);
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let km = xkb::Keymap::new_from_string(&ctx, keymap(&keys), xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .expect("keymap compiles");
        let state = xkb::State::new(&km);
        for (key, c) in taps.iter().zip(chars) {
            assert!(TEXT_KEYS.contains(key));
            assert_eq!(state.key_get_utf8(xkb::Keycode::new(key + 8)), c.to_string());
        }
        // Characters with a key of their own go out on it (Space on the space
        // bar, not on "1"); the "1" key is then left for '1' itself.
        assert_eq!(&taps[3..], &[30, 57, taps[0], taps[6], 2]);
        assert_ne!(taps[6], 30, "'A' cannot share the A key with 'a'");
        assert_eq!(state.key_get_one_sym(xkb::Keycode::new(KEY_BACKSPACE + 8)).raw(), 0xff08);
        // GTK3 needs a real modifier in the keymap to accept BackSpace.
        let mut state = xkb::State::new(&km);
        state.update_key(xkb::Keycode::new(KEY_LEFTSHIFT + 8), xkb::KeyDirection::Down);
        assert!(state.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE));
        assert!(!TEXT_KEYS.contains(&KEY_LEFTSHIFT));
    }
}
