mod common;

use common::{created, deleted, modified, p, Harness, START, UID};
use meshvault_core::fs::{EntryKind, FaultPlan, Fs, FsError, VolumeKind};
use meshvault_core::hash::{hash_bytes, FolderUid};
use meshvault_core::ignore::IgnoreSet;
use meshvault_core::path::PathError;
use meshvault_core::scan::{
    check_folder, FolderCheck, LocalChange, ScanConfig, ScanError, Scanner, SkipReason,
};
use meshvault_core::time::{Clock, Timestamp};
use proptest::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn scan_new_file_created() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"hello world")]);
    h.settle();
    let r = h.scan();
    assert_eq!(r.changes.len(), 1, "{r:?}");
    let LocalChange::Created(s) = &r.changes[0] else {
        panic!("{r:?}")
    };
    assert_eq!(s.path, p("a.txt"));
    assert_eq!(s.kind, EntryKind::File);
    assert_eq!(s.size, 11);
    assert_eq!(s.file_hash, Some(hash_bytes(b"hello world")));
    assert_eq!(s.chunks.len(), 1);
    assert_eq!(s.chunks[0].len, 11);
    assert!(r.unknown.is_empty() && r.skipped.is_empty() && r.deferred.is_empty());
    h.apply(&r);
    assert!(h.scan().changes.is_empty());
}

#[test]
fn scan_unchanged_file_not_reported() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"x"), ("d/b.txt", b"y")]);
    let first = h.scan_and_apply();
    assert_eq!(created(&first), ["a.txt", "d", "d/b.txt"]);
    assert!(h.scan().changes.is_empty());
}

#[test]
fn scan_modified_file() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"one")]);
    h.scan_and_apply();
    h.clock.advance(Duration::from_secs(60));
    h.fs.write_file(&p("a.txt"), b"two!");
    h.settle();
    let r = h.scan();
    assert_eq!(modified(&r), ["a.txt"]);
    let LocalChange::Modified(s) = &r.changes[0] else {
        panic!()
    };
    assert_eq!(s.file_hash, Some(hash_bytes(b"two!")));
    assert_eq!(s.size, 4);
}

#[test]
fn scan_deleted_file() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"one"), ("keep.txt", b"k")]);
    h.scan_and_apply();
    h.fs.remove(&p("a.txt"));
    let r = h.scan();
    assert_eq!(r.changes, [LocalChange::Deleted(p("a.txt"))]);
}

#[test]
fn scan_deleted_directory_reports_every_child() {
    let mut h = Harness::new();
    h.files(&[("d/x.txt", b"1"), ("d/e/y.txt", b"2"), ("top.txt", b"3")]);
    h.scan_and_apply();
    h.fs.remove(&p("d"));
    let r = h.scan();
    assert_eq!(deleted(&r), ["d", "d/e", "d/e/y.txt", "d/x.txt"]);
}

#[test]
fn scan_dir_replaced_by_file_deletes_children() {
    let mut h = Harness::new();
    h.files(&[("d/x.txt", b"1")]);
    h.scan_and_apply();
    h.fs.remove(&p("d"));
    h.fs.put_file(&p("d"), b"now a file");
    h.settle();
    let r = h.scan();
    assert_eq!(modified(&r), ["d"]);
    assert_eq!(deleted(&r), ["d/x.txt"]);
}

#[test]
fn scan_error_dir_marks_unknown_not_deleted() {
    let mut h = Harness::new();
    h.files(&[("top.txt", b"t"), ("d/x.txt", b"1"), ("d/e/y.txt", b"2")]);
    h.scan_and_apply();
    h.fs.remove(&p("top.txt"));
    h.fs.fail_read_dir(Some(&p("d")), FsError::PermissionDenied);
    let r = h.scan();
    assert_eq!(deleted(&r), ["top.txt"]);
    assert_eq!(r.unknown, [p("d")]);
}

#[test]
fn scan_unreadable_subdir_does_not_block_sibling_deletes() {
    let mut h = Harness::new();
    h.files(&[("d/x.txt", b"1"), ("d/e/y.txt", b"2")]);
    h.scan_and_apply();
    h.fs.remove(&p("d/x.txt"));
    h.fs.remove(&p("d/e/y.txt"));
    h.fs.fail_read_dir(Some(&p("d/e")), FsError::Io("boom".into()));
    let r = h.scan();
    assert_eq!(deleted(&r), ["d/x.txt"]);
    assert_eq!(r.unknown, [p("d/e")]);
}

#[test]
fn scan_root_listing_error_is_an_error_not_deletes() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"1")]);
    h.scan_and_apply();
    h.fs.fail_read_dir(None, FsError::PermissionDenied);
    let res = Scanner::new(&h.fs, ScanConfig::default(), IgnoreSet::default())
        .scan(&h.index, h.clock.as_ref() as &dyn Clock);
    assert_eq!(res, Err(ScanError::Root(FsError::PermissionDenied)));
}

#[test]
fn scan_permission_denied_keeps_old_version() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"old")]);
    h.scan_and_apply();
    h.clock.advance(Duration::from_secs(60));
    h.fs.write_file(&p("a.txt"), b"newer");
    h.settle();
    h.fs.fail_open_read(&p("a.txt"), FsError::PermissionDenied);
    let r = h.scan();
    assert!(r.changes.is_empty(), "{r:?}");
    assert_eq!(
        r.skipped,
        [("a.txt".to_string(), SkipReason::PermissionDenied)]
    );
}

#[test]
fn scan_busy_file_keeps_old_version() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"old")]);
    h.scan_and_apply();
    h.clock.advance(Duration::from_secs(60));
    h.fs.write_file(&p("a.txt"), b"newer");
    h.settle();
    h.fs.fail_open_read(&p("a.txt"), FsError::Busy);
    let r = h.scan();
    assert!(r.changes.is_empty(), "{r:?}");
    assert_eq!(r.skipped, [("a.txt".to_string(), SkipReason::Busy)]);
}

#[test]
fn scan_read_error_is_skipped_never_deleted() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"old")]);
    h.scan_and_apply();
    h.clock.advance(Duration::from_secs(60));
    h.fs.write_file(&p("a.txt"), b"newer");
    h.settle();
    h.fs.fail_open_read(&p("a.txt"), FsError::NotFound);
    let r = h.scan();
    assert!(r.changes.is_empty(), "{r:?}");
    assert_eq!(r.skipped.len(), 1);
}

#[test]
fn scan_growing_file_deferred() {
    let mut h = Harness::new();
    h.files(&[("a.bin", &[1u8; 64])]);
    h.settle();
    let n = Arc::new(AtomicU64::new(0));
    h.fs.on_read_eof(&p("a.bin"), move |f| {
        let k = n.fetch_add(1, Ordering::SeqCst) as usize;
        f.write_file(&p("a.bin"), &vec![1u8; 100 + 10 * k]);
        f.set_mtime(&p("a.bin"), START);
    });
    let r = h.scan();
    assert!(r.changes.is_empty(), "{r:?}");
    assert_eq!(r.deferred, [p("a.bin")]);
}

#[test]
fn scan_quiet_window_respected() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"fresh")]);
    let r = h.scan();
    assert!(r.changes.is_empty());
    assert_eq!(r.deferred, [p("a.txt")]);

    h.clock.advance(Duration::from_secs(9));
    assert_eq!(h.scan().deferred, [p("a.txt")]);

    h.clock.advance(Duration::from_secs(1));
    let r = h.scan();
    assert!(r.deferred.is_empty());
    assert_eq!(created(&r), ["a.txt"]);
}

#[test]
fn scan_future_mtime_is_deferred() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"x")]);
    h.fs.set_mtime(&p("a.txt"), Timestamp(START.0 + 3_600_000_000_000));
    h.settle();
    let r = h.scan();
    assert_eq!(r.deferred, [p("a.txt")]);
    assert!(r.changes.is_empty());
}

#[test]
fn scan_double_hash_mismatch_deferred() {
    let mut h = Harness::new();
    h.files(&[("a.bin", &[0u8; 64])]);
    h.settle();
    let n = Arc::new(AtomicU64::new(1));
    h.fs.on_read_eof(&p("a.bin"), move |f| {
        let k = n.fetch_add(1, Ordering::SeqCst);
        f.write_file(&p("a.bin"), &[k as u8; 64]);
        f.set_mtime(&p("a.bin"), Timestamp(START.0 - 1_000_000_000 * k as i64));
    });
    let r = h.scan();
    assert!(r.changes.is_empty(), "{r:?}");
    assert_eq!(r.deferred, [p("a.bin")]);
}

#[test]
fn scan_replaced_during_scan_rescanned() {
    let mut h = Harness::new();
    h.files(&[("a.bin", b"first version")]);
    h.settle();
    let done = Arc::new(AtomicBool::new(false));
    h.fs.on_read_eof(&p("a.bin"), move |f| {
        if !done.swap(true, Ordering::SeqCst) {
            f.replace_file(&p("a.bin"), b"replacement");
            f.set_mtime(&p("a.bin"), START);
        }
    });
    let r = h.scan();
    assert_eq!(created(&r), ["a.bin"]);
    let LocalChange::Created(s) = &r.changes[0] else {
        panic!()
    };
    assert_eq!(s.file_hash, Some(hash_bytes(b"replacement")));
    assert!(r.deferred.is_empty());
}

#[test]
fn scan_skips_links() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"x")]);
    h.fs.put_link(&p("l"));
    h.settle();
    let r = h.scan();
    assert_eq!(created(&r), ["a.txt"]);
    assert_eq!(r.skipped, [("l".to_string(), SkipReason::Link)]);
}

#[test]
fn scan_link_where_a_file_was_is_not_a_delete() {
    let mut h = Harness::new();
    h.files(&[("a.txt", b"x")]);
    h.scan_and_apply();
    h.fs.remove(&p("a.txt"));
    h.fs.put_link(&p("a.txt"));
    h.settle();
    let r = h.scan();
    assert!(deleted(&r).is_empty(), "{r:?}");
}

#[test]
fn scan_skips_meta_dir() {
    let mut h = Harness::new();
    h.fs.meta_write("index", b"db").unwrap();
    h.files(&[("a.txt", b"x")]);
    h.settle();
    let r = h.scan();
    assert_eq!(created(&r), ["a.txt"]);
    assert!(r.skipped.is_empty() && r.unknown.is_empty(), "{r:?}");
    assert!(!r
        .changes
        .iter()
        .any(|c| format!("{c:?}").contains("meshvault")));
}

#[test]
fn scan_skips_bad_names() {
    let mut h = Harness::new();
    h.fs.add_unrepresentable_entry(None, "CON");
    h.fs.add_unrepresentable_entry(None, "a:b");
    h.fs.put_dir(&p("d"));
    h.fs.add_unrepresentable_entry(Some(&p("d")), "trail.");
    h.files(&[("ok.txt", b"x")]);
    h.settle();
    let r = h.scan();
    assert_eq!(created(&r), ["d", "ok.txt"]);
    let mut skipped = r.skipped.clone();
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        skipped,
        [
            (
                "CON".to_string(),
                SkipReason::BadName(PathError::ReservedName("CON".into()))
            ),
            (
                "a:b".to_string(),
                SkipReason::BadName(PathError::DriveOrUnc)
            ),
            (
                "d/trail.".to_string(),
                SkipReason::BadName(PathError::TrailingDotOrSpace)
            ),
        ]
    );
}

#[test]
fn scan_detects_move_by_hash() {
    let mut h = Harness::new();
    h.files(&[("a.mov", b"video bytes"), ("other.mov", b"different")]);
    h.scan_and_apply();
    h.fs.put_dir(&p("sub"));
    h.fs.rename(&p("a.mov"), &p("sub/b.mov"));
    h.settle();
    let r = h.scan();
    assert!(deleted(&r).is_empty(), "{r:?}");
    assert_eq!(created(&r), ["sub"]);
    let moves: Vec<_> = r
        .changes
        .iter()
        .filter_map(|c| match c {
            LocalChange::Moved { from, to } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(moves.len(), 1, "{r:?}");
    assert_eq!(moves[0].0, p("a.mov"));
    assert_eq!(moves[0].1.path, p("sub/b.mov"));
    assert_eq!(moves[0].1.file_hash, Some(hash_bytes(b"video bytes")));
}

#[test]
fn scan_copy_is_not_a_move() {
    let mut h = Harness::new();
    h.files(&[("a.mov", b"same bytes")]);
    h.scan_and_apply();
    h.fs.put_file(&p("copy.mov"), b"same bytes");
    h.settle();
    let r = h.scan();
    assert_eq!(created(&r), ["copy.mov"]);
    assert!(deleted(&r).is_empty());
    assert!(!r
        .changes
        .iter()
        .any(|c| matches!(c, LocalChange::Moved { .. })));
}

#[test]
fn scan_case_only_rename_is_move() {
    let mut h = Harness::new();
    h.files(&[("A.mov", b"video bytes")]);
    h.scan_and_apply();
    h.fs.rename(&p("A.mov"), &p("a.mov"));
    h.settle();
    let r = h.scan();
    assert_eq!(r.changes.len(), 1, "{r:?}");
    let LocalChange::Moved { from, to } = &r.changes[0] else {
        panic!("{r:?}")
    };
    assert_eq!(from, &p("A.mov"));
    assert_eq!(to.path, p("a.mov"));
    assert_eq!(to.file_hash, Some(hash_bytes(b"video bytes")));
    assert!(!to.chunks.is_empty());
}

#[test]
fn scan_ignore_patterns_applied() {
    let mut h = Harness::new();
    h.files(&[("x.tmp", b"1"), ("cache/y.bin", b"2"), ("z.txt", b"3")]);
    h.settle();
    let r = h.scan_with(IgnoreSet::new(&["*.tmp", "cache"]).unwrap());
    assert_eq!(created(&r), ["z.txt"]);
    let mut skipped = r.skipped.clone();
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        skipped,
        [
            ("cache".to_string(), SkipReason::Ignored),
            ("x.tmp".to_string(), SkipReason::Ignored)
        ]
    );
}

#[test]
fn scan_ignored_paths_are_never_deleted() {
    let mut h = Harness::new();
    h.files(&[("x.tmp", b"1"), ("cache/y.bin", b"2")]);
    h.scan_and_apply();
    h.fs.remove(&p("x.tmp"));
    h.fs.remove(&p("cache"));
    let r = h.scan_with(IgnoreSet::new(&["*.tmp", "cache"]).unwrap());
    assert!(deleted(&r).is_empty(), "{r:?}");
}

#[test]
fn check_folder_marker_missing_stops() {
    let h = Harness::new();
    let bare = meshvault_core::fs::SimFs::new(h.clock.clone());
    bare.put_file(&p("a.txt"), b"x");
    assert_eq!(check_folder(&bare, UID, true), FolderCheck::MarkerMissing);
    assert_eq!(check_folder(&bare, UID, false), FolderCheck::MarkerMissing);
}

#[test]
fn check_folder_marker_mismatch_stops() {
    let h = Harness::new();
    assert_eq!(
        check_folder(&h.fs, FolderUid([0x22; 16]), true),
        FolderCheck::MarkerMismatch
    );
    h.fs.meta_write("folder.id", b"not a folder id").unwrap();
    assert_eq!(check_folder(&h.fs, UID, true), FolderCheck::MarkerMismatch);
}

#[test]
fn check_folder_ok_with_matching_marker() {
    let h = Harness::new();
    h.fs.put_file(&p("a.txt"), b"x");
    assert_eq!(check_folder(&h.fs, UID, true), FolderCheck::Ok);
    assert_eq!(check_folder(&h.fs, UID, false), FolderCheck::Ok);
}

#[test]
fn check_folder_rejects_non_ntfs() {
    let h = Harness::new();
    h.fs.set_volume_kind(VolumeKind::Other("exFAT".into()));
    assert_eq!(check_folder(&h.fs, UID, false), FolderCheck::VolumeNotNtfs);
}

#[test]
fn check_folder_root_missing_stops() {
    let h = Harness::new();
    h.fs.set_root_present(false);
    assert_eq!(check_folder(&h.fs, UID, true), FolderCheck::RootMissing);
}

#[test]
fn check_folder_empty_root_with_index_stops() {
    let h = Harness::new();
    assert_eq!(
        check_folder(&h.fs, UID, true),
        FolderCheck::RootEmptyWithIndex
    );
    assert_eq!(check_folder(&h.fs, UID, false), FolderCheck::Ok);
}

#[test]
fn check_folder_unreadable_root_stops() {
    let h = Harness::new();
    h.fs.fail_read_dir(None, FsError::PermissionDenied);
    assert_eq!(check_folder(&h.fs, UID, true), FolderCheck::Unreadable);
}

const TREE: [&str; 6] = [
    "top.txt",
    "a/x.txt",
    "a/y.txt",
    "a/b/z.txt",
    "c/w.txt",
    "c/d/e/v.txt",
];

fn injected(kind: usize) -> FsError {
    match kind {
        0 => FsError::PermissionDenied,
        1 => FsError::Busy,
        2 => FsError::Io("injected".into()),
        3 => FsError::NotFound,
        _ => FsError::Crashed,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn scan_never_reports_delete_under_unknown(
        removed in proptest::collection::vec(0usize..8, 0..4),
        faults in proptest::collection::vec((1u64..90, 0usize..5), 0..10),
    ) {
        let mut h = Harness::new();
        for f in TREE {
            h.fs.put_file(&p(f), f.as_bytes());
        }
        h.scan_and_apply();

        let targets = ["top.txt", "a/x.txt", "a/b/z.txt", "c/d/e/v.txt", "a/b", "c", "a/y.txt", "c/d"];
        for r in &removed {
            h.fs.remove(&p(targets[*r]));
        }
        h.settle();

        let base = h.fs.call_count();
        h.fs.set_fault_plan(FaultPlan {
            fail: faults.iter().map(|(n, k)| (base + n, injected(*k))).collect(),
            ..FaultPlan::default()
        });
        let res = Scanner::new(&h.fs, ScanConfig::default(), IgnoreSet::default())
            .scan(&h.index, h.clock.as_ref() as &dyn Clock);
        h.fs.set_fault_plan(FaultPlan::default());

        if let Ok(report) = res {
            for gone in deleted(&report) {
                let meta = h.fs.metadata(&p(&gone)).unwrap();
                prop_assert!(meta.is_none(), "reported {gone} deleted but it exists");
            }
            for dir in &report.unknown {
                let prefix = format!("{dir}/");
                for gone in deleted(&report) {
                    prop_assert!(!gone.starts_with(&prefix), "{gone} deleted under unknown {dir}");
                }
            }
        }
    }
}
