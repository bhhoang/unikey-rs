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

pub struct Injector {
    vk: ZwpVirtualKeyboardV1,
    time: u32,
}

/// XKB keymap with BackSpace and `chars` on `TEXT_KEYS`, one level each (so a
/// held Shift or Caps Lock cannot change them).
///
/// It also maps a Shift key (never pressed): GTK3 ignores BackSpace from a
/// keymap that has no modifier keys at all.
pub fn keymap(chars: &[char]) -> String {
    let mut codes = format!("    <BKSP> = {};\n    <LFSH> = {};\n", KEY_BACKSPACE + 8, KEY_LEFTSHIFT + 8);
    let mut syms =
        String::from("    key <BKSP> { [ BackSpace ] };\n    key <LFSH> { [ Shift_L ] };\n    modifier_map Shift { <LFSH> };\n");
    for (i, c) in chars.iter().enumerate() {
        codes += &format!("    <K{i:02}> = {};\n", TEXT_KEYS[i] + 8);
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

    fn upload(&self, chars: &[char]) -> bool {
        let text = keymap(chars);
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
            if !self.upload(chunk) {
                eprintln!("unikey: cannot create a keymap for the correction");
                return sent;
            }
            if n == 0 {
                for _ in 0..backs {
                    self.tap(KEY_BACKSPACE);
                    sent.push(KEY_BACKSPACE);
                }
            }
            for i in 0..chunk.len() {
                self.tap(TEXT_KEYS[i]);
                sent.push(TEXT_KEYS[i]);
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
        let chars = ['ê', 'ơ', 'Đ', 'a'];
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let km = xkb::Keymap::new_from_string(&ctx, keymap(&chars), xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .expect("keymap compiles");
        let state = xkb::State::new(&km);
        for (i, c) in chars.iter().enumerate() {
            assert_eq!(state.key_get_utf8(xkb::Keycode::new(TEXT_KEYS[i] + 8)), c.to_string());
        }
        assert_eq!(state.key_get_one_sym(xkb::Keycode::new(KEY_BACKSPACE + 8)).raw(), 0xff08);
        // GTK3 needs a real modifier in the keymap to accept BackSpace.
        let mut state = xkb::State::new(&km);
        state.update_key(xkb::Keycode::new(KEY_LEFTSHIFT + 8), xkb::KeyDirection::Down);
        assert!(state.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE));
        assert!(!TEXT_KEYS.contains(&KEY_LEFTSHIFT));
    }
}
