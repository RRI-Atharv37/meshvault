use meshvault_core::fs::{EntryKind, FaultPlan, Fs, FsError, SimFs, VolumeKind};
use meshvault_core::path::RelPath;
use meshvault_core::time::{FakeClock, Timestamp};
use std::io::{Read, Write};
use std::sync::Arc;

fn p(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn sim() -> SimFs {
    SimFs::new(Arc::new(FakeClock::new(Timestamp(1_000_000_000_000))))
}

fn write_file(fs: &SimFs, path: &str, data: &[u8], flush: bool) {
    let mut w = fs.create_new(&p(path)).unwrap();
    w.write_all(data).unwrap();
    if flush {
        w.flush_durable().unwrap();
    }
}

fn read_all(fs: &SimFs, path: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    fs.open_read(&p(path))
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    buf
}

#[test]
fn simfs_unflushed_write_lost_on_crash() {
    let fs = sim();
    write_file(&fs, "a.bin", b"data", false);
    assert!(fs.metadata(&p("a.bin")).unwrap().is_some());
    fs.crash();
    assert_eq!(fs.metadata(&p("a.bin")).unwrap(), None);
}

#[test]
fn simfs_flushed_write_survives_crash() {
    let fs = sim();
    write_file(&fs, "a.bin", b"data", true);
    fs.crash();
    assert_eq!(read_all(&fs, "a.bin"), b"data");
    assert_eq!(fs.metadata(&p("a.bin")).unwrap().unwrap().size, 4);
}

#[test]
fn simfs_torn_write_keeps_a_prefix_only_when_enabled() {
    let fs = sim();
    fs.set_fault_plan(FaultPlan {
        torn_writes: true,
        ..FaultPlan::default()
    });
    write_file(&fs, "a.bin", b"12345678", false);
    fs.crash();
    let left = fs.read_durable(&p("a.bin"));
    assert!(left.is_none() || left.as_deref() == Some(b"1234".as_slice()));
}

#[test]
fn simfs_move_durable_is_atomic() {
    let fs = sim();
    fs.put_file(&p("final.bin"), b"old");
    write_file(&fs, "tmp.bin", b"new", true);

    // A crash at the move leaves the old file and the temp file, never a mix.
    let before = fs.call_count();
    fs.set_fault_plan(FaultPlan {
        crash_at_call: Some(before + 1),
        ..FaultPlan::default()
    });
    assert_eq!(
        fs.move_durable(&p("tmp.bin"), &p("final.bin"), true),
        Err(FsError::Crashed)
    );
    assert_eq!(read_all(&fs, "final.bin"), b"old");
    assert_eq!(read_all(&fs, "tmp.bin"), b"new");

    // Without the crash the replacement is complete and durable.
    fs.set_fault_plan(FaultPlan::default());
    fs.move_durable(&p("tmp.bin"), &p("final.bin"), true)
        .unwrap();
    fs.crash();
    assert_eq!(read_all(&fs, "final.bin"), b"new");
    assert_eq!(fs.metadata(&p("tmp.bin")).unwrap(), None);
}

#[test]
fn simfs_move_of_unflushed_data_loses_the_data_not_the_name() {
    let fs = sim();
    write_file(&fs, "tmp.bin", b"unflushed", false);
    fs.move_durable(&p("tmp.bin"), &p("final.bin"), false)
        .unwrap();
    fs.crash();
    assert_eq!(fs.metadata(&p("final.bin")).unwrap().unwrap().size, 0);
}

#[test]
fn simfs_move_without_replace_fails_if_exists() {
    let fs = sim();
    fs.put_file(&p("a"), b"1");
    fs.put_file(&p("b"), b"2");
    assert_eq!(
        fs.move_durable(&p("a"), &p("b"), false),
        Err(FsError::AlreadyExists)
    );
    assert_eq!(read_all(&fs, "a"), b"1");
    assert_eq!(read_all(&fs, "b"), b"2");
    assert_eq!(
        fs.move_durable(&p("missing"), &p("c"), false),
        Err(FsError::NotFound)
    );
}

#[test]
fn simfs_move_allows_case_only_rename() {
    let fs = sim();
    fs.put_file(&p("A.mov"), b"x");
    fs.move_durable(&p("A.mov"), &p("a.mov"), false).unwrap();
    let names: Vec<_> = fs
        .read_dir(None)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a.mov"]);
}

#[test]
fn simfs_create_new_fails_if_exists() {
    let fs = sim();
    fs.put_file(&p("a"), b"1");
    assert!(matches!(
        fs.create_new(&p("a")),
        Err(FsError::AlreadyExists)
    ));
    assert!(matches!(
        fs.create_new(&p("A")),
        Err(FsError::AlreadyExists)
    ));
}

#[test]
fn simfs_is_case_insensitive_and_preserving() {
    let fs = sim();
    fs.put_file(&p("Dir/File.TXT"), b"x");
    assert!(fs.metadata(&p("dir/file.txt")).unwrap().is_some());
    let names: Vec<_> = fs
        .read_dir(Some(&p("dir")))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["File.TXT"]);
}

#[test]
fn simfs_fault_plan_crashes_at_call_n() {
    let fs = sim();
    fs.set_fault_plan(FaultPlan {
        crash_at_call: Some(3),
        ..FaultPlan::default()
    });
    let mut w = fs.create_new(&p("a")).unwrap();
    w.write_all(b"x").unwrap();
    w.flush_durable().unwrap();
    assert_eq!(fs.metadata(&p("a")), Err(FsError::Crashed));
    assert_eq!(fs.call_count(), 3);
    // The flushed file survived; the volume works again afterwards.
    assert_eq!(fs.metadata(&p("a")).unwrap().unwrap().size, 1);
}

#[test]
fn simfs_fault_plan_injects_error() {
    let fs = sim();
    fs.put_file(&p("a"), b"x");
    fs.set_fault_plan(FaultPlan {
        fail: vec![(2, FsError::Busy)],
        ..FaultPlan::default()
    });
    assert!(fs.metadata(&p("a")).unwrap().is_some());
    assert_eq!(fs.metadata(&p("a")), Err(FsError::Busy));
    assert!(fs.metadata(&p("a")).unwrap().is_some());
}

#[test]
fn simfs_links_reported_as_link() {
    let fs = sim();
    fs.put_link(&p("l"));
    fs.put_file(&p("f"), b"x");
    assert_eq!(fs.metadata(&p("l")).unwrap().unwrap().kind, EntryKind::Link);
    let mut entries = fs.read_dir(None).unwrap();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(entries[0].kind, EntryKind::File);
    assert_eq!(entries[1].kind, EntryKind::Link);
    assert!(fs.open_read(&p("l")).is_err());
}

#[test]
fn simfs_refuses_paths_through_a_link() {
    let fs = sim();
    fs.put_link(&p("l"));
    assert_eq!(fs.metadata(&p("l/x")), Err(FsError::LinkInPath));
    assert!(matches!(fs.open_read(&p("l/x")), Err(FsError::LinkInPath)));
    assert!(matches!(fs.create_new(&p("l/x")), Err(FsError::LinkInPath)));
    assert_eq!(fs.read_dir(Some(&p("l"))), Err(FsError::LinkInPath));
}

#[test]
fn simfs_free_space_enforced() {
    let fs = sim();
    fs.set_free_bytes(10);
    let mut w = fs.create_new(&p("a")).unwrap();
    w.write_all(b"1234567890").unwrap();
    let e = w.write_all(b"x").unwrap_err();
    assert_eq!(FsError::from(e), FsError::NoSpace);
    assert_eq!(fs.volume().unwrap().free_bytes, 0);
}

#[test]
fn simfs_volume_kind_configurable() {
    let fs = sim();
    assert_eq!(fs.volume().unwrap().kind, VolumeKind::Ntfs);
    fs.set_volume_kind(VolumeKind::Other("exFAT".into()));
    assert_eq!(fs.volume().unwrap().kind, VolumeKind::Other("exFAT".into()));
    fs.set_root_present(false);
    assert_eq!(fs.volume(), Err(FsError::NotFound));
}

#[test]
fn simfs_meta_files_are_durable_and_listed() {
    let fs = sim();
    assert_eq!(fs.meta_read("folder.id").unwrap(), None);
    fs.meta_write("folder.id", b"abc").unwrap();
    fs.crash();
    assert_eq!(fs.meta_read("folder.id").unwrap().unwrap(), b"abc");
    let names: Vec<_> = fs
        .read_dir(None)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, [".meshvault"]);
    assert!(fs.meta_write("../x", b"").is_err());
    assert!(fs.meta_write("a/b", b"").is_err());
}

#[test]
fn simfs_mtime_and_file_id_follow_the_rules() {
    let clock = Arc::new(FakeClock::new(Timestamp(5_000_000_000)));
    let fs = SimFs::new(clock.clone());
    fs.put_file(&p("a"), b"1");
    let m1 = fs.metadata(&p("a")).unwrap().unwrap();
    assert_eq!(m1.mtime, Timestamp(5_000_000_000));

    clock.set(Timestamp(9_000_000_000));
    fs.write_file(&p("a"), b"22");
    let m2 = fs.metadata(&p("a")).unwrap().unwrap();
    assert_eq!(
        (m2.file_id, m2.size, m2.mtime),
        (m1.file_id, 2, Timestamp(9_000_000_000))
    );

    fs.replace_file(&p("a"), b"3");
    let m3 = fs.metadata(&p("a")).unwrap().unwrap();
    assert_ne!(m3.file_id, m1.file_id);

    fs.put_dir(&p("d"));
    fs.rename(&p("a"), &p("d/b"));
    let m4 = fs.metadata(&p("d/b")).unwrap().unwrap();
    assert_eq!((m4.file_id, m4.mtime), (m3.file_id, m3.mtime));
}

#[test]
fn simfs_read_eof_hook_runs_before_eof_is_returned() {
    let fs = sim();
    fs.put_file(&p("a"), b"abc");
    fs.on_read_eof(&p("a"), |f| f.write_file(&p("a"), b"changed"));
    let mut r = fs.open_read(&p("a")).unwrap();
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"abc");
    assert_eq!(fs.read_file(&p("a")).unwrap(), b"changed");
}
