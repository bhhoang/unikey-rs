//! Types a string through a Wayland virtual keyboard (US layout), like `wtype`.
//! Used to test the input method end to end:  inject "Vieejt Nam" [--enter] [--delay-ms N]
use std::{io::Write, os::fd::AsFd, thread, time::Duration};

use wayland_client::{Connection, Dispatch, QueueHandle, protocol::{wl_registry, wl_seat}};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon::xkb;

const ROWS: [(&str, &str, u32); 6] = [
    ("1234567890-=", "!@#$%^&*()_+", 2),
    ("qwertyuiop[]", "QWERTYUIOP{}", 16),
    ("asdfghjkl;'", "ASDFGHJKL:\"", 30),
    ("`", "~", 41),
    ("\\", "|", 43),
    ("zxcvbnm,./", "ZXCVBNM<>?", 44),
];

fn lookup(c: char) -> Option<(u32, bool)> {
    match c {
        ' ' => return Some((57, false)),
        '\u{8}' => return Some((14, false)), // BackSpace
        '\n' => return Some((28, false)),    // Return
        '\u{1b}' => return Some((1, false)), // Escape
        '\u{1}' => return Some((29, false)), // marker: tap Ctrl+Shift (see below)
        '\u{2}' => return Some((44, false)), // marker: Alt+Z (see below)
        _ => {}
    }
    for (plain, shifted, base) in ROWS {
        if let Some(i) = plain.chars().position(|p| p == c) {
            return Some((base + i as u32, false));
        }
        if let Some(i) = shifted.chars().position(|p| p == c) {
            return Some((base + i as u32, true));
        }
    }
    None
}

#[derive(Default)]
struct S {
    seat: Option<wl_seat::WlSeat>,
    mgr: Option<ZwpVirtualKeyboardManagerV1>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for S {
    fn event(s: &mut Self, r: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = e {
            match interface.as_str() {
                "wl_seat" if s.seat.is_none() => s.seat = Some(r.bind(name, 1, qh, ())),
                "zwp_virtual_keyboard_manager_v1" => s.mgr = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}
wayland_client::delegate_noop!(S: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(S: ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(S: ZwpVirtualKeyboardV1);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let enter = args.iter().any(|a| a == "--enter");
    let delay = args.iter().position(|a| a == "--delay-ms").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(40);
    let text = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--delay-ms"))
        .map(|(_, a)| a.clone())
        .unwrap_or_default();

    let conn = Connection::connect_to_env().unwrap();
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut s = S::default();
    q.roundtrip(&mut s).unwrap();
    let vk = s.mgr.as_ref().unwrap().create_virtual_keyboard(s.seat.as_ref().unwrap(), &qh, ());

    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let km = xkb::Keymap::new_from_names(&ctx, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS).unwrap();
    let keymap = km.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let fd = unsafe { libc::memfd_create(c"keymap".as_ptr(), 0) };
    let mut f = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    f.write_all(keymap.as_bytes()).unwrap();
    f.write_all(&[0]).unwrap();
    vk.keymap(1, f.as_fd(), keymap.len() as u32 + 1);
    q.roundtrip(&mut s).unwrap();

    let t = std::cell::Cell::new(0u32);
    let tick = || {
        t.set(t.get() + delay as u32);
        t.get()
    };
    let press = |code: u32, shift: bool| {
        if shift {
            vk.key(tick(), 42, 1); // a real Shift key, as a keyboard sends it
            vk.modifiers(1, 0, 0, 0);
        }
        let now = tick();
        vk.key(now, code, 1);
        vk.key(now + 5, code, 0);
        if shift {
            vk.key(tick(), 42, 0);
            vk.modifiers(0, 0, 0, 0);
        }
        let _ = conn.flush();
        thread::sleep(Duration::from_millis(delay));
    };
    for c in text.chars() {
        // \x03 tap Caps Lock, \x04 hold Shift, \x05 release Shift, \x06 wait 1.5 s
        // (for changing focus meanwhile): modifier state across focus changes.
        let step = |code: u32, down: bool, depressed: u32, locked: u32| {
            vk.key(tick(), code, down as u32);
            vk.modifiers(depressed, 0, locked, 0);
            let _ = conn.flush();
            thread::sleep(Duration::from_millis(delay));
        };
        match c {
            '\u{3}' => {
                step(58, true, 0, 2);
                step(58, false, 0, 2);
                continue;
            }
            '\u{4}' => {
                step(42, true, 1, 0);
                continue;
            }
            '\u{5}' => {
                step(42, false, 0, 0);
                continue;
            }
            '\u{6}' => {
                thread::sleep(Duration::from_millis(1500));
                continue;
            }
            _ => {}
        }
        if c == '\u{2}' {
            for (code, mods, down) in [(56, 8, true), (44, 8, true), (44, 8, false), (56, 0, false)] {
                vk.key(tick(), code, down as u32);
                vk.modifiers(mods, 0, 0, 0);
                let _ = conn.flush();
                thread::sleep(Duration::from_millis(delay));
            }
            continue;
        }
        if c == '\u{1}' {
            // Ctrl+Shift tapped on its own: both down, then both up.
            for (code, mods, down) in [(29, 4, true), (42, 5, true), (42, 4, false), (29, 0, false)] {
                vk.key(tick(), code, down as u32);
                vk.modifiers(mods, 0, 0, 0);
                let _ = conn.flush();
                thread::sleep(Duration::from_millis(delay));
            }
            continue;
        }
        match lookup(c) {
            Some((code, shift)) => press(code, shift),
            None => eprintln!("inject: no key for {c:?}"),
        }
    }
    if enter {
        press(28, false);
    }
    q.roundtrip(&mut s).unwrap();
}
