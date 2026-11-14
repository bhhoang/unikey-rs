//! Applying the user's settings (see `unikey::settings`) to the engine, and
//! publishing whether Vietnamese typing is on.

use std::{
    fs,
    process::{Command, Stdio},
};

use unikey::{Engine, InputMethod, settings::{self, Settings, ToggleKey}};

pub struct Config {
    settings: Settings,
}

impl Config {
    pub fn load() -> Self {
        Config { settings: Settings::load() }
    }

    pub fn per_app_mode(&self) -> bool {
        self.settings.per_app_mode
    }

    pub fn toggle_key(&self) -> ToggleKey {
        self.settings.toggle_key
    }

    pub fn apply(&self, engine: &mut Engine) {
        let s = &self.settings;
        engine.set_options(s.options);
        if s.options.macro_enabled {
            let path = settings::macro_file();
            if let Err(e) = engine.load_macro_table(&path) {
                eprintln!("unikey: macro table {}: {e}", path.display());
            }
        }
        if s.input_method == InputMethod::UsrIM && !engine.load_user_key_map(&settings::keymap_file()) {
            eprintln!("unikey: falling back to Telex");
            engine.set_input_method(InputMethod::Telex);
            return;
        }
        engine.set_input_method(s.input_method);
    }

    /// Record whether Vietnamese typing is on ("VI") or off ("EN"), and nudge
    /// Waybar to redraw if `bar_signal` is set (custom module with that `"signal"`).
    pub fn publish_mode(&self, vietnamese: bool) {
        let file = settings::mode_file();
        if let Some(parent) = file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&file, if vietnamese { "VI\n" } else { "EN\n" });
        let n = self.settings.bar_signal;
        if n == 0 {
            return;
        }
        let _ = Command::new("pkill")
            .args([format!("-RTMIN+{n}").as_str(), "-x", "waybar"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|mut c| c.wait());
    }
}
