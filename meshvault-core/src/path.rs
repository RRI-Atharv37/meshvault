//! The one path normaliser: every path the engine stores, sends or touches goes through it.

use std::fmt;
use unicode_normalization::{is_nfc, UnicodeNormalization};

/// Longest accepted path in bytes.
pub const MAX_REL_PATH_BYTES: usize = 4096;
/// Longest accepted path component in bytes.
pub const MAX_COMPONENT_BYTES: usize = 255;
/// Deepest accepted nesting.
pub const MAX_DEPTH: usize = 64;
/// The per-folder metadata directory; never synced, never accepted from a peer.
pub const META_DIR: &str = ".meshvault";

/// Why a path was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// Empty path or empty component.
    #[error("path is empty")]
    Empty,
    /// Starts with a separator.
    #[error("path is absolute")]
    Absolute,
    /// Drive letter or UNC prefix.
    #[error("path has a drive or UNC prefix")]
    DriveOrUnc,
    /// Contains a `..` component.
    #[error("path contains '..'")]
    DotDot,
    /// Contains a `.` component.
    #[error("path contains '.'")]
    DotComponent,
    /// A device name Windows cannot hold as a file name.
    #[error("reserved name: {0}")]
    ReservedName(String),
    /// A component ends in a dot or a space.
    #[error("component ends in a dot or space")]
    TrailingDotOrSpace,
    /// A character Windows forbids in names.
    #[error("forbidden character {0:?}")]
    ForbiddenChar(char),
    /// A control character.
    #[error("control character")]
    ControlChar,
    /// More than [`MAX_REL_PATH_BYTES`].
    #[error("path too long")]
    TooLong,
    /// A component longer than [`MAX_COMPONENT_BYTES`].
    #[error("component too long")]
    ComponentTooLong,
    /// More than [`MAX_DEPTH`] components.
    #[error("path too deep")]
    TooDeep,
    /// Inside [`META_DIR`].
    #[error("path is inside the metadata directory")]
    MetaDir,
    /// Normalisation did not reach a stable NFC form.
    #[error("path is not NFC after normalising")]
    NotNfcAfterNormalise,
}

/// A validated, NFC-normalised, `/`-separated path relative to a share root.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

/// The case-insensitive identity of a [`RelPath`]: two paths that name one Windows file share a key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathKey(String);

const RESERVED: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM1", "COM2", "COM3", "COM4", "COM5",
    "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9",
];

impl RelPath {
    /// Validates and normalises `s`.
    pub fn parse(s: &str) -> Result<RelPath, PathError> {
        if s.is_empty() {
            return Err(PathError::Empty);
        }
        let mut chars = s.chars();
        let (first, second) = (chars.next(), chars.next());
        let is_sep = |c: Option<char>| matches!(c, Some('/' | '\\'));
        if is_sep(first) && is_sep(second) {
            return Err(PathError::DriveOrUnc);
        }
        if second == Some(':') && first.is_some_and(|c| c.is_ascii_alphabetic()) {
            return Err(PathError::DriveOrUnc);
        }
        if is_sep(first) {
            return Err(PathError::Absolute);
        }
        // NFC never grows a string past 3x, so a larger input cannot fit after normalising.
        if s.len() > 3 * MAX_REL_PATH_BYTES {
            return Err(PathError::TooLong);
        }
        let normalised: String = s.nfc().collect();
        if normalised.len() > MAX_REL_PATH_BYTES {
            return Err(PathError::TooLong);
        }
        if normalised.split('/').count() > MAX_DEPTH {
            return Err(PathError::TooDeep);
        }
        for (i, component) in normalised.split('/').enumerate() {
            check_component(component)?;
            if i == 0 && fold(component) == fold(META_DIR) {
                return Err(PathError::MetaDir);
            }
        }
        if !is_nfc(&normalised) {
            return Err(PathError::NotNfcAfterNormalise);
        }
        Ok(RelPath(normalised))
    }

    /// The path as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The case-insensitive key.
    pub fn key(&self) -> PathKey {
        PathKey(fold(&self.0).nfc().collect())
    }

    /// The containing directory, or `None` at the top level.
    pub fn parent(&self) -> Option<RelPath> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| RelPath(parent.to_string()))
    }

    /// Appends one validated component.
    pub fn join(&self, name: &str) -> Result<RelPath, PathError> {
        if name.contains('/') {
            return Err(PathError::ForbiddenChar('/'));
        }
        let name: String = name.nfc().collect();
        check_component(&name)?;
        if self.0.split('/').count() + 1 > MAX_DEPTH {
            return Err(PathError::TooDeep);
        }
        if self.0.len() + 1 + name.len() > MAX_REL_PATH_BYTES {
            return Err(PathError::TooLong);
        }
        Ok(RelPath(format!("{}/{}", self.0, name)))
    }

    /// The components, top first.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PathKey {
    /// The key as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Per-character simple uppercase; a character whose uppercase is several characters stays as is,
/// which approximates the NTFS upcase table.
pub(crate) fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

fn check_component(c: &str) -> Result<(), PathError> {
    if c.is_empty() {
        return Err(PathError::Empty);
    }
    if c == "." {
        return Err(PathError::DotComponent);
    }
    if c == ".." {
        return Err(PathError::DotDot);
    }
    if c.len() > MAX_COMPONENT_BYTES {
        return Err(PathError::ComponentTooLong);
    }
    for ch in c.chars() {
        if ch < '\u{20}' {
            return Err(PathError::ControlChar);
        }
        if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') {
            return Err(PathError::ForbiddenChar(ch));
        }
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Err(PathError::TrailingDotOrSpace);
    }
    let stem = c.split('.').next().unwrap_or(c);
    if RESERVED.contains(&fold(stem).as_str()) {
        return Err(PathError::ReservedName(c.to_string()));
    }
    Ok(())
}
