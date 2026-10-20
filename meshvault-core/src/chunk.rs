//! Content-defined chunking (FastCDC) of file data.

use crate::hash::{hash_bytes, Hash, HashWriter};
use fastcdc::v2020::{Error, StreamCDC};
use std::io;

/// Smallest chunk produced, except for the last chunk of a stream.
pub const CHUNK_MIN: u32 = 256 * 1024;
/// Target average chunk size.
pub const CHUNK_AVG: u32 = 1024 * 1024;
/// Largest chunk; nothing larger is produced or accepted.
pub const CHUNK_MAX: u32 = 4 * 1024 * 1024;

/// Where a chunk sits in its file and what it hashes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRef {
    /// Byte offset of the chunk in the file.
    pub offset: u64,
    /// Length in bytes.
    pub len: u32,
    /// BLAKE3 hash of the chunk data.
    pub hash: Hash,
}

/// Iterator over the chunks of a reader.
pub struct ChunkIter<R: io::Read>(StreamCDC<R>);

impl<R: io::Read> Iterator for ChunkIter<R> {
    type Item = io::Result<(ChunkRef, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let chunk = match self.0.next()? {
            Ok(c) => c,
            Err(Error::IoError(e)) => return Some(Err(e)),
            Err(e) => return Some(Err(io::Error::other(e.to_string()))),
        };
        let len = match u32::try_from(chunk.length) {
            Ok(len) if len <= CHUNK_MAX => len,
            _ => return Some(Err(io::Error::other("chunk larger than CHUNK_MAX"))),
        };
        let hash = hash_bytes(&chunk.data);
        Some(Ok((
            ChunkRef {
                offset: chunk.offset,
                len,
                hash,
            },
            chunk.data,
        )))
    }
}

/// Splits `r` into content-defined chunks.
pub fn chunk_stream<R: io::Read>(r: R) -> ChunkIter<R> {
    ChunkIter(StreamCDC::new(
        r,
        CHUNK_MIN as usize,
        CHUNK_AVG as usize,
        CHUNK_MAX as usize,
    ))
}

/// Hash of the ordered chunk list: BLAKE3 over `offset` (u64 LE), `len` (u32 LE) and `hash` of each chunk.
pub fn chunk_list_hash(chunks: &[ChunkRef]) -> Hash {
    let mut w = HashWriter::new();
    for c in chunks {
        w.update(&c.offset.to_le_bytes());
        w.update(&c.len.to_le_bytes());
        w.update(&c.hash.0);
    }
    w.finish()
}
