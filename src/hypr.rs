//! Hyprland IPC: which window has keyboard focus.
//!
//! Needed because the compositor does not always deactivate the input method
//! when focus leaves the text field's window (for example when that window is
//! closed). A stale activation would keep grabbing the keyboard and swallow
//! keys meant for other windows, including X11 ones.

use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
};

fn socket(name: &str) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(PathBuf::from(runtime).join("hypr").join(instance).join(name))
}

/// Strip the optional `0x` so addresses from both sockets compare equal.
fn norm(addr: &str) -> String {
    addr.trim().trim_start_matches("0x").to_owned()
}

/// The focused window's address and whether it is an X11 (XWayland) window.
pub fn active_window() -> Option<(String, bool)> {
    let mut s = UnixStream::connect(socket(".socket.sock")?).ok()?;
    s.write_all(b"j/activewindow").ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let value = |key: &str| {
        let rest = &out[out.find(&format!("\"{key}\":"))? + key.len() + 3..];
        Some(rest.trim_start().split([',', '\n', '}']).next()?.trim().trim_matches('"').to_owned())
    };
    Some((norm(&value("address")?), value("xwayland")? == "true"))
}

/// Focus-change notifications (Hyprland's event socket).
pub struct Events {
    stream: UnixStream,
    buf: Vec<u8>,
    /// Class from the last `activewindow` event; Hyprland sends it just before
    /// the matching `activewindowv2` (address) event.
    class: String,
}

impl Events {
    pub fn connect() -> Option<Self> {
        let stream = UnixStream::connect(socket(".socket2.sock")?).ok()?;
        stream.set_nonblocking(true).ok()?;
        Some(Events { stream, buf: Vec::new(), class: String::new() })
    }

    pub fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// (address, class) of windows that got focus since the last call
    /// ("" = none). `None` when the socket closed.
    pub fn focus_changes(&mut self) -> Option<Vec<(String, String)>> {
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return None,
            }
        }
        let found = take_focus_events(&mut self.buf, &mut self.class);
        Some(found)
    }
}

/// Consume complete lines of `buf`; return (address, class) for each focus change.
fn take_focus_events(buf: &mut Vec<u8>, class: &mut String) -> Vec<(String, String)> {
    let mut found = Vec::new();
    while let Some(end) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=end).collect();
        let line = String::from_utf8_lossy(&line);
        if let Some(rest) = line.strip_prefix("activewindow>>") {
            *class = rest.split(',').next().unwrap_or("").trim().to_owned();
        } else if let Some(addr) = line.strip_prefix("activewindowv2>>") {
            let addr = if addr.trim() == "," { String::new() } else { norm(addr) };
            found.push((addr, class.clone()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{norm, take_focus_events};

    #[test]
    fn pairs_each_focus_change_with_its_class() {
        let mut buf = b"activewindow>>zenity,uktest\nworkspace>>2\nactivewindowv2>>55e3d07944d0\nactivewindow>>org.wezfurlong.wezterm,nvim\nactivewindowv2>>0x1234\nactivewindow>>,\nactivewindowv2>>,\nactivewindow>>partial".to_vec();
        let mut class = String::new();
        let got = take_focus_events(&mut buf, &mut class);
        assert_eq!(
            got,
            vec![
                ("55e3d07944d0".into(), "zenity".into()),
                ("1234".into(), "org.wezfurlong.wezterm".into()),
                ("".into(), "".into()),
            ]
        );
        assert_eq!(buf, b"activewindow>>partial"); // incomplete line kept for later
    }

    #[test]
    fn addresses_from_both_sockets_match() {
        assert_eq!(norm("0x55e3d07944d0"), norm("55e3d07944d0\n"));
    }
}
