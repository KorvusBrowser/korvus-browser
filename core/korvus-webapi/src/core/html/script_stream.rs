//! A fetched classic script, compiled off the page's thread while the page runs other tasks -- Blink's
//! `ScriptStreamer`, which hands every fetched script of any size to V8's background compile
//! (`ScriptCompiler::StartStreaming`) and finishes it on the main thread when the script runs. korvus compiled each one
//! on the page's thread when it ran: 6.4% of lowes.com's main thread was top-level compiles (frame-pointer profile,
//! `korvus fetch`, 8 s), on top of the lazy compiles Chrome pays as well.
//!
//! The body is handed to V8 when its fetch settles ([`start`], from the script's settle), parsed and compiled on one of
//! this module's threads, and taken back by the classic-script compile ([`compile`]) -- by its text, so a script that
//! was not streamed, or whose text is not the one streamed, compiles as before. A stream nobody takes is kept with the
//! realm's state and freed with it (after its compile ends), before the isolate goes: `OpState` is dropped first.
//! Linux only: the shim calls the prebuilt V8's StreamedSource constructor by its symbol (`shim/stream.cc`), which
//! has been checked on that platform's build alone.

use deno_core::v8;

/// What the shim hands back: an opaque handle to the streamed source and its compile task.
#[repr(C)]
struct KorvusStream {
    _private: [u8; 0],
}

#[cfg(target_os = "linux")]
extern "C" {
    fn korvus_stream_start(isolate: *mut std::ffi::c_void, utf8: *const u8, len: usize) -> *mut KorvusStream;
    fn korvus_stream_run(s: *mut KorvusStream);
    fn korvus_stream_finish(context: *const v8::Context, s: *mut KorvusStream, full: *const v8::String, origin: *const v8::ScriptOrigin) -> *const v8::Script;
    fn korvus_stream_free(s: *mut KorvusStream);
}

/// The smallest body worth a stream: below it the hand-off costs about what the compile does.
const MIN_LEN: usize = 8 * 1024;

/// Whether `text` can be streamed. Not a text that starts with a byte order mark: V8's streaming UTF-8 decoder drops
/// a leading BOM (`Utf8ExternalStreamingStream`, `scanner-character-streams.cc`), so every function the stream
/// compiled sat one character off in the source the script is finished with, which keeps it. A lazily compiled
/// function was then parsed from the wrong offset -- a SyntaxError for a script that runs in Chrome, or V8's
/// `Check failed: start_position == start_position_from_data`, which killed the process (nzherald.co.nz).
fn streamable(text: &str) -> bool {
    text.len() >= MIN_LEN && !text.starts_with('\u{feff}')
}

/// The streams a realm keeps at most; the oldest untaken one goes first.
const KEEP: usize = 64;

/// The compile threads (Blink posts its streaming tasks to a worker pool).
const THREADS: usize = 2;

struct Done(std::sync::Mutex<bool>, std::sync::Condvar);

/// A stream started for a body: its handle, the body's length and hash, and its compile's end.
struct Pending {
    stream: *mut KorvusStream,
    len: usize,
    hash: u64,
    done: std::sync::Arc<Done>,
}

impl Pending {
    fn wait(&self) {
        let mut d = self.done.0.lock().unwrap_or_else(|e| e.into_inner());
        while !*d {
            d = self.done.1.wait(d).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        // its compile may still be running on a pool thread: freed after it ends
        self.wait();
        #[cfg(target_os = "linux")]
        // SAFETY: made by `korvus_stream_start`, its task has run, and nothing else holds it
        unsafe {
            korvus_stream_free(self.stream)
        };
    }
}

/// The realm's untaken streams, oldest first.
#[derive(Default)]
pub(crate) struct PendingStreams(Vec<Pending>);

struct Job(*mut KorvusStream, std::sync::Arc<Done>);
// SAFETY: the stream is used by the one pool thread that runs it, and by its owner only after `done` says it ended
unsafe impl Send for Job {}

fn pool() -> &'static std::sync::Mutex<std::sync::mpsc::Sender<Job>> {
    static POOL: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<Job>>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        for i in 0..THREADS {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name(format!("korvus-script-stream-{i}")).spawn(move || loop {
                let job = match rx.lock().unwrap_or_else(|e| e.into_inner()).recv() {
                    Ok(j) => j,
                    Err(_) => return,
                };
                #[cfg(target_os = "linux")]
                // SAFETY: the stream's owner frees it only after `done`, which is set below
                unsafe {
                    korvus_stream_run(job.0)
                };
                let mut d = job.1 .0.lock().unwrap_or_else(|e| e.into_inner());
                *d = true;
                job.1 .1.notify_all();
            });
        }
        std::sync::Mutex::new(tx)
    })
}

fn hash_of(text: &str) -> u64 {
    use std::hash::BuildHasher;
    korvus_dom::FxHashMap::<u8, u8>::default().hasher().hash_one(text.as_bytes())
}

/// A fetched classic script's body: compiled from now on, off this thread, for the script's run to take ([`compile`]).
pub(crate) fn start(scope: &mut v8::PinScope, body: &str) {
    if !cfg!(target_os = "linux") || !streamable(body) {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        // one stream per text: the parser hands a body over when it arrives and again with the batch it staged
        let hash = hash_of(body);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        if op_state.borrow().try_borrow::<PendingStreams>().is_some_and(|l| l.0.iter().any(|p| p.len == body.len() && p.hash == hash)) {
            return;
        }
        let isolate: &mut v8::Isolate = scope;
        // SAFETY: `UnsafeRawIsolatePtr` is `repr(transparent)` over the C++ `v8::Isolate*` (as `incumbent_realm`)
        let stream = unsafe {
            let raw: *mut std::ffi::c_void = std::mem::transmute(isolate.as_raw_isolate_ptr());
            korvus_stream_start(raw, body.as_ptr(), body.len())
        };
        if stream.is_null() {
            return;
        }
        let done = std::sync::Arc::new(Done(std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let sent = pool().lock().unwrap_or_else(|e| e.into_inner()).send(Job(stream, done.clone()));
        if sent.is_err() {
            // no thread to run it: not started, so nothing waits on it
            *done.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        }
        let pending = Pending { stream, len: body.len(), hash, done };
        let mut st = op_state.borrow_mut();
        if !st.has::<PendingStreams>() {
            st.put(PendingStreams::default());
        }
        let list = &mut st.borrow_mut::<PendingStreams>().0;
        if list.len() >= KEEP {
            list.remove(0);
        }
        list.push(pending);
    }
}

/// The classic script `code`, compiled from its stream when one was started for this text ([`start`]): waits for its
/// background compile and finishes it in this scope's context with `source` (the same text) and `origin`. `None`
/// when no stream has this text -- the caller compiles as before; `Some(None)` when it did not compile (the exception
/// pending in the caller's `TryCatch`, as a direct compile leaves it).
pub fn compile<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    code: &str,
    source: v8::Local<'s, v8::String>,
    origin: &v8::ScriptOrigin<'s>,
) -> Option<Option<v8::Local<'s, v8::Script>>> {
    if !cfg!(target_os = "linux") || !streamable(code) {
        return None;
    }
    let pending = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let list = &mut st.try_borrow_mut::<PendingStreams>()?.0;
        let hash = hash_of(code);
        let i = list.iter().position(|p| p.len == code.len() && p.hash == hash)?;
        list.remove(i)
    };
    pending.wait();
    #[cfg(target_os = "linux")]
    {
        let context = scope.get_current_context();
        // SAFETY: the stream's task has run; the context, source and origin are this scope's live handles, and the
        // answer is a `Local` in it
        let script = unsafe {
            let p = korvus_stream_finish(&*context, pending.stream, &*source, origin as *const v8::ScriptOrigin<'s> as *const v8::ScriptOrigin);
            if p.is_null() {
                None
            } else {
                Some(std::mem::transmute::<*const v8::Script, v8::Local<'s, v8::Script>>(p))
            }
        };
        drop(pending);
        Some(script)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, origin);
        drop(pending);
        None
    }
}
