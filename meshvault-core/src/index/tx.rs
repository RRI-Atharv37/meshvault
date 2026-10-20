//! Read and write transactions on the index.

use super::{IndexEntry, IndexError, IndexKind};
use crate::chunk::ChunkRef;
use crate::hash::{DeviceId, Hash};
use crate::path::{PathKey, RelPath};
use crate::time::Timestamp;
use rusqlite::{params, OptionalExtension, Row};

/// A read-only view of the index.
pub struct ReadTx<'a> {
    pub(super) conn: &'a rusqlite::Connection,
}

const ENTRY_COLUMNS: &str = "path, kind, size, mtime_ns, file_hash, chunk_list_hash, vector, \
     lamport, origin, origin_seq, deleted_at, suspect, file_id";

fn corrupt(what: &str) -> IndexError {
    IndexError::Corrupt(what.to_string())
}

fn to_i64(v: u64, what: &str) -> Result<i64, IndexError> {
    i64::try_from(v).map_err(|_| IndexError::Sqlite(format!("{what} out of range")))
}

fn to_u64(v: i64, what: &str) -> Result<u64, IndexError> {
    u64::try_from(v).map_err(|_| corrupt(what))
}

fn hash_from(blob: Vec<u8>) -> Result<Hash, IndexError> {
    <[u8; 32]>::try_from(blob)
        .map(Hash)
        .map_err(|_| corrupt("bad hash length"))
}

fn decode_entry(row: &Row<'_>) -> Result<IndexEntry, IndexError> {
    let path: String = row.get(0)?;
    let kind = match row.get::<_, i64>(1)? {
        0 => IndexKind::File,
        1 => IndexKind::Dir,
        2 => IndexKind::Tombstone,
        _ => return Err(corrupt("unknown entry kind")),
    };
    let optional_hash = |idx: usize| -> Result<Option<Hash>, IndexError> {
        row.get::<_, Option<Vec<u8>>>(idx)?
            .map(hash_from)
            .transpose()
    };
    let origin: Vec<u8> = row.get(8)?;
    let file_id = row
        .get::<_, Option<Vec<u8>>>(12)?
        .map(|b| <[u8; 16]>::try_from(b).map(u128::from_le_bytes))
        .transpose()
        .map_err(|_| corrupt("bad file id length"))?;
    Ok(IndexEntry {
        path: RelPath::parse(&path).map_err(|_| corrupt("invalid path in index"))?,
        kind,
        size: to_u64(row.get::<_, Option<i64>>(2)?.unwrap_or(0), "negative size")?,
        mtime: Timestamp(row.get::<_, Option<i64>>(3)?.unwrap_or(0)),
        file_hash: optional_hash(4)?,
        chunk_list_hash: optional_hash(5)?,
        vector: row.get(6)?,
        lamport: to_u64(row.get(7)?, "negative lamport")?,
        origin: DeviceId(<[u8; 32]>::try_from(origin).map_err(|_| corrupt("bad device id"))?),
        origin_seq: to_u64(row.get(9)?, "negative sequence")?,
        deleted_at: row.get::<_, Option<i64>>(10)?.map(Timestamp),
        suspect: row.get::<_, i64>(11)? != 0,
        file_id,
    })
}

impl ReadTx<'_> {
    /// A `meta` value.
    pub fn meta(&self, key: &str) -> Result<Option<Vec<u8>>, IndexError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// One path's row.
    pub fn entry(&self, key: &PathKey) -> Result<Option<IndexEntry>, IndexError> {
        let sql = format!("SELECT {ENTRY_COLUMNS} FROM entries WHERE key = ?1");
        self.conn
            .query_row(&sql, [key.as_str()], |r| Ok(decode_entry(r)))
            .optional()?
            .transpose()
    }

    /// One path's chunks in order.
    pub fn chunks(&self, key: &PathKey) -> Result<Vec<ChunkRef>, IndexError> {
        let mut stmt = self
            .conn
            .prepare("SELECT offset, len, hash FROM chunks WHERE key = ?1 ORDER BY idx")?;
        let rows = stmt.query_map([key.as_str()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (offset, len, hash) = row?;
            Ok(ChunkRef {
                offset: to_u64(offset, "negative chunk offset")?,
                len: u32::try_from(len).map_err(|_| corrupt("bad chunk length"))?,
                hash: hash_from(hash)?,
            })
        })
        .collect()
    }

    /// Every row, ordered by key.
    pub fn entries(&self) -> Result<Vec<IndexEntry>, IndexError> {
        let sql = format!("SELECT {ENTRY_COLUMNS} FROM entries ORDER BY key");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| Ok(decode_entry(r)))?;
        rows.map(|row| row?).collect()
    }
}

/// A read-write view inside one transaction.
pub struct WriteTx<'a> {
    pub(super) tx: &'a rusqlite::Transaction<'a>,
}

impl WriteTx<'_> {
    /// The read operations, on this transaction's state.
    pub fn read(&self) -> ReadTx<'_> {
        ReadTx { conn: self.tx }
    }

    /// Inserts or replaces a row together with its chunks.
    pub fn put_entry(&mut self, entry: &IndexEntry, chunks: &[ChunkRef]) -> Result<(), IndexError> {
        let key = entry.path.key();
        let kind = match entry.kind {
            IndexKind::File => 0,
            IndexKind::Dir => 1,
            IndexKind::Tombstone => 2,
        };
        self.tx
            .execute("DELETE FROM chunks WHERE key = ?1", [key.as_str()])?;
        self.tx.execute(
            "INSERT OR REPLACE INTO entries (key, path, kind, size, mtime_ns, file_hash, \
             chunk_list_hash, vector, lamport, origin, origin_seq, deleted_at, suspect, file_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                key.as_str(),
                entry.path.as_str(),
                kind,
                to_i64(entry.size, "size")?,
                entry.mtime.0,
                entry.file_hash.map(|h| h.0.to_vec()),
                entry.chunk_list_hash.map(|h| h.0.to_vec()),
                entry.vector,
                to_i64(entry.lamport, "lamport")?,
                entry.origin.0.to_vec(),
                to_i64(entry.origin_seq, "sequence")?,
                entry.deleted_at.map(|t| t.0),
                i64::from(entry.suspect),
                entry.file_id.map(|id| id.to_le_bytes().to_vec()),
            ],
        )?;
        let mut insert = self.tx.prepare(
            "INSERT INTO chunks (key, idx, offset, len, hash) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (idx, chunk) in chunks.iter().enumerate() {
            insert.execute(params![
                key.as_str(),
                idx as i64,
                to_i64(chunk.offset, "chunk offset")?,
                i64::from(chunk.len),
                chunk.hash.0.to_vec(),
            ])?;
        }
        Ok(())
    }

    /// Removes a row; its chunks go with it.
    pub fn remove_entry(&mut self, key: &PathKey) -> Result<(), IndexError> {
        self.tx
            .execute("DELETE FROM entries WHERE key = ?1", [key.as_str()])?;
        Ok(())
    }

    /// Sets a `meta` value.
    pub fn set_meta(&mut self, key: &str, value: &[u8]) -> Result<(), IndexError> {
        self.tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }
}
