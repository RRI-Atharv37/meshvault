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

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for b in bytes {
        write!(f, "{b:02x}")?;
    }
    Ok(())
}

/// A device's Ed25519 public key. Displays as its first 8 hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub [u8; 32]);

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0[..4])
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Identifies a team.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct TeamId(pub [u8; 16]);

/// The random identity stored in a folder's `folder.id` marker.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FolderUid(pub [u8; 16]);

impl FolderUid {
    /// Lowercase hex, 32 characters.
    pub fn to_hex(&self) -> String {
        self.to_string()
    }

    /// Parses exactly 32 hex characters, either case.
    pub fn from_hex(s: &str) -> Option<FolderUid> {
        if s.len() != 32 || !s.is_ascii() {
            return None;
        }
        let mut out = [0u8; 16];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(FolderUid(out))
    }
}

impl fmt::Display for FolderUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for FolderUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
