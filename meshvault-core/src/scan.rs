//! Finds local changes by comparing the filesystem with the index. An error is never a deletion.

use crate::chunk::{chunk_stream, ChunkRef};
use crate::fs::{DirEntry, EntryKind, Fs, FsError, Meta, VolumeKind};
use crate::hash::{FolderUid, Hash, HashWriter};
use crate::ignore::IgnoreSet;
use crate::index::{Index, IndexEntry, IndexError, IndexKind};
use crate::path::{fold, PathError, RelPath, META_DIR};
use crate::time::{Clock, Timestamp};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// The marker file inside the metadata directory that names the folder.
pub const FOLDER_ID_FILE: &str = "folder.id";

const MAX_ATTEMPTS: usize = 3;

/// Scanner timing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanConfig {
    /// A file modified more recently than this is deferred.
    pub quiet_window: Duration,
    /// How often a full reconcile runs even if the watcher is silent.
    pub reconcile_every: Duration,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            quiet_window: Duration::from_secs(10),
            reconcile_every: Duration::from_secs(30 * 60),
        }
    }
}

/// What a scan found out about one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrySnapshot {
    /// The path as it is on disk now.
    pub path: RelPath,
    /// File or directory.
    pub kind: EntryKind,
    /// Size in bytes.
    pub size: u64,
    /// Last write time.
    pub mtime: Timestamp,
    /// BLAKE3 of the content, for files.
    pub file_hash: Option<Hash>,
    /// The content's chunks, for files.
    pub chunks: Vec<ChunkRef>,
    /// Volume file ID.
    pub file_id: u128,
}

/// A change found locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalChange {
    /// A path that was not in the index.
    Created(EntrySnapshot),
    /// A path whose content or kind changed.
    Modified(EntrySnapshot),
    /// A path that is gone, found under a directory listed without error.
    Deleted(RelPath),
    /// The same content at a new path (or the same path in a different case).
    Moved {
        /// The old path.
        from: RelPath,
        /// The new state.
        to: EntrySnapshot,
    },
}

/// Why a path was left out of a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// A symlink, junction or other reparse point.
    Link,
    /// Access denied.
    PermissionDenied,
    /// Locked by another process.
    Busy,
    /// Any other read failure.
    ReadError,
    /// A name the path normaliser refuses.
    BadName(PathError),
    /// Matched an ignore pattern.
    Ignored,
}

/// The outcome of one scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Changes to record.
    pub changes: Vec<LocalChange>,
    /// Roots of subtrees that could not be listed completely; their old entries stand.
    pub unknown: Vec<RelPath>,
    /// Paths skipped and why; a path that is not a valid [`RelPath`] is shown as listed.
    pub skipped: Vec<(String, SkipReason)>,
    /// Files still being written or modified within the quiet window; retried next scan.
    pub deferred: Vec<RelPath>,
}

/// Whether a folder is safe to scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderCheck {
    /// Safe to scan.
    Ok,
    /// No `folder.id` marker.
    MarkerMissing,
    /// The marker names a different folder, or is unreadable as one.
    MarkerMismatch,
    /// The volume is not local NTFS.
    VolumeNotNtfs,
    /// The root is gone.
    RootMissing,
    /// The root is empty but the index says it holds files.
    RootEmptyWithIndex,
    /// The root or marker could not be read.
    Unreadable,
}

/// Why a scan could not run.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    /// The share root could not be listed; nothing can be concluded about any path.
    #[error("cannot list the share root: {0}")]
    Root(FsError),
    /// The index could not be read.
    #[error("index: {0}")]
    Index(#[from] IndexError),
}

/// Decides whether the folder at `fs` is the one the index describes. Anything but `Ok` means the
/// folder stops; nothing is treated as deleted.
pub fn check_folder(fs: &dyn Fs, expected: FolderUid, index_has_files: bool) -> FolderCheck {
    match fs.volume() {
        Ok(v) if v.kind == VolumeKind::Ntfs => {}
        Ok(_) => return FolderCheck::VolumeNotNtfs,
        Err(FsError::NotFound) => return FolderCheck::RootMissing,
        Err(_) => return FolderCheck::Unreadable,
    }
    match fs.meta_read(FOLDER_ID_FILE) {
        Ok(None) => return FolderCheck::MarkerMissing,
        Err(_) => return FolderCheck::Unreadable,
        Ok(Some(bytes)) => {
            let found = String::from_utf8(bytes)
                .ok()
                .and_then(|text| FolderUid::from_hex(text.trim()));
            if found != Some(expected) {
                return FolderCheck::MarkerMismatch;
            }
        }
    }
    if index_has_files {
        match fs.read_dir(None) {
            Ok(entries) => {
                if entries.iter().all(|e| fold(&e.name) == fold(META_DIR)) {
                    return FolderCheck::RootEmptyWithIndex;
                }
            }
            Err(_) => return FolderCheck::Unreadable,
        }
    }
    FolderCheck::Ok
}

/// Writes the marker for a new folder.
pub fn write_folder_marker(fs: &dyn Fs, uid: FolderUid) -> Result<(), FsError> {
    fs.meta_write(FOLDER_ID_FILE, uid.to_hex().as_bytes())
}

/// Walks a share and reports what changed since the index.
pub struct Scanner<'a> {
    fs: &'a dyn Fs,
    cfg: ScanConfig,
    ignore: IgnoreSet,
}

#[derive(PartialEq, Eq)]
enum Seen {
    Dir,
    File,
    Other,
}

enum Hashed {
    Done(EntrySnapshot),
    Deferred,
    Skipped(SkipReason),
}

struct ScanState {
    now: Timestamp,
    live: HashMap<String, IndexEntry>,
    seen: HashMap<String, Seen>,
    unknown_dirs: HashSet<String>,
    report: ScanReport,
    created: Vec<EntrySnapshot>,
}

fn reason_for(e: &FsError) -> SkipReason {
    match e {
        FsError::PermissionDenied => SkipReason::PermissionDenied,
        FsError::Busy => SkipReason::Busy,
        _ => SkipReason::ReadError,
    }
}

fn display(parent: Option<&RelPath>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}/{name}"),
        None => name.to_string(),
    }
}

fn ancestors_top_down(p: &RelPath) -> Vec<RelPath> {
    let mut out = Vec::new();
    let mut cur = p.parent();
    while let Some(c) = cur {
        cur = c.parent();
        out.push(c);
    }
    out.reverse();
    out
}

impl<'a> Scanner<'a> {
    /// A scanner over `fs`.
    pub fn new(fs: &'a dyn Fs, cfg: ScanConfig, ignore: IgnoreSet) -> Scanner<'a> {
        Scanner { fs, cfg, ignore }
    }

    /// One full scan. A deletion is reported only for a path whose parent directory was listed
    /// without error; anything else leaves the old index entry standing.
    pub fn scan(&self, index: &Index, clock: &dyn Clock) -> Result<ScanReport, ScanError> {
        let entries = index.read(|r| r.entries())?;
        let mut st = ScanState {
            now: clock.now(),
            live: entries
                .into_iter()
                .filter(|e| e.kind != IndexKind::Tombstone)
                .map(|e| (e.path.key().as_str().to_string(), e))
                .collect(),
            seen: HashMap::new(),
            unknown_dirs: HashSet::new(),
            report: ScanReport::default(),
            created: Vec::new(),
        };

        let root = self.fs.read_dir(None).map_err(ScanError::Root)?;
        let mut pending: Vec<(Option<RelPath>, Vec<DirEntry>)> = vec![(None, root)];
        while let Some((parent, mut listing)) = pending.pop() {
            listing.sort_by_key(|e| fold(&e.name));
            let mut subdirs = Vec::new();
            for entry in listing {
                if parent.is_none() && fold(&entry.name) == fold(META_DIR) {
                    continue;
                }
                let joined = match &parent {
                    Some(p) => p.join(&entry.name),
                    None => RelPath::parse(&entry.name),
                };
                let path = match joined {
                    Ok(p) => p,
                    Err(e) => {
                        st.report.skipped.push((
                            display(parent.as_ref(), &entry.name),
                            SkipReason::BadName(e),
                        ));
                        continue;
                    }
                };
                let key = path.key().as_str().to_string();
                if self.ignore.is_ignored(&path) {
                    st.seen.insert(key, Seen::Other);
                    st.report
                        .skipped
                        .push((path.to_string(), SkipReason::Ignored));
                    continue;
                }
                match entry.kind {
                    EntryKind::Link => {
                        st.seen.insert(key, Seen::Other);
                        st.report.skipped.push((path.to_string(), SkipReason::Link));
                    }
                    EntryKind::Dir => {
                        if self.visit_dir(&mut st, path.clone(), &key) {
                            subdirs.push(path);
                        }
                    }
                    EntryKind::File => self.visit_file(index, &mut st, path, key)?,
                }
            }
            for dir in subdirs {
                match self.fs.read_dir(Some(&dir)) {
                    Ok(listing) => pending.push((Some(dir), listing)),
                    Err(e) => {
                        st.unknown_dirs.insert(dir.key().as_str().to_string());
                        st.report.skipped.push((dir.to_string(), reason_for(&e)));
                        st.report.unknown.push(dir);
                    }
                }
            }
        }
        Ok(self.finish(st))
    }

    fn visit_dir(&self, st: &mut ScanState, path: RelPath, key: &str) -> bool {
        st.seen.insert(key.to_string(), Seen::Dir);
        let meta = match self.fs.metadata(&path) {
            Ok(Some(m)) if m.kind == EntryKind::Dir => m,
            other => {
                let reason = other
                    .as_ref()
                    .err()
                    .map_or(SkipReason::ReadError, reason_for);
                st.unknown_dirs.insert(key.to_string());
                st.report.skipped.push((path.to_string(), reason));
                st.report.unknown.push(path);
                return false;
            }
        };
        let snapshot = EntrySnapshot {
            path: path.clone(),
            kind: EntryKind::Dir,
            size: 0,
            mtime: meta.mtime,
            file_hash: None,
            chunks: Vec::new(),
            file_id: meta.file_id,
        };
        match st.live.get(key) {
            Some(old) if old.kind == IndexKind::Dir => {
                if old.path != path {
                    st.report.changes.push(LocalChange::Moved {
                        from: old.path.clone(),
                        to: snapshot,
                    });
                }
            }
            Some(_) => st.report.changes.push(LocalChange::Modified(snapshot)),
            None => st.report.changes.push(LocalChange::Created(snapshot)),
        }
        true
    }

    fn visit_file(
        &self,
        index: &Index,
        st: &mut ScanState,
        path: RelPath,
        key: String,
    ) -> Result<(), ScanError> {
        st.seen.insert(key.clone(), Seen::File);
        let meta = match self.fs.metadata(&path) {
            Ok(Some(m)) if m.kind == EntryKind::File => m,
            other => {
                let reason = other
                    .as_ref()
                    .err()
                    .map_or(SkipReason::ReadError, reason_for);
                st.report.skipped.push((path.to_string(), reason));
                return Ok(());
            }
        };
        let old = st.live.get(&key);
        if let Some(old) = old {
            let unchanged = old.kind == IndexKind::File
                && old.size == meta.size
                && old.mtime == meta.mtime
                && old.file_id == Some(meta.file_id)
                && old.file_hash.is_some();
            if unchanged {
                if old.path != path {
                    let chunks = index.read(|r| r.chunks(&path.key()))?;
                    let snapshot = EntrySnapshot {
                        path,
                        kind: EntryKind::File,
                        size: meta.size,
                        mtime: meta.mtime,
                        file_hash: old.file_hash,
                        chunks,
                        file_id: meta.file_id,
                    };
                    st.report.changes.push(LocalChange::Moved {
                        from: old.path.clone(),
                        to: snapshot,
                    });
                }
                return Ok(());
            }
        }
        let known = old.is_some();
        match self.hash_file(st.now, &path, meta) {
            Hashed::Done(snapshot) if known => {
                st.report.changes.push(LocalChange::Modified(snapshot));
            }
            Hashed::Done(snapshot) => st.created.push(snapshot),
            Hashed::Deferred => st.report.deferred.push(path),
            Hashed::Skipped(reason) => st.report.skipped.push((path.to_string(), reason)),
        }
        Ok(())
    }

    fn hash_file(&self, now: Timestamp, path: &RelPath, first: Meta) -> Hashed {
        let quiet_ns = i64::try_from(self.cfg.quiet_window.as_nanos()).unwrap_or(i64::MAX);
        let mut before = first;
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                before = match self.fs.metadata(path) {
                    Ok(Some(m)) if m.kind == EntryKind::File => m,
                    Ok(_) => return Hashed::Skipped(SkipReason::ReadError),
                    Err(e) => return Hashed::Skipped(reason_for(&e)),
                };
            }
            if before.mtime.0 > now.0.saturating_sub(quiet_ns) {
                return Hashed::Deferred;
            }
            let reader = match self.fs.open_read(path) {
                Ok(r) => r,
                Err(e) => return Hashed::Skipped(reason_for(&e)),
            };
            let mut whole = HashWriter::new();
            let mut chunks = Vec::new();
            let mut total = 0u64;
            for item in chunk_stream(reader) {
                match item {
                    Ok((chunk, data)) => {
                        whole.update(&data);
                        total += u64::from(chunk.len);
                        chunks.push(chunk);
                    }
                    Err(e) => return Hashed::Skipped(reason_for(&FsError::from(e))),
                }
            }
            let after = match self.fs.metadata(path) {
                Ok(Some(m)) => m,
                Ok(None) => return Hashed::Skipped(SkipReason::ReadError),
                Err(e) => return Hashed::Skipped(reason_for(&e)),
            };
            if after == before && total == before.size {
                return Hashed::Done(EntrySnapshot {
                    path: path.clone(),
                    kind: EntryKind::File,
                    size: before.size,
                    mtime: before.mtime,
                    file_hash: Some(whole.finish()),
                    chunks,
                    file_id: before.file_id,
                });
            }
        }
        Hashed::Deferred
    }

    fn finish(&self, st: ScanState) -> ScanReport {
        let ScanState {
            live,
            seen,
            unknown_dirs,
            mut report,
            created,
            ..
        } = st;

        let mut gone: Vec<&IndexEntry> = live
            .iter()
            .filter(|(key, _)| !seen.contains_key(*key))
            .map(|(_, e)| e)
            .filter(|e| !self.ignore.is_ignored(&e.path))
            .filter(|e| {
                for ancestor in ancestors_top_down(&e.path) {
                    let akey = ancestor.key();
                    match seen.get(akey.as_str()) {
                        None | Some(Seen::File) => return true,
                        Some(Seen::Dir) if !unknown_dirs.contains(akey.as_str()) => {}
                        Some(_) => return false,
                    }
                }
                true
            })
            .collect();
        gone.sort_by(|a, b| a.path.cmp(&b.path));

        let mut taken = vec![false; gone.len()];
        for snapshot in created {
            let source = snapshot.file_hash.and_then(|hash| {
                gone.iter().enumerate().position(|(i, e)| {
                    !taken[i] && e.kind == IndexKind::File && e.file_hash == Some(hash)
                })
            });
            match source {
                Some(i) => {
                    taken[i] = true;
                    report.changes.push(LocalChange::Moved {
                        from: gone[i].path.clone(),
                        to: snapshot,
                    });
                }
                None => report.changes.push(LocalChange::Created(snapshot)),
            }
        }
        for (entry, moved) in gone.iter().zip(taken) {
            if !moved {
                report
                    .changes
                    .push(LocalChange::Deleted(entry.path.clone()));
            }
        }
        report
    }
}
