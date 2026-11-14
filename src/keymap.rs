//! User-defined key layout loader, a port of `usrkeymap.cpp`.
//! File lines look like `a = Tone1`; `;` starts a comment.

use std::{fs, path::Path};

use crate::tables::*;

const LABELS: &[(&str, i32)] = &[
    ("Tone0", vneTone0), ("Tone1", vneTone1), ("Tone2", vneTone2), ("Tone3", vneTone3),
    ("Tone4", vneTone4), ("Tone5", vneTone5), ("Roof-All", vneRoofAll), ("Roof-A", vneRoof_a),
    ("Roof-E", vneRoof_e), ("Roof-O", vneRoof_o), ("Hook-Bowl", vneHookAll),
    ("Hook-UO", vneHook_uo), ("Hook-U", vneHook_u), ("Hook-O", vneHook_o), ("Bowl", vneBowl),
    ("D-Mark", vneDd), ("Telex-W", vne_telex_w), ("Escape", vneEscChar),
    ("DD", vneCount + vnl_DD), ("dd", vneCount + vnl_dd), ("A^", vneCount + vnl_Ar),
    ("a^", vneCount + vnl_ar), ("A(", vneCount + vnl_Ab), ("a(", vneCount + vnl_ab),
    ("E^", vneCount + vnl_Er), ("e^", vneCount + vnl_er), ("O^", vneCount + vnl_Or),
    ("o^", vneCount + vnl_or), ("O+", vneCount + vnl_Oh), ("o+", vneCount + vnl_oh),
    ("U+", vneCount + vnl_Uh), ("u+", vneCount + vnl_uh),
];

/// Split `name = value`; None for blank, comment-only or malformed lines.
fn parse_name_value(line: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let line = &line[..line.iter().position(|&c| c == b';').unwrap_or(line.len())];
    let eq = line.iter().position(|&c| c == b'=')?;
    let trim = |s: &[u8]| -> Vec<u8> {
        let a = s.iter().position(|&c| c != b' ').unwrap_or(s.len());
        let b = s.iter().rposition(|&c| c != b' ').map_or(a, |p| p + 1);
        s[a..b.max(a)].to_vec()
    };
    let (name, value) = (trim(&line[..eq]), trim(&line[eq + 1..]));
    (!name.is_empty() && !value.is_empty()).then_some((name, value))
}

/// Load a user key layout into a 256-entry key -> event table.
pub fn load_key_map(path: &Path) -> Option<[i32; 256]> {
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to open file: {}: {e}", path.display());
            return None;
        }
    };
    let mut order: Vec<(u8, i32)> = Vec::new();
    let mut seen = [vneNormal; 256];
    for (n, line) in data.split(|&c| c == b'\n').enumerate() {
        let Some((name, value)) = parse_name_value(line) else { continue };
        if name.len() != 1 {
            eprintln!("Error in user key layout, line {}: key name is not a single character", n + 1);
            continue;
        }
        let Some(&(_, ev)) = LABELS.iter().find(|(l, _)| l.as_bytes() == value.as_slice()) else {
            eprintln!("Error in user key layout, line {}: command not found", n + 1);
            continue;
        };
        let c = name[0];
        if seen[c as usize] != vneNormal {
            continue; // already assigned, first definition wins
        }
        seen[c as usize] = ev;
        if ev < vneCount {
            seen[c.to_ascii_uppercase() as usize] = ev;
            order.push((c.to_ascii_uppercase(), ev));
        } else {
            order.push((c, ev));
        }
    }
    let mut map = [vneNormal; 256];
    for (key, action) in order {
        map[key as usize] = action;
        if action < vneCount {
            map[key.to_ascii_lowercase() as usize] = action;
        }
    }
    Some(map)
}
