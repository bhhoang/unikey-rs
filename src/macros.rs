//! Macro (text expansion) table, a port of `mactab.cpp`. Keys and texts are
//! stored as UniKey standard chars so keys compare against the engine's buffer.

use std::{fs, io, path::Path};

use crate::charset::{std_to_lower, unicode_to_std};

const MAX_MACRO_KEY_LEN: usize = 16;
const MAX_MACRO_TEXT_LEN: usize = 1024;
const MAX_MACRO_ITEMS: usize = 8192; // UniKey 4.3 raised the limit from 1024

#[derive(Default)]
pub struct MacroTable {
    items: Vec<(Vec<u32>, Vec<u32>)>,
}

fn to_std(s: &str) -> Vec<u32> {
    s.chars().map(|c| unicode_to_std(c as u32)).collect()
}

/// Case-insensitive order over Vietnamese letters, as `macKeyCompare`.
fn fold(s: &[u32]) -> impl Iterator<Item = u32> + '_ {
    s.iter().map(|&c| std_to_lower(c))
}

impl MacroTable {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// `key` is in standard chars, without terminator.
    pub fn lookup(&self, key: &[u32]) -> Option<&[u32]> {
        self.items
            .binary_search_by(|(k, _)| fold(k).cmp(fold(key)))
            .ok()
            .map(|i| self.items[i].1.as_slice())
    }

    /// Add one `key:text` item. False when it is malformed or the table is full.
    pub fn add_item(&mut self, item: &str) -> bool {
        let Some((key, text)) = item.split_once(':') else { return false };
        if self.items.len() >= MAX_MACRO_ITEMS {
            return false;
        }
        let key: String = key.chars().take(MAX_MACRO_KEY_LEN - 1).collect();
        let mut text = to_std(text);
        text.truncate(MAX_MACRO_TEXT_LEN);
        self.items.push((to_std(&key), text));
        true
    }

    /// Load a macro file (`key:text` per line, UTF-8). Unlike the original, an
    /// old VIQR-encoded file (no version header) is read as UTF-8 and left alone.
    pub fn load_from_file(&mut self, path: &Path) -> io::Result<()> {
        let data = fs::read(path)?;
        let text = String::from_utf8_lossy(&data);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        self.clear();
        let mut lines = text.lines().peekable();
        // Header: ";DO NOT DELETE THIS LINE*** version=1 ***"
        if let Some(first) = lines.peek() {
            if first.contains("***") && first.contains("version=") {
                lines.next();
            }
        }
        for line in lines {
            self.add_item(line);
        }
        self.items.sort_by(|(a, _), (b, _)| fold(a).cmp(fold(b)));
        Ok(())
    }
}
