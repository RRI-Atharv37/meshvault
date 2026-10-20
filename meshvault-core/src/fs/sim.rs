//! An in-memory filesystem that models what survives a power loss and can inject faults.

use super::{
    check_meta_name, DirEntry, EntryKind, Fs, FsError, FsRead, FsWrite, Meta, VolumeInfo,
    VolumeKind,
};
use crate::path::RelPath;
use crate::time::{Clock, Timestamp};
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

/// Faults a [`SimFs`] injects, by call number (counted from 1 across every `Fs` method and
/// `flush_durable`).
#[derive(Clone, Debug, Default)]
pub struct FaultPlan {
    /// Power loss at this call: volatile state is dropped and the call fails with `Crashed`
    /// without taking effect.
    pub crash_at_call: Option<u64>,
    /// Calls that fail with the given error without taking effect.
    pub fail: Vec<(u64, FsError)>,
    /// On a crash, unflushed new files keep only the first half of their bytes.
    pub torn_writes: bool,
}

type ReadHook = Box<dyn FnMut(&SimFs) + Send>;

#[derive(Clone)]
enum Content {
    File(Vec<u8>),
    Dir,
    Link,
}

#[derive(Clone)]
struct Node {
    name: String,
    content: Content,
    mtime: Timestamp,
    id: u128,
}

type Tree = BTreeMap<String, Node>;

struct Inner {
    durable: Tree,
    volatile: Tree,
    meta: BTreeMap<String, Vec<u8>>,
    next_id: u128,
    epoch: u64,
    calls: u64,
    plan: FaultPlan,
    volume_kind: VolumeKind,
    free_bytes: u64,
    root_present: bool,
    fail_dirs: Vec<(Option<String>, FsError)>,
    fail_opens: Vec<(String, FsError)>,
    hooks: Vec<(String, ReadHook)>,
    unrepresentable: Vec<(Option<String>, String)>,
}

/// A cloneable handle to one simulated volume.
#[derive(Clone)]
pub struct SimFs {
    inner: Arc<Mutex<Inner>>,
    clock: Arc<dyn Clock>,
}

fn key_of(p: &RelPath) -> String {
    p.key().as_str().to_string()
}

fn parent_key(key: &str) -> Option<&str> {
    key.rsplit_once('/').map(|(parent, _)| parent)
}

fn last_name(p: &RelPath) -> String {
    p.components().last().unwrap_or_default().to_string()
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

fn descendants(tree: &Tree, key: &str) -> Vec<String> {
    let prefix = format!("{key}/");
    tree.keys()
        .filter(|k| k.starts_with(&prefix))
        .cloned()
        .collect()
}

enum Walk {
    Ok,
    Missing,
}

impl Inner {
    fn new_id(&mut self) -> u128 {
        self.next_id += 1;
        self.next_id
    }

    fn walk_ancestors(&self, p: &RelPath) -> Result<Walk, FsError> {
        for a in ancestors_top_down(p) {
            match self.volatile.get(&key_of(&a)).map(|n| &n.content) {
                Some(Content::Dir) => {}
                Some(Content::Link) => return Err(FsError::LinkInPath),
                Some(Content::File(_)) | None => return Ok(Walk::Missing),
            }
        }
        Ok(Walk::Ok)
    }

    fn make_dirs(&mut self, p: Option<RelPath>, now: Timestamp, durable: bool) {
        let Some(p) = p else { return };
        let mut chain = ancestors_top_down(&p);
        chain.push(p);
        for dir in chain {
            let key = key_of(&dir);
            if !self.volatile.contains_key(&key) {
                let node = Node {
                    name: last_name(&dir),
                    content: Content::Dir,
                    mtime: now,
                    id: self.new_id(),
                };
                if durable {
                    self.durable.insert(key.clone(), node.clone());
                }
                self.volatile.insert(key, node);
            } else if durable && !self.durable.contains_key(&key) {
                let node = self.volatile[&key].clone();
                self.durable.insert(key, node);
            }
        }
    }

    fn make_durable_dirs(&mut self, dir_key: Option<&str>) {
        let Some(dir_key) = dir_key else { return };
        let mut prefix = String::new();
        for part in dir_key.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if !self.durable.contains_key(&prefix) {
                if let Some(node) = self.volatile.get(&prefix) {
                    self.durable.insert(prefix.clone(), node.clone());
                }
            }
        }
    }

    fn key_of_id(&self, id: u128) -> Option<String> {
        self.volatile
            .iter()
            .find(|(_, n)| n.id == id)
            .map(|(k, _)| k.clone())
    }

    fn put(&mut self, p: &RelPath, content: Content, now: Timestamp, keep_id: bool) {
        self.make_dirs(p.parent(), now, true);
        let key = key_of(p);
        let id = match self.volatile.get(&key) {
            Some(old) if keep_id => old.id,
            _ => self.new_id(),
        };
        let node = Node {
            name: last_name(p),
            content,
            mtime: now,
            id,
        };
        self.durable.insert(key.clone(), node.clone());
        self.volatile.insert(key, node);
    }

    fn remove_tree(tree: &mut Tree, key: &str) {
        for k in descendants(tree, key) {
            tree.remove(&k);
        }
        tree.remove(key);
    }

    fn move_node(tree: &mut Tree, from: &str, to: &str, new_name: &str, node: Node) {
        let moved: Vec<(String, Node)> = descendants(tree, from)
            .into_iter()
            .filter_map(|k| {
                tree.remove(&k)
                    .map(|n| (format!("{to}{}", &k[from.len()..]), n))
            })
            .collect();
        tree.remove(from);
        tree.insert(
            to.to_string(),
            Node {
                name: new_name.to_string(),
                ..node
            },
        );
        tree.extend(moved);
    }

    fn crash(&mut self) {
        if self.plan.torn_writes {
            let mut torn = Vec::new();
            for (key, node) in &self.volatile {
                if let Content::File(data) = &node.content {
                    if !self.durable.contains_key(key) && !data.is_empty() {
                        torn.push((key.clone(), node.clone(), data[..data.len() / 2].to_vec()));
                    }
                }
            }
            for (key, mut node, prefix) in torn {
                let parent_ok = parent_key(&key).is_none_or(|p| self.durable.contains_key(p));
                if parent_ok {
                    node.content = Content::File(prefix);
                    self.durable.insert(key, node);
                }
            }
        }
        self.volatile = self.durable.clone();
        self.epoch += 1;
    }
}

impl SimFs {
    /// An empty NTFS volume with plenty of space; new files take their mtime from `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> SimFs {
        SimFs {
            inner: Arc::new(Mutex::new(Inner {
                durable: Tree::new(),
                volatile: Tree::new(),
                meta: BTreeMap::new(),
                next_id: 0,
                epoch: 0,
                calls: 0,
                plan: FaultPlan::default(),
                volume_kind: VolumeKind::Ntfs,
                free_bytes: u64::MAX / 2,
                root_present: true,
                fail_dirs: Vec::new(),
                fail_opens: Vec::new(),
                hooks: Vec::new(),
                unrepresentable: Vec::new(),
            })),
            clock,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn counted(&self) -> Result<MutexGuard<'_, Inner>, FsError> {
        let mut g = self.lock();
        g.calls += 1;
        let n = g.calls;
        if g.plan.crash_at_call == Some(n) {
            g.crash();
            return Err(FsError::Crashed);
        }
        if let Some((_, err)) = g.plan.fail.iter().find(|(call, _)| *call == n) {
            return Err(err.clone());
        }
        Ok(g)
    }

    /// Sets the reported volume kind.
    pub fn set_volume_kind(&self, kind: VolumeKind) {
        self.lock().volume_kind = kind;
    }

    /// Sets the free space; bytes written through [`FsWrite`] use it up, and writes beyond it fail
    /// with `NoSpace`.
    pub fn set_free_bytes(&self, bytes: u64) {
        self.lock().free_bytes = bytes;
    }

    /// Makes the root vanish (`volume()` fails with `NotFound`) or return.
    pub fn set_root_present(&self, present: bool) {
        self.lock().root_present = present;
    }

    /// Installs a fault plan; call numbers are absolute, so use [`SimFs::call_count`] to aim.
    pub fn set_fault_plan(&self, plan: FaultPlan) {
        self.lock().plan = plan;
    }

    /// Drops everything not yet durable, as a power loss would.
    pub fn crash(&self) {
        self.lock().crash();
    }

    /// How many counted calls have been made.
    pub fn call_count(&self) -> u64 {
        self.lock().calls
    }

    /// Test setup: creates a file (and parents) durably. A new file gets a new file ID; an
    /// existing one keeps its ID.
    pub fn put_file(&self, p: &RelPath, data: &[u8]) {
        let now = self.clock.now();
        self.lock().put(p, Content::File(data.to_vec()), now, true);
    }

    /// Test setup: rewrites a file in place, keeping its file ID and setting mtime to now.
    pub fn write_file(&self, p: &RelPath, data: &[u8]) {
        self.put_file(p, data);
    }

    /// Test setup: replaces a file the way an editor's atomic save does: new file ID, mtime now.
    pub fn replace_file(&self, p: &RelPath, data: &[u8]) {
        let now = self.clock.now();
        self.lock().put(p, Content::File(data.to_vec()), now, false);
    }

    /// Test setup: creates a directory (and parents).
    pub fn put_dir(&self, p: &RelPath) {
        let now = self.clock.now();
        self.lock().make_dirs(Some(p.clone()), now, true);
    }

    /// Test setup: creates a link (symlink or junction stand-in).
    pub fn put_link(&self, p: &RelPath) {
        let now = self.clock.now();
        self.lock().put(p, Content::Link, now, false);
    }

    /// Test setup: removes a file, link or directory tree; missing paths are ignored.
    pub fn remove(&self, p: &RelPath) {
        let key = key_of(p);
        let mut g = self.lock();
        Inner::remove_tree(&mut g.volatile, &key);
        Inner::remove_tree(&mut g.durable, &key);
    }

    /// Test setup: renames keeping the file ID and mtime; replaces nothing.
    pub fn rename(&self, from: &RelPath, to: &RelPath) {
        let (from_key, to_key) = (key_of(from), key_of(to));
        let now = self.clock.now();
        let mut g = self.lock();
        g.make_dirs(to.parent(), now, true);
        for durable in [false, true] {
            let tree = if durable {
                &mut g.durable
            } else {
                &mut g.volatile
            };
            if let Some(node) = tree.get(&from_key).cloned() {
                Inner::move_node(tree, &from_key, &to_key, &last_name(to), node);
            }
        }
    }

    /// Test setup: sets a file's mtime.
    pub fn set_mtime(&self, p: &RelPath, t: Timestamp) {
        let key = key_of(p);
        let mut guard = self.lock();
        let inner = &mut *guard;
        for tree in [&mut inner.volatile, &mut inner.durable] {
            if let Some(n) = tree.get_mut(&key) {
                n.mtime = t;
            }
        }
    }

    /// Test setup: lists a name in `parent` that no [`RelPath`] can represent.
    pub fn add_unrepresentable_entry(&self, parent: Option<&RelPath>, name: &str) {
        self.lock()
            .unrepresentable
            .push((parent.map(key_of), name.to_string()));
    }

    /// The current (volatile) content, `None` if absent.
    pub fn read_file(&self, p: &RelPath) -> Option<Vec<u8>> {
        match self.lock().volatile.get(&key_of(p)).map(|n| &n.content) {
            Some(Content::File(d)) => Some(d.clone()),
            _ => None,
        }
    }

    /// The content that would survive a crash now, `None` if absent.
    pub fn read_durable(&self, p: &RelPath) -> Option<Vec<u8>> {
        match self.lock().durable.get(&key_of(p)).map(|n| &n.content) {
            Some(Content::File(d)) => Some(d.clone()),
            _ => None,
        }
    }

    /// Makes listing `dir` (`None` = root) fail with `err` until cleared.
    pub fn fail_read_dir(&self, dir: Option<&RelPath>, err: FsError) {
        self.lock().fail_dirs.push((dir.map(key_of), err));
    }

    /// Makes opening `p` for reading fail with `err` until cleared.
    pub fn fail_open_read(&self, p: &RelPath, err: FsError) {
        self.lock().fail_opens.push((key_of(p), err));
    }

    /// Removes every `fail_read_dir` and `fail_open_read`.
    pub fn clear_failures(&self) {
        let mut g = self.lock();
        g.fail_dirs.clear();
        g.fail_opens.clear();
    }

    /// Runs `hook` each time a reader of `p` reaches end of file, before the reader returns it.
    pub fn on_read_eof(&self, p: &RelPath, hook: impl FnMut(&SimFs) + Send + 'static) {
        self.lock().hooks.push((key_of(p), Box::new(hook)));
    }
}

struct SimReader {
    fs: SimFs,
    key: String,
    cursor: io::Cursor<Vec<u8>>,
    fired: bool,
}

impl io::Read for SimReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.cursor.read(buf)?;
        if n == 0 && !buf.is_empty() && !self.fired {
            self.fired = true;
            let (mut mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.fs.lock().hooks)
                .into_iter()
                .partition(|(k, _)| *k == self.key);
            self.fs.lock().hooks.extend(rest);
            for (_, hook) in &mut mine {
                hook(&self.fs);
            }
            self.fs.lock().hooks.extend(mine);
        }
        Ok(n)
    }
}

impl io::Seek for SimReader {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.cursor.seek(pos)
    }
}

struct SimWriter {
    fs: SimFs,
    id: u128,
    epoch: u64,
}

impl SimWriter {
    fn node<'a>(&self, g: &'a mut Inner) -> io::Result<&'a mut Node> {
        if g.epoch != self.epoch {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "crashed"));
        }
        let key = g
            .key_of_id(self.id)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        g.volatile
            .get_mut(&key)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

impl io::Write for SimWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let now = self.fs.clock.now();
        let mut g = self.fs.lock();
        let need = buf.len() as u64;
        if need > g.free_bytes {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        let node = self.node(&mut g)?;
        let Content::File(data) = &mut node.content else {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        };
        data.extend_from_slice(buf);
        node.mtime = now;
        g.free_bytes -= need;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl FsWrite for SimWriter {
    fn flush_durable(&mut self) -> Result<(), FsError> {
        let mut g = self.fs.counted()?;
        if g.epoch != self.epoch {
            return Err(FsError::Crashed);
        }
        let key = g.key_of_id(self.id).ok_or(FsError::NotFound)?;
        let node = g.volatile[&key].clone();
        g.make_durable_dirs(parent_key(&key));
        g.durable.insert(key, node);
        Ok(())
    }
}

impl Fs for SimFs {
    fn volume(&self) -> Result<VolumeInfo, FsError> {
        let g = self.counted()?;
        if !g.root_present {
            return Err(FsError::NotFound);
        }
        Ok(VolumeInfo {
            kind: g.volume_kind.clone(),
            free_bytes: g.free_bytes,
        })
    }

    fn metadata(&self, p: &RelPath) -> Result<Option<Meta>, FsError> {
        let g = self.counted()?;
        if matches!(g.walk_ancestors(p)?, Walk::Missing) {
            return Ok(None);
        }
        Ok(g.volatile.get(&key_of(p)).map(|n| {
            let (kind, size) = match &n.content {
                Content::File(d) => (EntryKind::File, d.len() as u64),
                Content::Dir => (EntryKind::Dir, 0),
                Content::Link => (EntryKind::Link, 0),
            };
            Meta {
                kind,
                size,
                mtime: n.mtime,
                file_id: n.id,
            }
        }))
    }

    fn read_dir(&self, p: Option<&RelPath>) -> Result<Vec<DirEntry>, FsError> {
        let g = self.counted()?;
        if !g.root_present {
            return Err(FsError::NotFound);
        }
        let dir_key = p.map(key_of);
        if let Some((_, err)) = g.fail_dirs.iter().find(|(k, _)| *k == dir_key) {
            return Err(err.clone());
        }
        if let Some(p) = p {
            if matches!(g.walk_ancestors(p)?, Walk::Missing) {
                return Err(FsError::NotFound);
            }
            match g.volatile.get(&key_of(p)).map(|n| &n.content) {
                Some(Content::Dir) => {}
                Some(Content::Link) => return Err(FsError::LinkInPath),
                Some(Content::File(_)) => return Err(FsError::Io("not a directory".into())),
                None => return Err(FsError::NotFound),
            }
        }
        let mut out: Vec<DirEntry> = g
            .volatile
            .iter()
            .filter(|(k, _)| parent_key(k) == dir_key.as_deref())
            .map(|(_, n)| DirEntry {
                name: n.name.clone(),
                kind: match n.content {
                    Content::File(_) => EntryKind::File,
                    Content::Dir => EntryKind::Dir,
                    Content::Link => EntryKind::Link,
                },
            })
            .collect();
        if dir_key.is_none() && !g.meta.is_empty() {
            out.push(DirEntry {
                name: crate::path::META_DIR.to_string(),
                kind: EntryKind::Dir,
            });
        }
        out.extend(
            g.unrepresentable
                .iter()
                .filter(|(parent, _)| *parent == dir_key)
                .map(|(_, name)| DirEntry {
                    name: name.clone(),
                    kind: EntryKind::File,
                }),
        );
        Ok(out)
    }

    fn open_read(&self, p: &RelPath) -> Result<Box<dyn FsRead>, FsError> {
        let g = self.counted()?;
        if matches!(g.walk_ancestors(p)?, Walk::Missing) {
            return Err(FsError::NotFound);
        }
        let key = key_of(p);
        if let Some((_, err)) = g.fail_opens.iter().find(|(k, _)| *k == key) {
            return Err(err.clone());
        }
        match g.volatile.get(&key).map(|n| &n.content) {
            Some(Content::File(data)) => Ok(Box::new(SimReader {
                fs: self.clone(),
                key,
                cursor: io::Cursor::new(data.clone()),
                fired: false,
            })),
            Some(_) => Err(FsError::Io("not a regular file".into())),
            None => Err(FsError::NotFound),
        }
    }

    fn create_new(&self, p: &RelPath) -> Result<Box<dyn FsWrite>, FsError> {
        let now = self.clock.now();
        let mut g = self.counted()?;
        if matches!(g.walk_ancestors(p)?, Walk::Missing) {
            return Err(FsError::NotFound);
        }
        let key = key_of(p);
        if g.volatile.contains_key(&key) {
            return Err(FsError::AlreadyExists);
        }
        let id = g.new_id();
        let epoch = g.epoch;
        g.volatile.insert(
            key,
            Node {
                name: last_name(p),
                content: Content::File(Vec::new()),
                mtime: now,
                id,
            },
        );
        Ok(Box::new(SimWriter {
            fs: self.clone(),
            id,
            epoch,
        }))
    }

    fn move_durable(&self, from: &RelPath, to: &RelPath, replace: bool) -> Result<(), FsError> {
        let mut g = self.counted()?;
        for p in [from, to] {
            if matches!(g.walk_ancestors(p)?, Walk::Missing) {
                return Err(FsError::NotFound);
            }
        }
        let (from_key, to_key) = (key_of(from), key_of(to));
        let src = g
            .volatile
            .get(&from_key)
            .cloned()
            .ok_or(FsError::NotFound)?;
        if from_key != to_key {
            match g.volatile.get(&to_key).map(|n| &n.content) {
                None => {}
                Some(Content::Dir) => return Err(FsError::PermissionDenied),
                Some(_) if !replace => return Err(FsError::AlreadyExists),
                Some(_) => {}
            }
        }
        if matches!(src.content, Content::Dir) && to_key.starts_with(&format!("{from_key}/")) {
            return Err(FsError::Io("cannot move a directory into itself".into()));
        }
        let durable_src = g.durable.get(&from_key).cloned().unwrap_or_else(|| {
            let content = match &src.content {
                Content::File(_) => Content::File(Vec::new()),
                other => other.clone(),
            };
            Node {
                content,
                ..src.clone()
            }
        });
        Inner::move_node(&mut g.volatile, &from_key, &to_key, &last_name(to), src);
        g.make_durable_dirs(parent_key(&to_key).map(str::to_string).as_deref());
        Inner::move_node(
            &mut g.durable,
            &from_key,
            &to_key,
            &last_name(to),
            durable_src,
        );
        Ok(())
    }

    fn remove_file(&self, p: &RelPath) -> Result<(), FsError> {
        let mut g = self.counted()?;
        if matches!(g.walk_ancestors(p)?, Walk::Missing) {
            return Err(FsError::NotFound);
        }
        let key = key_of(p);
        match g.volatile.get(&key).map(|n| &n.content) {
            None => return Err(FsError::NotFound),
            Some(Content::Dir) => return Err(FsError::PermissionDenied),
            Some(_) => {}
        }
        if let Some(Node {
            content: Content::File(d),
            ..
        }) = g.volatile.remove(&key)
        {
            g.free_bytes = g.free_bytes.saturating_add(d.len() as u64);
        }
        g.durable.remove(&key);
        Ok(())
    }

    fn create_dir(&self, p: &RelPath) -> Result<(), FsError> {
        let now = self.clock.now();
        let mut g = self.counted()?;
        if matches!(g.walk_ancestors(p)?, Walk::Missing) {
            return Err(FsError::NotFound);
        }
        let key = key_of(p);
        if g.volatile.contains_key(&key) {
            return Err(FsError::AlreadyExists);
        }
        let id = g.new_id();
        g.volatile.insert(
            key,
            Node {
                name: last_name(p),
                content: Content::Dir,
                mtime: now,
                id,
            },
        );
        Ok(())
    }

    fn meta_read(&self, name: &str) -> Result<Option<Vec<u8>>, FsError> {
        check_meta_name(name)?;
        Ok(self.counted()?.meta.get(name).cloned())
    }

    fn meta_write(&self, name: &str, data: &[u8]) -> Result<(), FsError> {
        check_meta_name(name)?;
        self.counted()?.meta.insert(name.to_string(), data.to_vec());
        Ok(())
    }
}
