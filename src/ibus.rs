//! GNOME on Wayland: unikey as an IBus engine.
//!
//! Mutter offers applications no input method protocol (no
//! zwp_input_method_v2): GNOME Shell hands every key typed into a text field to
//! IBus instead. There `unikey --ibus` is the engine. ibus-daemon starts it from
//! the component file `unikey ibus-install` writes, once the user picks
//! "Vietnamese (UniKey)" in Settings → Keyboard → Input Sources.
//!
//! Like the Wayland frontend there is no preedit (no underline): a converted key
//! deletes the characters it replaces and commits the new ones. If the
//! application has reported the text before the cursor and it matches what we
//! typed, the deletion is one DeleteSurroundingText, applied together with the
//! commit. Otherwise (terminals, which report nothing) it is Backspace key
//! presses, which GNOME Shell delivers before the commit.
//!
//! The engine speaks D-Bus to ibus-daemon on its private bus. The protocol is
//! the one libibus implements (`IBusFactory`, `IBusEngine`). Text, properties
//! and property lists travel as IBus "serializable" structs.

use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use dbus::{
    arg::{IterAppend, RefArg, Variant},
    channel::Channel,
    message::MessageType,
    strings::{Interface, Member, Signature},
    Message,
};
use unikey::{settings::{self, ToggleKey}, Engine};

use super::{config::Config, is_modifier, is_toggle_key, pending_signals, KS_BACKSPACE, KS_ESCAPE};

/// Bus name ibus-daemon waits for after starting us (the component's `<name>`).
const COMPONENT: &str = "org.freedesktop.IBus.UnikeyRs";
/// The engine's name: GNOME stores it in its input sources as ('ibus', ENGINE).
pub const ENGINE: &str = "unikey-rs";

const IFACE_FACTORY: &str = "org.freedesktop.IBus.Factory";
const IFACE_ENGINE: &str = "org.freedesktop.IBus.Engine";
const IFACE_SERVICE: &str = "org.freedesktop.IBus.Service";
const IFACE_PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const IFACE_INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";

// IBusModifierType bits (the same as GDK's and Clutter's).
const SHIFT: u32 = 1 << 0;
const LOCK: u32 = 1 << 1;
const CONTROL: u32 = 1 << 2;
const MOD1: u32 = 1 << 3; // Alt
const MOD4: u32 = 1 << 6; // Super
const SUPER: u32 = 1 << 26;
const HYPER: u32 = 1 << 27;
const META: u32 = 1 << 28;
const RELEASE: u32 = 1 << 30;

/// IBUS_CAP_SURROUNDING_TEXT: the client can report and delete text around the cursor.
const CAP_SURROUNDING_TEXT: u32 = 1 << 5;

/// IBusPropType / IBusPropState.
const PROP_NORMAL: u32 = 0;
const PROP_TOGGLE: u32 = 1;
const PROP_CHECKED: u32 = 1;

/// Content purposes where typing must not be converted (IBusInputPurpose:
/// digits, number, phone, password, PIN).
const BYPASS_PURPOSES: [u32; 5] = [2, 3, 4, 8, 9];
/// IBUS_INPUT_PURPOSE_TERMINAL.
const PURPOSE_TERMINAL: u32 = 10;

/// evdev code of Backspace, for the key events we forward.
const KEY_BACKSPACE: u32 = 14;
// Keypad digits, typed as digits (VNI uses them).
const KS_KP_0: u32 = 0xffb0;
const KS_KP_9: u32 = 0xffb9;
const KS_Z_LOWER: u32 = 0x7a;
const KS_Z_UPPER: u32 = 0x5a;

// ---- the engine ---------------------------------------------------------------------

struct Ime {
    ch: Channel,
    engine: Engine,
    cfg: Config,
    /// Engine objects ibus-daemon created (it normally makes one).
    objects: Vec<String>,
    next_id: u32,
    caps: u32,
    purpose: u32,
    /// The text before the cursor as the application last reported it since it
    /// got focus. `None`: never reported (terminals), or a selection.
    before: Option<String>,
    /// What we put before the cursor in this field, most recent last.
    shown: Vec<char>,
    toggle_held: Vec<u32>,
    toggle_armed: bool,
    /// Alt+Z switched the mode: its release is not the application's either.
    swallow: Option<u32>,
    after_escape: bool,
    last_char: Option<char>,
}

impl Ime {
    fn new(ch: Channel, cfg: Config) -> Self {
        let mut engine = Engine::new();
        cfg.apply(&mut engine);
        cfg.publish_mode(engine.viet_key());
        Ime {
            ch,
            engine,
            cfg,
            objects: Vec::new(),
            next_id: 1,
            caps: 0,
            purpose: 0,
            before: None,
            shown: Vec::new(),
            toggle_held: Vec::new(),
            toggle_armed: false,
            swallow: None,
            after_escape: false,
            last_char: None,
        }
    }

    fn send(&self, msg: Message) {
        let _ = self.ch.send(msg);
    }

    fn signal(&self, path: &str, member: &'static str) -> Message {
        Message::signal(&path.into(), &Interface::from(IFACE_ENGINE), &Member::from(member))
    }

    // ---- what the engine asks of the application ------------------------------------

    fn commit(&self, path: &str, text: &str) {
        let mut msg = self.signal(path, "CommitText");
        append_text(&mut IterAppend::new(&mut msg), text);
        self.send(msg);
    }

    fn forward_key(&self, path: &str, keyval: u32, keycode: u32, state: u32) {
        self.send(self.signal(path, "ForwardKeyEvent").append3(keyval, keycode, state));
    }

    /// Delete `backs` characters before the cursor and insert `text`.
    fn edit(&mut self, path: &str, backs: usize, text: &str) {
        log!("ibus edit: delete {backs}, insert {text:?}");
        if backs > 0 {
            if self.can_delete_surrounding(backs) {
                let n = backs as u32;
                self.send(self.signal(path, "DeleteSurroundingText").append2(-(n as i32), n));
            } else {
                for _ in 0..backs {
                    self.forward_key(path, KS_BACKSPACE, KEY_BACKSPACE, 0);
                    self.forward_key(path, KS_BACKSPACE, KEY_BACKSPACE, RELEASE);
                }
            }
        }
        if !text.is_empty() {
            self.commit(path, text);
        }
        let keep = self.shown.len().saturating_sub(backs);
        self.shown.truncate(keep);
        self.shown.extend(text.chars());
        if self.shown.len() > 256 {
            self.shown.drain(..128);
        }
    }

    /// Whether the application can delete the `n` characters before the cursor
    /// itself: it reports its text, and the last report ends with the characters
    /// we typed there. GNOME Shell drops a deletion it cannot place, so a report
    /// that has not caught up with our last edit means Backspace keys instead.
    fn can_delete_surrounding(&self, n: usize) -> bool {
        self.caps & CAP_SURROUNDING_TEXT != 0
            && self.shown.len() >= n
            && self
                .before
                .as_ref()
                .is_some_and(|b| b.chars().rev().take(n).eq(self.shown.iter().rev().take(n).copied()))
    }

    fn forget_text(&mut self) {
        self.shown.clear();
    }

    /// Terminals neither report their text nor take deletions.
    fn in_terminal(&self) -> bool {
        self.purpose == PURPOSE_TERMINAL || self.caps & CAP_SURROUNDING_TEXT == 0 || self.before.is_none()
    }

    // ---- keys -----------------------------------------------------------------------

    /// One key event from the focused field. True: we took care of it.
    fn process_key(&mut self, path: &str, keyval: u32, keycode: u32, state: u32) -> bool {
        log!("ibus key {keyval:#x} code {keycode} state {state:#x}");
        if state & RELEASE != 0 {
            if self.toggle_release(keyval) {
                self.toggle_vietnamese();
            }
            if self.swallow == Some(keycode) {
                self.swallow = None;
                return true;
            }
            return false;
        }
        if is_modifier(keyval) {
            self.toggle_press(keyval);
            return false;
        }
        self.toggle_armed = false;

        let alt = state & (MOD1 | META) != 0;
        let other_mods = state & (CONTROL | MOD4 | SUPER | HYPER) != 0;
        if self.cfg.toggle_key() == ToggleKey::AltZ && alt && !other_mods && matches!(keyval, KS_Z_LOWER | KS_Z_UPPER) {
            self.swallow = Some(keycode);
            self.toggle_vietnamese();
            return true;
        }

        if !self.engine.viet_key() || alt || other_mods || BYPASS_PURPOSES.contains(&self.purpose) {
            self.engine.reset();
            self.forget_text();
            return false;
        }

        if keyval == KS_BACKSPACE {
            let out = self.engine.process_backspace();
            if out.handled {
                self.edit(path, out.backs, &out.text);
                return true;
            }
            self.shown.pop(); // the application deletes it
            return false;
        }

        let c = match keyval {
            0x20..=0x7e => keyval as u8 as char,
            KS_KP_0..=KS_KP_9 => (b'0' + (keyval - KS_KP_0) as u8) as char,
            _ => {
                // Enter, Tab, arrows, function keys, non-ASCII letters ...: they end the word.
                self.engine.reset();
                self.forget_text();
                self.after_escape = keyval == KS_ESCAPE && self.in_terminal();
                return false;
            }
        };
        if !self.prepare_char(c) {
            self.shown.push(c);
            return false; // typed as is, by the application
        }
        let shift = state & SHIFT != 0;
        self.engine.set_caps_state(shift, state & LOCK != 0);
        // Shift+Space restores the typed keys of the last word (UniKey's "restore" key).
        let restore = c == ' ' && shift && !self.engine.at_word_beginning();
        let out = if restore { self.engine.restore_key_strokes() } else { self.engine.process(c as u32) };
        if out.handled {
            self.edit(path, out.backs, &out.text);
            true
        } else {
            self.shown.push(c);
            false
        }
    }

    /// The editor rules of the Wayland frontend (see `App::prepare_char`): after
    /// Escape in a terminal the next letter is a Vim command, and Telex `w` right
    /// after punctuation stays `w` (`:w`, `/word`). False: type `c` as is.
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

    fn toggle_press(&mut self, sym: u32) {
        if self.cfg.toggle_key() == ToggleKey::CtrlShift && is_toggle_key(sym) {
            self.toggle_held.push(sym);
            let ctrl = self.toggle_held.iter().any(|&s| matches!(s, super::KS_CONTROL_L | super::KS_CONTROL_R));
            let shift = self.toggle_held.iter().any(|&s| matches!(s, super::KS_SHIFT_L | super::KS_SHIFT_R));
            self.toggle_armed = ctrl && shift;
        }
    }

    /// True when releasing `sym` completes a Ctrl+Shift tap.
    fn toggle_release(&mut self, sym: u32) -> bool {
        if !is_toggle_key(sym) {
            return false;
        }
        if let Some(i) = self.toggle_held.iter().position(|&s| s == sym) {
            self.toggle_held.remove(i);
        }
        std::mem::take(&mut self.toggle_armed)
    }

    // ---- mode -----------------------------------------------------------------------

    fn toggle_vietnamese(&mut self) {
        self.set_vietnamese(!self.engine.viet_key());
    }

    fn set_vietnamese(&mut self, on: bool) {
        log!("ibus: {}", if on { "VI" } else { "EN" });
        self.engine.set_viet_key(on);
        self.engine.reset();
        self.forget_text();
        self.cfg.publish_mode(on);
        for path in self.objects.clone() {
            let mut msg = self.signal(&path, "UpdateProperty");
            self.append_mode_prop(&mut IterAppend::new(&mut msg));
            self.send(msg);
        }
    }

    /// The "InputMode" property: GNOME Shell shows its symbol (VI/EN) in the top
    /// bar and its switch in the input source menu.
    fn append_mode_prop(&self, ia: &mut IterAppend) {
        let on = self.engine.viet_key();
        let hint = match self.cfg.toggle_key() {
            ToggleKey::CtrlShift => "Ctrl+Shift",
            ToggleKey::AltZ => "Alt+Z",
            ToggleKey::None => "",
        };
        append_prop(ia, "InputMode", PROP_TOGGLE, "Gõ tiếng Việt", hint, on as u32, if on { "VI" } else { "EN" });
    }

    fn register_properties(&self, path: &str) {
        let mut msg = self.signal(path, "RegisterProperties");
        append_prop_list(&mut IterAppend::new(&mut msg), |ia| {
            self.append_mode_prop(ia);
            append_prop(ia, "Settings", PROP_NORMAL, "Cài đặt…", "", 0, "");
        });
        self.send(msg);
    }

    fn on_signal(&mut self, sig: i32) {
        match sig {
            libc::SIGUSR1 => {
                log!("reloading settings");
                self.cfg = Config::load();
                self.cfg.apply(&mut self.engine);
                self.toggle_held.clear();
                self.toggle_armed = false;
                let paths = self.objects.clone();
                paths.iter().for_each(|p| self.register_properties(p));
            }
            libc::SIGUSR2 => self.toggle_vietnamese(),
            _ => {}
        }
    }

    // ---- D-Bus ----------------------------------------------------------------------

    fn handle(&mut self, msg: Message) {
        if msg.msg_type() != MessageType::MethodCall {
            return; // NameAcquired and the like
        }
        let path = msg.path().map(|p| p.to_string()).unwrap_or_default();
        let iface = msg.interface().map(|i| i.to_string()).unwrap_or_default();
        let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
        let reply = match (iface.as_str(), member.as_str()) {
            (IFACE_FACTORY, "CreateEngine") => {
                let name: &str = msg.read1().unwrap_or_default();
                let object = format!("/org/freedesktop/IBus/Engine/{}", self.next_id);
                self.next_id += 1;
                log!("ibus: create engine {name:?} at {object}");
                self.objects.push(object.clone());
                msg.method_return().append1(dbus::Path::from(object))
            }
            (IFACE_SERVICE, "Destroy") => {
                self.objects.retain(|o| *o != path);
                msg.method_return()
            }
            (IFACE_ENGINE, "ProcessKeyEvent") => {
                let (keyval, keycode, state) = msg.read3::<u32, u32, u32>().unwrap_or_default();
                let done = self.process_key(&path, keyval, keycode, state);
                msg.method_return().append1(done)
            }
            (IFACE_ENGINE, "FocusIn" | "FocusInId") => {
                log!("ibus: focus in");
                self.engine.reset();
                self.forget_text();
                self.before = None;
                self.after_escape = false;
                self.last_char = None;
                // Ask the application for its text, so deletions can be atomic.
                self.send(self.signal(&path, "RequireSurroundingText"));
                self.register_properties(&path);
                msg.method_return()
            }
            (IFACE_ENGINE, "FocusOut" | "FocusOutId" | "Reset" | "Disable") => {
                log!("ibus: {member}");
                self.engine.reset();
                self.forget_text();
                self.toggle_held.clear();
                self.toggle_armed = false;
                msg.method_return()
            }
            (IFACE_ENGINE, "Enable") => {
                self.send(self.signal(&path, "RequireSurroundingText"));
                msg.method_return()
            }
            (IFACE_ENGINE, "SetCapabilities") => {
                self.caps = msg.read1().unwrap_or(0);
                log!("ibus: capabilities {:#x}", self.caps);
                msg.method_return()
            }
            (IFACE_ENGINE, "SetSurroundingText") => {
                let mut it = msg.iter_init();
                let text = it.get::<Variant<Box<dyn RefArg>>>().and_then(|v| ibus_text(&*v.0));
                it.next();
                let cursor: u32 = it.read().unwrap_or(0);
                let anchor: u32 = it.read().unwrap_or(cursor);
                self.before = text.filter(|_| cursor == anchor).map(|t| t.chars().take(cursor as usize).collect());
                log!("ibus: before cursor {:?}", self.before);
                msg.method_return()
            }
            (IFACE_ENGINE, "PropertyActivate") => {
                let (name, state) = msg.read2::<&str, u32>().unwrap_or_default();
                match name {
                    "InputMode" => self.set_vietnamese(state == PROP_CHECKED),
                    "Settings" => super::tray::open_settings(),
                    _ => {}
                }
                msg.method_return()
            }
            (IFACE_PROPERTIES, "Get") => {
                let (_, name) = msg.read2::<&str, &str>().unwrap_or_default();
                match engine_property(name) {
                    Some(value) => msg.method_return().append1(Variant(value)),
                    None => msg.error(&"org.freedesktop.DBus.Error.UnknownProperty".into(), c"no such property"),
                }
            }
            (IFACE_PROPERTIES, "GetAll") => {
                let all: HashMap<&str, Variant<bool>> =
                    ["FocusId", "ActiveSurroundingText"].map(|n| (n, Variant(engine_property(n).unwrap()))).into();
                msg.method_return().append1(all)
            }
            (IFACE_PROPERTIES, "Set") => {
                let (_, name) = msg.read2::<&str, &str>().unwrap_or_default();
                if name == "ContentType" {
                    let mut it = msg.iter_init();
                    it.next();
                    it.next();
                    if let Some(v) = it.get::<Variant<Box<dyn RefArg>>>() {
                        if let Some(mut fields) = v.0.as_iter() {
                            self.purpose = fields.next().and_then(|p| p.as_u64()).unwrap_or(0) as u32;
                            log!("ibus: content purpose {}", self.purpose);
                        }
                    }
                }
                msg.method_return()
            }
            (IFACE_INTROSPECTABLE, "Introspect") => msg.method_return().append1(INTROSPECTION),
            // Cursor location, candidate list, panel extensions ...: nothing to do.
            _ => msg.method_return(),
        };
        self.send(reply);
    }
}

/// The engine properties ibus-daemon reads when it creates the engine.
fn engine_property(name: &str) -> Option<bool> {
    match name {
        // Plain FocusIn/FocusOut calls, not the ones with a client id.
        "FocusId" => Some(false),
        // Applications should always report their text (see `can_delete_surrounding`).
        "ActiveSurroundingText" => Some(true),
        _ => None,
    }
}

const INTROSPECTION: &str = r#"<node>
  <interface name="org.freedesktop.IBus.Engine"/>
  <interface name="org.freedesktop.IBus.Service"/>
</node>"#;

// ---- IBus serializables ---------------------------------------------------------------

/// An IBusText: ("IBusText", {}, text, IBusAttrList).
fn append_text(ia: &mut IterAppend, text: &str) {
    ia.append_variant(&Signature::from("(sa{sv}sv)"), |ia| {
        ia.append_struct(|ia| {
            ia.append("IBusText");
            append_attachments(ia);
            ia.append(text);
            ia.append_variant(&Signature::from("(sa{sv}av)"), |ia| {
                ia.append_struct(|ia| {
                    ia.append("IBusAttrList");
                    append_attachments(ia);
                    ia.append_array(&Signature::from("v"), |_| {});
                })
            });
        })
    });
}

/// An IBusProperty: key, type, label, icon, tooltip, sensitive, visible, state,
/// sub-properties, symbol.
fn append_prop(ia: &mut IterAppend, key: &str, kind: u32, label: &str, tooltip: &str, state: u32, symbol: &str) {
    ia.append_variant(&Signature::from("(sa{sv}suvsvbbuvv)"), |ia| {
        ia.append_struct(|ia| {
            ia.append("IBusProperty");
            append_attachments(ia);
            ia.append(key);
            ia.append(kind);
            append_text(ia, label);
            ia.append("");
            append_text(ia, tooltip);
            ia.append(true);
            ia.append(true);
            ia.append(state);
            append_prop_list(ia, |_| {});
            append_text(ia, symbol);
        })
    });
}

/// An IBusPropList holding what `props` appends.
fn append_prop_list(ia: &mut IterAppend, props: impl FnOnce(&mut IterAppend)) {
    ia.append_variant(&Signature::from("(sa{sv}av)"), |ia| {
        ia.append_struct(|ia| {
            ia.append("IBusPropList");
            append_attachments(ia);
            ia.append_array(&Signature::from("v"), props);
        })
    });
}

fn append_attachments(ia: &mut IterAppend) {
    ia.append_dict(&Signature::from("s"), &Signature::from("v"), |_| {});
}

/// The string inside an IBusText.
fn ibus_text(v: &dyn RefArg) -> Option<String> {
    let mut fields = v.as_iter()?;
    (fields.next()?.as_str()? == "IBusText").then_some(())?;
    fields.next()?; // attachments
    Some(fields.next()?.as_str()?.to_owned())
}

// ---- running ------------------------------------------------------------------------

/// Serve as the IBus engine until ibus-daemon goes away.
pub fn run(sig_fd: i32) -> ! {
    let Some(address) = bus_address() else {
        super::die("cannot find the IBus daemon's address (is ibus running?)");
    };
    log!("ibus: connecting to {address}");
    let mut ch = Channel::open_private(&address)
        .and_then(|mut ch| ch.register().map(|_| ch))
        .unwrap_or_else(|e| super::die(&format!("cannot connect to IBus: {}", e.message().unwrap_or("?"))));
    // ibus-daemon started us and waits for the component's name before it asks
    // the factory for an engine.
    let request = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName")
        .unwrap()
        .append2(COMPONENT, 0u32);
    if let Err(e) = ch.send_with_reply_and_block(request, Duration::from_secs(5)) {
        super::die(&format!("cannot take the IBus name {COMPONENT}: {}", e.message().unwrap_or("?")));
    }
    ch.set_watch_enabled(true);
    let mut ime = Ime::new(ch, Config::load());
    loop {
        while let Some(msg) = ime.ch.pop_message() {
            ime.handle(msg);
        }
        ime.ch.flush();
        if !ime.ch.is_connected() {
            log!("ibus: daemon gone");
            std::process::exit(0);
        }
        let mut fds = [
            libc::pollfd { fd: ime.ch.watch().fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: sig_fd, events: libc::POLLIN, revents: 0 },
        ];
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        for sig in pending_signals(sig_fd) {
            ime.on_signal(sig);
        }
        if ime.ch.read_write(Some(Duration::ZERO)).is_err() {
            log!("ibus: connection lost");
            std::process::exit(0);
        }
    }
}

/// The address of the running ibus-daemon, found the way libibus finds it.
fn bus_address() -> Option<String> {
    if let Some(a) = env::var("IBUS_ADDRESS").ok().filter(|a| !a.is_empty()) {
        return Some(a);
    }
    let file = env::var_os("IBUS_ADDRESS_FILE").map(PathBuf::from).unwrap_or_else(address_file);
    read_address(&file).or_else(|| {
        // Started from somewhere else than the daemon's session: the newest live one.
        let mut files: Vec<_> = fs::read_dir(config_home().join("ibus/bus"))
            .ok()?
            .filter_map(|e| {
                let e = e.ok()?;
                Some((e.metadata().ok()?.modified().ok()?, e.path()))
            })
            .collect();
        files.sort();
        files.iter().rev().find_map(|(_, path)| read_address(path))
    })
}

/// `~/.config/ibus/bus/<machine id>-<host>-<display>`, as in `ibus_get_socket_path`.
fn address_file() -> PathBuf {
    let (host, display) = match env::var("WAYLAND_DISPLAY").ok().filter(|d| !d.is_empty()) {
        Some(wayland) => ("unix".to_owned(), wayland),
        None => {
            let x = env::var("DISPLAY").unwrap_or_default();
            let (host, rest) = x.split_once(':').unwrap_or(("", "0"));
            let number = rest.split('.').next().unwrap_or("0");
            let host = if host.is_empty() { "unix" } else { host };
            (host.to_owned(), number.to_owned())
        }
    };
    let machine_id = ["/var/lib/dbus/machine-id", "/etc/machine-id"]
        .iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();
    config_home().join("ibus/bus").join(format!("{}-{host}-{display}", machine_id.trim()))
}

/// IBUS_ADDRESS from an address file, if the daemon that wrote it still runs.
fn read_address(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
    let pid = value("IBUS_DAEMON_PID=")?;
    if !Path::new("/proc").join(pid).exists() {
        return None;
    }
    value("IBUS_ADDRESS=").filter(|a| !a.is_empty()).map(str::to_owned)
}

fn config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".config"))
}

// ---- installing ---------------------------------------------------------------------

/// The command that starts unikey: the AppImage itself when run from one.
fn launcher() -> String {
    env::var("APPIMAGE")
        .ok()
        .filter(|a| !a.is_empty())
        .or_else(|| env::current_exe().ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| "unikey".to_owned())
}

/// The command that opens the settings window.
fn settings_command() -> String {
    if let Some(appimage) = env::var("APPIMAGE").ok().filter(|a| !a.is_empty()) {
        return format!("{} settings", shell_quote(&appimage));
    }
    let sibling = env::current_exe().ok().and_then(|p| Some(p.parent()?.join("unikey-settings")));
    shell_quote(&sibling.filter(|p| p.exists()).map_or("unikey-settings".to_owned(), |p| p.display().to_string()))
}

fn shell_quote(s: &str) -> String {
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+".contains(&b)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The IBus component file: who to start, and the engine it provides.
pub fn component_xml(exec: &str, setup: &str) -> String {
    let (exec, setup) = (xml_escape(exec), xml_escape(setup));
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!-- Written by `unikey ibus-install`. -->
<component>
  <name>{COMPONENT}</name>
  <description>UniKey Vietnamese input method</description>
  <exec>{exec} --ibus</exec>
  <version>{version}</version>
  <license>GPL-3.0-or-later</license>
  <homepage>https://github.com/bhhoang/unikey-rs</homepage>
  <textdomain>unikey</textdomain>
  <engines>
    <engine>
      <name>{ENGINE}</name>
      <longname>UniKey</longname>
      <description>Vietnamese typing: Telex, VNI, VIQR (unikey-rs)</description>
      <language>vi</language>
      <license>GPL-3.0-or-later</license>
      <icon>input-keyboard</icon>
      <layout>us</layout>
      <rank>99</rank>
      <symbol>VI</symbol>
      <setup>{setup}</setup>
    </engine>
  </engines>
</component>
"#
    )
}

/// `unikey ibus-install`: register unikey with IBus (GNOME's input method
/// framework) and add it to GNOME's input sources.
pub fn install() {
    let xml = component_xml(&shell_quote(&launcher()), &settings_command());
    let target = settings::IBUS_COMPONENT;
    let written = if unsafe { libc::geteuid() } == 0 {
        fs::create_dir_all(Path::new(target).parent().unwrap()).and_then(|_| fs::write(target, &xml)).is_ok()
    } else {
        let tmp = env::temp_dir().join(format!("unikey-ibus-{}.xml", std::process::id()));
        let ok = fs::write(&tmp, &xml).is_ok() && {
            println!("Installing {target} (needs administrator rights):");
            Command::new("sudo")
                .arg("install")
                .arg("-Dm644")
                .arg(&tmp)
                .arg(target)
                .status()
                .is_ok_and(|s| s.success())
        };
        let _ = fs::remove_file(&tmp);
        ok
    };
    if !written {
        eprintln!("unikey: could not write {target}. Save this as that file by hand:\n\n{xml}");
        std::process::exit(1);
    }
    println!("Installed {target}.");

    // A running ibus-daemon reads its component files at start.
    if Command::new("ibus").arg("restart").output().is_ok_and(|o| o.status.success()) {
        println!("Restarted IBus.");
    } else {
        println!("Restart IBus (or log out and back in) so it finds UniKey.");
    }

    if settings::is_gnome() {
        match add_gnome_input_source() {
            Ok(true) => println!("Added \"Vietnamese (UniKey)\" to Settings → Keyboard → Input Sources."),
            Ok(false) => println!("UniKey is already in Settings → Keyboard → Input Sources."),
            Err(()) => println!("Add \"Vietnamese (UniKey)\" in Settings → Keyboard → Input Sources."),
        }
        println!("Switch to it with Super+Space. Ctrl+Shift (or Alt+Z) toggles VI/EN inside it.");
        println!("unikey no longer needs to be in your startup applications: IBus starts it.");
    } else {
        println!("Select \"UniKey\" in your IBus input methods (ibus-setup).");
    }
}

/// Append ('ibus', ENGINE) to GNOME's input sources. Ok(false): already there.
fn add_gnome_input_source() -> Result<bool, ()> {
    const SCHEMA: &str = "org.gnome.desktop.input-sources";
    let out = Command::new("gsettings").args(["get", SCHEMA, "sources"]).output().map_err(|_| ())?;
    if !out.status.success() {
        return Err(());
    }
    let current = String::from_utf8_lossy(&out.stdout);
    let Some(sources) = add_source(&current) else { return Ok(false) };
    let set = Command::new("gsettings").args(["set", SCHEMA, "sources", &sources]).status().map_err(|_| ())?;
    if set.success() { Ok(true) } else { Err(()) }
}

/// `sources` (gsettings' print of an `a(ss)`) with unikey appended, or `None`
/// if it is there already.
fn add_source(sources: &str) -> Option<String> {
    let entry = format!("('ibus', '{ENGINE}')");
    if sources.contains(&format!("'{ENGINE}'")) {
        return None;
    }
    let list = sources.trim().trim_start_matches("@a(ss)").trim();
    let inner = list.strip_prefix('[').and_then(|l| l.strip_suffix(']')).unwrap_or("").trim();
    Some(if inner.is_empty() {
        // GNOME's default (no sources) is the US layout: keep it first.
        format!("[('xkb', 'us'), {entry}]")
    } else {
        format!("[{inner}, {entry}]")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_source_is_appended_once() {
        assert_eq!(add_source("[('xkb', 'us')]\n").as_deref(), Some("[('xkb', 'us'), ('ibus', 'unikey-rs')]"));
        assert_eq!(add_source("@a(ss) []\n").as_deref(), Some("[('xkb', 'us'), ('ibus', 'unikey-rs')]"));
        assert_eq!(add_source("[('xkb', 'us'), ('ibus', 'unikey-rs')]"), None);
    }

    #[test]
    fn component_file_quotes_odd_paths() {
        let xml = component_xml(&shell_quote("/home/a b/unikey"), "unikey-settings");
        assert!(xml.contains("<exec>'/home/a b/unikey' --ibus</exec>"), "{xml}");
        assert!(xml.contains(&format!("<name>{ENGINE}</name>")));
        assert_eq!(shell_quote("/usr/bin/unikey"), "/usr/bin/unikey");
        assert_eq!(xml_escape("a&b<c"), "a&amp;b&lt;c");
    }
}
