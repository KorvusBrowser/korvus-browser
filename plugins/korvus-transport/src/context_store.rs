//! A browser context's shared state: what every page of one context sees alike -- its cookies, its origins'
//! `localStorage`, Cache API and IndexedDB -- held by the browser endpoint (`korvus browser`, `cdp::router`) and
//! reached from each page process of the context, as Chrome keeps a profile's storage in its browser process and
//! not in a renderer.
//!
//! Every page target is a process of its own (`cdp::router`), so two pages of one context share nothing in memory.
//! The router runs one [`Server`]; each page process of a context [`connect`]s to it with the context's id. The
//! server holds **slots**: named, versioned byte blobs, one set per context, dropped with the context. A page never
//! sees another context's slots (D8, invariant I2): a connection is bound to its context at `Hello`.
//!
//! The page side is [`Synced`]: a local copy of one slot's value, refreshed from the router when the router's
//! revision moved, and written back when a change left it different. [`Synced::update`] holds the slot's lock in
//! the router for the duration of the read-modify-write, so two pages writing one slot never lose either write.
//! No code here holds two slot locks at once, and each thread has its own connection, so a lock waited on blocks
//! that thread alone and no cycle can form.
//!
//! Without a router -- `korvus serve`, `fetch`, `run`, the tests -- nothing is connected and [`Synced`] is a plain
//! local value: the process is the context, as it always was.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Where a page process of a context finds its context's state: the router's store, the token every connection
/// shows, and the context it is bound to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Endpoint {
    pub addr: String,
    pub token: String,
    pub context: String,
}

#[derive(Serialize, Deserialize, Debug)]
enum Req {
    Hello { token: String, context: String },
    /// The slot's value when its revision is not `have`.
    Get { key: String, have: u64 },
    /// As `Get`, holding the slot's lock until this connection's `Unlock` (or until it closes).
    Lock { key: String, have: u64 },
    /// Release the lock, storing `put` first when the holder changed the value.
    Unlock { key: String, put: Option<Vec<u8>> },
    /// Store a value without a lock: last writer wins.
    Put { key: String, data: Vec<u8> },
}

#[derive(Serialize, Deserialize, Debug)]
enum Resp {
    Ok,
    Refused(String),
    /// The slot's revision, and its bytes unless the asker already had that revision. A slot never written is
    /// revision 0 with no bytes.
    Value { rev: u64, data: Option<Vec<u8>> },
    Rev(u64),
}

/// Frames are a little-endian u32 length and that many bytes of bincode.
fn write_frame<T: Serialize>(s: &mut TcpStream, v: &T) -> std::io::Result<()> {
    let body = bincode::serialize(v).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&body);
    s.write_all(&buf)
}

fn read_frame<T: DeserializeOwned>(s: &mut TcpStream) -> std::io::Result<T> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    s.read_exact(&mut body)?;
    bincode::deserialize(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ── the router's side ────────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Slot {
    rev: u64,
    data: Vec<u8>,
    /// The connection holding the slot's lock.
    holder: Option<u64>,
}

#[derive(Default)]
struct ContextState {
    slots: Mutex<HashMap<String, Slot>>,
    released: Condvar,
}

impl ContextState {
    /// A slot never written (revision 0) has no bytes to send.
    fn answer(slot: &Slot, have: u64) -> Resp {
        Resp::Value { rev: slot.rev, data: (slot.rev != have && slot.rev != 0).then(|| slot.data.clone()) }
    }
}

/// The store a browser endpoint runs for its contexts, on a loopback port of its own.
pub struct Server {
    addr: String,
    token: String,
    contexts: Arc<Mutex<HashMap<String, Arc<ContextState>>>>,
}

impl Server {
    /// Listen on an ephemeral loopback port. A connection must show this server's token, a fresh 128-bit secret, so
    /// another local process cannot read a context's cookies by guessing the port.
    pub fn start() -> std::io::Result<Server> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?.to_string();
        let mut secret = [0u8; 16];
        getrandom::getrandom(&mut secret).map_err(|e| std::io::Error::other(e.to_string()))?;
        let token: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        let contexts: Arc<Mutex<HashMap<String, Arc<ContextState>>>> = Arc::default();
        let (t, c) = (token.clone(), contexts.clone());
        std::thread::Builder::new().name("korvus-context-store".into()).spawn(move || {
            let mut next = 1u64;
            for stream in listener.incoming().flatten() {
                let (t, c, id) = (t.clone(), c.clone(), next);
                next += 1;
                let _ = std::thread::Builder::new().name("korvus-context-store-conn".into()).spawn(move || serve_conn(stream, id, &t, &c));
            }
        })?;
        Ok(Server { addr, token, contexts })
    }

    /// What a page process of `context` connects with.
    pub fn endpoint(&self, context: &str) -> Endpoint {
        Endpoint { addr: self.addr.clone(), token: self.token.clone(), context: context.to_string() }
    }

    /// A context begins: its slots start empty.
    pub fn open(&self, context: &str) {
        self.contexts.lock().unwrap_or_else(|e| e.into_inner()).entry(context.to_string()).or_default();
    }

    /// A context ends: its state goes with it, and a page that still asks is refused.
    pub fn close(&self, context: &str) {
        self.contexts.lock().unwrap_or_else(|e| e.into_inner()).remove(context);
    }

    /// Write `context`'s slots to `path` -- a named persona's profile (goal 1020 step 5): what its pages stored,
    /// cookies and storage, kept across runs. Written whole to a sibling file, then renamed over `path`.
    pub fn save(&self, context: &str, path: &std::path::Path) -> std::io::Result<()> {
        let rows: Vec<(String, Vec<u8>)> = {
            let map = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
            let Some(ctx) = map.get(context) else { return Ok(()) };
            let slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
            let mut rows: Vec<(String, Vec<u8>)> = slots.iter().filter(|(_, s)| s.rev != 0).map(|(k, s)| (k.clone(), s.data.clone())).collect();
            rows.sort();
            rows
        };
        let body = bincode::serialize(&rows).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let part = path.with_extension("part");
        std::fs::write(&part, body)?;
        std::fs::rename(&part, path)
    }

    /// Fill `context`'s slots from `path`, a profile [`save`](Self::save) wrote, when there is one -- before its first
    /// page, which then reads what the persona stored last time.
    pub fn load(&self, context: &str, path: &std::path::Path) -> std::io::Result<()> {
        let body = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let rows: Vec<(String, Vec<u8>)> = bincode::deserialize(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let map = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        let Some(ctx) = map.get(context) else { return Ok(()) };
        let mut slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
        for (k, data) in rows {
            slots.insert(k, Slot { rev: 1, data, holder: None });
        }
        Ok(())
    }

    /// Serialize `context`'s slots to a string blob -- the login-reuse case (goal 1021 step 5, E1; extended to the
    /// whole profile in step 13): what its pages stored, for a storage plugin's `storage_save` to keep and hand back
    /// next time ([`import`](Self::import)). Every storage area a context keeps is an ordinary slot of this store, so
    /// this carries the FULL profile by construction, each area told apart by its slot name's prefix (`storage_area`):
    /// the `cookies` slot, each origin's `localStorage`, every IndexedDB database (`indexedDB\0<origin>\0<db>`, its
    /// records already bytes) with its origin's `indexedDB-names` index, and every origin's CacheStorage (`caches`).
    /// A JSON object of slot name -> base64 of its bytes (so a slot's arbitrary bytes survive the UTF-8/NUL-free blob
    /// a C plugin holds as a string), its slots sorted as [`save`](Self::save)'s rows are. `None` when the context is
    /// gone or has stored nothing.
    pub fn export(&self, context: &str) -> Option<String> {
        use base64::Engine as _;
        let rows: std::collections::BTreeMap<String, String> = {
            let map = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
            let ctx = map.get(context)?;
            let slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots.iter().filter(|(_, s)| s.rev != 0).map(|(k, s)| (k.clone(), base64::engine::general_purpose::STANDARD.encode(&s.data))).collect()
        };
        if rows.is_empty() {
            return None;
        }
        serde_json::to_string(&rows).ok()
    }

    /// Fill `context`'s slots from a `blob` [`export`](Self::export) wrote (a storage plugin's `storage_load`), before
    /// its first page, which then reads the reused login -- the whole profile it banked, every area at once: the
    /// `cookies` slot, `localStorage`, every IndexedDB database (and its origin's `indexedDB-names` index) and
    /// CacheStorage, each restored to the slot its name picks out, so the context's first page pulls it (step 13).
    /// The context must be open ([`open`](Self::open)) already; a malformed blob, or a slot whose base64 does not
    /// decode, is skipped and the rest left untouched.
    pub fn import(&self, context: &str, blob: &str) {
        use base64::Engine as _;
        let Ok(rows) = serde_json::from_str::<std::collections::BTreeMap<String, String>>(blob) else { return };
        let map = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        let Some(ctx) = map.get(context) else { return };
        let mut slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
        for (k, b64) in rows {
            if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
                slots.insert(k, Slot { rev: 1, data, holder: None });
            }
        }
    }

    /// Slot `key` of `context` as it stands (empty when never written); `None` when the context is gone.
    pub fn slot(&self, context: &str, key: &str) -> Option<Vec<u8>> {
        let map = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        let ctx = map.get(context)?;
        let slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
        Some(slots.get(key).filter(|s| s.rev != 0).map(|s| s.data.clone()).unwrap_or_default())
    }

    /// Read-modify-write slot `key` of `context` from the router's side -- a DevTools client's browser-level write
    /// (`Storage.setCookies {browserContextId}`) -- waiting while a page holds its lock, so neither write is lost. The
    /// pages pull the new revision on their next use. `false` when the context is gone.
    pub fn update_slot(&self, context: &str, key: &str, f: impl FnOnce(&mut Vec<u8>)) -> bool {
        let ctx = {
            let map = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(context) {
                Some(c) => c.clone(),
                None => return false,
            }
        };
        let mut slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
        while slots.get(key).is_some_and(|s| s.holder.is_some()) {
            slots = ctx.released.wait(slots).unwrap_or_else(|e| e.into_inner());
        }
        let slot = slots.entry(key.to_string()).or_default();
        let mut data = if slot.rev == 0 { Vec::new() } else { slot.data.clone() };
        f(&mut data);
        slot.rev += 1;
        slot.data = data;
        true
    }

    /// The slots a context holds (tests and diagnostics).
    pub fn keys(&self, context: &str) -> Vec<String> {
        let map = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        let Some(ctx) = map.get(context) else { return Vec::new() };
        let mut keys: Vec<String> = ctx.slots.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        keys.sort();
        keys
    }
}

fn serve_conn(mut s: TcpStream, me: u64, token: &str, contexts: &Mutex<HashMap<String, Arc<ContextState>>>) {
    let _ = s.set_nodelay(true);
    let ctx = match read_frame::<Req>(&mut s) {
        Ok(Req::Hello { token: t, context }) if t == token => {
            let found = contexts.lock().unwrap_or_else(|e| e.into_inner()).get(&context).cloned();
            match found {
                Some(c) => {
                    let _ = write_frame(&mut s, &Resp::Ok);
                    c
                }
                None => {
                    let _ = write_frame(&mut s, &Resp::Refused(format!("no context {context}")));
                    return;
                }
            }
        }
        _ => {
            let _ = write_frame(&mut s, &Resp::Refused("bad hello".into()));
            return;
        }
    };
    let mut held: HashSet<String> = HashSet::new();
    while let Ok(req) = read_frame::<Req>(&mut s) {
        let resp = match req {
            Req::Hello { .. } => Resp::Refused("already bound".into()),
            Req::Get { key, have } => {
                let slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
                slots.get(&key).map_or(Resp::Value { rev: 0, data: None }, |slot| ContextState::answer(slot, have))
            }
            Req::Lock { key, have } => {
                let mut slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let slot = slots.entry(key.clone()).or_default();
                    match slot.holder {
                        Some(h) if h != me => slots = ctx.released.wait(slots).unwrap_or_else(|e| e.into_inner()),
                        _ => {
                            slot.holder = Some(me);
                            held.insert(key.clone());
                            break ContextState::answer(slot, have);
                        }
                    }
                }
            }
            Req::Unlock { key, put } => {
                let mut slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
                let slot = slots.entry(key.clone()).or_default();
                if slot.holder == Some(me) {
                    if let Some(data) = put {
                        slot.rev += 1;
                        slot.data = data;
                    }
                    slot.holder = None;
                    held.remove(&key);
                    ctx.released.notify_all();
                }
                Resp::Rev(slot.rev)
            }
            Req::Put { key, data } => {
                let mut slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
                let slot = slots.entry(key).or_default();
                slot.rev += 1;
                slot.data = data;
                Resp::Rev(slot.rev)
            }
        };
        if write_frame(&mut s, &resp).is_err() {
            break;
        }
    }
    // A page that went (or crashed) mid-update leaves no slot locked behind it.
    if !held.is_empty() {
        let mut slots = ctx.slots.lock().unwrap_or_else(|e| e.into_inner());
        for key in held {
            if let Some(slot) = slots.get_mut(&key) {
                if slot.holder == Some(me) {
                    slot.holder = None;
                }
            }
        }
        ctx.released.notify_all();
    }
}

// ── a page's side ────────────────────────────────────────────────────────────────────────────────────

static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();

thread_local! {
    /// Each thread its own connection: a lock one thread waits on never holds up another's unlock.
    static CONN: RefCell<Option<TcpStream>> = const { RefCell::new(None) };
}

/// Bind this process to its context's state in the router. Called once, by a page process the router started
/// (`korvus serve … --context <spec>`), before any page runs. Fails when the router refuses the context.
pub fn connect(endpoint: Endpoint) -> std::io::Result<()> {
    let s = open_conn(&endpoint)?;
    let _ = ENDPOINT.set(endpoint);
    CONN.with(|c| *c.borrow_mut() = Some(s));
    Ok(())
}

/// This process shares its context's state with the context's other pages.
pub fn is_remote() -> bool {
    ENDPOINT.get().is_some()
}

fn open_conn(e: &Endpoint) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect(&e.addr)?;
    let _ = s.set_nodelay(true);
    write_frame(&mut s, &Req::Hello { token: e.token.clone(), context: e.context.clone() })?;
    match read_frame::<Resp>(&mut s)? {
        Resp::Ok => Ok(s),
        Resp::Refused(why) => Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, why)),
        other => Err(std::io::Error::other(format!("unexpected hello answer {other:?}"))),
    }
}

fn call(req: &Req) -> std::io::Result<Resp> {
    let Some(e) = ENDPOINT.get() else { return Err(std::io::Error::other("no context store")) };
    CONN.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(open_conn(e)?);
        }
        let s = c.as_mut().expect("connection just opened");
        let r = write_frame(s, req).and_then(|_| read_frame::<Resp>(s));
        if r.is_err() {
            *c = None;
        }
        r
    })
}

/// The raw slot API, for a value that is not one serde type (IndexedDB's records are V8 values, which only the
/// page's isolate can turn into bytes): `(revision, bytes)`, the bytes only when the revision is not `have`. `None`
/// without a router.
pub fn get_raw(key: &str, have: u64) -> Option<(u64, Option<Vec<u8>>)> {
    match call(&Req::Get { key: key.to_string(), have }) {
        Ok(Resp::Value { rev, data }) => Some((rev, data)),
        _ => None,
    }
}

/// Store `data` in slot `key` (last writer wins); the new revision. `None` without a router.
pub fn put_raw(key: &str, data: Vec<u8>) -> Option<u64> {
    match call(&Req::Put { key: key.to_string(), data }) {
        Ok(Resp::Rev(r)) => {
            // ABI 3 (step 12, E2): a raw slot -- an IndexedDB record -- was written: a storage change.
            note_storage_change(key);
            Some(r)
        }
        _ => None,
    }
}

// ── ABI 3 (step 12): cookie (D9) and storage-change (E2) events to a watching plugin ───────────────────

/// This page process's context id -- the slot store it is bound to ([`connect`]) -- or "" when it runs no router
/// (`korvus serve` / `fetch` / `run` / the tests): the `ctx` the plugin events below carry.
fn context() -> String {
    ENDPOINT.get().map(|e| e.context.clone()).unwrap_or_default()
}

/// A slot key's storage AREA as the header names it to a plugin -- "local", "indexeddb", "cache" -- mapped from the
/// key's prefix (the segment before the first NUL: `localStorage` / `caches` / `indexedDB` / `indexedDB-names`, each
/// followed by an origin, and a record also by a database key). It is what
/// [`on_storage_change`](plugin_host::on_storage_change) reports as its `slot`. The `cookies` slot (no NUL) is
/// returned verbatim so [`note_storage_change`] can leave it to the D9 cookie events; any other unmapped prefix is
/// reported as-is. TODO(1021-verify): sessionStorage never reaches the router store (it is per-process), so "session"
/// is not emitted from here; the area vocabulary follows korvus_plugin.h's `on_storage_change` doc.
fn storage_area(key: &str) -> &str {
    match key.split('\u{0}').next().unwrap_or(key) {
        "localStorage" => "local",
        "caches" => "cache",
        "indexedDB" | "indexedDB-names" => "indexeddb",
        other => other,
    }
}

/// ABI 3 (step 12, D9): tell a cookie-watching plugin a cookie was set or deleted. The cookie jar
/// (`crate::cookies`) calls this as it records each `Set-Cookie`, `document.cookie` or `cookieStore` change, with
/// the change's cookie `name`, its `domain`, and `op` ("set" / "delete"). Gated on [`plugin_host::wants_cookies`],
/// so a run with no cookie watcher -- every no-plugin run (invariant I9/J2) -- does nothing.
pub fn note_cookie(name: &str, domain: &str, op: &str) {
    if plugin_host::wants_cookies() {
        plugin_host::on_cookie(&context(), name, domain, op);
    }
}

/// ABI 3 (step 12, E2): tell a storage-watching plugin a context's storage area changed. Called from the slot
/// write-back ([`Synced::end`]) and the raw put ([`put_raw`]) whenever a value actually changed. The `cookies`
/// slot is left to the D9 cookie events ([`note_cookie`]) and not reported as a storage change. Gated on
/// [`plugin_host::wants_storage_changes`], so a run with no storage watcher does nothing (invariant I9/J2).
fn note_storage_change(key: &str) {
    if !plugin_host::wants_storage_changes() {
        return;
    }
    let area = storage_area(key);
    if area == "cookies" {
        return;
    }
    plugin_host::on_storage_change(&context(), area);
}

/// One slot's value as this page last saw it. Without a router it is simply the value.
pub struct Synced<T> {
    key: String,
    rev: u64,
    /// The bytes of `rev`, to tell whether an update changed the value.
    bytes: Vec<u8>,
    value: T,
}

impl<T: Serialize + DeserializeOwned + Default> Synced<T> {
    pub fn new(key: impl Into<String>) -> Self {
        Synced { key: key.into(), rev: 0, bytes: Vec::new(), value: T::default() }
    }

    fn take(&mut self, rev: u64, data: Option<Vec<u8>>) {
        if let Some(d) = data {
            if let Ok(v) = bincode::deserialize::<T>(&d) {
                self.value = v;
                self.bytes = d;
            }
        }
        self.rev = rev;
    }

    /// Read the value, current with the context.
    pub fn read<R>(&mut self, f: impl FnOnce(&T) -> R) -> R {
        if is_remote() {
            if let Some((rev, data)) = get_raw(&self.key, self.rev) {
                self.take(rev, data);
            }
        }
        f(&self.value)
    }

    /// Read-modify-write the value, holding the slot's lock in the router meanwhile.
    pub fn update<R>(&mut self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock();
        f(&mut guard)
    }

    /// The value, locked in the router until the guard drops.
    pub fn lock(&mut self) -> SyncedGuard<'_, T> {
        let locked = self.begin();
        SyncedGuard { s: self, locked }
    }

    /// Take the slot's lock in the router and bring the value current; `false` without a router (or when it could
    /// not be reached), when the local value is the value. Pair with [`Synced::end`].
    pub fn begin(&mut self) -> bool {
        is_remote()
            && match call(&Req::Lock { key: self.key.clone(), have: self.rev }) {
                Ok(Resp::Value { rev, data }) => {
                    self.take(rev, data);
                    true
                }
                _ => false,
            }
    }

    /// Write the value back when it changed since [`Synced::begin`], and release the lock `begin` took.
    pub fn end(&mut self, locked: bool) {
        if !locked {
            return;
        }
        let now = bincode::serialize(&self.value).unwrap_or_default();
        let changed = now != self.bytes;
        let put = changed.then(|| now.clone());
        if let Ok(Resp::Rev(rev)) = call(&Req::Unlock { key: self.key.clone(), put }) {
            self.rev = rev;
            if changed {
                self.bytes = now;
                // ABI 3 (step 12, E2): the slot's value changed in the context; tell a storage-watching plugin.
                note_storage_change(&self.key);
            }
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn value_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

/// [`Synced::lock`]'s guard: the value, written back (when changed) and unlocked on drop.
pub struct SyncedGuard<'a, T: Serialize + DeserializeOwned + Default> {
    s: &'a mut Synced<T>,
    locked: bool,
}

impl<T: Serialize + DeserializeOwned + Default> std::ops::Deref for SyncedGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.s.value
    }
}

impl<T: Serialize + DeserializeOwned + Default> std::ops::DerefMut for SyncedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.s.value
    }
}

impl<T: Serialize + DeserializeOwned + Default> Drop for SyncedGuard<'_, T> {
    fn drop(&mut self) {
        self.s.end(self.locked);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(e: &Endpoint) -> TcpStream {
        open_conn(e).expect("hello accepted")
    }

    fn ask(s: &mut TcpStream, r: Req) -> Resp {
        write_frame(s, &r).unwrap();
        read_frame(s).unwrap()
    }

    #[test]
    fn a_context_sees_its_own_slots_and_no_other() {
        let srv = Server::start().unwrap();
        srv.open("A");
        srv.open("B");
        let mut a = conn(&srv.endpoint("A"));
        let mut a2 = conn(&srv.endpoint("A"));
        let mut b = conn(&srv.endpoint("B"));
        assert!(matches!(ask(&mut a, Req::Put { key: "k".into(), data: b"one".to_vec() }), Resp::Rev(1)));
        match ask(&mut a2, Req::Get { key: "k".into(), have: 0 }) {
            Resp::Value { rev: 1, data: Some(d) } => assert_eq!(d, b"one"),
            other => panic!("the context's other page should read it: {other:?}"),
        }
        assert!(matches!(ask(&mut b, Req::Get { key: "k".into(), have: 0 }), Resp::Value { rev: 0, data: None }));
        // an unchanged revision sends no bytes
        assert!(matches!(ask(&mut a2, Req::Get { key: "k".into(), have: 1 }), Resp::Value { rev: 1, data: None }));
    }

    #[test]
    fn a_profile_saved_from_one_run_fills_the_next_one_s_context() {
        let path = std::env::temp_dir().join(format!("korvus-profile-test-{}/store.bin", std::process::id()));
        let first = Server::start().unwrap();
        first.open("A");
        let mut a = conn(&first.endpoint("A"));
        assert!(matches!(ask(&mut a, Req::Put { key: "cookies".into(), data: b"sid=1".to_vec() }), Resp::Rev(1)));
        first.save("A", &path).unwrap();
        // another endpoint, another context id: the persona's profile is what carries over
        let next = Server::start().unwrap();
        next.open("B");
        next.load("B", &path).unwrap();
        let mut b = conn(&next.endpoint("B"));
        match ask(&mut b, Req::Get { key: "cookies".into(), have: 0 }) {
            Resp::Value { rev: 1, data: Some(d) } => assert_eq!(d, b"sid=1"),
            other => panic!("the profile's cookies should be there: {other:?}"),
        }
        next.load("B", &path.with_file_name("none.bin")).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_wrong_token_or_an_unknown_context_is_refused() {
        let srv = Server::start().unwrap();
        srv.open("A");
        let mut bad = srv.endpoint("A");
        bad.token = "0".repeat(32);
        assert!(open_conn(&bad).is_err());
        assert!(open_conn(&srv.endpoint("nope")).is_err());
        srv.close("A");
        assert!(open_conn(&srv.endpoint("A")).is_err(), "a closed context's state is gone");
    }

    #[test]
    fn a_lock_waits_for_its_holder_and_neither_write_is_lost() {
        let srv = Server::start().unwrap();
        srv.open("A");
        let e = srv.endpoint("A");
        let mut threads = Vec::new();
        for _ in 0..8 {
            let e = e.clone();
            threads.push(std::thread::spawn(move || {
                let mut s = conn(&e);
                for _ in 0..25 {
                    let n = match ask(&mut s, Req::Lock { key: "n".into(), have: u64::MAX }) {
                        Resp::Value { data, .. } => data.map(|d| u64::from_le_bytes(d.try_into().unwrap())).unwrap_or(0),
                        other => panic!("{other:?}"),
                    };
                    assert!(matches!(ask(&mut s, Req::Unlock { key: "n".into(), put: Some((n + 1).to_le_bytes().to_vec()) }), Resp::Rev(_)));
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        let mut s = conn(&e);
        match ask(&mut s, Req::Get { key: "n".into(), have: 0 }) {
            Resp::Value { data: Some(d), .. } => assert_eq!(u64::from_le_bytes(d.try_into().unwrap()), 200),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_holder_that_disconnects_releases_its_lock() {
        let srv = Server::start().unwrap();
        srv.open("A");
        let e = srv.endpoint("A");
        let mut first = conn(&e);
        assert!(matches!(ask(&mut first, Req::Lock { key: "k".into(), have: 0 }), Resp::Value { .. }));
        drop(first);
        let mut second = conn(&e);
        assert!(matches!(ask(&mut second, Req::Lock { key: "k".into(), have: 0 }), Resp::Value { .. }));
    }
}
