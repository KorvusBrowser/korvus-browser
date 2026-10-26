//! THE PAGE REALM FROM A SNAPSHOT (the context snapshot, slice 3: the runtime half).
//!
//! A warm start used to spend ~74 % of its instructions building the page realm (`install_window_realm`). A runtime
//! made from a snapshot of that realm deserialises it instead, and [`restore`] gives back what the install left in
//! Rust (`webapi::snapshot`, and this crate's `Location`).
//!
//! ONE PATH: the page realm of a binary that can MAKE a snapshot (korvus's own `main` calls
//! [`use_snapshots`]) comes from one whenever its template disk cache is on (`KORVUS_TPL_CACHE=off` installs). Every
//! other embedder -- the tests, the CDP harnesses -- builds the realm the ordinary way, which keeps the fresh install
//! exercised. Every refusal falls back to the ordinary install SILENTLY: its line goes to stderr only under
//! `KORVUS_SNAPSHOT_REPORT`, and its `.refused` file says why.
//!
//! - **The file** (`webapi::snapshot_file`) sits beside the blueprint files, under the build id, keyed by the
//!   environment and the persona's own stamp (`spec::Persona::source_stamp`). Its layout:
//!   `MAGIC | build id | checksum | payload`, where the payload is the external-reference table the snapshot was
//!   made with (offsets from the binary's anchor, `webapi::binding::refs`) followed by deno's snapshot (V8's blob
//!   and deno's sidecar, which carries the embedder's sections).
//! - **Read only if trusted:** the blob is code. It is read only if the file and its directory are this user's
//!   alone, the build id is this binary's, and the checksum holds. A release V8 does not verify a blob, and
//!   deserialising a damaged one is not memory-safe (MAIN, 10-13).
//! - **The key:** `KORVUS_SNAPSHOT_REPORT` keys nothing (it decides what is said, not what the realm is). The
//!   persona's environment (`TZ`, `LC_ALL`) does: the maker sets it before it computes the file, as an engine does.
//! - **A miss** starts ONE child process per process ([`request`]): this binary with [`MAKE_ARG`], in the same
//!   environment, so with the same persona, at the lowest priority (`nice -n 19`, where it exists). A V8 FATAL while
//!   making the snapshot ends the child, not the page, and the page installs as ever. No process waits for a maker.
//! - **One maker per key:** an exclusive lock beside the file. A maker that finds it held leaves.
//! - **The maker** ([`make_main`]):
//!   - installs the realm in ordinary runtimes until the pointers it hands V8 stop changing;
//!   - installs it once more in a runtime made for a snapshot, with that table;
//!   - refuses BEFORE V8 serialises anything if that install handed V8 a pointer the table lacks, or a heap pointer.
//!   - Its refusal (`.refused`) is written first, as a provisional one with the attempt it is, and removed on
//!     success. A maker that died (killed, a V8 FATAL, or past its bound of [`MAKE_LIFETIME_SECS`]) leaves it, and a
//!     later miss starts another, [`MAKE_ATTEMPTS`] in all; a refusal that says why stands for its key.
//! - **A hit** ([`load`]) makes the runtime from the snapshot, and [`restore`] replaces the install. Restore runs in
//!   two halves, the process's statics written last (`webapi::snapshot::PageStatics`). If it fails, the engine is
//!   built the ordinary way on clean statics, and the file is removed.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use deno_core::v8;
use deno_core::{JsRuntime, RuntimeOptions};
use spec::Persona;

/// The argument that makes this binary the snapshot's maker ([`make_main`]).
pub const MAKE_ARG: &str = "__korvus-make-snapshot";

/// The file's tag, and its format's version.
const MAGIC: &[u8; 8] = b"KVSNAP\x02\x00";

static EXE: OnceLock<PathBuf> = OnceLock::new();
/// A snapshot failed to restore in this process (or the process built a page the ordinary way first): no later
/// engine here tries one (a brand registry numbered by an ordinary install need not match the snapshot's).
static OFF: AtomicBool = AtomicBool::new(false);

static CHILD_ARGS: OnceLock<Vec<String>> = OnceLock::new();

/// This binary can make the snapshot (it answers [`MAKE_ARG`]): its page realms come from one.
pub fn use_snapshots(exe: PathBuf) {
    let _ = EXE.set(exe);
}

/// The arguments the maker gets after [`MAKE_ARG`], so it wears what this process wears: the identity a plugin
/// answered here, which the maker (which loads no plugin) cannot ask for itself. Set once, before the first engine.
pub fn child_args(args: Vec<String>) {
    let _ = CHILD_ARGS.set(args);
}

fn enabled() -> bool {
    // a run that observes the install (the layer dump, coverage, the touch and stub logs) installs
    EXE.get().is_some() && !OFF.load(Ordering::Relaxed) && !webapi::snapshot::observes_install()
}

/// Whether the snapshot's lines go to stderr: under `KORVUS_SNAPSHOT_REPORT` only -- a refusal falls back to the
/// ordinary install silently, and its `.refused` says why.
fn reporting() -> bool {
    std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some()
}

/// The provisional refusal a maker writes before it starts ([`make_main`]), followed by ` attempt=N`: what a maker that
/// died leaves.
const DIED: &str = "make: the maker ended before it finished (a V8 FATAL?)";
/// How many makers a key gets when they die before one finishes.
const MAKE_ATTEMPTS: u32 = 3;
/// A maker's bound: past it, the maker ends as one that died (it makes in a few seconds).
const MAKE_LIFETIME_SECS: u64 = 120;

/// How many makers died on this key, when `why` is a died maker's provisional refusal; `None` for one that says why.
fn died_attempts(why: &str) -> Option<u32> {
    why.strip_prefix(DIED)?.trim().strip_prefix("attempt=")?.parse().ok()
}

/// The file this process's snapshot lives in, set by its first engine's persona (a process wears one persona: it
/// comes from the environment).
static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn path_for(persona: &dyn Persona) -> Option<PathBuf> {
    webapi::snapshot_file(persona.source_stamp())
}

fn file() -> Option<PathBuf> {
    PATH.get().cloned().flatten()
}

fn refused_file(path: &Path) -> PathBuf {
    path.with_extension("refused")
}

/// The table a snapshot was made with, as THIS process's pointers.
struct Refs(&'static [v8::ExternalReference]);
// SAFETY: the table is leaked once and never written; its entries are addresses of code and statics in this binary.
unsafe impl Send for Refs {}
// SAFETY: as above.
unsafe impl Sync for Refs {}

/// A snapshot found for this process ([`load`]).
#[derive(Clone, Copy)]
pub(crate) struct Loaded {
    pub(crate) blob: &'static [u8],
    refs: &'static Refs,
}

impl Loaded {
    /// The extension that hands deno the snapshot's external-reference table, after deno's own and the ops': the
    /// same list, in the same place, as the runtime that made the snapshot had ([`make`]).
    pub(crate) fn refs_ext(&self) -> deno_core::Extension {
        refs_ext(self.refs.0)
    }
}

fn refs_ext(refs: &'static [v8::ExternalReference]) -> deno_core::Extension {
    deno_core::Extension { name: "korvus_snapshot_refs", external_references: std::borrow::Cow::Borrowed(refs), ..Default::default() }
}

fn table(offsets: &[i64]) -> Option<&'static Refs> {
    let ptrs = webapi::binding::refs::pointers(offsets)?;
    let v: Vec<v8::ExternalReference> = ptrs.into_iter().map(|p| v8::ExternalReference { pointer: p as *mut std::ffi::c_void }).collect();
    Some(Box::leak(Box::new(Refs(Box::leak(v.into_boxed_slice())))))
}

/// A snapshot this process will not use: removed, and its refusal kept with why (no maker is started for its key
/// again); said on stderr under `KORVUS_SNAPSHOT_REPORT` only.
fn refuse(path: &Path, why: &str) {
    if reporting() {
        eprintln!("korvus: the page realm's snapshot {} is refused ({why}); the page realm installs the ordinary way", path.display());
    }
    let _ = std::fs::remove_file(path);
    let _ = webapi::snapshot::write_private(&refused_file(path), format!("load: {why}\n").as_bytes());
}

/// This build's snapshot of the page realm for this environment and persona, read once per process. `None` on a
/// miss (which [`request`]s one), and for a file it refuses.
pub(crate) fn load(persona: &dyn Persona) -> Option<Loaded> {
    if !enabled() {
        return None;
    }
    static L: OnceLock<(Option<Loaded>, bool)> = OnceLock::new();
    let (got, miss) = *L.get_or_init(|| {
        let Some(path) = PATH.get_or_init(|| path_for(persona)).clone() else { return (None, false) };
        if !path.exists() {
            return (None, true);
        }
        if !webapi::snapshot::trusted(&path) {
            // not ours to remove: another user's, or one anybody could have written
            if reporting() {
                eprintln!("korvus: {} is not this user's alone; the page realm installs the ordinary way", path.display());
            }
            return (None, false);
        }
        let Ok(bytes) = std::fs::read(&path) else { return (None, false) };
        match parse(&bytes) {
            Ok((offsets, blob)) => {
                let Some(refs) = table(&offsets) else {
                    refuse(&path, "a recorded pointer outside this binary");
                    return (None, false);
                };
                let all: &'static [u8] = Box::leak(bytes.into_boxed_slice());
                (Some(Loaded { blob: &all[blob], refs }), false)
            }
            Err(why) => {
                refuse(&path, &why);
                (None, false)
            }
        }
    });
    if miss {
        request();
    }
    got
}

/// The file's table and where its blob is, once its tag, its build id and its checksum hold.
fn parse(b: &[u8]) -> Result<(Vec<i64>, std::ops::Range<usize>), String> {
    let bad = || "it does not decode".to_string();
    let u32_at = |at: usize| b.get(at..at + 4).and_then(|x| x.try_into().ok()).map(u32::from_le_bytes);
    let u64_at = |at: usize| b.get(at..at + 8).and_then(|x| x.try_into().ok()).map(u64::from_le_bytes);
    if b.get(..8) != Some(&MAGIC[..]) {
        return Err("not a snapshot of this format".to_string());
    }
    let idlen = u32_at(8).ok_or_else(bad)? as usize;
    let id = b.get(12..12 + idlen).ok_or_else(bad)?;
    if Some(id) != webapi::snapshot::build_id() {
        return Err("another build's".to_string());
    }
    let mut at = 12 + idlen;
    let sum = u64_at(at).ok_or_else(bad)?;
    at += 8;
    if webapi::snapshot::checksum(b.get(at..).ok_or_else(bad)?) != sum {
        return Err("its checksum does not hold".to_string());
    }
    let n = u32_at(at).ok_or_else(bad)? as usize;
    at += 4;
    let mut offsets = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        offsets.push(u64_at(at).ok_or_else(bad)? as i64);
        at += 8;
    }
    let len = u64_at(at).ok_or_else(bad)? as usize;
    at += 8;
    if at.checked_add(len) != Some(b.len()) {
        return Err(bad());
    }
    Ok((offsets, at..at + len))
}

/// A miss: start the child that makes the snapshot -- once per process; never for a key whose refusal says why, and
/// for one whose makers died, until [`MAKE_ATTEMPTS`] have.
pub(crate) fn request() {
    if !enabled() {
        return;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let (Some(exe), Some(path)) = (EXE.get(), file()) else { return };
        if let Ok(why) = std::fs::read_to_string(refused_file(&path)) {
            if died_attempts(&why).is_none_or(|n| n >= MAKE_ATTEMPTS) {
                return;
            }
        }
        // out of the page's way: the lowest priority, where `nice` exists
        let start = |nice: bool| {
            let mut c = if nice { std::process::Command::new("nice") } else { std::process::Command::new(exe) };
            if nice {
                c.arg("-n").arg("19").arg(exe);
            }
            c.arg(MAKE_ARG)
                .args(CHILD_ARGS.get().into_iter().flatten())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        };
        let child = start(true).or_else(|_| start(false));
        if let Ok(mut c) = child {
            // reaped when it ends, so a long-lived process (serve, CDP) keeps no zombie
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    });
}

/// This process built a page the ordinary way: no later engine here restores a snapshot (see [`OFF`]).
pub(crate) fn installed_fresh() {
    OFF.store(true, Ordering::Relaxed);
}

/// A snapshot that did not restore: refused (removed, its refusal kept), and no later engine of this process tries one.
/// Not one that is sound but cannot be used this run (`webapi::snapshot::NOT_NOW`: its surface's blueprint file is not
/// here): it stays, with no refusal, and the ordinary install that follows writes the file again -- the next run
/// restores.
pub(crate) fn failed(why: &str) {
    OFF.store(true, Ordering::Relaxed);
    if why.starts_with(webapi::snapshot::NOT_NOW) {
        if reporting() {
            eprintln!("korvus: snapshot: {why}; the page realm installs the ordinary way, and the snapshot is kept");
        }
        return;
    }
    if let Some(path) = file() {
        refuse(&path, &format!("restore: {why}"));
    }
}

/// In a runtime made from the snapshot, in place of `install_window_realm`: what the install left in Rust, back. The
/// process's statics are written last, by `PageStatics::apply`, once nothing that can fail is left.
pub(crate) fn restore(scope: &mut v8::PinScope) -> Result<(), String> {
    let data = deno_core::JsRuntime::op_state_from(scope).borrow_mut().try_take::<deno_core::snapshot::EmbedderSnapshotData>();
    let mut data = data.ok_or("the snapshot carries no page section")?;
    let mut at = 0;
    let statics = webapi::snapshot::restore_page(scope, &mut data, &mut at)?;
    crate::location::restore_location(scope, &mut data.store, &data.bytes, &mut at)?;
    if at != data.bytes.len() {
        return Err("the snapshot's page section has bytes nobody reads".to_string());
    }
    // the store, kept for the templates decoded on first lookup -- before the statics, whose passes may
    // look one up
    webapi::snapshot::keep_store(scope, std::mem::take(&mut data.store));
    statics.apply(scope)?;
    // the navigation hooks the page's install registers (a process static: this process may have none yet)
    crate::singletons::set_page_hooks();
    if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
        eprintln!("korvus: snapshot: the page realm was restored from this build's snapshot");
    }
    Ok(())
}

/// The child's whole run ([`MAKE_ARG`]): make this build's snapshot of the page realm for this environment and
/// `persona` -- the caller's, built from the same environment as the engine that missed -- or write why not. The exit
/// code says which.
pub fn make_main(persona: Arc<dyn Persona>) -> i32 {
    // The template reporter's diagnostics hold V8 Globals (`KORVUS_TPL_REPORT`: every function a template made, by
    // identity), and V8 refuses to serialise with them alive -- a FATAL in CreateBlob (CheckGlobalAndEternalHandles).
    // A reporting run installs and starts no maker (`webapi::snapshot::observes_install`); this guards a maker started
    // with it in its environment anyway. This process reports to no one (its stderr is null), and the variable keys
    // nothing. Before anything reads it (`webapi`'s `reporting()` keeps its first answer).
    std::env::remove_var("KORVUS_TPL_REPORT");
    webapi::enable_template_disk_cache();
    // the persona's environment FIRST (`TZ`, `LC_ALL`): it is part of the file's key, as it is in the engine that
    // missed, which set it before it looked
    crate::engine::persona_env(persona.as_ref());
    let Some(path) = path_for(persona.as_ref()) else { return 2 };
    let Some(dir) = path.parent() else { return 2 };
    if webapi::snapshot::private_dir(dir).is_err() {
        return 2;
    }
    // ONE maker per key: an exclusive lock beside the file, held until this process ends; a maker that finds it held
    // leaves (another is making this very file)
    let mut lock_oo = std::fs::OpenOptions::new();
    lock_oo.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        lock_oo.mode(0o600); // Windows: the lock file inherits the user-private NTFS ACLs of its dir
    }
    let Ok(lock) = lock_oo.open(path.with_extension("lock")) else { return 2 };
    if lock.try_lock().is_err() {
        return 0;
    }
    if path.exists() {
        return 0;
    }
    // what a maker killed while it wrote left (its temporary file is named by its pid): this one holds the lock
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_str().is_some_and(|n| n.starts_with('.') && n.ends_with(".snap.tmp")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // A V8 FATAL (or a kill) ends this process without a word: the refusal is written FIRST, provisional and with the
    // attempt this is, and removed on success -- a later miss starts another maker, MAKE_ATTEMPTS in all
    let attempt = std::fs::read_to_string(refused_file(&path)).ok().and_then(|w| died_attempts(&w)).unwrap_or(0) + 1;
    let _ = webapi::snapshot::write_private(&refused_file(&path), format!("{DIED} attempt={attempt}\n").as_bytes());
    // and a bound on this process's life: a maker that hangs ends as one that died
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(MAKE_LIFETIME_SECS));
        std::process::exit(4);
    });
    match make(persona, &path) {
        Ok(()) => {
            let _ = std::fs::remove_file(refused_file(&path));
            0
        }
        Err(why) => {
            let _ = webapi::snapshot::write_private(&refused_file(&path), format!("make: {why}\n").as_bytes());
            3
        }
    }
}

fn options(persona: &Arc<dyn Persona>, refs: Option<&'static [v8::ExternalReference]>) -> RuntimeOptions {
    let mut extensions = vec![crate::korvus_ext::init(persona.clone()), webapi::webapi_core_ext::init()];
    if let Some(r) = refs {
        extensions.push(refs_ext(r));
    }
    RuntimeOptions { extensions, ..Default::default() }
}

/// The global's own names in `rt`'s main context as V8 and deno made it, before any install.
fn context_names(rt: &mut JsRuntime) -> Vec<String> {
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    webapi::install::installer::global_names(scope)
}

/// What `Engine::assemble_shared` does to build the page realm, in `rt`.
fn install(rt: &mut JsRuntime, oracle: &str) {
    webapi::install_loaf_monitor(rt.v8_isolate());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    crate::engine::install_window_realm(scope, oracle);
}

fn make(persona: Arc<dyn Persona>, path: &Path) -> Result<(), String> {
    use webapi::binding::refs;
    refs::start_recording();
    let oracle = persona.device_oracle_json();
    // 1. ordinary installs until the pointers they hand V8 stop changing
    let mut known = usize::MAX;
    let mut plain_names = None;
    for _ in 0..3 {
        let mut rt = crate::new_runtime(options(&persona, None));
        if plain_names.is_none() {
            plain_names = Some(context_names(&mut rt));
        }
        install(&mut rt, &oracle);
        drop(rt);
        let n = refs::offsets().len();
        if n == known {
            break;
        }
        known = n;
    }
    if refs::foreign() > 0 {
        return Err(format!("the install hands V8 heap pointers: {}", refs::foreign_sites().join(", ")));
    }
    // 2. the install that is snapshotted, in a runtime made with that table. A pointer only that install hands V8 (one
    //    no ordinary install made) joins the table, and the install runs again in a new runtime -- at most three times;
    //    each such pointer is named by its `refs::` call site (stderr under KORVUS_SNAPSHOT_REPORT, the refusal at the end)
    let mut offsets = refs::offsets();
    let mut tries = 0;
    let mut rt = loop {
        let table = table(&offsets).ok_or("a recorded pointer outside the binary")?;
        let mut rt = crate::new_snapshot_runtime(options(&persona, Some(table.0)));
        // V8's late names: what a plain context's global has and this one's lacks (V8 adds them only outside a snapshot
        // maker, bootstrapper.cc). The install leaves them to V8; the restore seats them (`webapi::install::installer`)
        let late = webapi::install::installer::set_v8_late(plain_names.as_deref().unwrap_or_default(), &context_names(&mut rt));
        if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
            eprintln!("korvus: snapshot maker: V8's late names, left to the restore: {}", late.join(", "));
        }
        install(&mut rt, &oracle);
        if refs::foreign() > 0 {
            return Err(format!("the snapshotted install hands V8 heap pointers: {}", refs::foreign_sites().join(", ")));
        }
        let after = refs::offsets();
        if after.len() == offsets.len() {
            break rt;
        }
        let new: Vec<String> = after[offsets.len()..].iter().map(|o| format!("{} (+{o:#x})", refs::site_of(*o).unwrap_or_else(|| "?".to_string()))).collect();
        tries += 1;
        if tries == 3 {
            return Err(format!("the snapshotted install keeps handing V8 pointers the table lacks: {}", new.join(", ")));
        }
        if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
            eprintln!("korvus: snapshot maker: pointers only the snapshot runtime handed V8, retrying with them: {}", new.join(", "));
        }
        // (deno leaks a runtime made for a snapshot that was never taken: this short-lived process lets it)
        drop(rt);
        offsets = after;
    };
    {
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        webapi::snapshot::prepare_page_snapshot(scope)?;
    }
    let mut failed = None;
    // (the closure's signature comes from `snapshot_with_embedder`'s bound: higher-ranked over the scope's lifetimes)
    let blob = rt.snapshot_with_embedder(|scope, store| {
        let mut out = Vec::new();
        if let Err(e) = webapi::snapshot::snapshot_page(scope, store, &mut out) {
            failed = Some(e);
            return Vec::new();
        }
        crate::location::snapshot_location(scope, store, &mut out);
        // every Global V8 was not handed must be gone before it serialises: the isolate slots that hold some
        webapi::snapshot::release_isolate_slots(scope);
        out
    });
    if let Some(e) = failed {
        return Err(e);
    }
    let id = webapi::snapshot::build_id().ok_or("this binary has no build id")?;
    let mut payload = Vec::with_capacity(blob.len() + offsets.len() * 8 + 16);
    payload.extend_from_slice(&(offsets.len() as u32).to_le_bytes());
    for o in &offsets {
        payload.extend_from_slice(&o.to_le_bytes());
    }
    payload.extend_from_slice(&(blob.len() as u64).to_le_bytes());
    payload.extend_from_slice(&blob);
    let mut file = Vec::with_capacity(payload.len() + id.len() + 24);
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&(id.len() as u32).to_le_bytes());
    file.extend_from_slice(id);
    file.extend_from_slice(&webapi::snapshot::checksum(&payload).to_le_bytes());
    file.extend_from_slice(&payload);
    let dir = path.parent().ok_or("no directory for the snapshot")?;
    // written whole (this user's alone), then renamed: a reader sees no file or the whole one
    let tmp = dir.join(format!(".{}.snap.tmp", std::process::id()));
    webapi::snapshot::write_private(&tmp, &file).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}
