//! The per-folder SQLite index: what the engine believes every path currently is.

use crate::hash::{DeviceId, FolderUid, Hash, TeamId};
use crate::path::RelPath;
use crate::time::Timestamp;
use rusqlite::{Connection, ErrorCode, OpenFlags, TransactionBehavior};
use std::io;
use std::path::Path;

mod tx;

pub use tx::{ReadTx, WriteTx};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = include_str!("schema.sql");

/// Why the index could not be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    /// The database failed its integrity check or is not a database; the folder stops.
    #[error("index is corrupt: {0}")]
    Corrupt(String),
    /// The database was written by a different schema version.
    #[error("unsupported index schema version {0}")]
    SchemaVersion(u32),
    /// The database belongs to a different folder.
    #[error("index belongs to a different folder")]
    WrongFolder,
    /// `create` refused to overwrite an existing database.
    #[error("index already exists")]
    AlreadyExists,
    /// Any other database failure.
    #[error("index database error: {0}")]
    Sqlite(String),
}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> IndexError {
        match e.sqlite_error_code() {
            Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => {
                IndexError::Corrupt(e.to_string())
            }
            _ => IndexError::Sqlite(e.to_string()),
        }
    }
}

/// What an index row records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A deleted path, kept so the deletion can propagate.
    Tombstone,
}

/// One path's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    /// The path as stored.
    pub path: RelPath,
    /// File, directory or tombstone.
    pub kind: IndexKind,
    /// Size in bytes.
    pub size: u64,
    /// Last write time.
    pub mtime: Timestamp,
    /// BLAKE3 of the content, for files.
    pub file_hash: Option<Hash>,
    /// Hash over the chunk list, for files.
    pub chunk_list_hash: Option<Hash>,
    /// Encoded version vector.
    pub vector: Vec<u8>,
    /// Lamport counter.
    pub lamport: u64,
    /// The device whose update produced this version.
    pub origin: DeviceId,
    /// That device's sequence number for the update.
    pub origin_seq: u64,
    /// When the path was deleted, for tombstones.
    pub deleted_at: Option<Timestamp>,
    /// Set when a peer's claim about this path failed verification.
    pub suspect: bool,
    /// Volume file ID last seen for this path.
    pub file_id: Option<u128>,
}

/// The database settings in force, for checks and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pragmas {
    /// Journal mode, lowercase (`wal`).
    pub journal_mode: String,
    /// Synchronous level (2 = FULL).
    pub synchronous: u32,
    /// Whether foreign keys are enforced.
    pub foreign_keys: bool,
}

/// Keys used in the `meta` table.
pub mod meta_keys {
    /// Schema version, 4 bytes little endian.
    pub const SCHEMA_VERSION: &str = "schema_version";
    /// The folder's [`FolderUid`](crate::hash::FolderUid), 16 bytes.
    pub const FOLDER_UID: &str = "folder_uid";
    /// The team's [`TeamId`](crate::hash::TeamId), 16 bytes.
    pub const TEAM_ID: &str = "team_id";
    /// This device's [`DeviceId`](crate::hash::DeviceId), 32 bytes.
    pub const LOCAL_DEVICE: &str = "local_device";
}

/// An open, verified index.
pub struct Index {
    conn: Connection,
}

fn configure(conn: &Connection) -> Result<(), IndexError> {
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(IndexError::Sqlite("could not enable WAL".into()));
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    Ok(())
}

fn verify_integrity(conn: &Connection) -> Result<(), IndexError> {
    let report = |e: rusqlite::Error| IndexError::Corrupt(e.to_string());
    let mut stmt = conn.prepare("PRAGMA integrity_check").map_err(report)?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(report)?
        .collect::<Result<_, _>>()
        .map_err(report)?;
    if rows.len() != 1 || rows[0] != "ok" {
        return Err(IndexError::Corrupt(
            rows.into_iter().next().unwrap_or_default(),
        ));
    }
    let mut stmt = conn.prepare("PRAGMA foreign_key_check").map_err(report)?;
    if stmt.exists([]).map_err(report)? {
        return Err(IndexError::Corrupt("foreign key violation".into()));
    }
    Ok(())
}

impl Index {
    /// Creates a new database for a folder. Fails if one exists.
    pub fn create(
        path: &Path,
        folder: FolderUid,
        team: TeamId,
        device: DeviceId,
    ) -> Result<Index, IndexError> {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(IndexError::AlreadyExists)
            }
            Err(e) => return Err(IndexError::Sqlite(e.kind().to_string())),
        }
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&conn)?;
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA)?;
        for (key, value) in [
            (
                meta_keys::SCHEMA_VERSION,
                SCHEMA_VERSION.to_le_bytes().to_vec(),
            ),
            (meta_keys::FOLDER_UID, folder.0.to_vec()),
            (meta_keys::TEAM_ID, team.0.to_vec()),
            (meta_keys::LOCAL_DEVICE, device.0.to_vec()),
        ] {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(Index { conn })
    }

    /// Opens an existing database after an integrity check and folder and schema checks.
    pub fn open(path: &Path, expected: FolderUid) -> Result<Index, IndexError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&conn)?;
        verify_integrity(&conn)?;
        let index = Index { conn };
        index.read(|r| -> Result<(), IndexError> {
            let version = r
                .meta(meta_keys::SCHEMA_VERSION)
                .map_err(|_| IndexError::Corrupt("not a MeshVault index".into()))?
                .ok_or_else(|| IndexError::Corrupt("missing schema version".into()))?;
            let version = <[u8; 4]>::try_from(version)
                .map(u32::from_le_bytes)
                .map_err(|_| IndexError::Corrupt("bad schema version".into()))?;
            if version != SCHEMA_VERSION {
                return Err(IndexError::SchemaVersion(version));
            }
            let uid = r
                .meta(meta_keys::FOLDER_UID)?
                .ok_or_else(|| IndexError::Corrupt("missing folder id".into()))?;
            if uid != expected.0 {
                return Err(IndexError::WrongFolder);
            }
            Ok(())
        })?;
        Ok(index)
    }

    /// The settings in force on this connection.
    pub fn pragmas(&self) -> Result<Pragmas, IndexError> {
        let journal_mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let synchronous: u32 = self
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))?;
        let foreign_keys: i64 = self
            .conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
        Ok(Pragmas {
            journal_mode: journal_mode.to_lowercase(),
            synchronous,
            foreign_keys: foreign_keys != 0,
        })
    }

    /// Runs `f` against a consistent snapshot.
    pub fn read<T>(&self, f: impl FnOnce(&ReadTx<'_>) -> T) -> T {
        match self.conn.unchecked_transaction() {
            Ok(tx) => f(&ReadTx { conn: &tx }),
            Err(_) => f(&ReadTx { conn: &self.conn }),
        }
    }

    /// Runs `f` in one transaction: committed if it returns `Ok`, rolled back otherwise.
    pub fn write<T>(
        &mut self,
        f: impl FnOnce(&mut WriteTx<'_>) -> Result<T, IndexError>,
    ) -> Result<T, IndexError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&mut WriteTx { tx: &tx })?;
        tx.commit()?;
        Ok(value)
    }
}
