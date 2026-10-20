use meshvault_core::chunk::ChunkRef;
use meshvault_core::hash::{hash_bytes, DeviceId, FolderUid, TeamId};
use meshvault_core::index::{meta_keys, Index, IndexEntry, IndexError, IndexKind};
use meshvault_core::path::RelPath;
use meshvault_core::time::Timestamp;
use std::path::PathBuf;

const UID: FolderUid = FolderUid([7; 16]);
const TEAM: TeamId = TeamId([3; 16]);
const DEV: DeviceId = DeviceId([9; 32]);

fn db_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("index")
}

fn entry(path: &str) -> IndexEntry {
    IndexEntry {
        path: RelPath::parse(path).unwrap(),
        kind: IndexKind::File,
        size: 12,
        mtime: Timestamp(1_700_000_000_123_456_789),
        file_hash: Some(hash_bytes(b"content")),
        chunk_list_hash: Some(hash_bytes(b"chunks")),
        vector: vec![1, 2, 3],
        lamport: 5,
        origin: DEV,
        origin_seq: 4,
        deleted_at: None,
        suspect: false,
        file_id: Some(u128::MAX - 3),
    }
}

fn chunks() -> Vec<ChunkRef> {
    vec![
        ChunkRef {
            offset: 0,
            len: 7,
            hash: hash_bytes(b"c1"),
        },
        ChunkRef {
            offset: 7,
            len: 5,
            hash: hash_bytes(b"c2"),
        },
    ]
}

#[test]
fn index_create_and_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let idx = Index::create(&path, UID, TEAM, DEV).unwrap();
    drop(idx);
    let idx = Index::open(&path, UID).unwrap();
    idx.read(|r| {
        assert_eq!(
            r.meta(meta_keys::FOLDER_UID).unwrap().unwrap(),
            UID.0.to_vec()
        );
        assert_eq!(
            r.meta(meta_keys::TEAM_ID).unwrap().unwrap(),
            TEAM.0.to_vec()
        );
        assert_eq!(
            r.meta(meta_keys::LOCAL_DEVICE).unwrap().unwrap(),
            DEV.0.to_vec()
        );
        assert_eq!(
            r.meta(meta_keys::SCHEMA_VERSION).unwrap().unwrap(),
            1u32.to_le_bytes()
        );
        assert!(r.entries().unwrap().is_empty());
    });
}

#[test]
fn index_create_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    Index::create(&path, UID, TEAM, DEV).unwrap();
    assert!(matches!(
        Index::create(&path, UID, TEAM, DEV),
        Err(IndexError::AlreadyExists)
    ));
}

#[test]
fn index_open_missing_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Index::open(&db_path(&dir), UID).is_err());
}

#[test]
fn index_uses_wal_and_full_sync() {
    let dir = tempfile::tempdir().unwrap();
    let idx = Index::create(&db_path(&dir), UID, TEAM, DEV).unwrap();
    let p = idx.pragmas().unwrap();
    assert_eq!(p.journal_mode, "wal");
    assert_eq!(p.synchronous, 2);
    assert!(p.foreign_keys);
    drop(idx);
    let p = Index::open(&db_path(&dir), UID).unwrap().pragmas().unwrap();
    assert_eq!(
        (p.journal_mode.as_str(), p.synchronous, p.foreign_keys),
        ("wal", 2, true)
    );
}

#[test]
fn index_integrity_check_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let mut idx = Index::create(&path, UID, TEAM, DEV).unwrap();
    idx.write(|w| {
        for i in 0..200 {
            w.put_entry(&entry(&format!("dir/file{i}.mov")), &chunks())?;
        }
        Ok(())
    })
    .unwrap();
    drop(idx);

    let mut bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() > 3 * 4096);
    for b in &mut bytes[4096..3 * 4096] {
        *b = 0xA5;
    }
    std::fs::write(&path, &bytes).unwrap();
    let _ = std::fs::remove_file(dir.path().join("index-wal"));

    assert!(matches!(
        Index::open(&path, UID),
        Err(IndexError::Corrupt(_))
    ));
}

#[test]
fn index_open_garbage_file_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    std::fs::write(&path, vec![0x42; 10_000]).unwrap();
    assert!(matches!(
        Index::open(&path, UID),
        Err(IndexError::Corrupt(_))
    ));
}

#[test]
fn index_wrong_folder_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    drop(Index::create(&path, UID, TEAM, DEV).unwrap());
    assert!(matches!(
        Index::open(&path, FolderUid([8; 16])),
        Err(IndexError::WrongFolder)
    ));
}

#[test]
fn index_schema_version_checked() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    drop(Index::create(&path, UID, TEAM, DEV).unwrap());
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
            [99u32.to_le_bytes().to_vec()],
        )
        .unwrap();
    }
    assert!(matches!(
        Index::open(&path, UID),
        Err(IndexError::SchemaVersion(99))
    ));
}

#[test]
fn index_write_tx_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = Index::create(&db_path(&dir), UID, TEAM, DEV).unwrap();
    let result: Result<(), IndexError> = idx.write(|w| {
        w.put_entry(&entry("a.mov"), &chunks())?;
        w.set_meta("local_seq", &[1])?;
        Err(IndexError::Sqlite("boom".into()))
    });
    assert_eq!(result, Err(IndexError::Sqlite("boom".into())));
    idx.read(|r| {
        assert!(r
            .entry(&RelPath::parse("a.mov").unwrap().key())
            .unwrap()
            .is_none());
        assert!(r.meta("local_seq").unwrap().is_none());
    });

    idx.write(|w| w.put_entry(&entry("a.mov"), &chunks()))
        .unwrap();
    idx.read(|r| {
        assert!(r
            .entry(&RelPath::parse("a.mov").unwrap().key())
            .unwrap()
            .is_some())
    });
}

#[test]
fn index_entry_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = Index::create(&db_path(&dir), UID, TEAM, DEV).unwrap();
    let mut tomb = entry("gone.txt");
    tomb.kind = IndexKind::Tombstone;
    tomb.file_hash = None;
    tomb.chunk_list_hash = None;
    tomb.deleted_at = Some(Timestamp(42));
    tomb.suspect = true;
    tomb.file_id = None;
    let mut dir_entry = entry("d");
    dir_entry.kind = IndexKind::Dir;
    dir_entry.file_hash = None;
    dir_entry.chunk_list_hash = None;

    idx.write(|w| {
        w.put_entry(&entry("Dir/File.MOV"), &chunks())?;
        w.put_entry(&tomb, &[])?;
        w.put_entry(&dir_entry, &[])
    })
    .unwrap();

    idx.read(|r| {
        let key = RelPath::parse("dir/file.mov").unwrap().key();
        let got = r.entry(&key).unwrap().unwrap();
        assert_eq!(got, entry("Dir/File.MOV"));
        assert_eq!(got.path.as_str(), "Dir/File.MOV");
        assert_eq!(r.chunks(&key).unwrap(), chunks());
        assert_eq!(r.entry(&tomb.path.key()).unwrap().unwrap(), tomb);
        assert_eq!(r.entry(&dir_entry.path.key()).unwrap().unwrap(), dir_entry);
        assert_eq!(r.entries().unwrap().len(), 3);
    });

    idx.write(|w| {
        let mut changed = entry("Dir/File.MOV");
        changed.size = 99;
        w.put_entry(&changed, &chunks()[..1])
    })
    .unwrap();
    idx.read(|r| {
        let key = RelPath::parse("dir/file.mov").unwrap().key();
        assert_eq!(r.entry(&key).unwrap().unwrap().size, 99);
        assert_eq!(r.chunks(&key).unwrap(), chunks()[..1].to_vec());
    });
}

#[test]
fn index_chunks_cascade_on_entry_delete() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let mut idx = Index::create(&path, UID, TEAM, DEV).unwrap();
    idx.write(|w| w.put_entry(&entry("a.mov"), &chunks()))
        .unwrap();

    let count = || -> i64 {
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(), 2);
    idx.write(|w| w.remove_entry(&RelPath::parse("A.MOV").unwrap().key()))
        .unwrap();
    assert_eq!(count(), 0);
    idx.read(|r| assert!(r.entries().unwrap().is_empty()));
}

#[test]
#[cfg(windows)]
#[ignore = "slow: spawns and kills a child process"]
fn index_survives_process_kill() {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    drop(Index::create(&path, UID, TEAM, DEV).unwrap());

    for round in 0..3u64 {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "index_kill_child_writer",
                "--nocapture",
                "--include-ignored",
            ])
            .env("MESHVAULT_INDEX_KILL_CHILD", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(400 + 150 * round));
        child.kill().unwrap();
        child.wait().unwrap();

        let idx = Index::open(&path, UID).expect("integrity check passes after kill");
        idx.read(|r| {
            let all = r.entries().unwrap();
            for e in &all {
                assert_eq!(
                    r.chunks(&e.path.key()).unwrap().len(),
                    2,
                    "no half-written entry"
                );
            }
        });
    }
}

#[test]
#[cfg(windows)]
#[ignore = "child of index_survives_process_kill"]
fn index_kill_child_writer() {
    let Ok(path) = std::env::var("MESHVAULT_INDEX_KILL_CHILD") else {
        return;
    };
    let mut idx = Index::open(&PathBuf::from(path), UID).unwrap();
    let mut i = 0u64;
    loop {
        idx.write(|w| {
            w.put_entry(&entry(&format!("k/file{i}.mov")), &chunks())?;
            w.put_entry(&entry(&format!("k/other{i}.mov")), &chunks())
        })
        .unwrap();
        i += 1;
    }
}
