//! The real filesystem backend: local NTFS on Windows only.
//!
//! Every open passes `FILE_FLAG_OPEN_REPARSE_POINT`, and every operation first checks that no
//! parent component is a link, because that flag only protects the last component. A local process
//! that swaps a directory for a link between that check and the operation is outside the threat
//! model: it could already edit the share's files directly.

use super::{
    check_meta_name, DirEntry, EntryKind, Fs, FsError, FsRead, FsWrite, Meta, VolumeInfo,
    VolumeKind,
};
use crate::path::{RelPath, META_DIR};
use crate::time::Timestamp;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetFileInformationByHandle, GetVolumeInformationW,
    GetVolumePathNameW, MoveFileExW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH,
};

const DRIVE_REMOTE: u32 = 4;
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
const MAX_META_FILE_BYTES: u64 = 16 * 1024 * 1024;
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// A share root on a local NTFS volume.
pub struct WindowsFs {
    root: PathBuf,
}

fn map_io(e: io::Error) -> FsError {
    match e.raw_os_error() {
        Some(32 | 33) => FsError::Busy,
        Some(5) => FsError::PermissionDenied,
        Some(2 | 3) => FsError::NotFound,
        Some(80 | 183) => FsError::AlreadyExists,
        Some(39 | 112) => FsError::NoSpace,
        Some(code) => FsError::Io(format!("os error {code}")),
        None => FsError::from(e),
    }
}

fn tolerate(result: io::Result<()>, accepted: FsError) -> Result<(), FsError> {
    match result.map_err(map_io) {
        Err(e) if e == accepted => Ok(()),
        other => other,
    }
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn file_info(f: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION {
        dwFileAttributes: 0,
        ftCreationTime: windows_sys::Win32::Foundation::FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        ftLastAccessTime: windows_sys::Win32::Foundation::FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        ftLastWriteTime: windows_sys::Win32::Foundation::FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        dwVolumeSerialNumber: 0,
        nFileSizeHigh: 0,
        nFileSizeLow: 0,
        nNumberOfLinks: 0,
        nFileIndexHigh: 0,
        nFileIndexLow: 0,
    };
    // SAFETY: the handle is valid for the lifetime of `f` and `info` is a properly sized out-parameter.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

fn open_no_access(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(0)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

fn stat(path: &Path) -> Result<Option<Meta>, FsError> {
    let file = match open_no_access(path) {
        Ok(f) => f,
        Err(e) => {
            return match map_io(e) {
                FsError::NotFound => Ok(None),
                other => Err(other),
            }
        }
    };
    let info = file_info(&file).map_err(map_io)?;
    let kind = if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        EntryKind::Link
    } else if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let size = if kind == EntryKind::File {
        (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow)
    } else {
        0
    };
    let ft = (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(info.ftLastWriteTime.dwLowDateTime);
    let nanos = (i128::from(ft) - i128::from(FILETIME_UNIX_EPOCH)) * 100;
    let mtime =
        Timestamp(i64::try_from(nanos).unwrap_or(if nanos < 0 { i64::MIN } else { i64::MAX }));
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    let file_id = (u128::from(info.dwVolumeSerialNumber) << 64) | u128::from(index);
    Ok(Some(Meta {
        kind,
        size,
        mtime,
        file_id,
    }))
}

fn move_file(from: &Path, to: &Path, replace: bool) -> Result<(), FsError> {
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
    if ok == 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    Ok(())
}

struct VolumeProbe {
    fs_name: String,
    drive_type: u32,
}

fn probe_volume(path: &Path) -> Result<VolumeProbe, FsError> {
    let path_w = wide(path);
    let mut mount = vec![0u16; 1024];
    // SAFETY: `path_w` is NUL-terminated and `mount` is a writable buffer of the stated length.
    let ok = unsafe { GetVolumePathNameW(path_w.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) };
    if ok == 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    let mut fs_name = vec![0u16; 64];
    // SAFETY: `mount` is NUL-terminated by the call above; the out buffer length matches.
    let ok = unsafe {
        GetVolumeInformationW(
            mount.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    // SAFETY: `mount` is NUL-terminated.
    let drive_type = unsafe { GetDriveTypeW(mount.as_ptr()) };
    let end = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    Ok(VolumeProbe {
        fs_name: String::from_utf16_lossy(&fs_name[..end]),
        drive_type,
    })
}

fn free_bytes(path: &Path) -> Result<u64, FsError> {
    let path_w = wide(path);
    let mut available = 0u64;
    // SAFETY: `path_w` is NUL-terminated; unused out-parameters may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path_w.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    Ok(available)
}

impl WindowsFs {
    /// Opens `root`, refusing anything that is not local NTFS.
    pub fn open(root: &Path) -> Result<WindowsFs, FsError> {
        let canonical = std::fs::canonicalize(root).map_err(map_io)?;
        if !std::fs::metadata(&canonical).map_err(map_io)?.is_dir() {
            return Err(FsError::Io("share root is not a directory".into()));
        }
        if canonical.to_string_lossy().starts_with(r"\\?\UNC\") {
            return Err(FsError::UnsupportedVolume("network share".into()));
        }
        let probe = probe_volume(&canonical)?;
        if probe.drive_type == DRIVE_REMOTE {
            return Err(FsError::UnsupportedVolume("network drive".into()));
        }
        if !probe.fs_name.eq_ignore_ascii_case("NTFS") {
            return Err(FsError::UnsupportedVolume(probe.fs_name));
        }
        Ok(WindowsFs { root: canonical })
    }

    /// The absolute path (with the long-path prefix) a relative path resolves to.
    pub fn resolve(&self, p: &RelPath) -> PathBuf {
        let mut abs = self.root.clone();
        for component in p.components() {
            abs.push(component);
        }
        abs
    }

    fn check_parents(&self, p: &RelPath) -> Result<(), FsError> {
        let mut ancestors = Vec::new();
        let mut cur = p.parent();
        while let Some(a) = cur {
            cur = a.parent();
            ancestors.push(a);
        }
        for a in ancestors.iter().rev() {
            match stat(&self.resolve(a))? {
                Some(Meta {
                    kind: EntryKind::Link,
                    ..
                }) => return Err(FsError::LinkInPath),
                Some(Meta {
                    kind: EntryKind::Dir,
                    ..
                }) => {}
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    fn meta_dir(&self) -> PathBuf {
        self.root.join(META_DIR)
    }

    fn meta_dir_state(&self) -> Result<bool, FsError> {
        match stat(&self.meta_dir())? {
            None => Ok(false),
            Some(Meta {
                kind: EntryKind::Dir,
                ..
            }) => Ok(true),
            Some(Meta {
                kind: EntryKind::Link,
                ..
            }) => Err(FsError::LinkInPath),
            Some(_) => Err(FsError::Io("metadata path is not a directory".into())),
        }
    }
}

struct WinWriter(File);

impl Write for WinWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl FsWrite for WinWriter {
    fn flush_durable(&mut self) -> Result<(), FsError> {
        self.0.sync_all().map_err(map_io)
    }
}

impl Fs for WindowsFs {
    fn volume(&self) -> Result<VolumeInfo, FsError> {
        let probe = probe_volume(&self.root)?;
        let kind = if probe.fs_name.eq_ignore_ascii_case("NTFS") {
            VolumeKind::Ntfs
        } else {
            VolumeKind::Other(probe.fs_name)
        };
        Ok(VolumeInfo {
            kind,
            free_bytes: free_bytes(&self.root)?,
        })
    }

    fn metadata(&self, p: &RelPath) -> Result<Option<Meta>, FsError> {
        self.check_parents(p)?;
        stat(&self.resolve(p))
    }

    fn read_dir(&self, p: Option<&RelPath>) -> Result<Vec<DirEntry>, FsError> {
        let abs = match p {
            Some(p) => {
                self.check_parents(p)?;
                match stat(&self.resolve(p))? {
                    None => return Err(FsError::NotFound),
                    Some(Meta {
                        kind: EntryKind::Link,
                        ..
                    }) => return Err(FsError::LinkInPath),
                    Some(Meta {
                        kind: EntryKind::File,
                        ..
                    }) => return Err(FsError::Io("not a directory".into())),
                    Some(_) => self.resolve(p),
                }
            }
            None => self.root.clone(),
        };
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&abs).map_err(map_io)? {
            let entry = entry.map_err(map_io)?;
            let attrs = entry.metadata().map_err(map_io)?.file_attributes();
            let kind = if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                EntryKind::Link
            } else if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
            });
        }
        Ok(out)
    }

    fn open_read(&self, p: &RelPath) -> Result<Box<dyn FsRead>, FsError> {
        self.check_parents(p)?;
        let file = OpenOptions::new()
            .read(true)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.resolve(p))
            .map_err(map_io)?;
        let attrs = file_info(&file).map_err(map_io)?.dwFileAttributes;
        if attrs & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 {
            return Err(FsError::Io("not a regular file".into()));
        }
        Ok(Box::new(file))
    }

    fn create_new(&self, p: &RelPath) -> Result<Box<dyn FsWrite>, FsError> {
        self.check_parents(p)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ)
            .open(self.resolve(p))
            .map_err(map_io)?;
        Ok(Box::new(WinWriter(file)))
    }

    fn move_durable(&self, from: &RelPath, to: &RelPath, replace: bool) -> Result<(), FsError> {
        self.check_parents(from)?;
        self.check_parents(to)?;
        move_file(&self.resolve(from), &self.resolve(to), replace)
    }

    fn remove_file(&self, p: &RelPath) -> Result<(), FsError> {
        self.check_parents(p)?;
        let abs = self.resolve(p);
        if matches!(
            stat(&abs)?,
            Some(Meta {
                kind: EntryKind::Dir,
                ..
            })
        ) {
            return Err(FsError::PermissionDenied);
        }
        std::fs::remove_file(abs).map_err(map_io)
    }

    fn create_dir(&self, p: &RelPath) -> Result<(), FsError> {
        self.check_parents(p)?;
        std::fs::create_dir(self.resolve(p)).map_err(map_io)
    }

    fn meta_read(&self, name: &str) -> Result<Option<Vec<u8>>, FsError> {
        check_meta_name(name)?;
        if !self.meta_dir_state()? {
            return Ok(None);
        }
        let file = match OpenOptions::new()
            .read(true)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.meta_dir().join(name))
        {
            Ok(f) => f,
            Err(e) => {
                return match map_io(e) {
                    FsError::NotFound => Ok(None),
                    other => Err(other),
                }
            }
        };
        if file_info(&file).map_err(map_io)?.dwFileAttributes
            & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY)
            != 0
        {
            return Err(FsError::Io("not a regular file".into()));
        }
        let mut data = Vec::new();
        file.take(MAX_META_FILE_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(map_io)?;
        if data.len() as u64 > MAX_META_FILE_BYTES {
            return Err(FsError::Io("metadata file too large".into()));
        }
        Ok(Some(data))
    }

    fn meta_write(&self, name: &str, data: &[u8]) -> Result<(), FsError> {
        check_meta_name(name)?;
        if !self.meta_dir_state()? {
            tolerate(std::fs::create_dir(self.meta_dir()), FsError::AlreadyExists)?;
            self.meta_dir_state()?;
        }
        let tmp = self.meta_dir().join(format!("{name}.tmp"));
        tolerate(std::fs::remove_file(&tmp), FsError::NotFound)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ)
            .open(&tmp)
            .map_err(map_io)?;
        file.write_all(data).map_err(map_io)?;
        file.sync_all().map_err(map_io)?;
        drop(file);
        move_file(&tmp, &self.meta_dir().join(OsStr::new(name)), true)
    }
}
