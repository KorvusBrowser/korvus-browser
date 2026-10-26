//! The origin private file system's STORE: one directory tree per origin, shared by every
//! realm of this process -- a page and its workers are separate isolates on separate threads, and Chrome
//! 150 lists a file a worker created from the page's root (measured, `tools/realweb/seed/opfs`). In
//! memory for the process's life, like a fresh profile's.
//!
//! Handles address entries by PATH from the origin's root, not by identity: a handle whose entry was
//! removed answers NotFoundError (Chrome: `getFile()` on a file whose directory was removed), and a new
//! entry at the same path is the same entry to it.
//!
//! Chrome also partitions a third-party frame's storage by its top-level site; one store per origin is
//! the same thing as long as one process shows one top-level site, which `korvus fetch` does.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// A directory's entries, by name. `BTreeMap` for the listing order: Chrome lists a directory in
/// DESCENDING byte order of the names (measured: `sub,c,b,aa,a,B,1`, `éa,zeta,beta,a b,Alpha,A,2,10`).
pub(crate) type Entries = BTreeMap<String, Node>;

#[derive(Debug)]
pub(crate) enum Node {
    Dir(Entries),
    File(FileData),
}

#[derive(Debug, Default)]
pub(crate) struct FileData {
    pub(crate) bytes: Vec<u8>,
    /// Milliseconds since the epoch of the last change.
    pub(crate) modified: f64,
    /// Open writable streams (shared) or an open sync access handle (exclusive).
    pub(crate) lock: Lock,
}

/// Who has the file open. Holders of one shared kind share it; any other combination is refused (Chrome 150:
/// a second "siloed" writable opens, an access handle beside a writable does not, two "read-only" or two
/// "readwrite-unsafe" access handles do).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lock {
    #[default]
    None,
    /// This many open "siloed" `FileSystemWritableFileStream`s (each writes its own swap copy).
    Writable(u32),
    /// A "readwrite" access handle, an "exclusive" writable -- or a swap entry, which nothing may take.
    Exclusive,
    /// This many "read-only" access handles.
    ReadOnly(u32),
    /// This many "readwrite-unsafe" access handles.
    Unsafe(u32),
}

impl Node {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Node::Dir(_) => "directory",
            Node::File(_) => "file",
        }
    }
}

/// Why an operation failed, as the DOMException Chrome answers with (name and the sentence it uses
/// when the operation's own prefix is not part of it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fail {
    /// NotFoundError.
    NotFound,
    /// TypeMismatchError: the path exists as the other kind.
    TypeMismatch,
    /// InvalidModificationError: a non-empty directory removed without `recursive`.
    NotEmpty,
    /// NoModificationAllowedError: the file is locked.
    Locked,
}

impl Fail {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Fail::NotFound => "NotFoundError",
            Fail::TypeMismatch => "TypeMismatchError",
            Fail::NotEmpty => "InvalidModificationError",
            Fail::Locked => "NoModificationAllowedError",
        }
    }

    /// Chrome 150's sentences (measured).
    pub(crate) fn message(self) -> &'static str {
        match self {
            Fail::NotFound => "A requested file or directory could not be found at the time an operation was processed.",
            Fail::TypeMismatch => "The path supplied exists, but was not an entry of requested type.",
            Fail::NotEmpty => "The object can not be modified in this way.",
            Fail::Locked => "An attempt was made to modify an object where modifications are not allowed.",
        }
    }
}

/// Chrome 150's rule for an entry name (measured: `""`, `"."`, `".."`, and a name holding `/` or `\`
/// are refused with "Name is not allowed.").
pub(crate) fn name_allowed(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\'))
}

fn store() -> MutexGuard<'static, HashMap<String, Node>> {
    static STORE: OnceLock<Mutex<HashMap<String, Node>>> = OnceLock::new();
    STORE.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner())
}

/// Run `f` over `origin`'s root directory (made on first use).
pub(crate) fn with_root<R>(origin: &str, f: impl FnOnce(&mut Entries) -> R) -> R {
    let mut s = store();
    let root = s.entry(origin.to_string()).or_insert_with(|| Node::Dir(Entries::new()));
    match root {
        Node::Dir(e) => f(e),
        Node::File(_) => unreachable!("an origin's root is a directory"),
    }
}

/// The node at `path` under `root`.
pub(crate) fn node_at<'a>(root: &'a mut Entries, path: &[String]) -> Option<&'a mut Node> {
    let (last, parents) = path.split_last()?;
    let dir = dir_at(root, parents)?;
    dir.get_mut(last)
}

/// The directory at `path` under `root` (`root` itself for the empty path).
pub(crate) fn dir_at<'a>(root: &'a mut Entries, path: &[String]) -> Option<&'a mut Entries> {
    let mut cur = root;
    for name in path {
        match cur.get_mut(name)? {
            Node::Dir(e) => cur = e,
            Node::File(_) => return None,
        }
    }
    Some(cur)
}

/// Whether the entry at `path` exists and is of `kind`.
pub(crate) fn check(origin: &str, path: &[String], kind: &str) -> Result<(), Fail> {
    with_root(origin, |root| {
        if path.is_empty() {
            return if kind == "directory" { Ok(()) } else { Err(Fail::TypeMismatch) };
        }
        match node_at(root, path) {
            None => Err(Fail::NotFound),
            Some(n) if n.kind() != kind => Err(Fail::TypeMismatch),
            Some(_) => Ok(()),
        }
    })
}

/// `getFileHandle` / `getDirectoryHandle`'s steps under the directory `dir`: the child `name` of
/// `kind`, made when `create` and it is missing. The directory itself must still exist.
pub(crate) fn get_child(origin: &str, dir: &[String], name: &str, kind: &str, create: bool, now: f64) -> Result<(), Fail> {
    with_root(origin, |root| {
        let d = dir_at(root, dir).ok_or(Fail::NotFound)?;
        match d.get(name) {
            Some(n) if n.kind() == kind => Ok(()),
            Some(_) => Err(Fail::TypeMismatch),
            None if !create => Err(Fail::NotFound),
            None => {
                let node = if kind == "file" { Node::File(FileData { modified: now, ..Default::default() }) } else { Node::Dir(Entries::new()) };
                d.insert(name.to_string(), node);
                Ok(())
            }
        }
    })
}

/// Whether anything under `node` is locked (a file with an open writable or access handle).
fn any_locked(node: &Node) -> bool {
    match node {
        Node::File(f) => f.lock != Lock::None,
        Node::Dir(e) => e.values().any(any_locked),
    }
}

/// `removeEntry(name, { recursive })` under `dir`.
pub(crate) fn remove_child(origin: &str, dir: &[String], name: &str, recursive: bool) -> Result<(), Fail> {
    with_root(origin, |root| {
        let d = dir_at(root, dir).ok_or(Fail::NotFound)?;
        let n = d.get(name).ok_or(Fail::NotFound)?;
        if let Node::Dir(e) = n {
            if !e.is_empty() && !recursive {
                return Err(Fail::NotEmpty);
            }
        }
        if any_locked(n) {
            return Err(Fail::Locked);
        }
        d.remove(name);
        Ok(())
    })
}

/// The names in the directory at `dir`, with each entry's kind, in Chrome's listing order.
pub(crate) fn list(origin: &str, dir: &[String]) -> Result<Vec<(String, &'static str)>, Fail> {
    with_root(origin, |root| {
        let d = dir_at(root, dir).ok_or(Fail::NotFound)?;
        Ok(d.iter().rev().map(|(k, v)| (k.clone(), v.kind())).collect())
    })
}

/// Run `f` over the file at `path`.
pub(crate) fn with_file<R>(origin: &str, path: &[String], f: impl FnOnce(&mut FileData) -> R) -> Result<R, Fail> {
    with_root(origin, |root| match node_at(root, path) {
        Some(Node::File(fd)) => Ok(f(fd)),
        Some(Node::Dir(_)) => Err(Fail::TypeMismatch),
        None => Err(Fail::NotFound),
    })
}

/// Make the file at `path` DURABLE: its bytes written to this process's backing file for it and synced to
/// the disk (`fsync`). `FileSystemSyncAccessHandle.flush()` is exactly that in Chrome -- "persists any changes
/// made to the file ... to disk" -- and costs a disk sync: Chrome 150 measured 1.12 ms for a one-byte file
/// (a challenge provider challenge worker times it); korvus's flush did nothing and answered in 5 µs, a time no
/// real storage gives. The store itself stays in memory (it is the live copy every realm reads); this is the
/// durable copy a flush promises. The backing file's name is a hash of origin and path, so no page-chosen
/// name reaches the file system.
pub(crate) fn persist(origin: &str, path: &[String]) -> std::io::Result<()> {
    // the file's own write in flight first ([`persist_in_background`])
    let inflight = pending().remove(&(origin.to_string(), path.to_vec()));
    if let Some(h) = inflight {
        let _ = h.join();
    }
    persist_now(origin, path)
}

/// [`persist`], off this thread: what the browser does for an access handle's file when the handle opens, so
/// the worker's call does not wait for the disk (Chrome 150 answers `createSyncAccessHandle()` in 0.2-0.4 ms).
/// A later [`persist`] of the file waits for it. A file whose creation is still being recorded
/// ([`create_in_background`]) needs nothing more.
pub(crate) fn persist_in_background(origin: &str, path: &[String]) {
    let key = (origin.to_string(), path.to_vec());
    if pending().contains_key(&key) {
        return;
    }
    let (o, p) = (origin.to_string(), path.to_vec());
    let h = std::thread::spawn(move || {
        let _ = backing_file(&o, &p);
    });
    pending().insert(key, h);
}

/// The storage service's own disk work when a key's bucket first opens -- Chrome 150's first `getDirectory()` of a
/// key opens the bucket's databases, 4.8-10.2 ms of real reads and writes -- here the backing directory made and its
/// entry synced, off this thread, once per key. korvus touched the disk first at the worker's first `flush()`, which
/// then paid the device waking and the journal committing the directory and file (1.5-9.7 ms, Chrome 1.1;
/// the challenge worker times that flush and posts it).
/// Returns the flag the work sets when it is done -- the opening's answer waits for it
/// ([`crate::binding::protos::Settle::TaskGated`]), as Chrome's `getDirectory()` answers once its bucket is open.
pub(crate) fn open_bucket_in_background(origin: &str) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    static OPENED: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    let mut opened = OPENED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(done) = opened.get(origin) {
        return done.clone();
    }
    let done = Arc::new(AtomicBool::new(false));
    opened.insert(origin.to_string(), done.clone());
    let flag = done.clone();
    std::thread::spawn(move || {
        if let Ok(d) = backing_dir() {
            if let Ok(f) = std::fs::File::open(&d) {
                let _ = f.sync_all();
            }
        }
        flag.store(true, Ordering::Release);
    });
    done
}

/// A file made in the bucket (`getFileHandle(name, {create: true})`): its backing file created and its creation
/// committed -- the file and its directory synced -- off this thread, as the browser records a new file before it
/// answers. A later `flush()` ([`persist`], which waits for this) then syncs the file's data only, as Chrome's does.
/// Returns the flag the work sets when it is done: the `getFileHandle()` answer waits for it.
pub(crate) fn create_in_background(origin: &str, path: &[String]) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let key = (origin.to_string(), path.to_vec());
    if pending().contains_key(&key) {
        done.store(true, Ordering::Release);
        return done;
    }
    let (o, p) = (origin.to_string(), path.to_vec());
    let flag = done.clone();
    let h = std::thread::spawn(move || {
        if let Ok(f) = backing_file(&o, &p) {
            let _ = f.sync_all();
        }
        if let Ok(d) = backing_dir() {
            if let Ok(f) = std::fs::File::open(&d) {
                let _ = f.sync_all();
            }
        }
        flag.store(true, Ordering::Release);
    });
    pending().insert(key, h);
    done
}

/// The file's backing file, opened for writing and created if it is not there yet -- never truncated.
fn backing_file(origin: &str, path: &[String]) -> std::io::Result<std::fs::File> {
    use std::hash::{Hash, Hasher};
    let dir = backing_dir()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    origin.hash(&mut h);
    path.hash(&mut h);
    std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(dir.join(format!("{:016x}", h.finish())))
}

type FileKey = (String, Vec<String>);

/// The backing writes in flight, by file.
fn pending() -> std::sync::MutexGuard<'static, HashMap<FileKey, std::thread::JoinHandle<()>>> {
    static P: OnceLock<Mutex<HashMap<FileKey, std::thread::JoinHandle<()>>>> = OnceLock::new();
    P.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// A digest of the bytes each backing file was last synced with: a `flush()` with nothing new to persist costs
/// what Chrome's does (0.0-0.4 ms, measured), not a rewrite and a disk sync (korvus: ~1 ms every time).
fn synced() -> std::sync::MutexGuard<'static, HashMap<FileKey, u64>> {
    static S: OnceLock<Mutex<HashMap<FileKey, u64>>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn persist_now(origin: &str, path: &[String]) -> std::io::Result<()> {
    use std::hash::{Hash, Hasher};
    let bytes = with_file(origin, path, |fd| fd.bytes.clone()).map_err(|f| std::io::Error::other(f.message()))?;
    let digest = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    };
    let key = (origin.to_string(), path.to_vec());
    if synced().get(&key) == Some(&digest) {
        return Ok(());
    }
    // The bytes written over the file in place and its DATA synced (`fdatasync`), as Chrome's `base::File::Flush`
    // does: recreating the file and syncing all of it committed a new inode each flush.
    use std::io::{Seek, SeekFrom, Write};
    let mut f = backing_file(origin, path)?;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&bytes)?;
    f.set_len(bytes.len() as u64)?;
    f.sync_data()?;
    synced().insert(key, digest);
    Ok(())
}

/// This process's OPFS backing directory (created on first use), after removing the ones processes that are
/// gone left behind.
fn backing_dir() -> std::io::Result<std::path::PathBuf> {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    if let Some(d) = DIR.get() {
        return Ok(d.clone());
    }
    let base = std::env::temp_dir();
    // The sweep of dead processes' directories runs OFF this thread, and LATER: a flush costs what a disk sync
    // costs, and a sync commits every pending change of the file system -- a first flush that also listed the
    // temp directory took 13 ms, and one whose sync carried the sweep's deletions 10-22 ms, where Chrome's took
    // 1-2 ms. Chrome runs its own storage housekeeping as best-effort work after startup.
    let sweep = base.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        if let Ok(entries) = std::fs::read_dir(&sweep) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(pid) = name.strip_prefix("korvus-opfs-").and_then(|p| p.parse::<u32>().ok()) {
                    if pid != std::process::id() && !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        }
    });
    let d = base.join(format!("korvus-opfs-{}", std::process::id()));
    std::fs::create_dir_all(&d)?;
    Ok(DIR.get_or_init(|| d).clone())
}

/// Take `want` on the file's lock (a count of 1 for the shared kinds).
pub(crate) fn lock(origin: &str, path: &[String], want: Lock) -> Result<(), Fail> {
    with_file(origin, path, |fd| {
        fd.lock = match (fd.lock, want) {
            (Lock::None, w) => w,
            (Lock::Writable(n), Lock::Writable(_)) => Lock::Writable(n + 1),
            (Lock::ReadOnly(n), Lock::ReadOnly(_)) => Lock::ReadOnly(n + 1),
            (Lock::Unsafe(n), Lock::Unsafe(_)) => Lock::Unsafe(n + 1),
            _ => return Err(Fail::Locked),
        };
        Ok(())
    })?
}

/// Release one hold of the file's lock. A file that has gone since holds nothing to release.
pub(crate) fn unlock(origin: &str, path: &[String]) {
    let _ = with_file(origin, path, |fd| {
        fd.lock = match fd.lock {
            Lock::Writable(n) if n > 1 => Lock::Writable(n - 1),
            Lock::ReadOnly(n) if n > 1 => Lock::ReadOnly(n - 1),
            Lock::Unsafe(n) if n > 1 => Lock::Unsafe(n - 1),
            _ => Lock::None,
        }
    });
}

/// A writable's swap entry beside the file at `path`: Chrome 150 lists `<name>.crswap`, then `<name>.1.crswap`,
/// `<name>.2.crswap` for further open writables -- empty files nobody may remove -- until each writable
/// closes or aborts. Returns its name.
pub(crate) fn open_swap(origin: &str, path: &[String]) -> String {
    let Some((name, parent)) = path.split_last() else { return String::new() };
    with_root(origin, |root| {
        let Some(d) = dir_at(root, parent) else { return String::new() };
        let mut i = 0u32;
        loop {
            let swap = if i == 0 { format!("{name}.crswap") } else { format!("{name}.{i}.crswap") };
            if !d.contains_key(&swap) {
                d.insert(swap.clone(), Node::File(FileData { lock: Lock::Exclusive, ..Default::default() }));
                return swap;
            }
            i += 1;
        }
    })
}

/// Remove the swap entry `swap` beside the file at `path`.
pub(crate) fn close_swap(origin: &str, path: &[String], swap: &str) {
    let Some((_, parent)) = path.split_last() else { return };
    with_root(origin, |root| {
        if let Some(d) = dir_at(root, parent) {
            d.remove(swap);
        }
    });
}

/// `move()`: the entry at `from` goes to directory `to_dir` under `name`. An entry already there is
/// replaced when both are files; a locked source is refused.
pub(crate) fn move_entry(origin: &str, from: &[String], to_dir: &[String], name: &str) -> Result<(), Fail> {
    with_root(origin, |root| {
        let (last, parents) = from.split_last().ok_or(Fail::Locked)?;
        {
            let n = node_at(root, from).ok_or(Fail::NotFound)?;
            if any_locked(n) {
                return Err(Fail::Locked);
            }
        }
        // A directory cannot move into itself or its own subtree.
        if to_dir.len() >= from.len() && to_dir[..from.len()] == *from {
            return Err(Fail::NotEmpty);
        }
        {
            let dest = dir_at(root, to_dir).ok_or(Fail::NotFound)?;
            if let Some(existing) = dest.get(name) {
                if existing.kind() == "directory" {
                    return Err(Fail::NotEmpty);
                }
                if any_locked(existing) {
                    return Err(Fail::Locked);
                }
            }
        }
        let node = dir_at(root, parents).and_then(|d| d.remove(last)).ok_or(Fail::NotFound)?;
        let dest = dir_at(root, to_dir).ok_or(Fail::NotFound)?;
        dest.insert(name.to_string(), node);
        Ok(())
    })
}

/// Write `data` at `at` into `bytes`, zero-filling a gap past the end. Returns the bytes written.
pub(crate) fn write_at(bytes: &mut Vec<u8>, at: usize, data: &[u8]) -> usize {
    let end = at + data.len();
    if bytes.len() < end {
        bytes.resize(end, 0);
    }
    bytes[at..end].copy_from_slice(data);
    data.len()
}

/// Read into `buf` from `at`: the bytes read (0 past the end).
pub(crate) fn read_at(bytes: &[u8], at: usize, buf: &mut [u8]) -> usize {
    if at >= bytes.len() {
        return 0;
    }
    let n = buf.len().min(bytes.len() - at);
    buf[..n].copy_from_slice(&bytes[at..at + n]);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn create_list_remove() {
        let o = "https://t.create-list.example";
        assert_eq!(get_child(o, &[], "f1", "file", false, 0.0), Err(Fail::NotFound));
        get_child(o, &[], "f1", "file", true, 0.0).unwrap();
        get_child(o, &[], "sub", "directory", true, 0.0).unwrap();
        get_child(o, &[], "a0", "file", true, 0.0).unwrap();
        assert_eq!(get_child(o, &[], "sub", "file", false, 0.0), Err(Fail::TypeMismatch));
        // Chrome's order: descending bytes.
        let names: Vec<_> = list(o, &[]).unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["sub", "f1", "a0"]);
        get_child(o, &p(&["sub"]), "g", "file", true, 0.0).unwrap();
        assert_eq!(remove_child(o, &[], "sub", false), Err(Fail::NotEmpty));
        remove_child(o, &[], "sub", true).unwrap();
        assert_eq!(remove_child(o, &[], "sub", false), Err(Fail::NotFound));
        assert_eq!(check(o, &p(&["sub", "g"]), "file"), Err(Fail::NotFound));
    }

    #[test]
    fn locks() {
        let o = "https://t.locks.example";
        get_child(o, &[], "f", "file", true, 0.0).unwrap();
        let f = p(&["f"]);
        lock(o, &f, Lock::Writable(1)).unwrap();
        lock(o, &f, Lock::Writable(1)).unwrap();
        assert_eq!(lock(o, &f, Lock::Exclusive), Err(Fail::Locked));
        assert_eq!(lock(o, &f, Lock::ReadOnly(1)), Err(Fail::Locked));
        assert_eq!(remove_child(o, &[], "f", false), Err(Fail::Locked));
        unlock(o, &f);
        unlock(o, &f);
        lock(o, &f, Lock::Exclusive).unwrap();
        assert_eq!(lock(o, &f, Lock::Writable(1)), Err(Fail::Locked));
        unlock(o, &f);
        lock(o, &f, Lock::ReadOnly(1)).unwrap();
        lock(o, &f, Lock::ReadOnly(1)).unwrap();
        unlock(o, &f);
        unlock(o, &f);
        remove_child(o, &[], "f", false).unwrap();
    }

    #[test]
    fn swaps() {
        let o = "https://t.swaps.example";
        get_child(o, &[], "f1", "file", true, 0.0).unwrap();
        let f = p(&["f1"]);
        let a = open_swap(o, &f);
        let b = open_swap(o, &f);
        assert_eq!((a.as_str(), b.as_str()), ("f1.crswap", "f1.1.crswap"));
        let names: Vec<_> = list(o, &[]).unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["f1.crswap", "f1.1.crswap", "f1"]);
        assert_eq!(remove_child(o, &[], "f1.crswap", false), Err(Fail::Locked));
        close_swap(o, &f, &a);
        assert_eq!(open_swap(o, &f), "f1.crswap");
    }

    #[test]
    fn bytes() {
        let mut b = Vec::new();
        assert_eq!(write_at(&mut b, 10, &[65, 66]), 2);
        assert_eq!(b.len(), 12);
        let mut buf = [9u8; 20];
        assert_eq!(read_at(&b, 0, &mut buf), 12);
        assert_eq!(&buf[..12], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 65, 66]);
        assert_eq!(read_at(&b, 100, &mut buf), 0);
    }

    #[test]
    fn names() {
        for bad in ["", ".", "..", "a/b", "a\\b"] {
            assert!(!name_allowed(bad), "{bad:?}");
        }
        assert!(name_allowed("a.b"));
        assert!(name_allowed("..."));
    }

    #[test]
    fn moves() {
        let o = "https://t.moves.example";
        get_child(o, &[], "m1", "file", true, 0.0).unwrap();
        get_child(o, &[], "d", "directory", true, 0.0).unwrap();
        move_entry(o, &p(&["m1"]), &[], "m2").unwrap();
        assert_eq!(check(o, &p(&["m2"]), "file"), Ok(()));
        move_entry(o, &p(&["m2"]), &p(&["d"]), "m2").unwrap();
        assert_eq!(check(o, &p(&["d", "m2"]), "file"), Ok(()));
        assert_eq!(move_entry(o, &p(&["d"]), &p(&["d"]), "x"), Err(Fail::NotEmpty));
    }
}
