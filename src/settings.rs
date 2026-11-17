//! User settings, shared by the input method and the settings app.
//!
//! `~/.config/unikey/config`, one `key = value` per line, `#` comments:
//!
//! ```text
//! input_method = telex      # telex | vni | viqr | msvi | simple-telex | simple-telex2 | user
//! free_marking = true       # put marks anywhere in the word, not only at its end
//! modern_style = false      # tone placement: hoà (modern) instead of hòa (classic)
//! spell_check  = true
//! auto_restore = true       # give back the typed keys for non-Vietnamese words
//! macro        = false      # expand ~/.config/unikey/macro
//! macro_case   = true       # macro text follows the typed case (vn/Vn/VN)
//! toggle_key   = ctrl+shift # VI/EN switch: ctrl+shift | alt+z | none
//! per_app_mode = false      # remember VI/EN for each application
//! bar_signal   = 0          # on VI/EN change send waybar SIGRTMIN+N (0 = off)
//! ```
//! A user key layout for `input_method = user` is read from `~/.config/unikey/keymap`.
//!
//! The running input method reloads this file on SIGUSR1 and toggles Vietnamese
//! typing on SIGUSR2. The current mode is shown as a tray icon and written as
//! "VI"/"EN" to `~/.local/state/unikey/mode`, for status bars and scripts.

use std::{fs, io, path::PathBuf};

use crate::{InputMethod, Options};

/// Shortcut that switches Vietnamese typing on and off (UniKey's two choices).
/// `None` leaves it to the user, e.g. a Hyprland bind to `pkill -USR2 -x unikey`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToggleKey {
    CtrlShift,
    AltZ,
    None,
}

/// Config-file names of the toggle keys, in display order.
pub const TOGGLE_KEYS: [(ToggleKey, &str); 3] =
    [(ToggleKey::CtrlShift, "ctrl+shift"), (ToggleKey::AltZ, "alt+z"), (ToggleKey::None, "none")];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub input_method: InputMethod,
    pub options: Options,
    pub toggle_key: ToggleKey,
    /// Remember VI/EN per application and restore it on focus (UniKey 4.6).
    pub per_app_mode: bool,
    /// Real-time signal number sent to Waybar when VI/EN changes (0 = none).
    pub bar_signal: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { input_method: InputMethod::Telex, options: Options::default(), toggle_key: ToggleKey::CtrlShift, per_app_mode: false, bar_signal: 0 }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("unikey")
}

pub fn macro_file() -> PathBuf {
    config_dir().join("macro")
}

pub fn keymap_file() -> PathBuf {
    config_dir().join("keymap")
}

/// "VI" or "EN": whether Vietnamese typing is on.
pub fn mode_file() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("unikey/mode")
}

/// Config-file names of the input methods, in display order.
pub const INPUT_METHODS: [(InputMethod, &str); 7] = [
    (InputMethod::Telex, "telex"),
    (InputMethod::Vni, "vni"),
    (InputMethod::Viqr, "viqr"),
    (InputMethod::MsVi, "msvi"),
    (InputMethod::SimpleTelex, "simple-telex"),
    (InputMethod::SimpleTelex2, "simple-telex2"),
    (InputMethod::UsrIM, "user"),
];

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

impl Settings {
    /// Read the config file; missing file or bad lines fall back to defaults
    /// (bad lines are reported on stderr).
    pub fn load() -> Self {
        let mut s = Settings::default();
        let Ok(text) = fs::read_to_string(config_dir().join("config")) else { return s };
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let o = &mut s.options;
            let ok = match k {
                "input_method" => {
                    INPUT_METHODS.iter().find(|(_, name)| *name == v).map(|(im, _)| s.input_method = *im).is_some()
                }
                "toggle_key" => {
                    TOGGLE_KEYS.iter().find(|(_, name)| *name == v).map(|(k, _)| s.toggle_key = *k).is_some()
                }
                "bar_signal" => v.parse().ok().filter(|n: &u8| *n <= 30).map(|n| s.bar_signal = n).is_some(),
                "per_app_mode" => parse_bool(v).map(|b| s.per_app_mode = b).is_some(),
                "free_marking" => parse_bool(v).map(|b| o.free_marking = b).is_some(),
                "modern_style" => parse_bool(v).map(|b| o.modern_style = b).is_some(),
                "spell_check" => parse_bool(v).map(|b| o.spell_check_enabled = b).is_some(),
                "auto_restore" => parse_bool(v).map(|b| o.auto_non_vn_restore = b).is_some(),
                "macro" => parse_bool(v).map(|b| o.macro_enabled = b).is_some(),
                "macro_case" => parse_bool(v).map(|b| o.macro_auto_case = b).is_some(),
                _ => false,
            };
            if !ok {
                eprintln!("unikey: config line {}: ignored `{line}`", n + 1);
            }
        }
        s
    }

    pub fn save(&self) -> io::Result<()> {
        let im = INPUT_METHODS.iter().find(|(im, _)| *im == self.input_method).map_or("telex", |(_, n)| n);
        let toggle = TOGGLE_KEYS.iter().find(|(k, _)| *k == self.toggle_key).map_or("ctrl+shift", |(_, n)| n);
        let o = &self.options;
        let text = format!(
            "# UniKey settings (also edited by unikey-settings)\n\
             input_method = {im}\n\
             free_marking = {}\n\
             modern_style = {}\n\
             spell_check  = {}\n\
             auto_restore = {}\n\
             macro        = {}\n\
             macro_case   = {}\n\
             toggle_key   = {toggle}\n\
             per_app_mode = {}\n\
             bar_signal   = {}\n",
            o.free_marking, o.modern_style, o.spell_check_enabled, o.auto_non_vn_restore, o.macro_enabled, o.macro_auto_case, self.per_app_mode,
            self.bar_signal
        );
        fs::create_dir_all(config_dir())?;
        fs::write(config_dir().join("config"), text)
    }
}

/// The IBus component file `unikey ibus-install` writes (see `ibus.rs`). IBus
/// only reads components from this system directory.
pub const IBUS_COMPONENT: &str = "/usr/share/ibus/component/unikey-rs.xml";

/// A GNOME session: no input method protocol on Wayland, only IBus.
pub fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("GNOME")))
}

/// On GNOME with `unikey ibus-install` done, IBus starts unikey when needed.
pub fn runs_in_ibus() -> bool {
    is_gnome() && std::path::Path::new(IBUS_COMPONENT).exists()
}

/// Process ids of running `unikey` input methods.
pub fn daemon_pids() -> Vec<i32> {
    let Ok(dir) = fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(|e| {
        let e = e.ok()?;
        let pid: i32 = e.file_name().to_str()?.parse().ok()?;
        (fs::read_to_string(e.path().join("comm")).ok()?.trim() == "unikey").then_some(pid)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("unikey-settings-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let mut s = Settings::default();
        s.input_method = InputMethod::Vni;
        s.options.modern_style = true;
        s.options.macro_enabled = true;
        s.options.macro_auto_case = false;
        s.toggle_key = ToggleKey::AltZ;
        s.per_app_mode = true;
        s.bar_signal = 8;
        s.save().unwrap();
        assert_eq!(Settings::load(), s);
        fs::remove_dir_all(&dir).ok();
    }
}
