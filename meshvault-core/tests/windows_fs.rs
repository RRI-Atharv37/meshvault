#![cfg(windows)]

use meshvault_core::fs::{EntryKind, Fs, FsError, WindowsFs};
use meshvault_core::path::RelPath;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn p(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn open(dir: &tempfile::TempDir) -> WindowsFs {
    WindowsFs::open(dir.path()).expect("temp dir is on a local NTFS volume")
}

fn put(fs: &WindowsFs, path: &str, data: &[u8]) {
    let mut w = fs.create_new(&p(path)).unwrap();
    w.write_all(data).unwrap();
    w.flush_durable().unwrap();
}

fn get(fs: &WindowsFs, path: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    fs.open_read(&p(path))
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    buf
}

fn non_ntfs_root() -> Option<PathBuf> {
    for letter in b'A'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if !root.exists() {
            continue;
        }
        let out = std::process::Command::new("fsutil")
            .args(["fsinfo", "volumeinfo", &format!("{}:", letter as char)])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if text.contains("File System Name") && !text.contains("NTFS") {
            return Some(root);
        }
    }
    None
}

fn make_junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("cmd runs");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn winfs_refuses_non_ntfs() {
    let Some(root) = non_ntfs_root() else {
        eprintln!("skipped: no non-NTFS volume available");
        return;
    };
    assert!(matches!(
        WindowsFs::open(&root),
        Err(FsError::UnsupportedVolume(_)) | Err(FsError::PermissionDenied)
    ));
}

#[test]
fn winfs_missing_root_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("nope");
    assert!(matches!(WindowsFs::open(&gone), Err(FsError::NotFound)));
}

#[test]
fn winfs_basic_round_trip_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let fs = open(&dir);
    assert_eq!(
        fs.volume().unwrap().kind,
        meshvault_core::fs::VolumeKind::Ntfs
    );
    fs.create_dir(&p("d")).unwrap();
    put(&fs, "d/a.txt", b"hello");
    assert_eq!(get(&fs, "d/a.txt"), b"hello");
    let m = fs.metadata(&p("d/a.txt")).unwrap().unwrap();
    assert_eq!((m.kind, m.size), (EntryKind::File, 5));
    assert_ne!(m.file_id, 0);
    assert_eq!(fs.metadata(&p("d")).unwrap().unwrap().kind, EntryKind::Dir);
    assert_eq!(fs.metadata(&p("nope")).unwrap(), None);
    let names: Vec<_> = fs
        .read_dir(Some(&p("d")))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a.txt"]);
    assert!(matches!(
        fs.create_new(&p("d/a.txt")),
        Err(FsError::AlreadyExists)
    ));
    fs.remove_file(&p("d/a.txt")).unwrap();
    assert_eq!(fs.metadata(&p("d/a.txt")).unwrap(), None);
}

#[test]
fn winfs_file_id_survives_rename() {
    let dir = tempfile::tempdir().unwrap();
    let fs = open(&dir);
    put(&fs, "a.bin", b"x");
    let before = fs.metadata(&p("a.bin")).unwrap().unwrap().file_id;
    fs.move_durable(&p("a.bin"), &p("b.bin"), false).unwrap();
    assert_eq!(fs.metadata(&p("b.bin")).unwrap().unwrap().file_id, before);
}

#[test]
fn winfs_meta_files_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let fs = open(&dir);
    assert_eq!(fs.meta_read("folder.id").unwrap(), None);
    fs.meta_write("folder.id", b"one").unwrap();
    fs.meta_write("folder.id", b"two").unwrap();
    assert_eq!(fs.meta_read("folder.id").unwrap().unwrap(), b"two");
    assert!(dir.path().join(".meshvault").join("folder.id").exists());
    assert!(fs.meta_write("..\\x", b"").is_err());
    assert!(fs.meta_read("a/b").is_err());
}

#[test]
fn winfs_move_durable_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let fs = open(&dir);
    put(&fs, "final.bin", b"old");
    put(&fs, "tmp.bin", b"new");
    assert_eq!(
        fs.move_durable(&p("tmp.bin"), &p("final.bin"), false),
        Err(FsError::AlreadyExists)
    );
    assert_eq!(get(&fs, "final.bin"), b"old");
    fs.move_durable(&p("tmp.bin"), &p("final.bin"), true)
        .unwrap();
    assert_eq!(get(&fs, "final.bin"), b"new");
    assert_eq!(fs.metadata(&p("tmp.bin")).unwrap(), None);
}

#[test]
fn winfs_never_follows_junction() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
    let dir = tempfile::tempdir().unwrap();
    make_junction(&dir.path().join("link"), outside.path());
    let fs = open(&dir);

    assert_eq!(
        fs.metadata(&p("link")).unwrap().unwrap().kind,
        EntryKind::Link
    );
    let listed = fs.read_dir(None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].kind, EntryKind::Link);

    assert_eq!(fs.metadata(&p("link/secret.txt")), Err(FsError::LinkInPath));
    assert!(matches!(
        fs.open_read(&p("link/secret.txt")),
        Err(FsError::LinkInPath)
    ));
    assert!(fs.open_read(&p("link")).is_err());
    assert_eq!(fs.read_dir(Some(&p("link"))), Err(FsError::LinkInPath));
    assert!(matches!(
        fs.create_new(&p("link/new.txt")),
        Err(FsError::LinkInPath)
    ));
    assert_eq!(fs.create_dir(&p("link/sub")), Err(FsError::LinkInPath));
    assert_eq!(
        fs.remove_file(&p("link/secret.txt")),
        Err(FsError::LinkInPath)
    );
    put(&fs, "real.txt", b"x");
    assert_eq!(
        fs.move_durable(&p("real.txt"), &p("link/real.txt"), true),
        Err(FsError::LinkInPath)
    );
    assert_eq!(
        fs.move_durable(&p("link/secret.txt"), &p("moved.txt"), true),
        Err(FsError::LinkInPath)
    );

    assert!(outside.path().join("secret.txt").exists());
    assert!(!outside.path().join("new.txt").exists());
    assert!(!outside.path().join("sub").exists());
    assert!(!outside.path().join("real.txt").exists());
}

#[test]
fn winfs_paths_stay_under_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("share");
    std::fs::create_dir(&root).unwrap();
    let fs = WindowsFs::open(&root).unwrap();
    let canonical_root = std::fs::canonicalize(&root).unwrap();

    let names = [
        "plain.txt",
        "with space.txt",
        "a..b",
        ".hidden",
        "\u{fc}ml\u{e4}ut.txt",
        "\u{65e5}\u{672c}\u{8a9e}",
        "PROGRA~1",
        "deep",
    ];
    fs.create_dir(&p("deep")).unwrap();
    for n in names.iter().filter(|n| **n != "deep") {
        for path in [n.to_string(), format!("deep/{n}")] {
            let rel = p(&path);
            let resolved = fs.resolve(&rel);
            assert!(resolved.starts_with(&canonical_root), "{resolved:?}");
            put(&fs, &path, b"x");
            assert_eq!(get(&fs, &path), b"x");
            assert!(resolved.exists());
            fs.move_durable(
                &rel,
                &p(&format!("moved-{}", path.replace('/', "-"))),
                false,
            )
            .unwrap();
        }
    }

    let mut outside: Vec<_> = std::fs::read_dir(parent.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    outside.sort();
    assert_eq!(outside, ["share"]);
}

#[test]
fn winfs_long_paths_work() {
    let dir = tempfile::tempdir().unwrap();
    let fs = open(&dir);
    let mut path = String::new();
    for i in 0..12 {
        if i > 0 {
            path.push('/');
        }
        path.push_str(&"d".repeat(30));
        fs.create_dir(&p(&path)).unwrap();
    }
    let file = format!("{path}/{}.bin", "f".repeat(40));
    assert!(file.len() > 260);
    put(&fs, &file, b"long");
    assert_eq!(get(&fs, &file), b"long");
}

#[test]
fn winfs_case_only_rename() {
    let dir = tempfile::tempdir().unwrap();
    let fs = open(&dir);
    put(&fs, "A.mov", b"x");
    fs.move_durable(&p("A.mov"), &p("a.mov"), false).unwrap();
    let names: Vec<_> = fs
        .read_dir(None)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a.mov"]);
}
