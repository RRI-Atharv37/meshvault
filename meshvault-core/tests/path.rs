use meshvault_core::path::{
    PathError, RelPath, MAX_COMPONENT_BYTES, MAX_DEPTH, MAX_REL_PATH_BYTES,
};
use proptest::prelude::*;

fn err(s: &str) -> PathError {
    RelPath::parse(s).expect_err(s)
}

#[test]
fn path_accepts_simple() {
    for s in [
        "a",
        "a/b/c.mov",
        "dir with space/f.txt",
        "\u{fc}n\u{ef}/\u{65e5}\u{672c}\u{8a9e}.txt",
        ".hidden",
        "a..b",
        "a/.meshvault2",
        "x.y.z",
    ] {
        let p = RelPath::parse(s).expect(s);
        assert_eq!(p.as_str(), s);
    }
}

#[test]
fn path_rejects_empty() {
    for s in ["", "a//b", "a/", "a/b/"] {
        assert_eq!(err(s), PathError::Empty, "{s:?}");
    }
}

#[test]
fn path_rejects_absolute() {
    for s in ["/a", "/", "\\a"] {
        assert_eq!(err(s), PathError::Absolute, "{s:?}");
    }
}

#[test]
fn path_rejects_drive_and_unc() {
    for s in ["C:x", "C:\\x", "c:/x", "\\\\srv\\s", "//srv/s"] {
        assert_eq!(err(s), PathError::DriveOrUnc, "{s:?}");
    }
}

#[test]
fn path_rejects_dotdot() {
    for s in ["..", "a/..", "../a", "a/../b"] {
        assert_eq!(err(s), PathError::DotDot, "{s:?}");
    }
}

#[test]
fn path_rejects_dot_component() {
    for s in [".", "a/./b", "./a"] {
        assert_eq!(err(s), PathError::DotComponent, "{s:?}");
    }
}

#[test]
fn path_rejects_backslash() {
    assert_eq!(err("a\\b"), PathError::ForbiddenChar('\\'));
}

#[test]
fn path_rejects_reserved_names() {
    let mut names: Vec<String> = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for i in 1..=9 {
        names.push(format!("COM{i}"));
        names.push(format!("LPT{i}"));
    }
    for n in &names {
        for variant in [
            n.clone(),
            n.to_lowercase(),
            format!("{n}.txt"),
            format!("{}.mov", n.to_lowercase()),
            format!("dir/{n}"),
            format!("dir/{n}.tar.gz"),
        ] {
            assert!(
                matches!(err(&variant), PathError::ReservedName(_)),
                "{variant:?}"
            );
        }
    }
    assert!(matches!(err("Com1.mov"), PathError::ReservedName(_)));
    for ok in ["CON1", "COMM", "console.txt", "nul-x", "com10", "auxiliary"] {
        RelPath::parse(ok).expect(ok);
    }
}

#[test]
fn path_rejects_trailing_dot_and_space() {
    for s in ["a.", "a ", "dir./x", "a/b ", "a/b."] {
        assert_eq!(err(s), PathError::TrailingDotOrSpace, "{s:?}");
    }
}

#[test]
fn path_rejects_forbidden_chars() {
    for c in ['<', '>', ':', '"', '|', '?', '*'] {
        assert_eq!(err(&format!("dir/a{c}b")), PathError::ForbiddenChar(c));
    }
}

#[test]
fn path_rejects_control_chars() {
    for s in ["a\u{0}b", "a\nb", "a\u{1f}", "d/\tx", "a\rb"] {
        assert_eq!(err(s), PathError::ControlChar, "{s:?}");
    }
}

#[test]
fn path_rejects_too_long() {
    let comp = "a".repeat(MAX_COMPONENT_BYTES);
    let parts = MAX_REL_PATH_BYTES / MAX_COMPONENT_BYTES + 2;
    let s = vec![comp; parts].join("/");
    assert!(s.len() > MAX_REL_PATH_BYTES && parts <= MAX_DEPTH);
    assert_eq!(err(&s), PathError::TooLong);
}

#[test]
fn path_rejects_long_component() {
    let s = format!("ok/{}", "b".repeat(MAX_COMPONENT_BYTES + 1));
    assert_eq!(err(&s), PathError::ComponentTooLong);
    RelPath::parse(&format!("ok/{}", "b".repeat(MAX_COMPONENT_BYTES))).unwrap();
}

#[test]
fn path_rejects_too_deep() {
    let ok = vec!["d"; MAX_DEPTH].join("/");
    RelPath::parse(&ok).unwrap();
    let deep = vec!["d"; MAX_DEPTH + 1].join("/");
    assert_eq!(err(&deep), PathError::TooDeep);
}

#[test]
fn path_rejects_meta_dir() {
    for s in [
        ".meshvault",
        ".MeshVault/x",
        ".MESHVAULT/a/b",
        ".meshvault/index",
    ] {
        assert_eq!(err(s), PathError::MetaDir, "{s:?}");
    }
}

#[test]
fn path_normalises_to_nfc() {
    let p = RelPath::parse("e\u{301}.txt").unwrap();
    assert_eq!(p.as_str(), "\u{e9}.txt");
    let q = RelPath::parse("dir/A\u{30a}.txt").unwrap();
    assert_eq!(q.as_str(), "dir/\u{c5}.txt");
}

#[test]
fn path_key_case_insensitive() {
    let a = RelPath::parse("A.mov").unwrap();
    let b = RelPath::parse("a.mov").unwrap();
    assert_ne!(a, b);
    assert_eq!(a.key(), b.key());
    assert_ne!(
        RelPath::parse("a.mov").unwrap().key(),
        RelPath::parse("b.mov").unwrap().key()
    );
    assert_eq!(
        RelPath::parse("Dir/File.TXT").unwrap().key(),
        RelPath::parse("dir/file.txt").unwrap().key()
    );
}

#[test]
fn path_key_nfc_insensitive() {
    assert_eq!(
        RelPath::parse("e\u{301}.mov").unwrap().key(),
        RelPath::parse("\u{e9}.mov").unwrap().key()
    );
    assert_eq!(
        RelPath::parse("e\u{301}.mov").unwrap().key(),
        RelPath::parse("\u{c9}.MOV").unwrap().key()
    );
}

#[test]
fn path_key_special_cases() {
    let k = |s: &str| RelPath::parse(s).unwrap().key();
    // Simple per-character uppercase: a character whose uppercase is several characters stays as is.
    assert_ne!(k("\u{df}"), k("SS"));
    assert_eq!(k("\u{df}"), k("\u{df}"));
    // Dotless i uppercases to I; dotted capital I stays distinct.
    assert_eq!(k("\u{131}"), k("I"));
    assert_ne!(k("\u{130}"), k("I"));
    // Final sigma, sigma and capital sigma are one name.
    assert_eq!(k("\u{3c2}"), k("\u{3c3}"));
    assert_eq!(k("\u{3c3}"), k("\u{3a3}"));
}

#[test]
fn path_join_validates() {
    let base = RelPath::parse("a").unwrap();
    assert_eq!(base.join("b").unwrap().as_str(), "a/b");
    assert_eq!(base.join("..").unwrap_err(), PathError::DotDot);
    assert!(matches!(
        base.join("con").unwrap_err(),
        PathError::ReservedName(_)
    ));
    assert_eq!(base.join("x/y").unwrap_err(), PathError::ForbiddenChar('/'));
    assert_eq!(base.join("").unwrap_err(), PathError::Empty);
    assert_eq!(base.join("x.").unwrap_err(), PathError::TrailingDotOrSpace);
}

#[test]
fn path_parent_and_components() {
    let p = RelPath::parse("a/b/c").unwrap();
    assert_eq!(p.parent().unwrap().as_str(), "a/b");
    assert_eq!(p.parent().unwrap().parent().unwrap().as_str(), "a");
    assert_eq!(RelPath::parse("a").unwrap().parent(), None);
    assert_eq!(p.components().collect::<Vec<_>>(), ["a", "b", "c"]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn path_parse_never_panics(s in "\\PC*") {
        let _ = RelPath::parse(&s);
    }

    #[test]
    fn path_parse_never_panics_on_pathy_input(s in "[a-zA-Z0-9./\\\\:*? \u{301}\u{e9}]{0,80}") {
        let _ = RelPath::parse(&s);
    }

    #[test]
    fn path_parse_display_round_trip(s in "[a-zA-Z0-9./ \u{301}\u{e9}\u{131}]{0,60}") {
        if let Ok(p) = RelPath::parse(&s) {
            let again = RelPath::parse(&p.to_string()).expect("accepted output re-parses");
            prop_assert_eq!(&again, &p);
            prop_assert_eq!(again.key(), p.key());
            prop_assert!(!p.as_str().is_empty());
            prop_assert!(!p.as_str().starts_with('/') && !p.as_str().ends_with('/'));
        }
    }
}
