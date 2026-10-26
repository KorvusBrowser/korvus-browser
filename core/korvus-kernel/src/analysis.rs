//! The call recorder's contract (korvus-analysis, Layer 1): what one recorded Web API call is, and the
//! queue it leaves the JS thread on.
//!
//! # Who writes, who reads
//!
//! `korvus-webapi`, built with its `analysis` feature, records every page call that enters native code
//! (a template function, a getter, a setter, a stub, an op2 member) as a [`Call`] -- see
//! `core/korvus-webapi/src/analysis_hook.rs`. `plugins/korvus-analysis` drains the queue on its own
//! thread and writes NDJSON. Without the feature nothing here is ever called: a production build has no
//! hook on the member-call path at all, because any hook there cost 5-12 % per call (`touch-log`,
//! measured 2026-09-27).
//!
//! # Never blocking the engine
//!
//! The values a call carries live in its isolate and can only be read on its thread, at the moment of the
//! call -- so the CAPTURE is synchronous and small: a digest per argument, the result's digest, the stack
//! frames, an id. Everything else is the writer's. The JS thread appends to a THREAD-LOCAL batch (no
//! lock, no syscall, no allocation beyond the call's own) and hands a full batch over with `try_send`.
//! The writer's drain thread only copies batches out (formatting is another thread's), so the channel
//! has room unless a burst outruns a memcpy; then the JS thread waits for room (up to 2 s), and only a
//! writer that is gone makes it DROP a batch, counted in [`dropped`]. The trace says how much it lost.
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};

/// A recorded value: enough to compare two engines' answers and to follow an object from the call that
/// returned it to the calls that receive it, never the value itself (no JS runs to make a digest).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Val {
    Undefined,
    Null,
    Bool { v: bool },
    Num { v: f64 },
    BigInt { v: String },
    /// a string; `len` is the whole length in UTF-16 units, `v` at most [`STR_CAP`] bytes of it
    Str { v: String, len: usize },
    Symbol,
    /// an object: its constructor name and V8's identity hash (stable for the object's life, invisible to JS)
    Obj { class: &'static str, id: i32 },
    Fn { name: String, id: i32 },
}

/// The longest string prefix a digest keeps.
pub const STR_CAP: usize = 256;

/// One JS stack frame at the call.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Frame {
    pub script: String,
    pub func: String,
    pub line: u32,
    pub col: u32,
}

/// How the call entered native code.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Kind {
    /// a template function, getter or setter (`cb_fused`)
    Call = 0,
    /// a member with no implementation (a stub row): the page reached something korvus does not do
    Stub = 1,
    /// an op2 class member (`behavior_rt::dispatch!`), nested inside the call that reached it
    Op = 2,
    /// not a call: the full JS stack of a call SITE, emitted the first time the site is seen (a call
    /// carries only its `site` id unless the recorder runs with full stacks)
    Site = 3,
    /// the engine calling INTO the page: a listener, a timer, a frame callback, a microtask, an observer.
    /// The label is the IDL callback type the engine invoked (`EventListener`, `EventHandlerNonNull`,
    /// `Function` for a timer, `FrameRequestCallback`, `VoidFunction`, `MutationCallback`, ...), `this`
    /// the callback's this value, `args` what it was called with; calls the page makes inside it are its
    /// children (`parent`).
    Callback = 4,
    /// not a call: what a realm id names -- its global's class and its URL, emitted the first time a
    /// thread records a call in that realm, so a diff pairs realms by identity, not by order
    Realm = 5,
    /// a script's call to a JS BUILTIN (`Math.sin`, `JSON.parse`, `Intl.DateTimeFormat.prototype.
    /// resolvedOptions`; a construct is labelled with the constructor, `Intl.DateTimeFormat`), reported by
    /// V8 itself (tools/v8-trace, `--korvus-trace-builtins`): `this`, `args`, `result`; a call that
    /// throws is not reported
    Builtin = 6,
    /// a script's read of an interface CONSTANT (`Node.ELEMENT_NODE`), reported by V8 the same way:
    /// `this` the interface object, `result` the value
    Get = 7,
}

/// One recorded call.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Call {
    /// the recording process (a Chromium trace interleaves several renderers; korvus has one)
    pub proc: u32,
    /// process-wide order of ENTRY (one counter for every thread)
    pub seq: u64,
    /// the call this one ran inside, on the same thread (an op inside its template body)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// nanoseconds since the recorder started
    pub t: u64,
    pub thread: u64,
    /// identity hash of the realm's global object: the page's window, a frame's, a worker's
    pub realm: i32,
    pub kind: Kind,
    /// `Interface.member` (`get x` / `set x` for an accessor)
    pub label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub this: Option<Val>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Val>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Val>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub threw: bool,
    /// the innermost script on the JS stack (its URL), on every call -- what tells a page call from an
    /// engine-internal one without a stack walk
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<&'static str>,
    /// the call site, recorded for the first few calls of each label on a thread (`SITES`): its full
    /// stack is the `Site` record with this id and `proc`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<u32>,
    /// the full stack, only when the recorder runs with `STACK=full`
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<Frame>,
}

/// A call site's full stack, written once per site (`Kind::Site`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Site {
    pub kind: Kind,
    pub proc: u32,
    pub site: u32,
    pub stack: Vec<Frame>,
}

/// What a realm id names (`Kind::Realm`), written once per realm and thread.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Realm {
    pub kind: Kind,
    pub proc: u32,
    pub realm: i32,
    /// the global's class: `Window`, `DedicatedWorkerGlobalScope`, ...
    pub global: String,
    /// the realm's URL (its document's or its worker's), "" when the engine has none to give
    pub url: String,
}

/// One line of the trace.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum Rec {
    Call(Call),
    Site(Site),
    Realm(Realm),
}

/// Bytes a thread's buffer holds before it is handed to the writer.
pub const BATCH_BYTES: usize = 128 * 1024;
/// Buffers the channel holds before a send drops one.
pub const QUEUE: usize = 1024;
/// How far past the write position [`committed`] asks for batch lines (bytes): ~20 records.
const PREFETCH_AHEAD: usize = 2048;

static ON: AtomicBool = AtomicBool::new(false);
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// How many 50 us rounds a full channel is waited on before a batch is dropped (2 s).
static WAIT_ROUNDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(40_000);
static TX: OnceLock<SyncSender<(Vec<u8>, u32)>> = OnceLock::new();
static START: OnceLock<std::time::Instant> = OnceLock::new();
static THREADS: AtomicU64 = AtomicU64::new(1);
/// The stack depth a SITE captures (`KORVUS_ANALYSIS_STACK`, default 8; 0 = no sites, no stacks).
static DEPTH: AtomicU64 = AtomicU64::new(8);
/// `KORVUS_ANALYSIS_STACK=full`: every call carries its own stack (the cost the site table avoids).
static FULL: AtomicBool = AtomicBool::new(false);
static SITES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
/// `KORVUS_ANALYSIS_SITES`: how many calls of each label read their call site (read once, at [`open`]).
static SITES_PER_LABEL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(4);

/// Open the queue and hand its receiving end to the writer: buffers of encoded records with their record
/// count ([`decode`] turns one into [`Rec`]s). The first caller wins; later ones get `None`.
pub fn open(stack_depth: usize, full_stacks: bool) -> Option<Receiver<(Vec<u8>, u32)>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    TX.set(tx).ok()?;
    let _ = START.set(std::time::Instant::now());
    #[cfg(target_arch = "x86_64")]
    calibrate_tsc();
    DEPTH.store(stack_depth as u64, Ordering::Relaxed);
    FULL.store(full_stacks, Ordering::Relaxed);
    if let Some(n) = std::env::var("KORVUS_ANALYSIS_SITES").ok().and_then(|v| v.parse().ok()) {
        SITES_PER_LABEL.store(n, Ordering::Relaxed);
    }
    ON.store(true, Ordering::Release);
    Some(rx)
}

/// Is a recorder listening? The hook's first question: with nobody listening it records nothing.
#[inline]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

#[inline]
pub fn stack_depth() -> usize {
    DEPTH.load(Ordering::Relaxed) as usize
}

/// Every call carries its own stack (no site table).
#[inline]
pub fn full_stacks() -> bool {
    FULL.load(Ordering::Relaxed)
}

/// This process's id, as every record carries it (read once: `getpid` per call showed in the profile).
pub fn proc_id() -> u32 {
    static PID: OnceLock<u32> = OnceLock::new();
    *PID.get_or_init(std::process::id)
}

/// Nanoseconds since the recorder opened. On x86_64 this reads the time-stamp counter (a few ns; the
/// vDSO clock was ~3% of a recorded call's profile) scaled by a rate measured once at `open`.
/// The raw clock a record carries: TSC ticks on x86_64 (converted by [`ticks_to_ns`] in the writer).
#[inline(always)]
fn ticks() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: RDTSC has no preconditions on x86_64.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        now_ns()
    }
}

/// A record's raw clock as nanoseconds since the recorder started.
fn ticks_to_ns(t: u64) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        match TSC.get() {
            Some(&(t0, per_ns)) => (t.saturating_sub(t0) as f64 / per_ns) as u64,
            None => 0,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        t
    }
}

pub fn now_ns() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        let (t0, per_ns) = match TSC.get() {
            Some(&v) => v,
            None => return 0,
        };
        // SAFETY: RDTSC has no preconditions on x86_64.
        let t = unsafe { core::arch::x86_64::_rdtsc() };
        (t.saturating_sub(t0) as f64 / per_ns) as u64
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        START.get().map(|s| s.elapsed().as_nanos() as u64).unwrap_or(0)
    }
}

/// (counter at open, ticks per ns), measured over 5 ms at `open`.
#[cfg(target_arch = "x86_64")]
static TSC: OnceLock<(u64, f64)> = OnceLock::new();

#[cfg(target_arch = "x86_64")]
fn calibrate_tsc() {
    // SAFETY: RDTSC has no preconditions on x86_64.
    let (c0, i0) = (unsafe { core::arch::x86_64::_rdtsc() }, std::time::Instant::now());
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (c1, i1) = (unsafe { core::arch::x86_64::_rdtsc() }, std::time::Instant::now());
    let per_ns = (c1 - c0) as f64 / (i1 - i0).as_nanos().max(1) as f64;
    let _ = TSC.set((c0, per_ns.max(0.001)));
}

// ---- the recorder's own time (goal T) ---------------------------------------------------------------------

/// TSC ticks spent inside the recorder, one counter per thread (each written only by its thread, so an
/// update is a plain load and store, no locked instruction), summed by [`recorder_ns`]: what a traced
/// run's page clocks leave out.
static RECORDER_TICKS: Mutex<Vec<&'static AtomicU64>> = Mutex::new(Vec::new());

thread_local! {
    /// This thread's recorder-time counter, registered on first use. CONSTANT-initialized: an access is
    /// a plain thread-pointer read, where a lazily-initialized thread-local called an accessor with an
    /// init check on every guard drop.
    static MY_TICKS: std::cell::Cell<Option<&'static AtomicU64>> = const { std::cell::Cell::new(None) };
}

/// This thread's counter in [`RECORDER_TICKS`].
#[inline]
fn my_ticks() -> &'static AtomicU64 {
    MY_TICKS.with(|m| match m.get() {
        Some(c) => c,
        None => register_ticks(m),
    })
}

#[cold]
#[inline(never)]
fn register_ticks(m: &std::cell::Cell<Option<&'static AtomicU64>>) -> &'static AtomicU64 {
    let c: &'static AtomicU64 = Box::leak(Box::new(AtomicU64::new(0)));
    RECORDER_TICKS.lock().unwrap_or_else(|e| e.into_inner()).push(c);
    m.set(Some(c));
    c
}

/// The recorder's own time on the BINDING path (goal T), with one clock read a call instead of four
/// (the four reads of two [`Spent`] guards were ~15-20 ns of a recorded setter, measured
/// 2026-10-06). Every [`CALL_TIME_EXACT`]th call times the recorder's work before and after the body
/// exactly, as [`Spent`] does, and keeps a running estimate of each; the others credit the estimates.
/// The clock read just before the body is the record's time.
pub struct CallTime {
    t0: u64,
    t1: u64,
    t2: u64,
    exact: bool,
}

/// One binding call in this many is timed exactly (the first of each thread among them).
pub const CALL_TIME_EXACT: u32 = 16;

thread_local! {
    /// (calls seen, estimated ticks before the body, estimated ticks after it)
    static CALL_TIME: std::cell::Cell<(u32, u64, u64)> = const { std::cell::Cell::new((0, 0, 0)) };
}

impl CallTime {
    /// At the call's very entry (before any recording work).
    #[inline]
    pub fn enter() -> CallTime {
        let n = CALL_TIME.with(|c| {
            let v = c.get();
            c.set((v.0.wrapping_add(1), v.1, v.2));
            v.0
        });
        let exact = n % CALL_TIME_EXACT == 0;
        CallTime { t0: if exact { ticks() } else { 0 }, t1: 0, t2: 0, exact }
    }

    /// Just before the body runs: the record's time (raw ticks, as [`Spent::at`]).
    #[inline]
    pub fn before_body(&mut self) -> u64 {
        self.t1 = ticks();
        self.t1
    }

    /// Just after the body ran.
    #[inline]
    pub fn after_body(&mut self) {
        if self.exact {
            self.t2 = ticks();
        }
    }

    /// The call is recorded: credit the recorder's time (exact, or the estimates).
    #[inline]
    pub fn done(self) {
        let add = CALL_TIME.with(|c| {
            let (n, pre, post) = c.get();
            if !self.exact {
                return pre + post;
            }
            let p = self.t1.wrapping_sub(self.t0);
            let q = ticks().wrapping_sub(self.t2);
            // a running mean over the last ~8 exact samples (the first sets it)
            let ewma = |e: u64, x: u64| if e == 0 { x } else { e - e / 8 + x / 8 };
            c.set((n, ewma(pre, p), ewma(post, q)));
            p + q
        });
        let c = my_ticks();
        c.store(c.load(Ordering::Relaxed).wrapping_add(add), Ordering::Relaxed);
    }

    /// The call is not recorded after all (not the page's): credit what an exact one measured.
    #[inline]
    pub fn abandon(self) {
        if self.exact {
            let c = my_ticks();
            c.store(c.load(Ordering::Relaxed).wrapping_add(ticks().wrapping_sub(self.t0)), Ordering::Relaxed);
        }
    }
}

/// Nanoseconds the recorder has spent so far: subtracted from every clock a page reads in a traced run
/// (`PerfClock::now_ms` here; V8's `Date` through tools/v8-trace), so a script that times itself sees
/// what it would without the recorder (goal T). 0 when the recorder is off.
///
/// `KORVUS_ANALYSIS_REAL_CLOCK=1` turns the subtraction off: a per-call bench
/// measures what recording COSTS, which a page clock that hides it cannot show (measured: a recorded
/// getter timed by the page at 16 ns).
/// Another component's own recorder ticks (the patched V8's hooks, tools/v8-trace), added to this
/// recorder's: set once by whoever links that component.
static EXTRA_TICKS: OnceLock<fn() -> u64> = OnceLock::new();

/// Count `f`'s ticks as the recorder's too ([`recorder_ns`]).
pub fn add_recorder_ticks(f: fn() -> u64) {
    let _ = EXTRA_TICKS.set(f);
}

pub fn recorder_ns() -> u64 {
    static REAL: OnceLock<bool> = OnceLock::new();
    if *REAL.get_or_init(|| std::env::var_os("KORVUS_ANALYSIS_REAL_CLOCK").is_some_and(|v| !v.is_empty() && v != "0")) {
        return 0;
    }
    #[cfg(target_arch = "x86_64")]
    {
        match TSC.get() {
            Some(&(_, per_ns)) => {
                let mut ticks: u64 = RECORDER_TICKS.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|c| c.load(Ordering::Relaxed)).sum();
                if let Some(f) = EXTRA_TICKS.get() {
                    ticks += f();
                }
                (ticks as f64 / per_ns) as u64
            }
            None => 0,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// Counts the time from its creation to its drop as the recorder's ([`recorder_ns`]). Put first in each
/// piece of recording work, so every return path is counted; a page's own work runs outside it.
pub struct Spent(u64);

impl Spent {
    /// When this started, in the raw clock a record carries: a call opened right after its guard takes
    /// its timestamp from here ([`call_begin_at`]) rather than reading the clock a second time.
    #[inline(always)]
    pub fn at(&self) -> u64 {
        self.0
    }
}

impl Spent {
    #[inline(always)]
    pub fn new() -> Spent {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: RDTSC has no preconditions on x86_64.
            Spent(unsafe { core::arch::x86_64::_rdtsc() })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            Spent(0)
        }
    }
}

impl Default for Spent {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Spent {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: as above.
            let t = unsafe { core::arch::x86_64::_rdtsc() };
            // this thread's own counter: one writer, so load + store
            let c = my_ticks();
            c.store(c.load(Ordering::Relaxed).wrapping_add(t.wrapping_sub(self.0)), Ordering::Relaxed);
        }
    }
}

/// Records lost because the writer was behind.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

// ---- the binary record ----------------------------------------------------------------------
//
// A call is ENCODED on the JS thread into bytes -- no allocation, no formatting -- and decoded back into a
// [`Call`] / [`Site`] by the writer, which serialises them exactly as before. Layout, little-endian:
//   call: 1 seq:u64 parent:u64(0=none) t:u64 thread:u64 realm:i32 kind:u8 label:sstr
//         this:opt<val> nargs:u8 val* result:opt<val> threw:u8 site:u32(0=none) frames
//   site: 2 site:u32 frames
//   realm: 3 realm:i32 global:str url:str
//   val:  0 undefined | 1 null | 2 bool u8 | 3 num f64 | 4 bigint str | 5 str len:u32 str | 6 symbol
//         | 7 obj class:sstr id:i32 | 8 fn name:str id:i32
//         | 9 str len:u32 n:u32 n Latin-1 bytes | 10 str len:u32 n:u32 n UTF-16 units (LE)
//   (9/10: a string's units copied raw -- the writer turns them into UTF-8, not the JS thread)
//   str:  len:u32 bytes;  sstr: a &'static str as ptr:u64 len:u64;  frames: n:u16 (str str u32 u32)*
// A call is written into a per-DEPTH scratch buffer (entry: header, this, args; exit: the rest), so a
// call made during another's body cannot interleave with it, and moved whole into the thread's buffer.

const TAG_CALL: u8 = 1;
const TAG_SITE: u8 = 2;
const TAG_REALM: u8 = 3;

/// The encoder handed to the hook.
pub struct Enc<'a>(&'a mut Vec<u8>);

impl Enc<'_> {
    /// `N` bytes in ONE write: one capacity check and one copy, where a field-by-field record made one
    /// of each per field (the header alone was ~9 checked writes a call).
    #[inline]
    fn block<const N: usize>(&mut self, b: [u8; N]) {
        self.0.extend_from_slice(&b);
    }
    #[inline]
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    #[inline]
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    fn sstr(&mut self, s: &'static str) {
        self.u64(s.as_ptr() as u64);
        self.u64(s.len() as u64);
    }
    #[inline]
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    /// A string of at most `max` bytes, written by `fill` straight into the buffer (it returns how
    /// many bytes it wrote; they must be UTF-8).
    #[inline]
    fn str_with(&mut self, max: usize, fill: impl FnOnce(&mut [u8]) -> usize) {
        let at = self.0.len();
        self.u32(0);
        let start = self.0.len();
        self.0.resize(start + max, 0);
        let n = fill(&mut self.0[start..]).min(max);
        self.0.truncate(start + n);
        self.0[at..at + 4].copy_from_slice(&(n as u32).to_le_bytes());
    }
    #[inline]
    pub fn none(&mut self) {
        self.u8(0);
    }
    #[inline]
    pub fn some(&mut self) {
        self.u8(1);
    }
    #[inline]
    pub fn count(&mut self, n: u8) {
        self.u8(n);
    }
    #[inline]
    pub fn undefined(&mut self) {
        self.u8(0);
    }
    #[inline]
    pub fn null(&mut self) {
        self.u8(1);
    }
    #[inline]
    pub fn bool(&mut self, v: bool) {
        self.u8(2);
        self.u8(v as u8);
    }
    #[inline]
    pub fn num(&mut self, v: f64) {
        self.u8(3);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn bigint(&mut self, v: &str) {
        self.u8(4);
        self.str(v);
    }
    /// a string whose whole length is `len` UTF-16 units; `fill` writes at most `max` bytes of it
    #[inline]
    pub fn string(&mut self, len: usize, max: usize, fill: impl FnOnce(&mut [u8]) -> usize) {
        self.u8(5);
        self.u32(len as u32);
        self.str_with(max, fill);
    }
    /// A one-byte (Latin-1) string whose whole length is `len` units: `fill` writes its first `n` bytes
    /// raw into the record (the writer converts; [`Dec::val`]).
    #[inline]
    pub fn latin1(&mut self, len: usize, n: usize, fill: impl FnOnce(&mut [u8])) {
        self.u8(9);
        self.u32(len as u32);
        self.u32(n as u32);
        let at = self.0.len();
        self.0.resize(at + n, 0);
        fill(&mut self.0[at..]);
    }
    /// [`Enc::latin1`] from bytes already in hand: the tag and both lengths as one block, then the
    /// bytes (no zero-fill first).
    #[inline]
    pub fn latin1_bytes(&mut self, len: usize, bytes: &[u8]) {
        let mut b = [0u8; 9];
        b[0] = 9;
        b[1..5].copy_from_slice(&(len as u32).to_le_bytes());
        b[5..9].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        self.block(b);
        self.0.extend_from_slice(bytes);
    }
    /// A two-byte string: its first `n` UTF-16 units, raw, little-endian.
    #[inline]
    pub fn utf16(&mut self, len: usize, units: &[u16]) {
        self.u8(10);
        self.u32(len as u32);
        self.u32(units.len() as u32);
        for u in units {
            self.0.extend_from_slice(&u.to_le_bytes());
        }
    }
    #[inline]
    pub fn symbol(&mut self) {
        self.u8(6);
    }
    #[inline]
    pub fn obj(&mut self, class: &'static str, id: i32) {
        // tag 7, the class as `sstr` (pointer, length), the id: one block
        let mut b = [0u8; 21];
        b[0] = 7;
        b[1..9].copy_from_slice(&(class.as_ptr() as u64).to_le_bytes());
        b[9..17].copy_from_slice(&(class.len() as u64).to_le_bytes());
        b[17..21].copy_from_slice(&(id as u32).to_le_bytes());
        self.block(b);
    }
    #[inline]
    pub fn func(&mut self, name: &str, id: i32) {
        self.u8(8);
        self.str(name);
        self.u32(id as u32);
    }
    /// the end of a call: whether it threw, its site, and its own frames (STACK=full; else none)
    #[inline]
    pub fn tail(&mut self, threw: bool, site: Option<u32>, frames: &[Frame]) {
        if frames.is_empty() {
            // threw, site, a frame count of 0: one block
            let mut b = [0u8; 7];
            b[0] = threw as u8;
            b[1..5].copy_from_slice(&site.unwrap_or(0).to_le_bytes());
            self.block(b);
            return;
        }
        self.u8(threw as u8);
        self.u32(site.unwrap_or(0));
        self.frames(frames);
    }
    #[inline]
    fn frames(&mut self, frames: &[Frame]) {
        self.u16(frames.len() as u16);
        for f in frames {
            self.str(&f.script);
            self.str(&f.func);
            self.u32(f.line);
            self.u32(f.col);
        }
    }
}

struct Local {
    thread: u64,
    buf: Vec<u8>,
    records: u32,
    /// one scratch buffer per nesting depth, reused
    scratch: Vec<Vec<u8>>,
    depth: usize,
    sites: ConstMap<(i32, u32, u32), u32>,
    /// the realms this thread has written a [`Realm`] record for
    realms: std::collections::HashSet<i32, ConstHasher>,
    /// the calls this thread is inside of, innermost last
    open: Vec<u64>,
    /// this thread's last entry number: a call's `seq` is (thread << 40) | it -- unique in the process
    /// and in entry order on its thread, with no shared counter (a locked add per call)
    next: u64,
}

impl Drop for Local {
    // A worker thread's last partial buffer leaves with the thread.
    fn drop(&mut self) {
        send(std::mem::take(&mut self.buf), std::mem::take(&mut self.records));
    }
}

/// A hasher with no per-map random state, so the maps of [`Local`] can be built in a `const`.
type ConstHasher = std::hash::BuildHasherDefault<std::hash::DefaultHasher>;
type ConstMap<K, V> = std::collections::HashMap<K, V, ConstHasher>;

thread_local! {
    /// CONSTANT-initialized: a lazily-initialized thread-local made every access -- two a
    /// recorded call -- a call to an accessor with an init check. The thread id and the first batch are
    /// taken on first use instead ([`Local::tid`], [`committed`]).
    static LOCAL: RefCell<Local> = const {
        RefCell::new(Local {
            thread: 0,
            buf: Vec::new(),
            records: 0,
            scratch: Vec::new(),
            depth: 0,
            sites: ConstMap::with_hasher(ConstHasher::new()),
            realms: std::collections::HashSet::with_hasher(ConstHasher::new()),
            open: Vec::new(),
            next: 0,
        })
    };
}

impl Local {
    /// This thread's id in the trace, taken on first use (0 is never one: the counter starts at 1).
    #[inline]
    fn tid(&mut self) -> u64 {
        if self.thread == 0 {
            self.thread = THREADS.fetch_add(1, Ordering::Relaxed);
        }
        self.thread
    }
}

fn send(buf: Vec<u8>, records: u32) {
    if buf.is_empty() {
        return;
    }
    let Some(tx) = TX.get() else { return };
    let mut batch = (buf, records);
    // A full channel: the thread WAITS for the writer's drain thread (it only copies, never formats --
    // plugins/korvus-analysis) to take a batch, up to 2 s; only then is the batch dropped and counted. A
    // trace with holes is the wrong evidence (a live challenge's whole main worker
    // fell in the 914 983 dropped records), and with the page clock real (KORVUS_ANALYSIS_REAL_CLOCK) a
    // wait is only a slower machine for the length of a burst.
    for _ in 0..WAIT_ROUNDS.load(Ordering::Relaxed) {
        match tx.try_send(batch) {
            Ok(()) => return,
            Err(TrySendError::Full(b)) => {
                batch = b;
                std::thread::sleep(std::time::Duration::from_micros(50));
            }
            Err(TrySendError::Disconnected((_, n))) => {
                DROPPED.fetch_add(n as u64, Ordering::Relaxed);
                return;
            }
        }
    }
    DROPPED.fetch_add(batch.1 as u64, Ordering::Relaxed);
}

#[inline]
fn commit(l: &mut Local, bytes: &[u8]) {
    l.buf.extend_from_slice(bytes);
    committed(l);
}

/// A record has just been appended to the thread's batch: count it, and hand the batch on when full.
#[inline]
fn committed(l: &mut Local) {
    // A recycled batch's lines were last READ by the writer thread, on another core, so the first write
    // to each waited for the line to come back (a quarter of `call_close`'s samples
    // sat on its first store into the batch). Asking for lines well ahead, for writing, overlaps that
    // wait with the calls in between; a prefetch never faults, so past the buffer's end it is harmless.
    #[cfg(target_arch = "x86_64")]
    // SAFETY: a prefetch is a hint: any address is allowed, nothing is read or written.
    unsafe {
        let ahead = l.buf.as_ptr().wrapping_add(l.buf.len() + PREFETCH_AHEAD) as *const i8;
        core::arch::x86_64::_mm_prefetch::<{ core::arch::x86_64::_MM_HINT_ET0 }>(ahead);
        core::arch::x86_64::_mm_prefetch::<{ core::arch::x86_64::_MM_HINT_ET0 }>(ahead.wrapping_add(64));
    }
    l.records += 1;
    if l.buf.len() >= BATCH_BYTES {
        let next = reuse().unwrap_or_else(|| Vec::with_capacity(BATCH_BYTES + 4096));
        let full = std::mem::replace(&mut l.buf, next);
        let n = std::mem::take(&mut l.records);
        send(full, n);
    }
}

/// Emptied batch buffers the writer hands back ([`recycle`]): a fresh 128 KB buffer per batch is a
/// fresh mmap, whose first touch faults page by page on the JS thread (the profile: the kernel's
/// page clearing was the largest single cost of a tight recorded loop). A recycled one is warm.
static SPARE: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

fn reuse() -> Option<Vec<u8>> {
    SPARE.lock().ok()?.pop()
}

/// The writer is done with `buf`: keep it for the next batch (up to the channel's depth).
pub fn recycle(mut buf: Vec<u8>) {
    buf.clear();
    if let Ok(mut s) = SPARE.lock() {
        if s.len() < QUEUE {
            s.push(buf);
        }
    }
}

/// This thread's id in the trace.
pub fn thread_id() -> u64 {
    LOCAL.with(|l| l.borrow_mut().tid())
}

/// A call starts: its header, and whatever `entry` encodes (receiver, arguments), go into this depth's
/// scratch buffer under ONE borrow of the thread's state. Returns its seq.
pub fn call_begin_with(kind: Kind, label: &'static str, realm: i32, entry: impl FnOnce(&mut Enc)) -> u64 {
    // raw TSC ticks: the writer converts them to ns ([`decode`]), not the JS thread
    call_begin_at(ticks(), kind, label, realm, entry)
}

/// [`call_begin_with`] stamped `t` (raw ticks, e.g. [`Spent::at`]).
pub fn call_begin_at(t: u64, kind: Kind, label: &'static str, realm: i32, entry: impl FnOnce(&mut Enc)) -> u64 {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        let l = &mut *l;
        l.next += 1;
        let seq = (l.tid() << 40) | l.next;
        let parent = l.open.last().copied().unwrap_or(0);
        l.open.push(seq);
        let d = l.depth;
        l.depth += 1;
        if l.scratch.len() <= d {
            l.scratch.push(Vec::with_capacity(512));
        }
        let thread = l.tid();
        let buf = &mut l.scratch[d];
        buf.clear();
        let mut e = Enc(buf);
        e.u8(TAG_CALL);
        e.u64(seq);
        e.u64(parent);
        e.u64(t);
        e.u64(thread);
        e.u32(realm as u32);
        e.u8(kind as u8);
        e.sstr(label);
        entry(&mut e);
        seq
    })
}

/// A call record's fixed header (tag, seq, parent, clock, thread, realm, kind, label as `sstr`), as one
/// block.
#[inline]
fn call_header(seq: u64, parent: u64, t: u64, thread: u64, realm: i32, kind: Kind, label: &'static str) -> [u8; 54] {
    let mut b = [0u8; 54];
    b[0] = TAG_CALL;
    b[1..9].copy_from_slice(&seq.to_le_bytes());
    b[9..17].copy_from_slice(&parent.to_le_bytes());
    b[17..25].copy_from_slice(&t.to_le_bytes());
    b[25..33].copy_from_slice(&thread.to_le_bytes());
    b[33..37].copy_from_slice(&(realm as u32).to_le_bytes());
    b[37] = kind as u8;
    b[38..46].copy_from_slice(&(label.as_ptr() as u64).to_le_bytes());
    b[46..54].copy_from_slice(&(label.len() as u64).to_le_bytes());
    b
}

/// A call opens with nothing encoded yet: its seq and its parent's, the parent being whatever call is
/// open on this thread. The whole record is written when it closes ([`call_close`]) -- straight into the
/// thread's batch, no scratch buffer and no copy (begin's encode + end's copy were ~30 ns of
/// every recorded call). For a caller whose entry values stay valid until the exit: a binding's `this`
/// and arguments are handles that live for the whole callback.
#[inline]
pub fn call_open() -> (u64, u64) {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        l.next += 1;
        let seq = (l.tid() << 40) | l.next;
        let parent = l.open.last().copied().unwrap_or(0);
        l.open.push(seq);
        (seq, parent)
    })
}

/// The call `seq` (from [`call_open`]) closes: its header, then `rest` (entry values, exit values,
/// tail) -- the same bytes [`call_begin_at`] + [`call_end_with`] write.
#[inline]
pub fn call_close(seq: u64, parent: u64, t: u64, kind: Kind, label: &'static str, realm: i32, rest: impl FnOnce(&mut Enc)) {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        let l = &mut *l;
        if let Some(i) = l.open.iter().rposition(|&s| s == seq) {
            l.open.truncate(i);
        }
        let thread = l.tid();
        let mut e = Enc(&mut l.buf);
        e.block(call_header(seq, parent, t, thread, realm, kind, label));
        rest(&mut e);
        committed(l);
    })
}

/// [`call_begin_with`] with nothing more to encode yet.
pub fn call_begin(kind: Kind, label: &'static str, realm: i32) -> u64 {
    call_begin_with(kind, label, realm, |_| {})
}

/// Write more of the innermost open call.
pub fn with_call<R>(f: impl FnOnce(&mut Enc) -> R) -> R {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        let d = l.depth - 1;
        f(&mut Enc(&mut l.scratch[d]))
    })
}

/// The call `seq` ends: `exit` encodes the rest (result, tail), and the call moves into the thread's
/// buffer -- under ONE borrow.
pub fn call_end_with(seq: u64, exit: impl FnOnce(&mut Enc)) {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        let l = &mut *l;
        if let Some(i) = l.open.iter().rposition(|&s| s == seq) {
            l.open.truncate(i);
        }
        l.depth -= 1;
        let d = l.depth;
        let mut bytes = std::mem::take(&mut l.scratch[d]);
        exit(&mut Enc(&mut bytes));
        commit(l, &bytes);
        l.scratch[d] = bytes;
    })
}

/// The call `seq` ended, its tail already written.
pub fn call_end(seq: u64) {
    call_end_with(seq, |_| {})
}

/// The id of the call site keyed `(script id, line, column)` on this thread, and whether it is new (the
/// caller then records its full stack with [`site`]).
pub fn site_id(key: (i32, u32, u32)) -> (u32, bool) {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        if let Some(&id) = l.sites.get(&key) {
            return (id, false);
        }
        let id = SITES.fetch_add(1, Ordering::Relaxed);
        l.sites.insert(key, id);
        (id, true)
    })
}

/// Record a site's full stack (once per site).
pub fn site(id: u32, stack: Vec<Frame>) {
    let mut bytes = Vec::with_capacity(256);
    let mut e = Enc(&mut bytes);
    e.u8(TAG_SITE);
    e.u32(id);
    e.frames(&stack);
    LOCAL.with(|l| commit(&mut l.borrow_mut(), &bytes));
}

/// Whether this thread has yet to describe `realm` (true once: the caller then records [`realm`]).
pub fn realm_is_new(realm: i32) -> bool {
    LOCAL.with(|l| l.borrow_mut().realms.insert(realm))
}

/// Record what the realm id `id` names (once per realm and thread, before its first call).
pub fn realm(id: i32, global: &str, url: &str) {
    let mut bytes = Vec::with_capacity(64 + url.len());
    let mut e = Enc(&mut bytes);
    e.u8(TAG_REALM);
    e.u32(id as u32);
    e.str(global);
    e.str(url);
    LOCAL.with(|l| commit(&mut l.borrow_mut(), &bytes));
}

/// Record a call that carries no values (an op2 member: its label is all `dispatch!` has), nested in
/// whatever call is open on this thread.
pub fn record_bare(kind: Kind, label: &'static str) {
    if !enabled() {
        return;
    }
    let seq = call_begin(kind, label, 0);
    with_call(|e| {
        e.none();
        e.count(0);
        e.none();
        e.tail(false, None, &[]);
    });
    call_end(seq);
}

/// Hand this thread's partial buffer to the writer now (the engine's end, a test's end).
pub fn flush() {
    let (b, n) = LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        (std::mem::take(&mut l.buf), std::mem::take(&mut l.records))
    });
    send(b, n);
}

// ---- decoding (the writer's side) ---------------------------------------------------------------------

struct Dec<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i + n)?;
        self.i += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn sstr(&mut self) -> Option<&'static str> {
        let (p, n) = (self.u64()? as usize, self.u64()? as usize);
        // SAFETY: an sstr is only ever encoded from a `&'static str` (a literal, `intern`, `intern_str`)
        // in this process, which lives for the process.
        Some(unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(p as *const u8, n)) })
    }
    fn val(&mut self) -> Option<Val> {
        Some(match self.u8()? {
            0 => Val::Undefined,
            1 => Val::Null,
            2 => Val::Bool { v: self.u8()? != 0 },
            3 => Val::Num { v: f64::from_le_bytes(self.take(8)?.try_into().ok()?) },
            4 => Val::BigInt { v: self.str()? },
            5 => {
                let len = self.u32()? as usize;
                Val::Str { v: self.str()?, len }
            }
            6 => Val::Symbol,
            7 => Val::Obj { class: self.sstr()?, id: self.u32()? as i32 },
            8 => Val::Fn { name: self.str()?, id: self.u32()? as i32 },
            9 => {
                let len = self.u32()? as usize;
                let n = self.u32()? as usize;
                Val::Str { v: self.take(n)?.iter().map(|&b| char::from(b)).collect(), len }
            }
            10 => {
                let len = self.u32()? as usize;
                let n = self.u32()? as usize;
                let raw = self.take(n * 2)?;
                let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                Val::Str { v: String::from_utf16_lossy(&units), len }
            }
            _ => return None,
        })
    }
    fn opt_val(&mut self) -> Option<Option<Val>> {
        Some(if self.u8()? == 0 { None } else { Some(self.val()?) })
    }
    fn frames(&mut self) -> Option<Vec<Frame>> {
        let n = self.u16()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(Frame { script: self.str()?, func: self.str()?, line: self.u32()?, col: self.u32()? });
        }
        Some(out)
    }
}

fn kind_of(k: u8) -> Kind {
    match k {
        1 => Kind::Stub,
        2 => Kind::Op,
        3 => Kind::Site,
        4 => Kind::Callback,
        5 => Kind::Realm,
        6 => Kind::Builtin,
        7 => Kind::Get,
        _ => Kind::Call,
    }
}

/// Decode one buffer of records (as [`open`]'s receiver delivers them). A malformed tail is dropped.
pub fn decode(bytes: &[u8]) -> Vec<Rec> {
    let mut d = Dec { b: bytes, i: 0 };
    let mut out = Vec::new();
    let proc = proc_id();
    while d.i < bytes.len() {
        let rec = (|| -> Option<Rec> {
            match d.u8()? {
                TAG_CALL => {
                    let seq = d.u64()?;
                    let parent = d.u64()?;
                    let t = ticks_to_ns(d.u64()?);
                    let thread = d.u64()?;
                    let realm = d.u32()? as i32;
                    let kind = kind_of(d.u8()?);
                    let label = d.sstr()?;
                    let this = d.opt_val()?;
                    let n = d.u8()? as usize;
                    let mut args = Vec::with_capacity(n);
                    for _ in 0..n {
                        args.push(d.val()?);
                    }
                    let result = d.opt_val()?;
                    let threw = d.u8()? != 0;
                    let site = d.u32()?;
                    let stack = d.frames()?;
                    Some(Rec::Call(Call {
                        proc,
                        seq,
                        parent: (parent != 0).then_some(parent),
                        t,
                        thread,
                        realm,
                        kind,
                        label,
                        this,
                        args,
                        result,
                        threw,
                        script: None,
                        site: (site != 0).then_some(site),
                        stack,
                    }))
                }
                TAG_SITE => {
                    let site = d.u32()?;
                    Some(Rec::Site(Site { kind: Kind::Site, proc, site, stack: d.frames()? }))
                }
                TAG_REALM => {
                    let realm = d.u32()? as i32;
                    Some(Rec::Realm(Realm { kind: Kind::Realm, proc, realm, global: d.str()?, url: d.str()? }))
                }
                _ => None,
            }
        })();
        match rec {
            Some(r) => out.push(r),
            None => break,
        }
    }
    out
}

/// `s` as a process-lifetime `&'static str`, made once per distinct string (class names, script URLs).
pub fn intern_str(s: &str) -> &'static str {
    static STRS: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    let mut m = STRS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&x) = m.get(s) {
        return x;
    }
    let x: &'static str = Box::leak(s.to_string().into_boxed_str());
    m.insert(x);
    x
}

/// How many calls of each label on a thread get a call site (`KORVUS_ANALYSIS_SITES`, default 4).
#[inline]
pub fn sites_per_label() -> u32 {
    SITES_PER_LABEL.load(Ordering::Relaxed)
}

/// A label for `iface` + `member`, made once per distinct pair and kept for the process: a call carries
/// `&'static str`, so recording one never allocates a label.
pub fn intern(iface: &str, member: &str) -> &'static str {
    static LABELS: OnceLock<Mutex<std::collections::HashMap<String, &'static str>>> = OnceLock::new();
    let key = format!("{iface}.{member}");
    let mut m = LABELS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = m.get(&key) {
        return s;
    }
    let s: &'static str = Box::leak(key.clone().into_boxed_str());
    m.insert(key, s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(rx: &Receiver<(Vec<u8>, u32)>) -> Vec<Call> {
        let (b, _) = rx.recv().expect("a buffer");
        decode(&b).into_iter().filter_map(|r| match r { Rec::Call(c) => Some(c), _ => None }).collect()
    }

    // One test owns the process-wide queue (it can be opened once): encoding round-trips, nesting, the
    // flush, and the WAIT -- a full channel is waited on while a drain makes room; only a batch nothing
    // takes is dropped and counted.
    #[test]
    fn the_queue_round_trips_nests_calls_and_waits_before_it_drops() {
        let rx = open(0, false).expect("first open");
        assert!(open(0, false).is_none(), "one recorder per process");
        assert!(enabled());

        let outer = call_begin(Kind::Call, "Outer.call", 7);
        with_call(|e| {
            e.some();
            e.obj("HTMLDocument", 42);
            e.count(2);
            e.string(3, 256, |b| { b[..3].copy_from_slice(b"abc"); 3 });
            e.num(1.5);
        });
        record_bare(Kind::Op, "Inner.op");
        with_call(|e| {
            e.some();
            e.bool(true);
            e.tail(false, Some(9), &[]);
        });
        call_end(outer);
        site(9, vec![Frame { script: "s.js".into(), func: "f".into(), line: 3, col: 4 }]);
        flush();
        let (b, n) = rx.recv().expect("a buffer");
        assert_eq!(n, 3);
        let recs = decode(&b);
        let Rec::Call(op) = &recs[0] else { panic!("op first: it ended first") };
        assert_eq!(op.label, "Inner.op");
        assert_eq!(op.parent, Some(outer), "an op records the call it ran inside");
        let Rec::Call(c) = &recs[1] else { panic!() };
        assert_eq!((c.seq, c.realm, c.label, c.site), (outer, 7, "Outer.call", Some(9)));
        let j = serde_json::to_string(c).unwrap();
        assert!(j.contains(r#""this":{"t":"obj","class":"HTMLDocument","id":42}"#), "{j}");
        assert!(j.contains(r#""args":[{"t":"str","v":"abc","len":3},{"t":"num","v":1.5}]"#), "{j}");
        assert!(j.contains(r#""result":{"t":"bool","v":true}"#), "{j}");
        let Rec::Site(st) = &recs[2] else { panic!() };
        assert_eq!((st.site, st.stack[0].line), (9, 3));

        // Fill the channel without draining it: the caller waits for room (here: 10 rounds, not 2 s), and
        // a batch nothing ever takes is counted as dropped.
        WAIT_ROUNDS.store(10, Ordering::Relaxed);
        let before = dropped();
        for _ in 0..(QUEUE + 2) {
            record_bare(Kind::Op, "X.y");
            flush();
        }
        assert_eq!(dropped() - before, 2, "two buffers past capacity, never drained, are dropped after the wait");
        // ... and a batch the drain takes while the caller waits is not: room appears, the send goes through.
        // (the drain hands the receiver back: dropped there, it would disconnect the channel under the wait)
        let drain = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let got = rx.recv().is_ok();
            (rx, got)
        });
        WAIT_ROUNDS.store(40_000, Ordering::Relaxed);
        let before = dropped();
        record_bare(Kind::Op, "X.y");
        flush();
        assert_eq!(dropped() - before, 0, "a full channel the drain empties is waited on, not dropped");
        let (_rx, got) = drain.join().expect("drain");
        assert!(got);
        let _ = calls;
    }

    #[test]
    fn a_label_is_interned_once() {
        let a = intern("Document", "get compatMode");
        let b = intern("Document", "get compatMode");
        assert!(std::ptr::eq(a, b));
        assert_eq!(a, "Document.get compatMode");
    }

    #[test]
    fn a_site_is_new_once_per_key_and_thread() {
        let (a, new_a) = site_id((7, 3, 9));
        let (b, new_b) = site_id((7, 3, 9));
        let (c, new_c) = site_id((7, 4, 9));
        assert!(new_a && !new_b && new_c);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
