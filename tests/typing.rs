//! Typing behaviour with real Vietnamese, as an editor would show it.

mod common;
use common::{Editor, typed};
use unikey::InputMethod::*;
use unikey::Options;

#[test]
fn telex_words() {
    assert_eq!(typed(Telex, "Vieejt Nam"), "Việt Nam");
    assert_eq!(typed(Telex, "tooi yeeu ddatas nuowcs"), "tôi yêu đất nước");
    assert_eq!(typed(Telex, "nguwowif"), "người");
    assert_eq!(typed(Telex, "truwowngf"), "trường");
    assert_eq!(typed(Telex, "thuowngr"), "thưởng");
    assert_eq!(typed(Telex, "quas"), "quá");
    assert_eq!(typed(Telex, "giaos"), "giáo");
    assert_eq!(typed(Telex, "DDaji hojc"), "Đại học");
}

#[test]
fn vni_words() {
    assert_eq!(typed(Vni, "Vie6t5 Nam"), "Việt Nam");
    assert_eq!(typed(Vni, "to6i ye6u d9a61t nu7o71c"), "tôi yêu đất nước");
    assert_eq!(typed(Vni, "ngu7o7i2"), "người");
}

#[test]
fn viqr_words() {
    assert_eq!(typed(Viqr, "Vie^.t Nam"), "Việt Nam");
    assert_eq!(typed(Viqr, "to^i"), "tôi");
}

#[test]
fn tone_goes_on_the_right_vowel() {
    // old style (default): hòa, thủy; modern style: hoà, thuỷ
    assert_eq!(typed(Telex, "hoaf"), "hòa");
    assert_eq!(typed(Telex, "thuyr"), "thủy");
    let mut e = Editor::new(Telex);
    let mut o = e.engine.options();
    o.modern_style = true;
    e.engine.set_options(o);
    assert_eq!(e.type_str("hoaf"), "hoà");
}

#[test]
fn typing_a_mark_twice_undoes_it() {
    assert_eq!(typed(Telex, "aas"), "ấ");
    assert_eq!(typed(Telex, "aass"), "âs");
    assert_eq!(typed(Telex, "ddd"), "dd");
    assert_eq!(typed(Telex, "tooo"), "too");
}

#[test]
fn plain_ascii_is_left_alone() {
    // keys the engine does not convert are passed through, not retyped
    let mut e = Editor::new(Telex);
    for c in "hello".chars() {
        e.engine.set_caps_state(false, false);
        let out = e.engine.process(c as u32);
        assert!(!out.handled || out.text.contains(c), "{c}: {out:?}");
        e.key(c);
    }
}

#[test]
fn non_vietnamese_words_are_restored() {
    // "ws"/"fs" style English words come back as typed when auto-restore is on
    assert_eq!(typed(Telex, "text "), "text ");
    assert_eq!(typed(Telex, "windows "), "windows ");
}

#[test]
fn backspace_keeps_the_word_consistent() {
    let mut e = Editor::new(Telex);
    e.type_str("Vieejt");
    e.backspace();
    e.type_str("t");
    assert_eq!(e.text, "Việt");
    // removing a letter that moves the tone mark
    let mut e = Editor::new(Telex);
    e.type_str("hoaf");
    e.backspace();
    assert_eq!(e.text, "hò");
}

#[test]
fn caps_and_shift() {
    assert_eq!(typed(Telex, "VIEETJ"), "VIỆT");
    assert_eq!(typed(Telex, "Aa"), "Â");
}

#[test]
fn macros_expand_on_word_break() {
    let dir = std::env::temp_dir().join(format!("unikey-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("macro");
    std::fs::write(&file, ";DO NOT DELETE THIS LINE*** version=1 ***\nvn:Việt Nam\n").unwrap();
    // The expansion follows the case typed: vn -> lower, Vn -> as stored, VN -> upper.
    for (typed, shown) in [("vn ", "việt nam "), ("Vn ", "Việt Nam "), ("VN ", "VIỆT NAM ")] {
        let mut e = Editor::new(Telex);
        e.engine.load_macro_table(&file).unwrap();
        e.engine.set_options(Options { macro_enabled: true, ..e.engine.options() });
        assert_eq!(e.type_str(typed), shown);
    }
    // UniKey 4.3: auto-case can be turned off, then the text is used as written
    let mut e = Editor::new(Telex);
    e.engine.load_macro_table(&file).unwrap();
    e.engine.set_options(Options { macro_enabled: true, macro_auto_case: false, ..e.engine.options() });
    assert_eq!(e.type_str("vn VN "), "Việt Nam Việt Nam ");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn vietnamese_off_means_no_conversion() {
    let mut e = Editor::new(Telex);
    e.engine.set_viet_key(false);
    assert_eq!(e.type_str("vieejt"), "vieejt");
}

/// fcitx5-unikey's Telex table: the open UniKey engine as maintained today.
#[test]
fn fcitx5_unikey_telex_table() {
    let table = include_str!("golden/fcitx5_telex.tsv");
    let mut failures = Vec::new();
    let mut count = 0;
    for line in table.lines().filter(|l| !l.starts_with('#')) {
        let (keys, want) = line.split_once('\t').unwrap();
        let mut e = Editor::new(Telex);
        e.engine.set_options(Options {
            spell_check_enabled: false,
            macro_enabled: true,
            auto_non_vn_restore: false,
            ..e.engine.options()
        });
        let got = e.type_str(keys).to_owned();
        count += 1;
        if got != want {
            failures.push(format!("{keys}: got {got}, want {want}"));
        }
    }
    assert!(count > 1000);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
}

/// UniKey 4.6: Simple Telex never turns a word-initial w into ư; w only marks
/// a preceding u, o or a.
#[test]
fn simple_telex_w() {
    assert_eq!(typed(SimpleTelex, "w "), "w ");
    assert_eq!(typed(SimpleTelex, "web "), "web ");
    assert_eq!(typed(SimpleTelex, "tuw "), "tư ");
    assert_eq!(typed(SimpleTelex, "nguwowif "), "người ");
    assert_eq!(typed(SimpleTelex, "awn "), "ăn ");
    assert_eq!(typed(Telex, "w "), "ư ");
}

#[test]
fn w_undo_ignores_case() {
    // Shift+W then w undoes "Ư" too; the letter keeps the first key's case.
    for (keys, want) in [("WW", "W"), ("Ww", "W"), ("ww", "w"), ("wW", "w"), ("AWw", "Aw"), ("aww", "aw")] {
        assert_eq!(typed(Telex, keys), want, "{keys}");
    }
}
