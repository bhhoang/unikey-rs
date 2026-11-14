//! X11 support: XWayland windows under a Wayland compositor, and whole X11
//! sessions. X11 applications cannot talk to a Wayland input method, so for
//! them we act as a plain X11 client: watch their keystrokes (XInput2 raw
//! events; XWayland only sends them while an X11 window has focus) and type
//! corrections back with XTest. No special privileges are needed.
//!
//! X11 clients keep their own copy of the keymap, so the injection keyboard's
//! keymap trick (see `inject`) does not reach them. Instead Vietnamese letters
//! are put on spare keycodes and left there: a small cache, so the letters typed
//! most (ê, ơ, ư ...) need no remapping, and a correction is one burst of keys
//! with no waiting.
//!
//! This is an observer: the application has already received the key as typed
//! and the correction follows right after.

use std::{
    os::fd::{AsRawFd, RawFd},
    thread::sleep,
    time::{Duration, Instant},
};

use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xinput::{self, ConnectionExt as _, XIEventMask},
        xkb::{self, ConnectionExt as _},
        xproto::{AtomEnum, ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT},
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

/// X keycode of Backspace if the layout has none (keysym 0xff08 is looked up first).
const FALLBACK_BACKSPACE: u8 = 22;
const KS_BACKSPACE: u32 = 0xff08;
/// Spare keycodes we borrow for letters.
const MAX_SPARES: usize = 16;
/// Toolkits need a moment to notice a remapped keycode; a key typed sooner is lost.
const REMAP_SETTLE: Duration = Duration::from_millis(10);
/// `XIAllMasterDevices`.
const ALL_MASTER_DEVICES: u16 = 1;

fn err<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> String {
    move |e| format!("{what}: {e}")
}

/// A keysym we put on a borrowed keycode: a Latin-1 letter or a Unicode keysym
/// (not the XF86 function-key range).
fn typed_keysym(k: u32) -> bool {
    matches!(k, 0x41..=0x5a | 0x61..=0x7a | 0xc0..=0xff)
        || ((0x0100_0100..0x0110_0000).contains(&k) && !(0x1008_0000..0x1009_0000).contains(&k))
}

fn keysym(ch: char) -> u32 {
    if (ch as u32) < 0x100 { ch as u32 } else { 0x0100_0000 + ch as u32 }
}

pub enum Input {
    /// A key press/release; X keycode (evdev code + 8).
    Key { keycode: u32, pressed: bool },
    /// A mouse button press: the cursor may have moved, so a new word starts.
    Click,
    /// The server's modifier and layout state changed.
    Mods(Mods),
}

/// XKB state as xkbcommon's `update_mask` takes it: depressed, latched and
/// locked modifiers, then depressed, latched and locked layout.
#[derive(Clone, Copy)]
pub struct Mods(pub [u32; 6]);

fn mods(base: u16, latched: u16, locked: u16, groups: (i16, i16, u8)) -> Mods {
    Mods([base as u32, latched as u32, locked as u32, groups.0 as u32, groups.1 as u32, groups.2 as u32])
}

pub struct X11 {
    conn: RustConnection,
    root: u32,
    /// Device id of the XTest keyboard: our own keys, to be ignored.
    xtest_device: Option<u16>,
    backspace: u8,
    per_keycode: u8,
    /// Borrowed keycodes and the letter each carries, least recently used first.
    spares: Vec<(u8, Option<char>)>,
}

impl X11 {
    /// Connect to the X server (XWayland) named by `DISPLAY`.
    pub fn connect() -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(err("connect"))?;
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let root = setup.roots[screen].root;

        conn.xinput_xi_query_version(2, 2).map_err(err("XInput2"))?.reply().map_err(err("XInput2"))?;
        let mask = xinput::EventMask {
            deviceid: ALL_MASTER_DEVICES,
            mask: vec![(XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_KEY_RELEASE | XIEventMask::RAW_BUTTON_PRESS).into()],
        };
        conn.xinput_xi_select_events(root, &[mask])
            .map_err(err("select events"))?
            .check()
            .map_err(err("select events"))?;
        // The modifier state comes from the server, not from the raw keys: those
        // only arrive while an X11 window has focus, so a Shift released or a Caps
        // Lock toggled elsewhere would go unseen. State events come in order with
        // the raw keys.
        conn.xkb_use_extension(1, 0).map_err(err("XKB"))?.reply().map_err(err("XKB"))?;
        let state = xkb::EventType::STATE_NOTIFY;
        conn.xkb_select_events(xkb::ID::USE_CORE_KBD.into(), 0u16.into(), state, 0u16.into(), 0u16.into(), &xkb::SelectEventsAux::new())
            .map_err(err("XKB events"))?
            .check()
            .map_err(err("XKB events"))?;

        let map = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(err("keyboard mapping"))?
            .reply()
            .map_err(err("keyboard mapping"))?;
        let per = map.keysyms_per_keycode as usize;
        let row = |k: u8| &map.keysyms[(k - min) as usize * per..][..per];
        // Free keycodes, plus ones we borrowed before (the same letter on the
        // first four columns, the rest 0: no real key looks like that).
        let ours = |k: u8| {
            let r = row(k);
            r.len() >= 4 && r[..4].iter().all(|&s| s == r[0]) && r[4..].iter().all(|&s| s == 0) && typed_keysym(r[0])
        };
        let spares: Vec<(u8, Option<char>)> = (min..=max)
            .rev()
            .filter(|&k| row(k).iter().all(|&s| s == 0) || ours(k))
            .take(MAX_SPARES)
            .map(|k| (k, None))
            .collect();
        if spares.is_empty() {
            return Err("no unused keycode to type with".into());
        }
        let backspace = (min..=max).find(|&k| row(k)[0] == KS_BACKSPACE).unwrap_or(FALLBACK_BACKSPACE);

        let xtest_device = conn
            .xinput_xi_query_device(0u16)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| {
                r.infos
                    .iter()
                    .find(|i| {
                        // "Virtual core XTEST keyboard" (there is an XTEST pointer too)
                        let name = String::from_utf8_lossy(&i.name);
                        name.contains("XTEST") && name.contains("keyboard")
                    })
                    .map(|i| i.deviceid)
            });

        let x = X11 { conn, root, xtest_device, backspace, per_keycode: per as u8, spares };
        // XTest events sent right after connecting can be lost; let it settle.
        sleep(Duration::from_millis(400));
        Ok(x)
    }

    pub fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    /// Next key or click made by the user, if one is waiting. `Err` means the
    /// connection is gone.
    pub fn next_input(&self) -> Result<Option<Input>, ()> {
        loop {
            match self.conn.poll_for_event().map_err(|_| ())? {
                None => return Ok(None),
                Some(Event::XinputRawKeyPress(e)) if Some(e.sourceid) != self.xtest_device => {
                    return Ok(Some(Input::Key { keycode: e.detail, pressed: true }));
                }
                Some(Event::XinputRawKeyRelease(e)) if Some(e.sourceid) != self.xtest_device => {
                    return Ok(Some(Input::Key { keycode: e.detail, pressed: false }));
                }
                // 4-7 are scroll wheel steps, which move no cursor.
                Some(Event::XinputRawButtonPress(e)) if !(4..=7).contains(&e.detail) => return Ok(Some(Input::Click)),
                Some(Event::XkbStateNotify(e)) => {
                    let groups = (e.base_group, e.latched_group, u8::from(e.locked_group));
                    return Ok(Some(Input::Mods(mods(e.base_mods.into(), e.latched_mods.into(), e.locked_mods.into(), groups))));
                }
                Some(_) => {}
            }
        }
    }

    /// The server's current modifier and layout state.
    pub fn mods(&self) -> Option<Mods> {
        let s = self.conn.xkb_get_state(xkb::ID::USE_CORE_KBD.into()).ok()?.reply().ok()?;
        Some(mods(s.base_mods.into(), s.latched_mods.into(), s.locked_mods.into(), (s.base_group, s.latched_group, u8::from(s.locked_group))))
    }

    /// Whether an X11 window has the keyboard focus (under Wayland: the focused
    /// window is an XWayland one). 0 is None and 1 PointerRoot.
    pub fn has_focus(&self) -> bool {
        let focus = self.conn.get_input_focus().ok().and_then(|c| c.reply().ok());
        focus.is_some_and(|f| f.focus > 1 && f.focus != self.root)
    }

    /// The keyboard layout the X server was set up with (`setxkbmap` writes it
    /// to the root window): rules, model, layout, variant, options.
    pub fn layout_names(&self) -> Option<[String; 5]> {
        let atom = self.conn.intern_atom(true, b"_XKB_RULES_NAMES").ok()?.reply().ok()?.atom;
        let prop = self.conn.get_property(false, self.root, atom, AtomEnum::STRING, 0, 1024).ok()?.reply().ok()?;
        let mut parts = prop.value.split(|&b| b == 0).map(|p| String::from_utf8_lossy(p).into_owned());
        Some(std::array::from_fn(|_| parts.next().unwrap_or_default()))
    }

    fn tap(&self, keycode: u8) {
        for ev in [KEY_PRESS_EVENT, KEY_RELEASE_EVENT] {
            let _ = self.conn.xtest_fake_input(ev, keycode, 0, self.root, 0, 0, 0);
        }
    }

    fn sync(&self) {
        let _ = self.conn.flush();
        let _ = self.conn.get_input_focus().map(|c| c.reply());
    }

    /// Put `chars` (at most `spares.len()` distinct ones) on spare keycodes.
    /// Returns whether any keycode had to be remapped.
    fn map_chars(&mut self, chars: &[char]) -> bool {
        let mut changed = false;
        for &ch in chars {
            if let Some(i) = self.spares.iter().position(|s| s.1 == Some(ch)) {
                let s = self.spares.remove(i);
                self.spares.push(s); // most recently used last
                continue;
            }
            // Reuse the least recently used keycode not needed by this text.
            let Some(i) = self.spares.iter().position(|s| s.1.map_or(true, |c| !chars.contains(&c))) else {
                continue;
            };
            let (code, _) = self.spares.remove(i);
            let syms = vec![keysym(ch); self.per_keycode as usize];
            if self.conn.change_keyboard_mapping(1, code, self.per_keycode, &syms).is_ok() {
                self.spares.push((code, Some(ch)));
                changed = true;
            } else {
                self.spares.insert(0, (code, None));
            }
        }
        changed
    }

    /// Delete `backs` characters before the cursor, then type `text`, as one
    /// burst. Letters are typed through keycodes that carry the same keysym on
    /// every level, so Shift and Caps Lock held by the user cannot change them.
    pub fn edit(&mut self, backs: usize, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        let mut distinct: Vec<char> = Vec::new();
        for &c in &chars {
            if !distinct.contains(&c) {
                distinct.push(c);
            }
        }
        let mut backs = backs;
        // shortcut: text with more distinct letters than spares (long macros) is
        // typed in pieces, each waiting for its remap; fine for that rare case.
        for piece in chars.chunks(if distinct.len() <= self.spares.len() { chars.len().max(1) } else { self.spares.len() }) {
            let mut needed: Vec<char> = piece.to_vec();
            needed.dedup();
            let remapped = self.map_chars(&needed);
            let start = Instant::now();
            if remapped {
                self.sync(); // the mapping is on its way while the Backspaces go out
            }
            for _ in 0..backs {
                self.tap(self.backspace);
            }
            backs = 0;
            if remapped {
                let _ = self.conn.flush();
                sleep(REMAP_SETTLE.saturating_sub(start.elapsed()));
            }
            for &c in piece {
                if let Some(&(code, _)) = self.spares.iter().find(|s| s.1 == Some(c)) {
                    self.tap(code);
                }
            }
        }
        self.sync();
    }
}
