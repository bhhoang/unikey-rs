//! Replays typing sessions recorded from the UniKey C++ engine as maintained in
//! fcitx5-unikey (the 2005 ukengine plus later fixes: `q`/`qu` validation,
//! `uơ`/`ươ` rules) and checks this port answers identically, key for key.
//! Sessions mix all input methods, option combinations, macros, a user key
//! layout, backspace, key-stroke restores and resets.
//!
//! One deliberate difference from the C++ engine: when a mark is undone by a
//! key that is also a word break (VIQR `?`/`.`/`*`, MsVi digits) and the
//! auto-restore attempt fails, the C++ code zeroes the backspace count and
//! corrupts the text. The recordings were made with that slip patched out.

mod common;

use std::{fs, path::Path};

#[test]
fn matches_recorded_cpp_engine() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut checked = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "in") {
            let script = fs::read_to_string(&path).unwrap();
            let expected = fs::read_to_string(path.with_extension("out")).unwrap();
            let got = common::driver::run(&script, &dir);
            for (n, (g, e)) in got.lines().zip(expected.lines()).enumerate() {
                assert_eq!(g, e, "{}: output line {}", path.display(), n + 1);
            }
            assert_eq!(got.lines().count(), expected.lines().count(), "{}", path.display());
            checked += expected.lines().count();
        }
    }
    assert!(checked > 20_000, "golden corpus missing ({checked} lines)");
}
