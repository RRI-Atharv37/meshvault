#![allow(dead_code)]

use meshvault_core::fs::{EntryKind, SimFs};
use meshvault_core::hash::{DeviceId, FolderUid, TeamId};
use meshvault_core::ignore::IgnoreSet;
use meshvault_core::index::{Index, IndexEntry, IndexKind};
use meshvault_core::path::RelPath;
use meshvault_core::scan::{
    write_folder_marker, EntrySnapshot, LocalChange, ScanConfig, ScanReport, Scanner,
};
use meshvault_core::time::{Clock, FakeClock, Timestamp};
use std::sync::Arc;
use std::time::Duration;

pub const UID: FolderUid = FolderUid([0x11; 16]);
pub const START: Timestamp = Timestamp(1_800_000_000_000_000_000);

pub fn p(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

pub struct Harness {
    pub clock: Arc<FakeClock>,
    pub fs: SimFs,
    pub index: Index,
    pub dir: tempfile::TempDir,
}

impl Harness {
    pub fn new() -> Harness {
        let clock = Arc::new(FakeClock::new(START));
        let fs = SimFs::new(clock.clone());
        write_folder_marker(&fs, UID).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let index = Index::create(
            &dir.path().join("index"),
            UID,
            TeamId([2; 16]),
            DeviceId([1; 32]),
        )
        .unwrap();
        Harness {
            clock,
            fs,
            index,
            dir,
        }
    }

    pub fn settle(&self) {
        self.clock.advance(Duration::from_secs(11));
    }

    pub fn scan_with(&self, ignore: IgnoreSet) -> ScanReport {
        Scanner::new(&self.fs, ScanConfig::default(), ignore)
            .scan(&self.index, self.clock.as_ref() as &dyn Clock)
            .unwrap()
    }

    pub fn scan(&self) -> ScanReport {
        self.scan_with(IgnoreSet::default())
    }

    pub fn apply(&mut self, report: &ScanReport) {
        let now = self.clock.now();
        self.index
            .write(|w| {
                for change in &report.changes {
                    match change {
                        LocalChange::Created(s) | LocalChange::Modified(s) => {
                            w.put_entry(&entry_from(s), &s.chunks)?
                        }
                        LocalChange::Deleted(path) => w.put_entry(&tombstone(path, now), &[])?,
                        LocalChange::Moved { from, to } => {
                            if from.key() != to.path.key() {
                                w.put_entry(&tombstone(from, now), &[])?;
                            }
                            w.put_entry(&entry_from(to), &to.chunks)?
                        }
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    pub fn scan_and_apply(&mut self) -> ScanReport {
        self.settle();
        let report = self.scan();
        self.apply(&report);
        report
    }

    pub fn files(&mut self, files: &[(&str, &[u8])]) {
        for (path, data) in files {
            self.fs.put_file(&p(path), data);
        }
    }
}

pub fn entry_from(s: &EntrySnapshot) -> IndexEntry {
    IndexEntry {
        path: s.path.clone(),
        kind: match s.kind {
            EntryKind::Dir => IndexKind::Dir,
            _ => IndexKind::File,
        },
        size: s.size,
        mtime: s.mtime,
        file_hash: s.file_hash,
        chunk_list_hash: None,
        vector: vec![],
        lamport: 1,
        origin: DeviceId([1; 32]),
        origin_seq: 1,
        deleted_at: None,
        suspect: false,
        file_id: Some(s.file_id),
    }
}

pub fn tombstone(path: &RelPath, at: Timestamp) -> IndexEntry {
    IndexEntry {
        path: path.clone(),
        kind: IndexKind::Tombstone,
        size: 0,
        mtime: at,
        file_hash: None,
        chunk_list_hash: None,
        vector: vec![],
        lamport: 2,
        origin: DeviceId([1; 32]),
        origin_seq: 2,
        deleted_at: Some(at),
        suspect: false,
        file_id: None,
    }
}

pub fn deleted(report: &ScanReport) -> Vec<String> {
    let mut out: Vec<String> = report
        .changes
        .iter()
        .filter_map(|c| match c {
            LocalChange::Deleted(p) => Some(p.to_string()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

pub fn created(report: &ScanReport) -> Vec<String> {
    let mut out: Vec<String> = report
        .changes
        .iter()
        .filter_map(|c| match c {
            LocalChange::Created(s) => Some(s.path.to_string()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

pub fn modified(report: &ScanReport) -> Vec<String> {
    let mut out: Vec<String> = report
        .changes
        .iter()
        .filter_map(|c| match c {
            LocalChange::Modified(s) => Some(s.path.to_string()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}
