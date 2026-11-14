//! Letter case never changes which conversion happens: for every word, each
//! mix of upper and lower case gives the lowercase result in that case.
mod common;
use common::typed;
use unikey::InputMethod::{self, *};

fn variants(s: &str) -> Vec<String> {
    let idx: Vec<usize> = s.char_indices().filter(|(_, c)| c.is_ascii_alphabetic()).map(|(i, _)| i).collect();
    let n = idx.len().min(10);
    let mut out = vec![];
    for mask in 0u32..(1 << n) {
        let mut b = s.as_bytes().to_vec();
        for (k, &i) in idx.iter().take(n).enumerate() {
            if mask >> k & 1 == 1 { b[i] = b[i].to_ascii_uppercase(); }
        }
        out.push(String::from_utf8(b).unwrap());
    }
    out
}

#[test]
fn case_does_not_change_conversions() {
    let tsv = std::fs::read_to_string("tests/golden/fcitx5_telex.tsv").unwrap();
    let telex: Vec<String> = tsv.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split('\t').next()).map(str::to_owned).collect();
    let extra = ["vieejt", "nguwowif", "ddaats", "uw", "w", "ww", "aw", "aww", "oo", "ooo", "dd", "ddd", "ass", "hoaf", "thuyr", "quas", "giaos", "uow", "uowf", "huwowng", "khuyeens"];
    let vni = ["vie6t5", "ngu7o7i2", "d9a61t", "a66", "o77", "d99", "a11", "u7", "u77"];
    let viqr = ["vie^.t", "u+", "u++", "o^^", "dd", "ddd", "a((", "a''"];
    let sets: [(InputMethod, Vec<String>); 5] = [
        (Telex, telex.iter().cloned().chain(extra.iter().map(|s| s.to_string())).collect()),
        (SimpleTelex, extra.iter().map(|s| s.to_string()).collect()),
        (SimpleTelex2, extra.iter().map(|s| s.to_string()).collect()),
        (Vni, vni.iter().map(|s| s.to_string()).collect()),
        (Viqr, viqr.iter().map(|s| s.to_string()).collect()),
    ];
    let mut bad = std::collections::BTreeSet::new();
    for (im, words) in sets {
        for w in words {
            let base = typed(im, &w.to_lowercase()).to_lowercase();
            for v in variants(&w.to_lowercase()) {
                let got = typed(im, &v);
                if got.to_lowercase() != base {
                    bad.insert(format!("{im:?}\t{v}\t{got}\t(lower: {base})"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} case mismatches:\n{}", bad.len(), bad.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}
