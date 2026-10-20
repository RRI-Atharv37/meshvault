use meshvault_core::ignore::{IgnoreError, IgnoreSet};
use meshvault_core::path::RelPath;

fn p(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn set(patterns: &[&str]) -> IgnoreSet {
    IgnoreSet::new(patterns).unwrap()
}

#[test]
fn ignore_empty_set_ignores_nothing() {
    let s = IgnoreSet::default();
    assert!(!s.is_ignored(&p("a/b.tmp")));
    assert!(!IgnoreSet::new::<&str>(&[]).unwrap().is_ignored(&p("x")));
}

#[test]
fn ignore_rejects_empty_pattern() {
    assert_eq!(
        IgnoreSet::new(&[""]).unwrap_err(),
        IgnoreError::EmptyPattern
    );
}

#[test]
fn ignore_star_matches_within_component() {
    let s = set(&["*.tmp"]);
    assert!(s.is_ignored(&p("a.tmp")));
    assert!(s.is_ignored(&p("deep/er/a.tmp")));
    assert!(!s.is_ignored(&p("a.tmp.keep")));
    assert!(!s.is_ignored(&p("tmp")));
}

#[test]
fn ignore_name_pattern_matches_any_depth_and_children() {
    let s = set(&["cache"]);
    assert!(s.is_ignored(&p("cache")));
    assert!(s.is_ignored(&p("a/cache")));
    assert!(s.is_ignored(&p("a/cache/x/y.bin")));
    assert!(!s.is_ignored(&p("cachefile")));
}

#[test]
fn ignore_slash_pattern_anchors_at_root() {
    let s = set(&["render/previews"]);
    assert!(s.is_ignored(&p("render/previews")));
    assert!(s.is_ignored(&p("render/previews/a.mov")));
    assert!(!s.is_ignored(&p("x/render/previews")));
}

#[test]
fn ignore_double_star_crosses_components() {
    let s = set(&["a/**/z.bin"]);
    assert!(s.is_ignored(&p("a/z.bin")));
    assert!(s.is_ignored(&p("a/b/c/z.bin")));
    assert!(!s.is_ignored(&p("b/a/z.bin")));
}

#[test]
fn ignore_question_mark_matches_one_char() {
    let s = set(&["f?.txt"]);
    assert!(s.is_ignored(&p("f1.txt")));
    assert!(!s.is_ignored(&p("f12.txt")));
    assert!(!s.is_ignored(&p("f.txt")));
}

#[test]
fn ignore_case_insensitive() {
    let s = set(&["*.TMP", "Cache"]);
    assert!(s.is_ignored(&p("x.tmp")));
    assert!(s.is_ignored(&p("CACHE/y")));
}
