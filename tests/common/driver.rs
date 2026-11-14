//! Command-script driver shared by the golden test and the `oracle` example.
//! Commands and output format match the reference C++ driver used to record
//! the golden files: one line per command, `ret backs type hex(out)`.
use std::io::Write;
use std::path::Path;

use unikey::engine::OutputKind;
use unikey::{Engine, InputMethod, Output};

fn show(out: &mut impl std::io::Write, ret: bool, backs: usize, o: &Output) {
    let ty = if ret && o.kind == OutputKind::Key { 1 } else { 0 };
    let hex: String = if ret { o.text.bytes().map(|b| format!("{b:02x}")).collect() } else { String::new() };
    writeln!(out, "{} {} {} {}", ret as i32, backs, ty, hex).unwrap();
}

/// Run a command script (see the C++ reference driver) and return its output.
/// File arguments (macro table, key layout) are resolved against `dir`.
pub fn run(script: &str, dir: &Path) -> String {
    let mut e = Engine::new();
    let mut buf = Vec::new();
    let out = &mut buf;
    let mut tokens = script.split_whitespace().map(str::to_owned);
    while let Some(cmd) = tokens.next() {
        let mut arg = || tokens.next().unwrap();
        match cmd.as_str() {
            "IM" => {
                let im = match arg().as_str() {
                    "vni" => InputMethod::Vni,
                    "viqr" => InputMethod::Viqr,
                    "msvi" => InputMethod::MsVi,
                    "simple" => InputMethod::SimpleTelex,
                    "simple2" => InputMethod::SimpleTelex2,
                    _ => InputMethod::Telex,
                };
                e.set_input_method(im);
                writeln!(out, "ok").unwrap();
            }
            "USRMAP" => {
                let ok = e.load_user_key_map(&dir.join(arg()));
                if ok {
                    e.set_input_method(InputMethod::UsrIM);
                }
                writeln!(out, "{}", ok as i32).unwrap();
            }
            "OPT" => {
                let mut b = || arg() == "1";
                let mut o = e.options();
                o.free_marking = b();
                o.modern_style = b();
                o.macro_enabled = b();
                o.spell_check_enabled = b();
                o.auto_non_vn_restore = b();
                e.set_options(o);
                writeln!(out, "ok").unwrap();
            }
            "MACRO" => {
                writeln!(out, "{}", e.load_macro_table(&dir.join(arg())).is_ok() as i32).unwrap();
            }
            "KEY" => {
                let c: u32 = arg().parse().unwrap();
                let sh = arg() == "1";
                let caps = arg() == "1";
                e.set_caps_state(sh, caps);
                let o = e.process(c);
                show(out, o.handled, if o.handled { o.backs } else { 0 }, &o);
            }
            "BS" => {
                let o = e.process_backspace();
                show(out, o.handled, o.backs, &o);
            }
            "RESTORE" => {
                let o = e.restore_key_strokes();
                show(out, o.handled, o.backs, &o);
            }
            "RESET" => {
                e.reset();
                writeln!(out, "ok").unwrap();
            }
            "SINGLE" => {
                e.set_single_mode();
                writeln!(out, "ok").unwrap();
            }
            "PUT" => {
                e.pass(arg().parse().unwrap());
                writeln!(out, "ok").unwrap();
            }
            "WB" => writeln!(out, "{}", e.at_word_beginning() as i32).unwrap(),
            other => panic!("unknown command {other}"),
        }
    }
    String::from_utf8(buf).unwrap()
}
