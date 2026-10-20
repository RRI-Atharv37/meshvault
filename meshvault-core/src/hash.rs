//! BLAKE3 content hashes.

use std::fmt;

/// A BLAKE3 hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// The all-zero hash, used as "no content".
    pub const ZERO: Hash = Hash([0; 32]);
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Hashes a byte slice.
pub fn hash_bytes(b: &[u8]) -> Hash {
    Hash(*blake3::hash(b).as_bytes())
}

/// Incremental hasher; produces the same result as [`hash_bytes`] over the concatenated input.
pub struct HashWriter(blake3::Hasher);

impl HashWriter {
    /// Starts an empty hash.
    pub fn new() -> Self {
        HashWriter(blake3::Hasher::new())
    }

    /// Feeds more bytes.
    pub fn update(&mut self, b: &[u8]) {
        self.0.update(b);
    }

    /// Returns the hash of everything fed so far.
    pub fn finish(&self) -> Hash {
        Hash(*self.0.finalize().as_bytes())
    }
}

impl Default for HashWriter {
    fn default() -> Self {
        Self::new()
    }
}
