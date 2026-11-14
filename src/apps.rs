//! How each application takes our corrections, learned by watching it.
//!
//! `Atomic`: it applies "delete N characters + insert text" in one step
//! (GTK, Qt): no flicker. `Keys`: it ignores deletions (Chromium, Electron), so
//! we send Backspace key presses instead. Unknown applications are probed on
//! their first correction; the answer is kept in `~/.local/state/unikey/apps`.

use std::{collections::HashMap, fs, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Atomic,
    Keys,
}

pub struct Learned {
    path: PathBuf,
    map: HashMap<String, Mode>,
}

impl Learned {
    pub fn load() -> Self {
        let path = unikey::settings::mode_file().with_file_name("apps");
        let map = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (class, mode) = l.split_once('\t')?;
                let mode = match mode {
                    "atomic" => Mode::Atomic,
                    "keys" => Mode::Keys,
                    _ => return None,
                };
                Some((class.to_owned(), mode))
            })
            .collect();
        Learned { path, map }
    }

    pub fn get(&self, class: &str) -> Option<Mode> {
        self.map.get(class).copied()
    }

    pub fn set(&mut self, class: &str, mode: Mode) {
        if class.is_empty() || self.map.get(class) == Some(&mode) {
            return;
        }
        self.map.insert(class.to_owned(), mode);
        let mut lines: Vec<String> = self
            .map
            .iter()
            .map(|(c, m)| format!("{c}\t{}", if *m == Mode::Atomic { "atomic" } else { "keys" }))
            .collect();
        lines.sort();
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&self.path, lines.join("\n") + "\n");
    }
}

/// What a probe saw: the text before the cursor after an atomic edit that
/// turned `before` into `after` (both are what precedes the cursor).
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// The deletion was applied.
    Applied,
    /// The deletion was ignored, the insertion applied.
    Ignored,
    /// Neither (a stale report, or the user moved the cursor).
    Unclear,
}

pub fn judge(reported: &str, before: &str, after: &str, inserted: &str) -> Verdict {
    let applied = reported.ends_with(after);
    let ignored = reported.ends_with(&format!("{before}{inserted}"));
    match (applied, ignored) {
        (true, false) => Verdict::Applied,
        (false, true) => Verdict::Ignored,
        _ => Verdict::Unclear, // stale, cursor moved, or both fit (repeated letters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judges_probe_results() {
        // "Vie" + e: delete "e", insert "ê"
        assert_eq!(judge("Viê", "Vie", "Viê", "ê"), Verdict::Applied);
        assert_eq!(judge("Vieê", "Vie", "Viê", "ê"), Verdict::Ignored);
        assert_eq!(judge("Vi", "Vie", "Viê", "ê"), Verdict::Unclear); // stale report
        assert_eq!(judge("x", "Vie", "Viê", "ê"), Verdict::Unclear);
        // "a" + "a" -> "x": both outcomes end in "ax"
        assert_eq!(judge("aax", "a", "x", "x"), Verdict::Unclear);
    }
}
