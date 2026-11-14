//! Wayland frontend: UniKey as a native input method, behaving like UniKey on
//! Windows. There is no preedit (no underline): when the engine converts a key,
//! the app sends real Backspace key events and then commits the new text.
//!
//! While a text field is focused the compositor hands us the keyboard (input
//! method v2 grab). Keys we do not convert are replayed through a virtual
//! keyboard, so applications see them as normal key presses.

mod apps;
mod config;
mod hypr;
mod inject;
mod tray;
mod x11;

use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    time::{Duration, Instant},
};

use unikey::{Engine, settings::ToggleKey};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols_misc::{
    zwp_input_method_v2::client::{
        zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
        zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        zwp_input_method_v2::{self, ZwpInputMethodV2},
    },
    zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    },
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use xkbcommon::xkb;

/// How long a new text field may wait for its window to report focus.
const FIELD_CLAIM: Duration = Duration::from_millis(150);

/// Debug trace on stderr, enabled with `UNIKEY_LOG=1`.
macro_rules! log {
    ($($arg:tt)*) => {
        if std::env::var_os("UNIKEY_LOG").is_some() {
            eprintln!("unikey: {}", format_args!($($arg)*));
        }
    };
}

/// xkb keycodes are evdev codes + 8.
const XKB_OFFSET: u32 = 8;

// Keysyms (X11), only the ones we branch on.
const KS_BACKSPACE: u32 = 0xff08;
const KS_ESCAPE: u32 = 0xff1b;
const KS_SHIFT_L: u32 = 0xffe1;
const KS_SHIFT_R: u32 = 0xffe2;
const KS_CONTROL_L: u32 = 0xffe3;
const KS_CONTROL_R: u32 = 0xffe4;
const KS_MODIFIER_LAST: u32 = 0xffee; // Hyper_R, end of the modifier block
const KS_ISO_LEVEL3_SHIFT: u32 = 0xfe03;
const KS_NUM_LOCK: u32 = 0xff7f;
const KS_Z_LOWER: u32 = 0x7a;
const KS_Z_UPPER: u32 = 0x5a;

/// What happened to a physical key that is currently down.
enum Down {
    /// Replayed as a real press; release it when the user does.
    Verbatim,
    /// Handled as a tap (press+release sent already); the release is swallowed.
    Tap,
}

/// One thing to send to the application.
enum Act {
    Key(u32, bool),
    Mods(u32, u32, u32, u32),
}

/// How long to wait for an application to show the effect of a probe edit.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
/// An unexplained text change this soon after our own output is a reaction to it.
const OWN_CHANGE_WINDOW: Duration = Duration::from_millis(250);

struct Repeat {
    key: u32,
    due: Instant,
}

struct App {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwpInputMethodManagerV2>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    im: Option<ZwpInputMethodV2>,
    vk: Option<ZwpVirtualKeyboardV1>,
    /// Second virtual keyboard, for corrections (see `inject`).
    injector: Option<inject::Injector>,

    grab: Option<ZwpInputMethodKeyboardGrabV2>,

    // input-method state
    serial: u32,
    pending_active: bool,
    pending_purpose: u32,
    reactivated: bool,
    active: bool,
    /// The last text change reported for the field was not ours (a mouse click,
    /// a paste ...): judged at `done`.
    cycle_other: bool,
    /// Characters we put before the cursor recently (for byte counts).
    shown: Vec<char>,
    /// The focused field reported the text around its cursor (terminals never do).
    field_surr: bool,
    learned: apps::Learned,
    /// An atomic edit whose effect we wait to see in the next surrounding-text
    /// report: (text before it, text after it, inserted text, backspaces), and
    /// edits held back until then.
    probe: Option<(String, String, String, usize, Instant)>,
    held: VecDeque<(usize, String)>,
    /// Everything we send, in order, with the earliest time each item may go out.
    queue: VecDeque<(Instant, Act)>,
    /// When the next item may be sent: later than every queued item.
    tail: Instant,
    /// When we last sent something (to tell our own edits from the user's).
    last_emit: Instant,

    // keyboard state
    xkb_ctx: xkb::Context,
    xkb_state: Option<xkb::State>,
    keymap_sent: bool,
    down: HashMap<u32, (Down, u32)>, // evdev key -> (how, keysym)
    toggle_held: Vec<u32>, // Ctrl/Shift keysyms currently down
    toggle_armed: bool,
    repeat: Option<Repeat>,
    repeat_rate: u32,  // keys per second, 0 = off
    repeat_delay: u32, // ms

    /// The window whose text field activated us, and focus notifications.
    im_window: Option<String>,
    /// Per-app mode: VI/EN chosen for each application class, and the class in focus.
    // shortcut: kept in memory only, so it resets when unikey restarts; persist if asked.
    app_modes: HashMap<String, bool>,
    focused_class: String,
    focused_addr: String,
    /// For `prepare_char`.
    /// Escape was just pressed in a terminal-like window (see `prepare_char`).
    after_escape: bool,
    last_char: Option<char>,
    hypr_events: Option<hypr::Events>,
    /// Focus on other compositors (wlr-foreign-toplevel): each window's app id
    /// and whether it is activated, by protocol id, as of its last `done`.
    toplevel_mgr: Option<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<u32, (String, bool)>,
    /// When a new text field last set `im_window` (see `on_focus`).
    im_claimed_at: Option<Instant>,
    qh: Option<QueueHandle<App>>,

    // XWayland (see x11.rs)
    x: Option<x11::X11>,
    xkb_x: Option<xkb::State>,
    x_retry: Instant,

    engine: Engine,
    cfg: config::Config,
    tray: Option<ksni::Handle<tray::UnikeyTray>>,
    quit: bool,
}

impl App {
    fn new(cfg: config::Config) -> Self {
        let mut engine = Engine::new();
        cfg.apply(&mut engine);
        App {
            seat: None,
            manager: None,
            vk_manager: None,
            im: None,
            vk: None,
            injector: None,

            grab: None,
            serial: 0,
            pending_active: false,
            pending_purpose: 0,
            reactivated: false,
            active: false,
            cycle_other: false,
            shown: Vec::new(),
            field_surr: false,
            learned: apps::Learned::load(),
            probe: None,
            held: VecDeque::new(),
            queue: VecDeque::new(),
            tail: Instant::now(),
            last_emit: Instant::now(),
            xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            keymap_sent: false,
            down: HashMap::new(),
            toggle_held: Vec::new(),
            toggle_armed: false,
            repeat: None,
            repeat_rate: 25,
            repeat_delay: 600,
            im_window: None,
            app_modes: HashMap::new(),
            focused_class: String::new(),
            focused_addr: String::new(),
            after_escape: false,
            last_char: None,
            hypr_events: hypr::Events::connect(),
            toplevel_mgr: None,
            toplevels: HashMap::new(),
            im_claimed_at: None,
            qh: None,
            x: None,
            xkb_x: None,
            x_retry: Instant::now(),
            engine,
            cfg,
            tray: None,
            quit: false,
        }
    }

    // ---- output ---------------------------------------------------------------

    /// Queue `act`, to go out after everything queued before it.
    fn push(&mut self, act: Act) {
        let due = self.tail.max(Instant::now());
        self.tail = due;
        self.queue.push_back((due, act));
    }

    fn send_key(&mut self, key: u32, pressed: bool) {
        self.push(Act::Key(key, pressed));
    }

    /// Send what is due.
    fn flush(&mut self) {
        while let Some((due, _)) = self.queue.front() {
            if *due > Instant::now() {
                break;
            }
            let (_, act) = self.queue.pop_front().unwrap();
            self.last_emit = Instant::now();
            match act {
                Act::Key(key, pressed) => {
                    if let Some(vk) = &self.vk {
                        vk.key(boot_ms(), key, pressed as u32);
                    }
                }
                Act::Mods(d, l, k, g) => {
                    if let Some(vk) = &self.vk {
                        vk.modifiers(d, l, k, g);
                    }
                }
            }
        }
    }

    /// Delete `backs` characters before the cursor and insert `text`, with no
    /// preedit (so no underline).
    ///
    /// Applications that support it get both in one atomic step: no flicker.
    /// Others (terminals, Chromium/Electron) get Backspace key presses and then
    /// the text, like UniKey on Windows. Which one an application supports is
    /// learned on its first correction (see `apps`).
    fn edit(&mut self, backs: usize, text: &str) {
        if self.probe.is_some() {
            self.held.push_back((backs, text.to_owned()));
            return;
        }
        let mode = match (self.field_surr, self.learned.get(&self.focused_class)) {
            (false, _) => apps::Mode::Keys,
            (true, Some(mode)) => mode,
            (true, None) => apps::Mode::Atomic, // probe
        };
        log!("edit: delete {backs}, insert {text:?} ({mode:?})");
        if mode == apps::Mode::Atomic && self.shown.len() >= backs {
            let before: String = self.shown.iter().collect();
            let gone: usize = self.shown.drain(self.shown.len() - backs..).map(char::len_utf8).sum();
            self.shown.extend(text.chars());
            if let Some(im) = &self.im {
                if gone > 0 {
                    im.delete_surrounding_text(gone as u32, 0);
                }
                if !text.is_empty() {
                    im.commit_string(text.to_owned());
                }
                im.commit(self.serial);
            }
            self.last_emit = Instant::now();
            if gone > 0 && self.learned.get(&self.focused_class).is_none() {
                let after: String = self.shown.iter().collect();
                self.probe = Some((before, after, text.to_owned(), backs, Instant::now() + PROBE_TIMEOUT));
            }
        } else {
            self.key_edit(backs, text);
        }
        self.trim_shown();
    }

    /// Backspaces + the new characters as one burst of key presses (see `inject`).
    fn key_edit(&mut self, backs: usize, text: &str) {
        self.flush(); // forwarded keys still queued go first
        for _ in 0..backs {
            self.shown.pop();
        }
        self.shown.extend(text.chars());
        if let Some(inj) = self.injector.as_mut() {
            inj.emit(backs, text);
            self.last_emit = Instant::now();
        }
    }

    /// The same, for an X11 window (see `x11`).
    fn x_edit(&mut self, backs: usize, text: &str) {
        if let Some(x) = self.x.as_mut() {
            x.edit(backs, text);
            self.last_emit = Instant::now();
        }
    }

    fn trim_shown(&mut self) {
        if self.shown.len() > 256 {
            self.shown.drain(..128);
        }
    }

    /// The field reported the text before its cursor: settle a pending probe.
    fn on_surrounding(&mut self, before_cursor: &str) {
        let Some((before, after, inserted, backs, _)) = &self.probe else { return };
        let verdict = apps::judge(before_cursor, before, after, inserted);
        log!("probe for {:?}: {verdict:?} (reported {before_cursor:?}, before {before:?}, after {after:?})", self.focused_class);
        let class = self.focused_class.clone();
        match verdict {
            apps::Verdict::Applied => self.learned.set(&class, apps::Mode::Atomic),
            apps::Verdict::Ignored => {
                // Undo what the ignored deletion left: retype the replacement after
                // deleting it and the characters that should have gone.
                let (n, text) = (inserted.chars().count() + backs, inserted.clone());
                self.learned.set(&class, apps::Mode::Keys);
                self.probe = None;
                let model = std::mem::take(&mut self.shown);
                self.key_edit(n, &text);
                self.shown = model;
            }
            apps::Verdict::Unclear => {
                // Some clients (Chromium) report the text from before our edit;
                // an empty commit makes them report again.
                if let Some(im) = &self.im {
                    im.commit(self.serial);
                }
                return;
            }
        }
        self.probe = None;
        self.release_held();
    }

    /// No clear answer in time: assume the safe way (keys) for this application.
    fn probe_timed_out(&mut self) {
        if self.probe.as_ref().is_some_and(|p| Instant::now() >= p.4) {
            log!("probe for {:?}: no answer, using keys", self.focused_class);
            let class = self.focused_class.clone();
            self.learned.set(&class, apps::Mode::Keys);
            self.probe = None;
            self.release_held();
        }
    }

    fn release_held(&mut self) {
        while let Some((backs, text)) = self.held.pop_front() {
            self.edit(backs, &text);
            if self.probe.is_some() {
                break;
            }
        }
    }

    /// Forget what we know about the text around the cursor (new field, a key we
    /// do not track, a click ...).
    fn forget_text(&mut self) {
        self.shown.clear();
        self.probe = None;
        self.held.clear();
    }

    // ---- key handling ---------------------------------------------------------

    fn mod_active(&self, name: &str) -> bool {
        self.xkb_state
            .as_ref()
            .is_some_and(|s| s.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE))
    }

    fn on_key(&mut self, key: u32, pressed: bool) {
        let Some(state) = &self.xkb_state else {
            // No keymap yet: just pass everything through.
            self.send_key(key, pressed);
            return;
        };
        let code = xkb::Keycode::new(key + XKB_OFFSET);

        if !pressed {
            let (how, sym) = self.down.remove(&key).unwrap_or((Down::Verbatim, 0));
            if matches!(how, Down::Verbatim) {
                self.send_key(key, false);
                self.flush();
            }
            self.toggle_release(sym);
            if self.repeat.as_ref().is_some_and(|r| r.key == key) {
                self.repeat = None;
            }
            return;
        }

        let sym = state.key_get_one_sym(code).raw();
        let utf8 = state.key_get_utf8(code);

        // Modifier keys: replay, never reset the word (they may arm the toggle).
        if is_modifier(sym) {
            self.toggle_press(sym);
            self.down.insert(key, (Down::Verbatim, sym));
            self.send_key(key, true);
            self.flush();
            return;
        }
        self.toggle_armed = false;

        if self.is_alt_z(sym, self.mod_active("Mod1"), self.mod_active("Control") || self.mod_active("Mod4")) {
            self.down.insert(key, (Down::Tap, sym)); // swallowed: the app never sees Alt+Z
            self.toggle_vietnamese();
            return;
        }

        let combo = self.mod_active("Control") || self.mod_active("Mod1") || self.mod_active("Mod4");
        let ascii = utf8.chars().next().filter(|c| utf8.chars().count() == 1 && (' '..='~').contains(c));

        if !self.engine.viet_key() || combo {
            self.engine.reset();
            self.verbatim(key, sym);
            return;
        }

        if sym == KS_BACKSPACE {
            self.begin_tap(key, sym);
            self.backspace();
            return;
        }

        let Some(c) = ascii else {
            // Enter, Tab, arrows, function keys, non-ASCII letters ...: they end the word.
            self.engine.reset();
            // Fields that report their text are not terminals: no Vim there.
            self.after_escape = sym == KS_ESCAPE && !self.field_surr;
            self.verbatim(key, sym);
            return;
        };

        self.begin_tap(key, sym);
        self.character(c);
    }

    /// With the Alt+Z shortcut: is this key press Alt+Z?
    fn is_alt_z(&self, sym: u32, alt: bool, other_mods: bool) -> bool {
        self.cfg.toggle_key() == ToggleKey::AltZ && alt && !other_mods && matches!(sym, KS_Z_LOWER | KS_Z_UPPER)
    }

    /// With the Ctrl+Shift shortcut: Ctrl+Shift pressed and released on their
    /// own toggles Vietnamese typing.
    fn toggle_press(&mut self, sym: u32) {
        if self.cfg.toggle_key() == ToggleKey::CtrlShift && is_toggle_key(sym) {
            self.toggle_held.push(sym);
            let ctrl = self.toggle_held.iter().any(|&s| matches!(s, KS_CONTROL_L | KS_CONTROL_R));
            let shift = self.toggle_held.iter().any(|&s| matches!(s, KS_SHIFT_L | KS_SHIFT_R));
            self.toggle_armed = ctrl && shift;
        }
    }

    fn toggle_release(&mut self, sym: u32) {
        if is_toggle_key(sym) {
            if let Some(i) = self.toggle_held.iter().position(|&s| s == sym) {
                self.toggle_held.remove(i);
            }
            if self.toggle_armed {
                self.toggle_armed = false;
                self.toggle_vietnamese();
            }
        }
    }

    fn verbatim(&mut self, key: u32, sym: u32) {
        self.forget_text();
        self.down.insert(key, (Down::Verbatim, sym));
        self.send_key(key, true);
        self.flush();
    }

    /// Mark `key` as handled-by-tap and arm key repeat for it.
    fn begin_tap(&mut self, key: u32, sym: u32) {
        self.down.insert(key, (Down::Tap, sym));
        if self.repeat_rate > 0 {
            self.repeat = Some(Repeat { key, due: Instant::now() + Duration::from_millis(self.repeat_delay as u64) });
        }
    }

    /// Editor-friendly rules applied before the engine sees `c`. Returns false
    /// when `c` must be typed as is.
    ///
    /// - In a terminal, the first letter after Escape is a Vim command (`i`, `a`,
    ///   `o` ...): typed as is, and the text after it starts a new word instead of
    ///   "iviet" failing spell check.
    /// - Telex `w` right after punctuation stays `w`: `:w`, `:wq`, `/word`.
    ///   After a space or at the start it is still "ư".
    fn prepare_char(&mut self, c: char) -> bool {
        if std::mem::take(&mut self.after_escape) {
            self.engine.reset();
            self.last_char = Some(c);
            return false;
        }
        let after_symbol = self.last_char.is_some_and(|p| p.is_ascii_punctuation());
        self.last_char = Some(c);
        if matches!(c, 'w' | 'W') && after_symbol && self.engine.at_word_beginning() {
            self.engine.pass(c as u32);
            return false;
        }
        true
    }

    fn character(&mut self, c: char) {
        if !self.prepare_char(c) {
            self.edit(0, c.encode_utf8(&mut [0; 4]));
            return;
        }
        self.engine.set_caps_state(self.mod_active("Shift"), self.mod_active("Lock"));
        // Shift+Space restores the typed keys of the last word (UniKey's "restore" key).
        let restore = c == ' ' && self.mod_active("Shift") && !self.engine.at_word_beginning();
        let out = if restore { self.engine.restore_key_strokes() } else { self.engine.process(c as u32) };
        if out.handled {
            self.edit(out.backs, &out.text);
        } else {
            self.edit(0, c.encode_utf8(&mut [0; 4]));
        }
    }

    fn backspace(&mut self) {
        let out = self.engine.process_backspace();
        if out.handled {
            self.edit(out.backs, &out.text);
        } else {
            self.edit(1, ""); // a plain Backspace, kept in order with the rest
        }
    }

    // ---- XWayland ---------------------------------------------------------------

    fn connect_x(&mut self) {
        self.x_retry = Instant::now() + Duration::from_secs(5);
        let x = match x11::X11::connect() {
            Ok(x) => x,
            Err(why) => {
                log!("XWayland not available: {why}");
                return;
            }
        };
        // Hyprland's layout setting, else what the X server was set up with.
        let hypr = hypr_option("input:kb_layout");
        let [rules, model, layout, variant, options] = if !hypr.is_empty() {
            [String::new(), String::new(), hypr, hypr_option("input:kb_variant"), hypr_option("input:kb_options")]
        } else {
            x.layout_names().unwrap_or_default()
        };
        let keymap = xkb::Keymap::new_from_names(
            &self.xkb_ctx,
            rules.as_str(),
            model.as_str(),
            if layout.is_empty() { "us" } else { layout.as_str() },
            variant.as_str(),
            Some(options),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        );
        let Some(keymap) = keymap else {
            log!("XWayland: cannot build the keyboard layout");
            return;
        };
        let mut state = xkb::State::new(&keymap);
        if let Some(x11::Mods([d, l, k, dg, lg, kg])) = x.mods() {
            state.update_mask(d, l, k, dg, lg, kg);
        }
        self.xkb_x = Some(state);
        self.x = Some(x);
        log!("X server connected");
    }

    fn pump_x(&mut self) {
        loop {
            let next = match &self.x {
                Some(x) => x.next_input(),
                None => return,
            };
            match next {
                Ok(Some(x11::Input::Key { keycode, pressed })) => self.on_x_key(keycode, pressed),
                Ok(Some(x11::Input::Click)) => self.engine.reset(),
                Ok(Some(x11::Input::Mods(x11::Mods([d, l, k, dg, lg, kg])))) => {
                    if let Some(s) = self.xkb_x.as_mut() {
                        s.update_mask(d, l, k, dg, lg, kg);
                    }
                }
                Ok(None) => return,
                Err(()) => {
                    log!("X server connection lost");
                    self.x = None;
                    self.x_retry = Instant::now() + Duration::from_secs(5);
                    return;
                }
            }
        }
    }

    /// A key typed into an X11 window. The application already has it; if the
    /// engine converts something we correct the text afterwards.
    fn on_x_key(&mut self, keycode: u32, pressed: bool) {
        let code = xkb::Keycode::new(keycode);
        let Some(state) = self.xkb_x.as_mut() else { return };
        let sym = state.key_get_one_sym(code).raw();
        if !pressed {
            state.update_key(code, xkb::KeyDirection::Up);
            self.toggle_release(sym);
            return;
        }
        let utf8 = state.key_get_utf8(code);
        let active = |n: &str| state.mod_name_is_active(n, xkb::STATE_MODS_EFFECTIVE);
        let (shift, caps) = (active("Shift"), active("Lock"));
        let (alt, other_mods) = (active("Mod1"), active("Control") || active("Mod4"));
        let combo = alt || other_mods;
        state.update_key(code, xkb::KeyDirection::Down);
        if self.is_alt_z(sym, alt, other_mods) {
            self.toggle_vietnamese(); // X11 apps still receive Alt+Z itself
            return;
        }

        if is_modifier(sym) {
            self.toggle_press(sym);
            return;
        }
        self.toggle_armed = false;
        if !self.engine.viet_key() || combo {
            self.engine.reset();
            return;
        }

        if sym == KS_BACKSPACE {
            // The application has already deleted one character itself.
            let out = self.engine.process_backspace();
            if out.handled {
                self.x_edit(out.backs.saturating_sub(1), &out.text);
            }
            return;
        }

        let ascii = utf8.chars().next().filter(|c| utf8.chars().count() == 1 && (' '..='~').contains(c));
        let Some(c) = ascii else {
            self.engine.reset();
            self.after_escape = sym == KS_ESCAPE;
            return;
        };
        if !self.prepare_char(c) {
            return; // already typed by the application
        }
        self.engine.set_caps_state(shift, caps);
        let restore = c == ' ' && shift && !self.engine.at_word_beginning();
        let out = if restore { self.engine.restore_key_strokes() } else { self.engine.process(c as u32) };
        log!("x key {c:?}: handled={} backs={} text={:?}", out.handled, out.backs, out.text);
        if out.handled {
            // The typed character is already in the text: delete it too.
            self.x_edit(out.backs + 1, &out.text);
        }
    }

    fn fire_repeat(&mut self) {
        let Some(r) = &self.repeat else { return };
        if Instant::now() < r.due {
            return;
        }
        let key = r.key;
        let Some(&(_, sym)) = self.down.get(&key) else {
            self.repeat = None;
            return;
        };
        let interval = Duration::from_millis(1000 / self.repeat_rate.max(1) as u64);
        self.repeat = Some(Repeat { key, due: Instant::now() + interval });
        if sym == KS_BACKSPACE {
            self.backspace();
        } else if let Some(state) = &self.xkb_state {
            let utf8 = state.key_get_utf8(xkb::Keycode::new(key + XKB_OFFSET));
            if let Some(c) = utf8.chars().next().filter(|c| (' '..='~').contains(c)) {
                self.character(c);
            }
        }
    }

    fn next_timeout(&self) -> i32 {
        let retry = self.x.is_none().then_some(self.x_retry);
        let probe = self.probe.as_ref().map(|p| p.4);
        let due = [self.repeat.as_ref().map(|r| r.due), self.queue.front().map(|(d, _)| *d), retry, probe]
            .into_iter()
            .flatten()
            .min();
        match due {
            Some(d) => {
                let ms = d.saturating_duration_since(Instant::now()).as_millis();
                // round up so we never wake just before the deadline
                (ms + 1).min(i32::MAX as u128) as i32
            }
            None => -1,
        }
    }

    fn on_signal(&mut self, sig: i32) {
        match sig {
            libc::SIGUSR1 => {
                log!("reloading settings");
                self.cfg = config::Config::load();
                self.cfg.apply(&mut self.engine);
                self.toggle_held.clear();
                self.toggle_armed = false;
            }
            libc::SIGUSR2 => self.toggle_vietnamese(),
            _ => {}
        }
    }

    fn toggle_vietnamese(&mut self) {
        let on = !self.engine.viet_key();
        self.set_vietnamese(on);
    }

    fn set_vietnamese(&mut self, on: bool) {
        self.engine.set_viet_key(on);
        self.cfg.publish_mode(on);
        if let Some(t) = &self.tray {
            t.update(|t| t.vietnamese = on);
        }
    }

    // ---- input method lifecycle ----------------------------------------------------

    fn apply_pending(&mut self, qh: &QueueHandle<App>) {
        // Numeric / password style fields: leave the keyboard alone.
        let bypass = matches!(self.pending_purpose, 2 | 3 | 4 | 8 | 9 | 10 | 11 | 12);
        let tracks_focus = self.hypr_events.is_some() || self.toplevel_mgr.is_some();
        let settled = !self.reactivated && self.im_window.as_deref() == Some(self.focused_addr.as_str());
        if self.pending_active && tracks_focus && !settled {
            // The active field belongs to the focused window (compositors do not
            // always re-activate a field when its window comes back). Hyprland is
            // asked directly; elsewhere the focus from toplevel events is used.
            let focused = if self.hypr_events.is_some() {
                hypr::active_window()
            } else {
                let x11 = self.x.as_ref().is_some_and(|x| x.has_focus());
                (!self.focused_addr.is_empty()).then(|| (self.focused_addr.clone(), x11))
            };
            match focused {
                Some((addr, false)) => self.im_window = Some(addr),
                // An X11 window cannot have a Wayland text field: the report is
                // stale, from the window that lost focus (Chromium sends those).
                // The field stays with that window; `on_focus` grabs again when it
                // comes back, since nothing re-activates it then.
                Some((_, true)) => {}
                None => self.im_window = None,
            }
            if self.reactivated {
                self.im_claimed_at = Some(Instant::now());
            }
        }
        // A window that just lost focus can still report its field active (Chromium
        // does): only the focused window may hold the grab, as in `on_focus`.
        let elsewhere = tracks_focus
            && !self.focused_addr.is_empty()
            && self.im_window.as_deref() != Some(self.focused_addr.as_str());
        let want =
            self.pending_active && !bypass && (self.im_window.is_some() || !tracks_focus) && !elsewhere;
        log!("apply: want={want} active={} window={:?}", self.active, self.im_window);
        if want && !self.active {
            self.active = true;
            self.engine.reset();
            if let (Some(im), None) = (&self.im, &self.grab) {
                self.grab = Some(im.grab_keyboard(qh, ()));
            }
        } else if !want && self.active {
            self.active = false;
            self.engine.reset();
            self.release_grab();
        } else if want && self.reactivated {
            self.engine.reset(); // focus moved to another text field
        }
        self.reactivated = false;
    }

    /// Keyboard focus moved to `addr`. Only the window that activated us may keep
    /// the keyboard grab; anywhere else keys must reach the window untouched.
    fn on_focus(&mut self, addr: &str, class: &str) {
        self.engine.reset(); // like UniKey on Windows: a focus change starts a new word
        self.forget_text();
        // Remember the mode of the app we leave (toggled or not), so returning to
        // it restores it even after another app changed the mode.
        if self.cfg.per_app_mode() && !self.focused_class.is_empty() {
            self.app_modes.insert(self.focused_class.clone(), self.engine.viet_key());
        }
        self.focused_class = class.to_owned();
        if self.cfg.per_app_mode() {
            if let Some(&on) = self.app_modes.get(class) {
                if on != self.engine.viet_key() {
                    log!("per-app mode: {class} -> {}", if on { "VI" } else { "EN" });
                    self.set_vietnamese(on);
                }
            }
        }
        // Without Hyprland the compositor may announce a new text field just
        // before the focus change that brought it: that field claims this window.
        // A stale report from a window losing focus is no new field, so it cannot.
        if self.hypr_events.is_none()
            && !addr.is_empty()
            && self.pending_active
            && self.im_claimed_at.is_some_and(|t| t.elapsed() < FIELD_CLAIM)
        {
            self.im_window = Some(addr.to_owned());
        }
        let mine = self.pending_active && self.im_window.as_deref() == Some(addr);
        if self.active && !mine {
            log!("focus left the text field's window: releasing the keyboard");
            self.active = false;
            self.release_grab();
        } else if !self.active && mine {
            log!("focus back on the text field's window: grabbing the keyboard");
            self.active = true;
            if let (Some(im), Some(qh)) = (&self.im, &self.qh) {
                self.grab = Some(im.grab_keyboard(qh, ()));
            }
        }
    }

    fn pump_focus(&mut self) {
        let Some(events) = self.hypr_events.as_mut() else { return };
        match events.focus_changes() {
            Some(changes) => {
                for (addr, class) in changes {
                    self.focus_changed(addr, class);
                }
            }
            None => self.hypr_events = None,
        }
    }

    /// A focus report from Hyprland or toplevel events. They repeat when the
    /// focused window's title changes (terminal spinners, browser tabs): not a
    /// focus change.
    fn focus_changed(&mut self, addr: String, class: String) {
        if addr != self.focused_addr {
            log!("focus: {addr:?} ({class:?})");
            self.focused_addr = addr.clone();
            self.on_focus(&addr, &class);
        }
    }

    fn release_grab(&mut self) {
        if let Some(g) = self.grab.take() {
            g.release();
        }
        self.repeat = None;
        self.queue.clear();
        self.down.clear();
        self.toggle_held.clear();
        self.toggle_armed = false;
    }

    fn on_keymap(&mut self, fd: OwnedFd, size: u32) {
        let mut text = String::new();
        let mut file = std::fs::File::from(fd.try_clone().expect("dup keymap fd"));
        if file.read_to_string(&mut text).is_err() {
            eprintln!("unikey: cannot read the keyboard keymap");
            return;
        }
        text.truncate(text.trim_end_matches('\0').len());
        let keymap =
            xkb::Keymap::new_from_string(&self.xkb_ctx, text, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS);
        let Some(keymap) = keymap else {
            eprintln!("unikey: invalid keymap from the compositor");
            return;
        };
        self.xkb_state = Some(xkb::State::new(&keymap));
        if let Some(vk) = &self.vk {
            // Replayed keys must be interpreted with the same layout as the user's.
            vk.keymap(1, fd.as_fd(), size);
            self.keymap_sent = true;
        }
    }
}

fn is_toggle_key(sym: u32) -> bool {
    matches!(sym, KS_SHIFT_L | KS_SHIFT_R | KS_CONTROL_L | KS_CONTROL_R)
}

/// Keys that only change modifier state: replayed, they never end a word.
fn is_modifier(sym: u32) -> bool {
    (KS_SHIFT_L..=KS_MODIFIER_LAST).contains(&sym) || sym == KS_ISO_LEVEL3_SHIFT || sym == KS_NUM_LOCK
}

/// A Hyprland option as text ("us"), for building the same layout the compositor uses.
fn hypr_option(name: &str) -> String {
    std::process::Command::new("hyprctl")
        .args(["getoption", name])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("str: "))
                .map(|v| v.trim().to_owned())
        })
        .filter(|v| v != "[[EMPTY]]") // hyprctl's spelling of "unset"
        .unwrap_or_default()
}

/// Block SIGUSR1/SIGUSR2 and receive them through a file descriptor, so they are
/// handled in the event loop: SIGUSR1 reloads the settings, SIGUSR2 toggles
/// Vietnamese typing (used by unikey-settings and the Waybar indicator).
fn signal_fd() -> i32 {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::sigaddset(&mut set, libc::SIGUSR2);
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        libc::signalfd(-1, &set, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC)
    }
}

/// Signals waiting on `fd`.
fn pending_signals(fd: i32) -> Vec<i32> {
    let mut out = Vec::new();
    loop {
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        let n = unsafe { libc::read(fd, &mut info as *mut _ as *mut libc::c_void, size) };
        if n != size as isize {
            return out;
        }
        out.push(info.ssi_signo as i32);
    }
}

fn boot_ms() -> u32 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000) as u32
}

// ---- wayland plumbing -------------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(app: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_seat" if app.seat.is_none() => app.seat = Some(reg.bind(name, 1, qh, ())),
                "zwp_input_method_manager_v2" => app.manager = Some(reg.bind(name, 1, qh, ())),
                "zwp_virtual_keyboard_manager_v1" => app.vk_manager = Some(reg.bind(name, 1, qh, ())),
                "zwlr_foreign_toplevel_manager_v1" if app.hypr_events.is_none() => {
                    app.toplevel_mgr = Some(reg.bind(name, version.min(3), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for App {
    fn event(app: &mut Self, _: &ZwlrForeignToplevelManagerV1, ev: zwlr_foreign_toplevel_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Finished = ev {
            app.toplevel_mgr = None;
        }
    }

    wayland_client::event_created_child!(App, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for App {
    fn event(app: &mut Self, handle: &ZwlrForeignToplevelHandleV1, ev: zwlr_foreign_toplevel_handle_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::Proxy;
        use zwlr_foreign_toplevel_handle_v1::Event::*;
        let id = handle.id().protocol_id();
        let addr = id.to_string();
        match ev {
            AppId { app_id } => app.toplevels.entry(id).or_default().0 = app_id,
            State { state } => {
                let activated = zwlr_foreign_toplevel_handle_v1::State::Activated as u32;
                app.toplevels.entry(id).or_default().1 =
                    state.chunks_exact(4).any(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) == activated);
            }
            Done => {
                let (class, active) = app.toplevels.get(&id).cloned().unwrap_or_default();
                if active {
                    app.focus_changed(addr, class);
                } else if app.focused_addr == addr {
                    app.focus_changed(String::new(), String::new()); // nothing focused
                }
            }
            Closed => {
                app.toplevels.remove(&id);
                if app.focused_addr == addr {
                    app.focus_changed(String::new(), String::new());
                }
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for App {
    fn event(app: &mut Self, _: &ZwpInputMethodV2, ev: zwp_input_method_v2::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        use zwp_input_method_v2::Event::*;
        match ev {
            SurroundingText { text, cursor, .. } => {
                app.field_surr = true;
                let cursor = (cursor.max(0) as usize).min(text.len());
                if let Some(before) = text.get(..cursor) {
                    app.on_surrounding(before);
                }
            }
            Activate => {
                log!("activate");
                app.pending_active = true;
                app.reactivated = true;
                app.field_surr = false;
                app.forget_text();
            }
            TextChangeCause { cause } => {
                // (the enum lives in the text-input protocol crate; its name is enough)
                app.cycle_other = !format!("{cause:?}").contains("InputMethod");
            }
            ContentType { purpose, .. } => {
                app.pending_purpose = match purpose {
                    WEnum::Value(v) => v as u32,
                    WEnum::Unknown(v) => v,
                }
            }
            Done => {
                log!("done #{} (active={}, purpose={})", app.serial + 1, app.pending_active, app.pending_purpose);
                app.serial = app.serial.wrapping_add(1);
                // The text changed without us (a mouse click moved the cursor, say):
                // the word in progress no longer sits before the cursor.
                if app.cycle_other && app.last_emit.elapsed() > OWN_CHANGE_WINDOW && !app.reactivated {
                    log!("text changed by the user: new word");
                    app.engine.reset();
                    app.forget_text();
                }
                app.cycle_other = false;
                app.apply_pending(qh);
            }
            Unavailable => {
                eprintln!("unikey: another input method is already running on this seat");
                app.quit = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for App {
    fn event(app: &mut Self, _: &ZwpInputMethodKeyboardGrabV2, ev: zwp_input_method_keyboard_grab_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwp_input_method_keyboard_grab_v2::Event::*;
        match ev {
            Keymap { fd, size, .. } => {
                log!("grab keymap ({size} bytes)");
                app.on_keymap(fd, size)
            }
            Key { key, state, .. } => {
                let pressed = matches!(state, WEnum::Value(wayland_client::protocol::wl_keyboard::KeyState::Pressed));
                log!("grab key {key} pressed={pressed}");
                app.on_key(key, pressed);
            }
            Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(s) = &mut app.xkb_state {
                    s.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                app.push(Act::Mods(mods_depressed, mods_latched, mods_locked, group));
                app.flush();
            }
            RepeatInfo { rate, delay } => {
                app.repeat_rate = rate.max(0) as u32;
                app.repeat_delay = delay.max(0) as u32;
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(App: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(App: ZwpInputMethodManagerV2);
wayland_client::delegate_noop!(App: ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(App: ZwpVirtualKeyboardV1);

fn main() {
    let sig_fd = signal_fd();
    let cfg = config::Config::load();
    let mut app = App::new(cfg);
    let wayland = connect_wayland(&mut app);
    app.cfg.publish_mode(app.engine.viet_key());
    app.tray = Some(tray::spawn(app.engine.viet_key()));
    app.connect_x();
    match wayland {
        Some((conn, queue)) => run_wayland(app, conn, queue, sig_fd),
        None => run_x11(app, sig_fd),
    }
}

/// The Wayland input method, if there is a compositor that offers one (wlroots
/// compositors, Hyprland, Niri ...). `None` on GNOME/KDE or in an X11 session.
fn connect_wayland(app: &mut App) -> Option<(Connection, EventQueue<App>)> {
    let conn = Connection::connect_to_env().ok()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(app).unwrap_or_else(|e| die(&format!("registry: {e}")));
    let (Some(seat), Some(manager), Some(vk_manager)) =
        (app.seat.clone(), app.manager.clone(), app.vk_manager.clone())
    else {
        eprintln!("unikey: the compositor has no zwp_input_method_v2/zwp_virtual_keyboard_v1: X11 apps only");
        return None;
    };
    app.qh = Some(qh.clone());
    app.vk = Some(vk_manager.create_virtual_keyboard(&seat, &qh, ()));
    app.injector = Some(inject::Injector::new(vk_manager.create_virtual_keyboard(&seat, &qh, ())));
    app.im = Some(manager.get_input_method(&seat, &qh, ()));
    queue.roundtrip(app).unwrap_or_else(|e| die(&format!("input method: {e}")));
    Some((conn, queue))
}

/// No Wayland input method: type for X11 windows only (an X11 session, or
/// XWayland apps under a compositor without the protocols).
fn run_x11(mut app: App, sig_fd: i32) {
    if app.x.is_none() {
        die("no Wayland input method and no X server to type into");
    }
    while !app.quit {
        let mut fds = [
            libc::pollfd { fd: app.x.as_ref().map_or(-1, |x| x.fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: sig_fd, events: libc::POLLIN, revents: 0 },
        ];
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, app.next_timeout()) };
        for sig in pending_signals(sig_fd) {
            app.on_signal(sig);
        }
        app.pump_x();
        if app.x.is_none() {
            die("lost the X server connection"); // the session ended
        }
    }
}

fn run_wayland(mut app: App, conn: Connection, mut queue: EventQueue<App>, sig_fd: i32) {
    while !app.quit {
        queue.dispatch_pending(&mut app).unwrap_or_else(|e| die(&format!("dispatch: {e}")));
        let _ = conn.flush();
        let Some(guard) = conn.prepare_read() else { continue };
        let mut fds = [
            libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: app.x.as_ref().map_or(-1, |x| x.fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: app.hypr_events.as_ref().map_or(-1, |e| e.fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: sig_fd, events: libc::POLLIN, revents: 0 },
        ];
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, app.next_timeout()) };
        for sig in pending_signals(sig_fd) {
            app.on_signal(sig);
        }
        if fds[0].revents & libc::POLLIN != 0 {
            match guard.read() {
                // "No complete message yet": wayland-client documents it as harmless.
                Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => die(&format!("lost the Wayland connection: {e}")),
                Ok(_) => {}
            }
        } else {
            drop(guard);
        }
        queue.dispatch_pending(&mut app).unwrap_or_else(|e| die(&format!("dispatch: {e}")));
        app.pump_focus();
        app.probe_timed_out();
        app.fire_repeat();
        app.pump_x();
        if app.x.is_none() && Instant::now() >= app.x_retry {
            app.connect_x();
        }
        app.flush();
        let _ = conn.flush();
    }
}

fn die(msg: &str) -> ! {
    eprintln!("unikey: {msg}");
    std::process::exit(1);
}
