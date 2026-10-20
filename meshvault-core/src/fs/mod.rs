//! The filesystem as the engine sees it: a small trait, a simulated backend with crash injection,
//! and the real Windows backend.

use crate::path::RelPath;
use crate::time::Timestamp;
use std::io;

pub mod sim;
#[cfg(windows)]
pub mod windows;

pub use sim::{FaultPlan, SimFs};
#[cfg(windows)]
pub use windows::WindowsFs;

/// What a directory entry or a path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symlink, junction or any other reparse point; never followed.
    Link,
}

/// What `metadata` reports about a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    /// File, directory or link.
    pub kind: EntryKind,
    /// Size in bytes (0 for directories and links).
    pub size: u64,
    /// Last write time.
    pub mtime: Timestamp,
    /// Stable identity of the file on its volume; survives renames.
    pub file_id: u128,
}

/// One name in a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// The name as the filesystem reports it, not validated.
    pub name: String,
    /// File, directory or link.
    pub kind: EntryKind,
}

/// The filesystem type of the share's volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeKind {
    /// Local NTFS, the only supported kind.
    Ntfs,
    /// Anything else, named as reported.
    Other(String),
}

/// The share's volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Filesystem type.
    pub kind: VolumeKind,
    /// Bytes available to this user.
    pub free_bytes: u64,
}

/// A filesystem failure, already mapped away from raw OS errors.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FsError {
    /// The path does not exist.
    #[error("not found")]
    NotFound,
    /// The path already exists.
    #[error("already exists")]
    AlreadyExists,
    /// Access was denied.
    #[error("permission denied")]
    PermissionDenied,
    /// Another process holds the file open (sharing violation).
    #[error("in use by another process")]
    Busy,
    /// The volume is full.
    #[error("no space left on the volume")]
    NoSpace,
    /// A parent component of the path is a link; nothing behind it is touched.
    #[error("path crosses a link")]
    LinkInPath,
    /// The volume is not local NTFS.
    #[error("unsupported volume: {0}")]
    UnsupportedVolume(String),
    /// Any other failure.
    #[error("i/o error: {0}")]
    Io(String),
    /// Simulated power loss (SimFs only).
    #[error("crashed")]
    Crashed,
}

impl From<io::Error> for FsError {
    fn from(e: io::Error) -> FsError {
        match e.kind() {
            io::ErrorKind::NotFound => FsError::NotFound,
            io::ErrorKind::AlreadyExists => FsError::AlreadyExists,
            io::ErrorKind::PermissionDenied => FsError::PermissionDenied,
            io::ErrorKind::StorageFull => FsError::NoSpace,
            _ => FsError::Io(e.kind().to_string()),
        }
    }
}

/// A file opened for reading.
pub trait FsRead: io::Read + io::Seek + Send {}

impl<T: io::Read + io::Seek + Send> FsRead for T {}

/// A newly created file.
pub trait FsWrite: io::Write + Send {
    /// Forces data and metadata to stable storage.
    fn flush_durable(&mut self) -> Result<(), FsError>;
}

/// The operations the engine needs. Every path is relative to the share root and never follows
/// a link.
pub trait Fs: Send + Sync {
    /// The volume kind and free space; fails with `NotFound` if the root is gone.
    fn volume(&self) -> Result<VolumeInfo, FsError>;
    /// Metadata, or `None` if the path is missing.
    fn metadata(&self, p: &RelPath) -> Result<Option<Meta>, FsError>;
    /// Lists a directory (`None` = the root). Includes the metadata directory.
    fn read_dir(&self, p: Option<&RelPath>) -> Result<Vec<DirEntry>, FsError>;
    /// Opens a regular file for reading.
    fn open_read(&self, p: &RelPath) -> Result<Box<dyn FsRead>, FsError>;
    /// Creates a file, failing if it exists.
    fn create_new(&self, p: &RelPath) -> Result<Box<dyn FsWrite>, FsError>;
    /// Renames, atomically and durably. Without `replace` it fails if `to` exists.
    fn move_durable(&self, from: &RelPath, to: &RelPath, replace: bool) -> Result<(), FsError>;
    /// Removes a file.
    fn remove_file(&self, p: &RelPath) -> Result<(), FsError>;
    /// Creates a directory (parents must exist).
    fn create_dir(&self, p: &RelPath) -> Result<(), FsError>;
    /// Reads a file in the metadata directory, `None` if missing. `name` is one plain component.
    fn meta_read(&self, name: &str) -> Result<Option<Vec<u8>>, FsError>;
    /// Atomically and durably writes a file in the metadata directory, creating the directory.
    fn meta_write(&self, name: &str, data: &[u8]) -> Result<(), FsError>;
}

/// Rejects anything but one plain component for the metadata directory.
pub(crate) fn check_meta_name(name: &str) -> Result<(), FsError> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > crate::path::MAX_COMPONENT_BYTES
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        });
    if bad {
        return Err(FsError::Io("invalid metadata file name".into()));
    }
    Ok(())
}
