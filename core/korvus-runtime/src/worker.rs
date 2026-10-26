//! The real multi-isolate Web Worker platform. This module owns the cross-isolate structured
//! clone (V8 `ValueSerializer`/`ValueDeserializer`), the isolate-per-worker spawn, the channels, the
//! main-side `WorkerRegistry`, and the native `Worker.postMessage`/`terminate` methods (which kill the
//! last `surface_parity` LIES). rusty_v8 may be used ONLY in this crate (Cargo.toml invariant), so all V8
//! worker plumbing lives here.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{extension, op2, v8, JsRuntime, OpState, PollEventLoopOptions, RuntimeOptions};
use tokio::sync::{mpsc as tmpsc, Mutex as TMutex};
// Shared korvus ops/objects the worker scope reuses (crate-root items; `worker` is a descendant module so
// it can name them even though they are private). worker-host.js drives these via `Deno.core.ops`.
use crate::{
    op_atob, op_btoa, op_fetch, op_fetch_body, op_fetch_body_cancel, op_fetch_body_chunk, op_fetch_head, op_fetch_sync, op_kprint, op_random_bytes,
    op_uuid, HostLocation, PrintBuffer,
};

/// Serializer delegate: the only required method throws a `DataCloneError` for un-cloneable values
/// (host objects, functions). We expose no custom host objects, so the defaults cover everything else.
/// Serialize a JS value to a structured-clone byte buffer. The bytes are isolate-independent — this is
/// how a message crosses from one worker isolate to another (the sender serializes, the receiver
/// [`deserialize`]s). Un-cloneable values (functions, DOM host objects) throw `DataCloneError`.
pub fn serialize<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<v8::Value>) -> Vec<u8> {
    // The shared codec: serializable platform objects as host objects, no ports.
    webapi::wire_serialize(scope, value, &[])
}

/// Deserialize a structured-clone byte buffer into a JS value in the CURRENT isolate. Returns
/// `undefined` if the buffer is malformed.
pub fn deserialize<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: &[u8]) -> v8::Local<'s, v8::Value> {
    webapi::wire_deserialize(scope, bytes, &[])
}

/// Channel ids a WORKER allocates for the ports it transfers to its page: process-unique and
/// above the page's own `WorkerRegistry::port_next` range, so the two never collide.
static WORKER_PORT_IDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x4000_0000);

/// The ports in a `transfer` list, in order.
fn transfer_ports<'s>(scope: &mut v8::PinScope<'s, '_>, transfer: v8::Local<'s, v8::Value>) -> Vec<v8::Local<'s, v8::Object>> {
    let mut out = Vec::new();
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(transfer) {
        for i in 0..arr.length() {
            if let Some(item) = arr.get_index(scope, i) {
                if webapi::is_message_port(scope, item) {
                    if let Ok(o) = v8::Local::<v8::Object>::try_from(item) {
                        out.push(o);
                    }
                }
            }
        }
    }
    out
}

/// A worker's `postMessage` frame: the V8 stream alone when nothing is transferred -- it starts
/// with the 0xFF version tag, which is how a bare worker's receiver has always read it -- else
/// `[u32 n][u32 channel id * n][stream]`, the ports' in-message places written as their indices. Each port's
/// kept end is registered as remote under its new id.
pub(crate) fn worker_out_frame<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<'s, v8::Value>, transfer: v8::Local<'s, v8::Value>) -> Vec<u8> {
    let ports = transfer_ports(scope, transfer);
    let bytes = webapi::wire_serialize(scope, value, &ports);
    if ports.is_empty() {
        return bytes;
    }
    let mut ids = Vec::with_capacity(ports.len());
    for p in &ports {
        let id = WORKER_PORT_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(peer) = webapi::mp_take_peer(scope, (*p).into()) {
            webapi::mp_register_remote(scope, peer, id);
        }
        ids.push(id);
    }
    msg_encode(&bytes, &ids)
}

/// The page's side of a worker frame: the data, and the ports the worker transferred (fresh remote ports,
/// routed back to worker `id`'s port inbox) -- both built in `target`'s realm, the `Worker` object's.
///
/// This op is the MAIN realm's function, and V8 enters a function's creation context to call it: built where
/// it runs, a message to a worker made in a FRAME arrived made of the PAGE's `Object.prototype`, so the frame
/// saw `e.data.constructor !== Object` (a bot-challenge frame's worker answers;
/// stock Chrome builds them in the worker's owner realm).
#[op2]
pub fn op_worker_msg_in<'s>(scope: &mut v8::PinScope<'s, '_>, id: f64, #[buffer] frame: &[u8], target: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Value> {
    let (ids, body): (Vec<u32>, &[u8]) = if frame.first() == Some(&0xFF) || frame.len() < 4 {
        (Vec::new(), frame)
    } else {
        let n = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        let mut ids = Vec::with_capacity(n);
        let mut off = 4;
        for _ in 0..n {
            if off + 4 > frame.len() {
                break;
            }
            ids.push(u32::from_le_bytes([frame[off], frame[off + 1], frame[off + 2], frame[off + 3]]));
            off += 4;
        }
        (ids, &frame[off.min(frame.len())..])
    };
    if !ids.is_empty() {
        let op_state = JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let inbound = st.try_borrow::<WorkerRegistry>().and_then(|r| r.map.get(&(id as u32)).map(|h| h.port_inbound.clone()));
        if let (Some(pi), Some(routes)) = (inbound, st.try_borrow::<WorkerPortRoutes>()) {
            if let Ok(mut map) = routes.0.lock() {
                for cid in &ids {
                    map.insert(*cid, pi.clone());
                }
            }
        }
    }
    let realm = v8::Local::<v8::Object>::try_from(target).ok().and_then(|o| o.get_creation_context(scope));
    let realm = realm.unwrap_or_else(|| scope.get_current_context());
    let built = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let ports: Vec<v8::Local<v8::Object>> = ids.iter().map(|cid| webapi::mp_remote_port(scope, *cid)).collect();
        let data = webapi::wire_deserialize(scope, body, &ports);
        let port_vals: Vec<v8::Local<v8::Value>> = ports.iter().map(|p| (*p).into()).collect();
        let arr = v8::Array::new_with_elements(scope, &port_vals);
        let pair: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &[data, arr.into()]).into();
        v8::Global::new(scope, pair)
    };
    v8::Local::new(scope, &built)
}

// ---- Worker isolate: thread-per-worker + bidirectional structured-clone messaging ----
// A V8 isolate is thread-affine, so the worker `JsRuntime` is created, driven, and dropped entirely on its
// own OS thread; only serialized bytes cross the channels. This is the same model Deno's WebWorker uses.
// Both directions use a tokio unbounded channel: the receiving side awaits it in an async op (which parks
// that isolate's event loop like a pending fetch), the sending side pushes bytes (lock-free, any thread).

/// The worker isolate's outbound channel end (worker -> main), in its `OpState`.
pub(crate) struct WorkerSender(pub(crate) tmpsc::UnboundedSender<Vec<u8>>);
/// Wakes a page event loop parked on a live worker: every worker -> page send calls
/// [`wake_page`]. The page's receive promises are unref'd, so its loop reports idle while a worker runs
/// and used to sleep a fixed 5 ms quantum before polling again -- every worker message waited up to 5 ms:
/// 176 page<->worker round trips/s where Chrome 150 does ~168,000. One process-wide `Notify`: a spurious
/// wake of another engine's loop only makes it poll once.
pub(crate) static PAGE_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// A frame is on its way from a worker to a page.
pub(crate) fn wake_page() {
    PAGE_WAKE.notify_one();
}

/// The worker isolate's inbound channel end (main -> worker), awaited by `op_worker_self_recv`. Behind an
/// `Arc<tokio::Mutex>` because an async op can't hold the `OpState` borrow across `.await`.
struct WorkerReceiver(Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>);

/// Transferables: detach each transferable `ArrayBuffer` in the `transfer` list. Called AFTER
/// `serialize`, so the buffer's bytes are already copied into the message; detaching the source then gives
/// the observable transfer semantics (source `byteLength -> 0`, receiver gets the data). Cross-isolate
/// transfer is copy+detach (V8 backing stores are per-isolate); zero-copy is a later optimization.
pub fn apply_transfer(scope: &mut v8::PinScope, transfer: v8::Local<v8::Value>) {
    let arr = match v8::Local::<v8::Array>::try_from(transfer) {
        Ok(a) => a,
        Err(_) => return, // undefined / not a list -> nothing to transfer
    };
    for i in 0..arr.length() {
        if let Some(item) = arr.get_index(scope, i) {
            if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(item) {
                if ab.is_detachable() {
                    let _ = ab.detach(None);
                }
            }
        }
    }
}

/// `self.postMessage(value, transfer)` inside the worker: structured-clone `value` to bytes, ship them to
/// the main thread, then detach any transferred ArrayBuffers.
#[op2(fast)]
fn op_worker_self_post<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    transfer: v8::Local<'s, v8::Value>,
) {
    // A platform object no structured clone may copy is a DataCloneError, which V8's
    // serializer -- no host-object hook here -- would write as a plain object.
    if !webapi::refuse_platform_objects(scope, value, "DedicatedWorkerGlobalScope", "postMessage") {
        return;
    }
    // Raw V8 bytes when nothing is transferred (a BARE worker's receiver reads them as before), a port frame
    // otherwise (worker -> page port transfer).
    let bytes = worker_out_frame(scope, value, transfer);
    {
        let op_state = JsRuntime::op_state_from(scope);
        let state = op_state.borrow();
        if let Some(tx) = state.try_borrow::<WorkerSender>() {
            let _ = tx.0.send(bytes);
            wake_page();
        }
    }
    apply_transfer(scope, transfer);
}

/// Await the next inbound message (main -> worker) and return its structured-clone bytes. `op_msg_deserialize`
/// then turns them into a value; the worker's receive loop fires `onmessage`. Async so it parks the worker's
/// event loop (like a pending fetch) until a message arrives. Returns empty when the channel closes
/// (main dropped the sender = terminate) so the worker's receive loop breaks and the isolate exits.
#[op2(async(lazy), fast)]
#[buffer]
async fn op_worker_self_recv(state: Rc<RefCell<OpState>>) -> Vec<u8> {
    let rx = state.borrow().borrow::<WorkerReceiver>().0.clone();
    let mut guard = rx.lock().await;
    guard.recv().await.unwrap_or_default()
}

/// Deserialize structured-clone bytes into a value in the CURRENT isolate (used by the receive path on both
/// the worker and the main side). Native so it stays `[native code]`.
#[op2]
pub fn op_msg_deserialize<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[buffer] bytes: &[u8],
) -> v8::Local<'s, v8::Value> {
    deserialize(scope, bytes)
}

/// The worker's inbound BroadcastChannel channel (main/peer -> worker), awaited by `op_bc_self_recv`.
struct WorkerBcReceiver(Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>);

/// Await the next inbound cross-isolate BroadcastChannel frame for this worker (empty when the channel
/// closes). worker-host.js decodes it (name + structured-clone bytes) and calls `op_bc_deliver_external` to
/// deliver to the worker's local BroadcastChannels.
#[op2(async(lazy), fast)]
#[buffer]
async fn op_bc_self_recv(state: Rc<RefCell<OpState>>) -> Vec<u8> {
    let rx = state.borrow().borrow::<WorkerBcReceiver>().0.clone();
    let mut guard = rx.lock().await;
    guard.recv().await.unwrap_or_default()
}

/// The worker's inbound MessagePort channel (main -> worker), awaited by `op_port_self_recv`.
struct WorkerPortReceiver(Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>);

/// Await the next inbound cross-isolate MessagePort frame for this worker (empty when the channel closes).
/// worker-host.js decodes it (`[u32 channel_id][bytes]`) and calls `op_port_deliver_external` to deliver to
/// the transferred port the worker holds for that id.
#[op2(async(lazy), fast)]
#[buffer]
async fn op_port_self_recv(state: Rc<RefCell<OpState>>) -> Vec<u8> {
    let rx = state.borrow().borrow::<WorkerPortReceiver>().0.clone();
    let mut guard = rx.lock().await;
    guard.recv().await.unwrap_or_default()
}

extension!(
    korvus_worker_ext,
    ops = [op_worker_self_post, op_worker_self_recv, op_msg_deserialize],
    options = {
        sender: tmpsc::UnboundedSender<Vec<u8>>,
        receiver: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    },
    state = |state: &mut OpState, options| {
        state.put(WorkerSender(options.sender));
        state.put(WorkerReceiver(options.receiver));
    },
);

// The real WorkerGlobalScope. Carries the shared korvus ops (rng/uuid/base64/print) + the
// TextEncoder/Decoder cppgc objects + the `worker-host.js` esm entry that installs self/postMessage/
// onmessage/crypto onto the worker global. Loaded ALONGSIDE `webapi_core_ext` (SubtleCrypto/... cppgc) and
// `korvus_worker_ext` (messaging) in a real worker runtime; NOT in the bare-primitive runtime (unit tests).
/// The worker's outbound error channel (worker -> main). Uncaught worker errors flow here and become
/// `Worker.onerror` on the page.
pub(crate) struct WorkerErrSender(pub(crate) tmpsc::UnboundedSender<Vec<u8>>);

/// Report an uncaught worker error to the creator's `Worker` object: `report.rs` calls this when nothing in
/// this realm handled it, with the message and site this realm's own `ErrorEvent` carried. A DEDICATED
/// worker's only: a shared or service worker's runtime errors stay in that worker (HTML; Chrome 150,
/// measured: a shared worker's timer throw fires nothing at the page's `SharedWorker`).
#[op2(fast)]
fn op_worker_self_error(state: &mut OpState, #[string] message: &str, #[string] filename: &str, lineno: u32, colno: u32) {
    if state.try_borrow::<WorkerScope>().is_some_and(|s| s.kind != "dedicated") {
        return;
    }
    send_error_frame(state, &webapi::WorkerErrorFrame { message: Some(message.to_string()), filename: filename.to_string(), lineno, colno });
}

/// One [`webapi::WorkerErrorFrame`] to the creator's `Worker` object.
fn send_error_frame(state: &OpState, frame: &webapi::WorkerErrorFrame) {
    if let Some(tx) = state.try_borrow::<WorkerErrSender>() {
        let _ = tx.0.send(frame.encode());
        wake_page();
    }
}

// ---- W1.6: worker URL loading (`new Worker(url)`, `importScripts`) ----
//
// A worker isolate's own thread drives a CURRENT-THREAD tokio runtime (`spawn_on_thread`): its top-level
// script (below) fetches its OWN source asynchronously via `globalThis.fetch` (already wired through
// `op_fetch`, added to this extension's ops below) before running it, which is what keeps the `Worker`
// constructor on the MAIN thread synchronous -- it never blocks on the network itself, exactly as the
// brief's host.js comment anticipated.
//
// `importScripts`, though, is specified as SYNCHRONOUS: the calling script's later statements must see
// what an earlier `importScripts` call defined. A worker's own tokio runtime can't `block_on` a nested
// future (that panics: "Cannot start a runtime from within a runtime"), so a synchronous fetch instead
// hands the request to a SEPARATE background runtime and blocks the calling OS thread on a plain
// `std::sync::mpsc` recv -- an OS-level block, not a Tokio API, so it is safe to call from inside another
// runtime's worker thread.

/// The background runtime `blocking_fetch` submits work to. Built once, shared by every worker isolate.
fn fetch_pool() -> &'static tokio::runtime::Runtime {
    static POOL: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build worker importScripts fetch pool")
    })
}

/// `blocking_fetch`'s own give-up bound. `plugins/korvus-transport`'s shared client sets no
/// `.timeout(...)` (deliberately out of this unit's reach -- see the doc comment below), so without a
/// bound here a server that accepts the connection and never responds parks the calling worker's OS
/// thread in `rx.recv()` FOREVER: unlike a plain async fetch (droppable when the isolate tears down), a
/// thread blocked in a std::sync recv cannot be cancelled by anything.
///
/// The number: `WORKER_HARD_DEADLINE_MS`'s 15000ms (core/korvus-runtime/tests/wpt.rs), itself matched
/// to a reference engine's own per-test timeout -- a reference implementation:79-81
/// (`test_timeout_ms`, default 15000). That is the longest any caller in this codebase currently treats
/// a single WPT file's total worker round-trip as still "working", so it is also the longest a single
/// fetch inside one should be allowed to sit silent before this engine gives up on it -- not a round
/// number picked in isolation.
pub(crate) const BLOCKING_FETCH_TIMEOUT_MS: u64 = 15000;

/// Run `transport::fetch` to completion, blocking the CALLING OS thread (not the runtime it may already
/// be inside) until the response arrives OR `BLOCKING_FETCH_TIMEOUT_MS` elapses. Backs `importScripts`.
fn blocking_fetch(req: transport::FetchRequest) -> anyhow::Result<transport::FetchResponse> {
    let (tx, rx) = std::sync::mpsc::channel();
    fetch_pool().spawn(async move {
        let _ = tx.send(transport::fetch(req).await);
    });
    rx.recv_timeout(std::time::Duration::from_millis(BLOCKING_FETCH_TIMEOUT_MS))
        .map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => {
                anyhow::anyhow!("timed out after {BLOCKING_FETCH_TIMEOUT_MS}ms")
            }
            std::sync::mpsc::RecvTimeoutError::Disconnected => anyhow::anyhow!("worker fetch task dropped"),
        })?
}

/// Parse an `importScripts` argument relative to the worker's own URL (HTML "import scripts into worker global
/// scope" step 4; Blink's `ImportScriptsInternal`, worker_global_scope.cc:313-332). `None` is the caller's
/// `SyntaxError`: Chrome 150, measured, `importScripts('http://[')` and -- in a `blob:` worker, whose URL cannot be a
/// base -- `importScripts('/x.js')` both throw "The URL '...' is invalid.".
pub(crate) fn parse_import_url(base: &str, input: &str) -> Option<url::Url> {
    // AN ABSOLUTE URL NEEDS NO BASE, and this used to demand one: the base was parsed FIRST and a
    // worker whose own script URL is not a parseable URL (the `__workerScripts` seam's keys, a bare
    // `korvus run` with no document) failed every `importScripts` -- including
    // `importScripts("http://…")`, which resolves to itself. Measured on the release binary while
    // checking W6.2's `importScripts` row: "NetworkError: worker has no valid base URL" for an
    // absolute http URL. WHATWG resolves the input first and only consults the base for a relative
    // reference, which is what this does now.
    if let Ok(u) = url::Url::parse(input) {
        return Some(u);
    }
    url::Url::parse(base).ok()?.join(input).ok()
}

/// One `importScripts` URL, fetched: HTML's "fetch a classic worker-imported script", Blink's
/// `WorkerGlobalScope::FetchClassicImportedScript` (worker_global_scope.cc:403-423).
pub(crate) struct ImportedScript {
    /// The script's text.
    pub(crate) source: String,
    /// Its errors are MUTED: another origin delivered it (`GetSecurityOrigin()->CanReadContent(response_url)`,
    /// worker_global_scope.cc:362-365). Chrome 150, measured: a cross-origin script's `throw` and its syntax
    /// error both reach the caller as "NetworkError: ... The script at '<url>' failed to load."
    pub(crate) muted: bool,
}

/// The text of a script that arrived as bytes: Blink's `TextResourceDecoder` for plain text, UTF-8 when the
/// response names no charset (worker_classic_script_loader.cc:240-247), a byte order mark taken off.
fn script_text(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or_else(|| s.into_owned())
}

/// Do `a` and `b` have the same TUPLE origin? An opaque origin is never the same as anything.
fn same_tuple_origin(a: &url::Url, b: &url::Url) -> bool {
    let (oa, ob) = (a.origin(), b.origin());
    oa.is_tuple() && oa == ob
}

/// `importScripts(url, ...)`'s fetch of ONE parsed URL, run synchronously on the worker's thread. `None` is the
/// caller's `NetworkError` ("The script at '<url>' failed to load."), whatever the cause -- Blink's loader says only
/// that it failed (worker_global_scope.cc:349-356). Chrome 150, measured in a blob, a URL-loaded and a shared
/// worker (`tests/fixtures/worker-script-loading`):
///
/// ```text
/// blob: (the page's, its own, any type)       runs          revoked / never minted       NetworkError
/// data: (any type, base64 too)                runs
/// http(s) 2xx, a JavaScript MIME type         runs          nosniff + JavaScript         runs
/// http(s) text/plain text/html application/json image/png, no Content-Type, nosniff + text/plain   NetworkError
/// http(s) 404 500                             NetworkError  204 (empty)                  runs (nothing)
/// a same-origin redirect, a cross-origin script   run
/// about: javascript: ftp: file: ws:           NetworkError, nothing requested
/// ```
///
/// The MIME check is Blink's STRICT one (`FetchClientSettingsObjectImpl::MimeTypeCheckForClassicWorkerScript`: a
/// worker's settings are never a window's, fetch_client_settings_object_impl.cc:51-69), and `blob:` / `data:`
/// responses are outside it (`AllowedByNosniff::MimeTypeAsScript`, allowed_by_nosniff.cc:169-174). This sent every
/// URL to the HTTP transport, which refused `blob:` and `data:` ("builder error: invalid authority"): on
/// marriott.com a bot sensor sensor's worker lost 23 of its 120 signals with its blob import (ledger N23/N28).
///
/// This was an `#[op2]` a JS body called; it is a plain Rust function now, because the
/// caller is `worker_scope::cb_import_scripts` -- the `importScripts` ROW's body -- and a
/// service one Rust function calls does not need to be a JS-visible op.
pub(crate) fn import_scripts_source(state: &mut OpState, url: &url::Url) -> Option<ImportedScript> {
    let base = state.borrow::<HostLocation>().get();
    let own = url::Url::parse(&base).ok();
    match url.scheme() {
        // the worker's own blob URLs first, then every realm's of this process (a page's blob, read from its worker)
        "blob" => {
            let (_, bytes) = webapi::blob_url_bytes(state, url.as_str())?;
            let muted = !own.as_ref().is_some_and(|o| same_tuple_origin(o, url));
            Some(ImportedScript { source: script_text(&bytes), muted })
        }
        // a `data:` URL's content is readable by every origin (`SecurityOrigin::CanReadContent`)
        "data" => webapi::decode_data_url(url.as_str()).map(|(_, bytes)| ImportedScript { source: script_text(&bytes), muted: false }),
        "http" | "https" => import_over_http(state, url, &base, own.as_ref()),
        _ => None,
    }
}

/// [`import_scripts_source`] over the network: destination `script`, mode `no-cors`, the worker's own network
/// isolation and cookie context.
fn import_over_http(state: &mut OpState, url: &url::Url, base: &str, own: Option<&url::Url>) -> Option<ImportedScript> {
    // Initiated by the worker's environment: its URL, or for a `blob:` worker its ORIGIN with no `Referer`
    // (as `op_fetch`'s `prepare_page_request`); a `data:` worker's opaque origin names nothing.
    let blob_client = crate::blob_origin(base);
    let initiator = blob_client.clone().or_else(|| base.starts_with("http").then(|| base.to_string()));
    let mut req = transport::FetchRequest {
        url: url.to_string(),
        destination: transport::Destination::from("script"),
        mode: transport::Mode::NoCors,
        initiator: initiator.clone(),
        no_referrer: blob_client.is_some(),
        reporter: state.try_borrow::<WorkerReporter>().map(|r| r.0.clone()).unwrap_or_default(),
        ..Default::default()
    };
    // The WORKER's own persona -- `worker.rs` puts a `HostPersona` in the worker isolate's OpState at
    // boot (the window realm's seam, one realm sideways), so an `importScripts` from inside a worker
    // reaches the network wearing the same device the worker's `navigator` reports. Without this the
    // request names no persona and `transport::fetch` refuses it, which is the intended shape: a
    // worker that somehow has no persona must not silently borrow another one's ClientHello.
    if let Some(hp) = state.try_borrow::<crate::HostPersona>() {
        transport::apply_persona_headers(&*hp.0, &mut req);
    }
    if let Some(w) = state.try_borrow::<WorkerScope>() {
        req.no_client_hints = w.kind == "dedicated";
        // A dedicated worker's requests are its creator's, network isolation and all (`transport::partition`): its
        // imports key the HTTP cache by (top-level site, creator's site), the partition its creator's own loads are
        // in. Chrome 150, measured: a cross-site frame's worker imports the script the frame
        // loaded from the HTTP cache; here it was keyed (frame site, frame site) and downloaded again.
        if !w.top_level_url.is_empty() {
            req.top_level_url = Some(w.top_level_url.clone());
        }
        // ... and its cookie context (a worker a cross-site frame started is in that frame's context)
        let page_url = initiator.clone().unwrap_or_default();
        req.cookie_cross_site = transport::cookie_cross_site(&w.site_for_cookies, &page_url, url.as_str());
    }
    let resp = blocking_fetch(req).ok()?;
    // `WorkerClassicScriptLoader::DidReceiveResponse` (worker_classic_script_loader.cc:208-221): a non-2xx status
    // fails, and so does a type the strict check refuses -- a JavaScript MIME type or nothing, as a module script's
    if !(200..300).contains(&resp.status) || !webapi::page::script_response_ok(webapi::page::ScriptFlavor::Module, resp.status, &resp.headers) {
        return None;
    }
    let response_url = url::Url::parse(&resp.url).unwrap_or_else(|_| url.clone());
    let muted = !own.is_some_and(|o| same_tuple_origin(o, &response_url));
    Some(ImportedScript { source: resp.body, muted })
}

/// `self.location.href` for THIS worker isolate (its own script URL, never the page's). Read by
/// worker-host.js at its own (early) init time -- BEFORE worker-finalize.js snapshots `globalThis.location`
/// into a native getter closure -- so the location that ends up baked into that closure is the real one.
#[op2]
#[string]
fn op_worker_location_href(state: &mut OpState) -> String {
    state.borrow::<HostLocation>().get()
}

/// `self.location.origin` for THIS worker isolate. Companion to [`op_worker_location_href`].
#[op2]
#[string]
fn op_worker_location_origin(state: &mut OpState) -> String {
    worker_origin(&state.borrow::<HostLocation>().get())
}

/// Which WorkerGlobalScope this isolate actually IS. Every worker isolate shares one bootstrap
/// (`worker-finalize.js`), which until now unconditionally produced a `DedicatedWorkerGlobalScope`
/// realm -- including for the SharedWorker isolates `spawn_shared_worker_persona` starts. That is observable
/// and it is load-bearing: testharness.js picks its test environment with
/// `global_scope instanceof DedicatedWorkerGlobalScope` FIRST, so a shared worker running the real
/// suite reported its results down the dedicated `postMessage` plane, which a `SharedWorker` page holds
/// no end of. The scope kind travels from the spawn site to the bootstrap so the realm can be the one
/// it claims to be.
#[derive(Clone)]
pub struct WorkerScope {
    /// "dedicated" | "shared" | "service".
    pub kind: String,
    /// `SharedWorkerGlobalScope.name` -- the `new SharedWorker(url, name)` name. Empty otherwise.
    pub name: String,
    /// A service worker's registering page -- its `Client`'s `url`. Empty otherwise.
    pub client_url: String,
    /// The creating document's site for cookies: `"null"` for a worker a cross-site frame
    /// started, `""` for the page's own. A request from the worker is judged in that context.
    pub site_for_cookies: String,
    /// The creating document's TOP-LEVEL document URL: a dedicated worker's requests
    /// carry its creator's network isolation -- Chrome 150 put the challenge worker's `fetch` on the
    /// connection of the cross-site frame that started it (`transport::partition`). Empty for a shared or
    /// service worker and a bare isolate, whose requests key by their own site.
    pub top_level_url: String,
}

/// The scope kind of THIS worker isolate ("dedicated" | "shared" | "service"), read by
/// worker-finalize.js to decide which global-scope interface the realm gets.
#[op2]
#[string]
fn op_worker_scope_kind(state: &mut OpState) -> String {
    state.try_borrow::<WorkerScope>().map(|s| s.kind.clone()).unwrap_or_else(|| "dedicated".to_string())
}

/// This isolate's scope kind, read straight off `OpState` from Rust -- what [`op_worker_scope_kind`]
/// answers to JS, from the same `WorkerScope` record. The realm block uses it to decide
/// whether this isolate gets the ServiceWorker fetch transport.
fn scope_kind_of(scope: &mut v8::PinScope) -> String {
    let op_state = JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<WorkerScope>().map(|s| s.kind.clone()).unwrap_or_else(|| "dedicated".to_string())
}

/// `SharedWorkerGlobalScope.name` for THIS isolate (empty in every other scope).
#[op2]
#[string]
fn op_worker_scope_name(state: &mut OpState) -> String {
    state.try_borrow::<WorkerScope>().map(|s| s.name.clone()).unwrap_or_default()
}

/// The page that registered THIS service worker (empty in every other scope): what its `Client`'s `url`
/// answers (`webapi`'s `core/workers/service_worker.rs`).
#[op2]
#[string]
fn op_worker_client_url(state: &mut OpState) -> String {
    state.try_borrow::<WorkerScope>().map(|s| s.client_url.clone()).unwrap_or_default()
}

/// The worker's own `HostLocation` value: the real script URL for the URL-loading path, "about:blank" when
/// the caller gave none (matches `window.location`'s own default; also what the `__workerScripts`/Blob:
/// test seam gets when its key isn't a real URL).
fn worker_location(url: &str) -> String {
    if url.is_empty() { "about:blank".to_string() } else { url.to_string() }
}

/// `url`'s origin (ascii-serialized), or "" when it isn't a valid absolute URL (the test/Blob seam's keys
/// are often not real URLs at all -- "./echo-worker.js", "blob:...").
fn worker_origin(url: &str) -> String {
    url::Url::parse(url).map(|u| u.origin().ascii_serialization()).unwrap_or_default()
}

/// What a worker realm does the moment its top-level script -- a literal (the `__workerScripts` / Blob:
/// seam) or a fetched one ([`ScriptLoad`]) -- has been evaluated: `execute_script` has just returned and no
/// microtask has run (deno_core's policy is explicit). A SharedWorker releases the `connect`s that arrived
/// while its script loaded; a ServiceWorker starts its install -> activate lifecycle, then does the same.
///
/// This used to be JS appended to the script -- `CONNECT_FLUSH`, `SW_FLUSH` -- which
/// reached two native functions through a `Symbol.for("korvus.bridge")` holder on the worker's global,
/// where a page's worker script could find it (Chrome's worker global has no symbol-keyed member at all,
/// measured on all three kinds). The same moment, from Rust, needs no holder. As the appended strings did,
/// it runs only after a script that completed, and one `try` covered both steps: a lifecycle start that
/// throws skips the flush.
fn after_top_level(runtime: &mut JsRuntime, threw: bool) {
    let ctx = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
    let kind = scope_kind_of(scope);
    if kind != "shared" && kind != "service" {
        return;
    }
    v8::tc_scope!(let tc, scope);
    if kind == "service" {
        // A service worker whose script threw is not one that loaded.
        if threw {
            return;
        }
        webapi::sw_script_loaded(tc);
        if tc.has_caught() {
            tc.reset();
            return;
        }
    }
    crate::worker_recv::flush_connects(tc);
    tc.reset();
}

/// B3/B4 (ABI 3) for a WORKER realm: before the worker's own top-level script runs, install the loaded
/// plugins' bound globals (`host->add_binding`) on this worker's scope and run their init scripts for this
/// realm kind, in load order -- the worker counterpart of `run_doc_scripts`'s page block (webapi's
/// `browsing_context.rs`) and `navigation.rs`'s top-page block. This is the pre-script point the TODO in
/// `run_doc_scripts` names: the worker realms boot from korvus-runtime, not the document path, so their
/// `init_scripts(REALM_*_WORKER)` injection is HERE. Bindings first -- an init script may call one.
///
/// Nothing is installed or run until a plugin fills `add_binding` / `init_script`, so a no-plugin worker is
/// byte-for-byte unchanged (invariant I9/J2). The realm kind is this isolate's `WorkerScope` kind
/// ([`scope_kind_of`]): "dedicated" -> `REALM_DEDICATED_WORKER`, "shared" -> `REALM_SHARED_WORKER`,
/// "service" -> `REALM_SERVICE_WORKER`. (The realm's TIMERS already read the virtual clock under F5: a
/// worker arms and dispatches through the SAME `crate::scheduler` the page does, whose `set_timer` / `rearm`
/// / `op_timer_next_due` call `sched_now`, and its `performance.now()` is webapi's virtual-aware reading.)
fn install_worker_plugin_env(runtime: &mut JsRuntime) {
    if !plugin_host::any() {
        return;
    }
    let ctx = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
    let realm = match scope_kind_of(scope).as_str() {
        "shared" => plugin_host::REALM_SHARED_WORKER,
        "service" => plugin_host::REALM_SERVICE_WORKER,
        _ => plugin_host::REALM_DEDICATED_WORKER,
    };
    // the worker's plugin bindings (empty until a plugin adds one), on THIS realm's global -- the same
    // `install_plugin_bindings_current` the top page uses, in the other realm.
    webapi::install_plugin_bindings_current(scope);
    // then each init script for this realm, as a CLASSIC script in the worker scope: a throw is REPORTED in
    // the worker's own realm and the worker keeps running (as `run_worker_classic` reports the worker's own
    // script's throw), and a `terminate()` during one stops the boot.
    for code in plugin_host::init_scripts(realm) {
        match crate::script_exec::run_worker_script(scope, &code, "korvus:plugin-init") {
            (_, true) => return,
            (Some((err, site)), false) => {
                let err = v8::Local::new(scope, &err);
                crate::script_exec::report(scope, err, site);
            }
            (None, false) => {}
        }
    }
}

/// The DevTools reporter of THIS worker isolate's own requests (`op_fetch`, `op_fetch_sync`,
/// `importScripts`), in its `OpState`. See `transport::Reporter`.
pub(crate) struct WorkerReporter(pub(crate) transport::Reporter);

/// A worker's top-level script, loaded from its URL on the worker's OWN thread before any of its code
/// runs.
///
/// Measured on stock Chrome 150.0.7871.46 (loopback server log + auto-attached CDP):
///
/// ```text
/// script         Sec-Fetch-Mode   Sec-Fetch-Dest   requestWillBeSent on   response on
/// new Worker     same-origin      worker           the page               the worker
/// SharedWorker   same-origin      sharedworker     the page               (not reported)
/// SW register    same-origin      serviceworker    the service worker     the service worker
/// ```
///
/// The fetch is awaited on the worker's OWN runtime (no second thread pool, no blocking wait), and BEFORE its
/// realm is made: Chrome 150 asks for the script the moment the worker is constructed (`DedicatedWorker::Start`
/// -> `CreateWorkerHost`, dedicated_worker.cc:271-318) and creates the worker thread -- the worker's time origin
/// (`WorkerThread`, worker_thread.cc:500) -- only when the response is in (`OnScriptLoadStarted` ->
/// `ContinueStart`, :356-375). A message posted to the worker meanwhile waits in its channel.
pub(crate) struct ScriptLoad {
    url: String,
    destination: &'static str,
    /// The document that started the worker: its `Referer`/`Sec-Fetch-Site`, and its mixed-content context.
    initiator: Option<String>,
    /// The starting environment's URL is no referrer (a `blob:` worker's nested worker).
    no_referrer: bool,
    reporter: transport::Reporter,
    /// The response check the script must pass: `Classic` is Blink's lax check for a worker a WINDOW made,
    /// `Module` the strict one for a module worker or a worker a worker made
    /// (`FetchClientSettingsObjectImpl::MimeTypeCheckForClassicWorkerScript`); `None` checks no type (a service
    /// worker's script, not measured here).
    check: Option<webapi::page::ScriptFlavor>,
    /// The creator's network isolation and cookie context (see [`WorkerScope::top_level_url`]).
    top_level_url: Option<String>,
    cookie_cross_site: bool,
}

/// A worker's script as it arrived ([`load_worker_script`]).
struct LoadedScript {
    code: String,
    timing: spec::FetchTiming,
    headers: Vec<(String, String)>,
    /// The response's URL -- the worker's URL, after a same-origin redirect (Chrome 150, measured: `location.href`
    /// is the redirect's target).
    url: String,
}

/// The `Initiator` a request of the environment at `url` names, and whether it may name a referrer: an http(s)
/// document or worker is itself; a `blob:` worker is its blob's ORIGIN, with no `Referer`; anything
/// else (an opaque origin) names nothing.
fn request_client(url: &str) -> (Option<String>, bool) {
    if let Some(origin) = crate::blob_origin(url) {
        return (Some(origin), true);
    }
    (url.starts_with("http").then(|| url.to_string()), false)
}

/// How a worker isolate starts: the script it loads (if its source is a URL) and the reporter of its own
/// requests.
#[derive(Default)]
pub(crate) struct WorkerBoot {
    load: Option<ScriptLoad>,
    reporter: transport::Reporter,
    /// `{type: "module"}`: the script is an ES module -- `main_url` is its own URL (a
    /// `blob:` one included), and its imports are fetched by a [`crate::KorvusModuleLoader`] that wears
    /// the persona and names `page_url` as their initiator.
    module: bool,
    main_url: String,
    /// `worker` / `sharedworker` / `serviceworker` (empty for a bare isolate): a service worker is the one
    /// realm where `import()` is disallowed (HTML; Chrome rejects it), so the one a classic script gets no
    /// module loader in.
    destination: &'static str,
    page_url: Option<String>,
    /// The creator's cross-origin isolation, for a worker that inherits it.
    isolated: bool,
    /// The creator's site for cookies, which the worker's requests are judged in.
    site_for_cookies: String,
    /// The creator's top-level document URL (see [`WorkerScope::top_level_url`]); `op_worker_spawn` states it.
    top_level_url: String,
    /// The CSP list the worker's realm holds, as delivering headers: its creator's for a `blob:` / `data:` script
    /// (`op_worker_spawn` states it); a fetched script's realm takes its own response's instead.
    csp_headers: Vec<(String, String)>,
    /// This browsing session's farbling seed, inherited from the window so the worker's
    /// `navigator.hardwareConcurrency`/`deviceMemory` farble to the SAME per-origin value the window
    /// does. `None` leaves the worker's scalars static (a non-farbling persona, or a bare test isolate).
    session_seed: Option<spec::farble::SessionSeed>,
    /// A dedicated worker's `WorkerOptions.name`: its global's `name` (Chrome 150; korvus answered "").
    name: String,
    /// When the worker may begin: its thread waits here before it makes its global -- its script is already being
    /// fetched -- until its creator's task is over ([`op_worker_start`]). `None` begins at once.
    start: Option<Arc<WorkerStart>>,
}

/// THIS worker's script is a module (`{type: "module"}`): `importScripts` throws (HTML "import scripts into worker
/// global scope" step 1; worker_global_scope.cc:298-304). In the worker's `OpState`.
pub(crate) struct ModuleWorker;

/// A WORKER BEGINS WHEN ITS CREATOR'S TASK IS OVER. Chrome 150, three runs alike: a blob: and a
/// URL worker made at the head of a 200 ms task both have `performance.timeOrigin` 201 ms after `new Worker` -- the
/// global scope is made, and the script fetched, once that task has ended; korvus's thread made its realm at once (1
/// ms). The thread is spawned in the constructor, so a message posted before it begins waits in its channel as before;
/// it waits HERE until the creating realm's microtask checkpoint opens the gate.
#[derive(Default)]
pub struct WorkerStart {
    open: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl WorkerStart {
    /// Let the worker begin.
    pub fn open(&self) {
        self.open.store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.open.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// A fresh DevTools target id: 32 uppercase hex characters, the shape of every Chrome `targetId`.
fn new_target_id() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// Announce a worker target to the request ledger and return its id. BEFORE the worker thread exists,
/// so a client is told about the target before it is told about anything the target fetched.
fn announce_target(kind: &str, url: &str) -> String {
    let id = new_target_id();
    transport::ledger().announce_target(transport::ChildTarget { id: id.clone(), kind: kind.to_string(), url: url.to_string() });
    id
}

/// The [`WorkerBoot`] for a worker whose literal `source` may be empty (then `url` is loaded, reported by
/// `load_reporter`), whose own requests are reported on `target`.
fn worker_boot(state: &OpState, source: &str, url: &str, destination: &'static str, load_reporter: transport::Reporter, target: String) -> WorkerBoot {
    let (initiator, no_referrer) = request_client(&state.try_borrow::<HostLocation>().map(|l| l.get()).unwrap_or_default());
    // A worker a worker makes has its script checked strictly (fetch_client_settings_object_impl.cc:64-69: nested
    // workers are new, so no legacy type is let through); a window's worker, laxly.
    let nested = state.try_borrow::<WorkerScope>().is_some();
    let load = (source.is_empty() && !url.is_empty()).then(|| ScriptLoad {
        url: url.to_string(),
        destination,
        initiator,
        no_referrer,
        reporter: load_reporter,
        check: match destination {
            "serviceworker" => None,
            _ if nested => Some(webapi::page::ScriptFlavor::Module),
            _ => Some(webapi::page::ScriptFlavor::Classic),
        },
        top_level_url: None,
        cookie_cross_site: false,
    });
    WorkerBoot {
        load,
        reporter: transport::Reporter { target: Some(target), response_target: None },
        module: false,
        isolated: false,
        main_url: url.to_string(),
        destination,
        page_url: state.try_borrow::<HostLocation>().map(|l| l.get()).filter(|u| u.starts_with("http")),
        // `op_worker_spawn` states its creator's; a shared or service worker's requests stay the page's.
        site_for_cookies: String::new(),
        top_level_url: String::new(),
        csp_headers: Vec::new(),
        // Inherit the creating session's farbling seed, so the worker farbles to the same
        // per-origin values the window does.
        session_seed: state.try_borrow::<spec::farble::SessionSeed>().copied(),
        name: String::new(),
        start: None,
    }
}

/// The worker realm's CSP list, delivered by `headers` ([`webapi::page::set_document_csp`] in the worker's realm):
/// what its `eval`, `new Function` and string timers are judged by.
fn set_worker_csp(runtime: &mut deno_core::JsRuntime, headers: &[(String, String)]) {
    let ctx = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
    webapi::page::set_document_csp(scope, headers);
}

/// Fetch a worker's script over the network, wearing `persona`. `Err` is a script that did not load -- the creator's
/// `Worker` gets a plain `error` `Event` and no realm is made (`OnScriptLoadStartFailed`, dedicated_worker.cc:378-382).
/// Chrome 150, measured from a page (`tests/fixtures/worker-script-loading`):
///
/// ```text
/// 2xx text/javascript text/plain text/html, no Content-Type          runs (a module worker: a JavaScript type only)
/// 2xx image/png text/csv, nosniff + text/plain, 404                   error Event
/// a same-origin redirect                                              runs, its `location` the redirect's target
/// a cross-origin redirect                                             error Event (`CheckSameOriginEnforcement`)
/// from a WORKER: text/plain, no Content-Type                          error Event (the strict check)
/// ```
///
/// Only http(s) reaches the network: a `blob:` or `data:` script was read by the constructor (a module one whose type
/// is not JavaScript is refused here), and no other scheme passes the constructor's checks.
async fn load_worker_script(load: &ScriptLoad, persona: Option<&std::sync::Arc<dyn spec::Persona>>) -> Result<LoadedScript, ()> {
    let request_url = url::Url::parse(&load.url).map_err(|_| ())?;
    if !matches!(request_url.scheme(), "http" | "https") {
        return Err(());
    }
    let mut req = transport::FetchRequest {
        url: load.url.clone(),
        destination: transport::Destination::from(load.destination),
        mode: transport::Mode::SameOrigin,
        initiator: load.initiator.clone(),
        no_referrer: load.no_referrer,
        reporter: load.reporter.clone(),
        top_level_url: load.top_level_url.clone(),
        cookie_cross_site: load.cookie_cross_site,
        ..Default::default()
    };
    // A dedicated worker's script request carries no Client Hints (Brave 151 and Chrome
    // 150, classic and module, over http and https).
    req.no_client_hints = load.destination == "worker";
    // A service worker's script request says what it is for (Service Workers, "Update" step 9.3).
    if load.destination == "serviceworker" {
        req.headers.push(("service-worker".to_string(), "script".to_string()));
    }
    if let Some(p) = persona {
        transport::apply_persona_headers(&**p, &mut req);
    }
    let r = transport::fetch(req).await.map_err(|_| ())?;
    // `WorkerClassicScriptLoader::DidReceiveResponse` (worker_classic_script_loader.cc:208-232): a 2xx status, a type
    // the check lets through, and a response from the request's own origin
    if !(200..300).contains(&r.status) {
        return Err(());
    }
    if let Some(flavor) = load.check {
        if !webapi::page::script_response_ok(flavor, r.status, &r.headers) {
            return Err(());
        }
    }
    let url = if r.url.is_empty() { load.url.clone() } else { r.url.clone() };
    if !url::Url::parse(&url).is_ok_and(|u| same_tuple_origin(&u, &request_url)) {
        return Err(());
    }
    Ok(LoadedScript { code: r.body, timing: r.timing, headers: r.headers, url })
}

extension!(
    korvus_worker_host_ext,
    // W1.6: op_fetch (shared with the window -- resolves relative URLs against HostLocation below, and
    // falls back to the default transport::fetch when no TransportHandle is in this isolate's OpState,
    // exactly as op_fetch's own comment anticipates for workers) backs `globalThis.fetch`
    // (shared-globals.js); op_worker_import_scripts_fetch backs `importScripts` (worker-finalize.js).
    // Phase 3 (Part 2) worker-navigator needed two more window ops here (`op_persona_hints` and
    // `op_nav_glue_register`) so that the SAME `nav-glue.js` could run in this realm.
    // deleted that file: the worker `WorkerNavigator`'s bodies are the same Rust table the window
    // installs, reading the same persona service, so neither op exists any more.
    // `op_worker_post_fn` / `op_worker_terminate_fn` are here so the `Worker` TABLE's two
    // method rows have the SAME native body in this realm as in the window (invariant I8) -- the table
    // says `native`, and a row that fell back to the D2 stub in one realm only would make the live
    // layer disagree with the table it came from. Since the nested workers below, this realm's
    // constructor writes the isolate id both bodies read, as the window's does.
    // `op_worker_scope_fn` is the FACTORY the `WorkerGlobalScope` /
    // `DedicatedWorkerGlobalScope` tables call for every row whose body is a kernel service (the five
    // timers, `importScripts`, `close`, `postMessage`, the two animation-frame methods); the bodies
    // are `worker_scope.rs`. The `close()` flag the receive loops read -- `globalThis.__closed` once,
    // then `op_worker_scope_closed` -- needs no op at all: the loops are Rust
    // (`worker_recv.rs`) and read `WorkerClosed` off `OpState`.
    ops = [op_kprint, op_random_bytes, op_uuid, op_btoa, op_atob, op_bc_self_recv, op_port_self_recv, op_worker_self_error, op_fetch, op_fetch_head, op_fetch_body, op_fetch_body_chunk, op_fetch_body_cancel, op_fetch_sync, crate::worker_scope::op_worker_scope_fn, op_worker_location_href, op_worker_location_origin, op_worker_scope_kind, op_worker_scope_name, op_worker_client_url, // The SAME timer task source the window runs (invariant I8 -- one algorithm, both
        // realms). worker-host.js's pump is the only realm-specific half.
        crate::scheduler::op_timer_set, crate::scheduler::op_timer_clear, crate::scheduler::op_timer_pump_claim, crate::scheduler::op_timer_next_due, crate::scheduler::op_timer_run, op_worker_post_fn, op_worker_terminate_fn,
        // NESTED WORKERS (korvus-analysis, the live a challenge provider challenge): a worker's own `new Worker(...)`
        // spawns a worker of its own, over the same ops and the same `Worker` table as the window's. They
        // were missing, so the constructor branded and returned a worker that never ran -- no message, no
        // error -- and a bot-challenge frame, whose first worker starts the workers that do the work,
        // never finished. Chrome 150, measured: a worker's two inner blob workers run and relay.
        op_worker_spawn, op_worker_start, op_worker_recv, op_worker_bc_recv, op_worker_port_recv, op_worker_err_recv, op_worker_timing_recv, op_worker_msg_in,
        // `WebSocket` is [Exposed=(Window,Worker)]: the same kernel service the window registers.
        crate::websocket::op_ws_open, crate::websocket::op_ws_next, crate::websocket::op_ws_send_text, crate::websocket::op_ws_send_binary, crate::websocket::op_ws_close, crate::websocket::op_ws_abort],
    // `objects = [JsTextEncoder, JsTextDecoder, JsNavigator]` WAS HERE: korvus-webapi's own classes since
    // `install_worker_bootstrap` builds the two codec interfaces from their tables and
    // mints `navigator` from the own class, whose proto this file then points at the table.
    // THIS EXTENSION HAS NO ESM AT ALL, and `core/korvus-runtime/src/js/worker-host.js`
    // is deleted.
    //
    // The layer shrank in three steps and each one killed a reason it had to be JS. W6.2 deleted
    // `worker-finalize.js`, `generated_worker_shells.js` and `worker-native.js` -- every
    // capture-shaped worker interface, the twenty-one XHR builder ops, twelve `op_nativize`-masked
    // `WorkerGlobalScope.prototype` members, the global reparent and the own-copy sweep are
    // `webapi::install_worker_shells`, `install::live::install_worker_crypto_xhr` and
    // `core/workers/worker_global_scope.rs` plus `src/worker_scope.rs`. W5.3 took the rest of
    // `worker-host.js`: its eleven own-global assignments and the `EventTarget` prototype object are
    // `install::live::install_worker_bootstrap` (the `objects =` classes were never ESM-only -- see
    // `binding::hidden::ops_object`), its DOMException error-builder registration went with them, its
    // `Deno`/`__bootstrap` strip is `webapi::hide_deno`, and its three async receive loops plus the
    // connect glue are `src/worker_recv.rs`. `events.js` left this extension's list with the
    // `EventTarget` assignment that imported it.
    //
    // So a worker realm is now assembled entirely by `spawn_on_thread`'s Rust block, in one order,
    // with no ESM evaluation to sequence against (invariants I8, I9).
    options = {
        // Cross-isolate BroadcastChannel: `bc_sender` forwards a worker BC post to main; `bc_receiver`
        // delivers a main (or peer) BC post into the worker (awaited by `op_bc_self_recv`).
        bc_sender: tmpsc::UnboundedSender<Vec<u8>>,
        bc_receiver: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
        // W3b MessagePort: `port_sender` ships a remote port's post to main (which routes it to the peer's
        // isolate); `port_receiver` delivers a post routed INTO this worker (awaited by `op_port_self_recv`).
        port_sender: tmpsc::UnboundedSender<Vec<u8>>,
        port_receiver: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
        err_sender: tmpsc::UnboundedSender<Vec<u8>>,
        // This worker's OWN script URL (never the page's) -- backs `op_fetch`'s relative-URL
        // resolution via `HostLocation`. It backed `op_worker_import_scripts_fetch` too, until a later change
        // made the worker scope five `interface!` tables and deleted that op.
        location: String,
        // 4.3b: which WorkerGlobalScope this isolate is ("dedicated" | "shared" | "service") and, for a
        // shared one, its `new SharedWorker(url, name)` name. See [`WorkerScope`].
        scope_kind: String,
        scope_name: String,
        // B22: a service worker's registering page (`Client.url`); empty for the other kinds.
        client_url: String,
        // The creating document's site for cookies (`"null"` under a cross-site ancestor).
        site_for_cookies: String,
        // The creating document's top-level document URL (a dedicated worker's).
        top_level_url: String,
    },
    state = |state: &mut OpState, options| {
        state.put(PrintBuffer::default()); // the worker's own print sink (op_kprint); drained on exit.
        state.put(crate::scheduler::Scheduler::default()); // This realm's timer task source.
        // This realm's OWN high-resolution clock: `performance.timeOrigin` in a worker is when the
        // WORKER started, and `performance.now()` is monotonic from there. The worker used to run
        // `Date.now() - origin` in JS, a 1 ms clock where the window had a 100 us-quantized one; both
        // realms read `spec::PerfClock` now.
        state.put(spec::PerfClock::default());
        state.put(HostLocation::new(options.location));
        state.put(WorkerRegistry::default()); // this worker's own (nested) workers, by id
        state.put(WorkerScope {
            kind: options.scope_kind,
            name: options.scope_name,
            client_url: options.client_url,
            site_for_cookies: options.site_for_cookies,
            top_level_url: options.top_level_url,
        });
        // Install the webapi BroadcastChannel forward hook so the worker's `cb_bc_post_message` ships each
        // post to main after its local fan-out. bc_encode packs (name, structured-clone bytes) into a frame.
        let bc_sender = options.bc_sender;
        state.put(webapi::BcForward(Box::new(move |name, bytes| {
            let _ = bc_sender.send(bc_encode(&name, &bytes));
            wake_page();
        })));
        state.put(WorkerBcReceiver(options.bc_receiver));
        // W3b: a remote port's post routes out through PortForward -> the worker's port_out channel -> main.
        let port_sender = options.port_sender;
        state.put(webapi::PortForward(Box::new(move |id, bytes| {
            let _ = port_sender.send(port_encode(id, &bytes));
            wake_page();
        })));
        state.put(WorkerPortReceiver(options.port_receiver));
        state.put(WorkerErrSender(options.err_sender));
    },
);

/// A live worker isolate. Two message planes, each with its own channel pair: `outbound`/`inbound` carry
/// `Worker.postMessage` (worker<->main), and `bc_outbound`/`bc_inbound` carry cross-isolate BroadcastChannel
/// traffic (length-prefixed `[u32 name_len][name][structured-clone bytes]`, see [`bc_encode`]). The worker
/// owns its whole `JsRuntime` on its own thread; only bytes cross.
pub struct WorkerHandle {
    pub outbound: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    pub inbound: tmpsc::UnboundedSender<Vec<u8>>,
    pub bc_outbound: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    pub bc_inbound: tmpsc::UnboundedSender<Vec<u8>>,
    /// W3b cross-isolate MessagePort traffic (length-prefixed `[u32 channel_id][structured-clone bytes]`, see
    /// [`port_encode`]): `port_outbound` = a remote port's post leaving the worker; `port_inbound` = a post
    /// routed INTO this worker for a transferred port it holds.
    pub port_outbound: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    pub port_inbound: tmpsc::UnboundedSender<Vec<u8>>,
    /// Uncaught worker errors (worker -> main), as a UTF-8 message; drives `Worker.onerror`.
    pub err_outbound: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    /// The transport's timing of a dedicated worker's own script (worker -> main, one JSON
    /// `spec::FetchTiming`): the PARENT's resource-timing entry for it.
    pub timing_outbound: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    /// A thread-safe handle to the worker isolate (populated once the worker builds its runtime). Lets
    /// `terminate()` interrupt a BUSY worker (`isolate.terminate_execution()`); dropping the channels alone
    /// only stops an idle one. `IsolateHandle` is Send+Sync by design (built for cross-thread termination).
    pub terminate: Arc<std::sync::Mutex<Option<v8::IsolateHandle>>>,
    /// The worker's start gate ([`WorkerStart`]); `None` for one that began at once.
    pub start: Option<Arc<WorkerStart>>,
    /// Set when this handle is let go of: a worker whose script is still on its way makes no realm and runs nothing
    /// (`terminate()` before the script loaded: no isolate exists yet to interrupt).
    pub ended: Arc<std::sync::atomic::AtomicBool>,
}

/// A handle let go of (terminated, or its creator gone) lets a worker still waiting at its gate begin -- and find its
/// channels closed -- rather than wait for ever.
impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.ended.store(true, std::sync::atomic::Ordering::Release);
        if let Some(s) = &self.start {
            s.open();
        }
    }
}

/// The channel ends handed to a worker runtime's extensions (Post + BroadcastChannel, each direction; plus
/// a worker->main error channel for uncaught worker errors -> `Worker.onerror`).
pub struct WorkerChannels {
    pub out_tx: tmpsc::UnboundedSender<Vec<u8>>,
    pub in_rx: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    pub bc_out_tx: tmpsc::UnboundedSender<Vec<u8>>,
    pub bc_in_rx: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    pub port_out_tx: tmpsc::UnboundedSender<Vec<u8>>,
    pub port_in_rx: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>,
    pub err_out_tx: tmpsc::UnboundedSender<Vec<u8>>,
}

/// Encode a cross-isolate MessagePort message: `[u32 LE channel_id][structured-clone bytes]`. The receiving
/// side (`op_port_deliver_external`, after decoding in JS) splits it back and delivers to the local port
/// registered for that id.
pub fn port_encode(id: u32, msg: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + msg.len());
    v.extend_from_slice(&id.to_le_bytes());
    v.extend_from_slice(msg);
    v
}

/// Encode a cross-isolate BroadcastChannel message: `[u32 LE name_len][name utf8][structured-clone bytes]`.
/// The receiving side (`op_bc_deliver_external`, called after decoding in JS) splits it back.
pub fn bc_encode(name: &str, msg: &[u8]) -> Vec<u8> {
    let nb = name.as_bytes();
    let mut v = Vec::with_capacity(4 + nb.len() + msg.len());
    v.extend_from_slice(&(nb.len() as u32).to_le_bytes());
    v.extend_from_slice(nb);
    v.extend_from_slice(msg);
    v
}

/// Shared thread/channel/runtime setup: run `source` on its own OS thread + current-thread tokio + a worker
/// `JsRuntime` whose extension set `make_exts` builds from the four channel ends. Any worker-host esm entry
/// point in that set evaluates during `JsRuntime::new` (installing the worker scope) BEFORE `source` runs.
///
/// `host` says whether `make_exts` built a REAL WorkerGlobalScope (`korvus_worker_host_ext`) or the
/// bare messaging-only isolate the low-level tests use. It is a declaration by the caller rather than
/// a question asked of the realm, because it decides one thing the realm cannot answer for itself:
/// whether `Deno` is stripped before `source` runs. A bare worker's whole source IS `Deno.core.ops.…`.
///
/// `location` is the worker's URL; `make_exts` is handed the one the realm is made with -- a URL-loaded script's
/// RESPONSE URL, which is the worker's own after a same-origin redirect.
///
/// THE ORDER IS CHROME 150'S (measured, `tests/worker_script_loading_live.rs`; dedicated_worker.cc:271-375): a URL-loaded
/// script is requested at once -- while the creating task still runs -- and the realm (its time origin, its global) is
/// made when the response is in and, for a dedicated worker, the creating task is over; its first statement runs right
/// after. A script that does not load makes no realm: the creator's `Worker` gets its `error` `Event`. korvus asked for
/// the script only once the task was over AND its realm was built (175 ms after `new Worker` at the head of a 150 ms
/// task, where Chrome's request leaves at once), so the worker's time origin preceded its script by the whole fetch.
fn spawn_on_thread<F>(source: String, location: String, persona: Option<std::sync::Arc<dyn spec::Persona>>, host: bool, boot: WorkerBoot, make_exts: F) -> WorkerHandle
where
    F: FnOnce(WorkerChannels, String) -> Vec<deno_core::Extension> + Send + 'static,
{
    let (out_tx, out_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // worker -> main (postMessage)
    let (in_tx, in_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // main -> worker (postMessage)
    let (bc_out_tx, bc_out_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // worker -> main (BroadcastChannel)
    let (bc_in_tx, bc_in_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // main -> worker (BroadcastChannel)
    let (port_out_tx, port_out_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // worker -> main (MessagePort)
    let (port_in_tx, port_in_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // main -> worker (MessagePort)
    let (err_out_tx, err_out_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // worker -> main (uncaught errors)
    let (timing_out_tx, timing_out_rx) = tmpsc::unbounded_channel::<Vec<u8>>(); // worker -> main (its script's timing)
    let terminate: Arc<std::sync::Mutex<Option<v8::IsolateHandle>>> = Arc::new(std::sync::Mutex::new(None));
    let terminate_worker = terminate.clone();
    let start = boot.start.clone();
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ended_worker = ended.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(_) => return,
        };
        rt.block_on(async move {
            // The script is asked for NOW, and the worker begins when it has arrived and its creator's task is over
            // ([`WorkerStart`]): the two waits run together.
            let gate = async {
                if let Some(gate) = boot.start.clone() {
                    gate.wait().await;
                }
            };
            let fetch = async {
                match &boot.load {
                    Some(load) => Some(load_worker_script(load, persona.as_ref()).await),
                    None => None,
                }
            };
            let ((), loaded) = tokio::join!(gate, fetch);
            // terminated (or its creator gone) while it waited: nothing of it runs
            if ended_worker.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let (loaded, location) = match loaded {
                Some(Ok(script)) => {
                    let url = script.url.clone();
                    (Some(script), url)
                }
                // Nothing loaded, and nothing will run: the creator's `Worker` gets its `error` Event and no realm is
                // made.
                Some(Err(())) => {
                    let _ = err_out_tx.send(webapi::WorkerErrorFrame::not_loaded().encode());
                    wake_page();
                    return;
                }
                None => (None, location),
            };
            let chans = WorkerChannels {
                out_tx,
                in_rx: Arc::new(TMutex::new(in_rx)),
                bc_out_tx,
                bc_in_rx: Arc::new(TMutex::new(bc_in_rx)),
                port_out_tx,
                port_in_rx: Arc::new(TMutex::new(port_in_rx)),
                err_out_tx,
            };
            let extensions = make_exts(chans, location);
            // A module worker's imports: the page's loader, over this worker's own fetch state.
            // A CLASSIC worker's `import()` loads through the same loader (Chrome 150, measured: a classic
            // blob worker imports its own blob URL, its page's, and a `data:` module); without one, deno_core
            // answered every `import()` "Module loading is not supported." -- except in a service worker.
            let worker_fetch = (boot.module || boot.destination != "serviceworker").then(|| {
                std::rc::Rc::new(std::cell::RefCell::new(crate::modules::ModuleFetchState {
                    // A dedicated worker's imports carry no Client Hints.
                    identity: persona.as_ref().map(|p| {
                        let mut id = transport::persona_identity(&**p);
                        id.headers.retain(|(k, _)| !k.to_ascii_lowercase().starts_with("sec-ch-ua"));
                        id
                    }),
                    initiator: boot.page_url.clone(),
                    ..Default::default()
                }))
            });
            let module_loader: Option<std::rc::Rc<dyn deno_core::ModuleLoader>> = worker_fetch
                .clone()
                .map(|fetch| std::rc::Rc::new(crate::KorvusModuleLoader { fetch }) as std::rc::Rc<dyn deno_core::ModuleLoader>);
            let mut runtime = crate::new_runtime(RuntimeOptions { extensions, module_loader, ..Default::default() });
            // The worker's timer task source, asked between every two tasks whether a timer came due, as the page's is
            // (`scheduler::wake_due_timers`). Without it a worker timer that came due during a run of short tasks was
            // seen only when the loop's 1 ms turn ended: beside a `MessageChannel` ping-pong of near-empty tasks a
            // `setTimeout(50)` ran 0.4-1.2 ms late (9 of 14 samples), Chrome 152 0-0.3 ms (13 of 14; 2026-10-23).
            runtime.set_task_source_hook(crate::scheduler::wake_due_timers);
            // `import(blobURL)` in a worker reads this worker's own blob registry.
            if let Some(f) = &worker_fetch {
                f.borrow_mut().blobs = webapi::blob_bytes_handle(&runtime.op_state().borrow());
                // the loader's finished module fetches, for this worker's resource timeline
                runtime.op_state().borrow_mut().put(f.borrow().timings.clone());
            }
            // Publish the isolate handle so the main thread's terminate() can interrupt this worker.
            if let Ok(mut g) = terminate_worker.lock() {
                *g = Some(runtime.v8_isolate().thread_safe_handle());
            }
            if boot.module {
                runtime.op_state().borrow_mut().put(ModuleWorker);
            }
            // Persona-in-worker: the parent hands each real worker its persona (Arc, Send+Sync) so the
            // worker's Web-API values (navigator, ...) read the SAME identity as the window, live from
            // op_state -- exactly the window's `HostPersona` seam, in the other realm. A bare worker
            // (no DOM) gets None and this is skipped.
            // The target this isolate's own requests are reported on.
            runtime.op_state().borrow_mut().put(WorkerReporter(boot.reporter.clone()));
            // The session's farbling seed, so this worker's navigator scalars farble to the
            // same per-origin value the window's do (the worker's navigator getter reads it from OpState).
            // The getter keys on the host from `CookieUrl`; a dedicated worker shares the page's origin,
            // so seed it from `page_url` -- without it the worker would farble under the empty host (or
            // stay static), diverging from the window on the same signal, which is itself a tell.
            if let Some(seed) = boot.session_seed {
                let os = runtime.op_state();
                let mut st = os.borrow_mut();
                st.put(seed);
                if let Some(page) = boot.page_url.as_ref().filter(|u| u.starts_with("http")) {
                    st.put(webapi::CookieUrl(page.clone()));
                }
            }
            if let Some(p) = &persona {
                let state = runtime.op_state();
                let mut st = state.borrow_mut();
                st.put(crate::HostPersona(p.clone()));
                // The persona is also a SERVICE on a kernel `Context`, which
                // is how a `korvus-webapi` table body reads it (`core/navigator`'s scalar getters,
                // `core/html/plugins.rs`). The window gets this from `PersonaPlugin` at engine
                // construction; the worker isolate had `HostPersona` only, so every webapi body that
                // reads the persona as a service answered the persona-less default in this realm.
                // One root Context with the one service the worker has -- the runtime PROVIDES, the
                // Web API READS, in both realms.
                let ctx = spec::kernel::Context::root();
                ctx.provide(spec::PersonaHandle(p.clone()));
                // The rasterizer too: an `OffscreenCanvas` 2D context in a worker drew
                // into nothing, so `getImageData` answered `undefined` where Brave answers pixels.
                // The same plugin the window registers, so both realms rasterize one way.
                let mut loader = spec::kernel::Loader::new();
                if render::wanted() {
                    loader.register(render::RenderPlugin);
                }
                // ...and the page pipeline's services the 2D context reads -- CSS colour and font
                // parsing, the font tables `measureText` measures with -- so a worker's canvas
                // parses `rgba(0,0,0,0)` and measures text the way the window's does.
                if std::env::var("KORVUS_NO_PAGE").as_deref() != Ok("1") {
                    loader.register(page::PagePlugin);
                }
                let _ = loader.apply(&ctx, &spec::kernel::PluginConfig::default());
                st.put(ctx);
            }
            // THE WEB API INSTALL -- ONLY IN A HOST WORKER. The bare messaging-only isolate (`spawn_worker`,
            // `host == false`, tests only) loads no `webapi_core_ext`: no Web API ops and no cppgc classes, so an
            // install there builds a degraded surface. It used to run anyway (this comment called it "a no-op"),
            // and it was not one: it RECORDED templates under the same surface key a real worker uses, so the
            // next real worker in the process was built from the bare one's blueprints -- its MessagePort,
            // BroadcastChannel and PerformanceObserver deliveries never arrived (found by running
            // `worker::tests` single-threaded: 15/15 hung after `worker_bidirectional_echo`). A bare worker's
            // script is `Deno.core.ops.…`; it needs none of this.
            if host {
                let ctx = runtime.main_context();
                v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
                // THE REGISTRY, FIRST. Every worker-exposed interface the capture records
                // that is not a hand table: the constructor, the prototype, the members, the chain
                // (with a phantom parent for a base the realm does not expose), the shape finalize,
                // the WebIDL arity guards and the slot backfill. This was `generated_worker_shells.js`'s
                // opening statement, `op_install_worker_registry()`, and it keeps its position
                // relative to every live install below it -- they all run after it, exactly as they
                // all ran after the ESM layer before.
                // THE BOOTSTRAP, BEFORE THE REGISTRY. `worker-host.js`'s eleven
                // own-global assignments and its two-entry constructor-`name` rename loop:
                // the `new JsNavigator()` instance, the `deno_core` `objects =` class templates
                // (TextEncoder/TextDecoder/DOMQuad/DOMRect/DOMMatrix/DOMPoint) and the native
                // `EventTarget` interface object + prototype the rest of this realm chains onto.
                // FIRST, because it is the slot those statements held: they were the opening of the
                // worker's ESM entry point and nothing ran between them and `install_worker_shells`.
                // The registry below SKIPS a name the global already carries, and the four reconciles
                // VERIFY the classes it just put there, so the position is load-bearing in both
                // directions.
                webapi::install::live::install_worker_bootstrap(scope);
                {
                    let global = scope.get_current_context().global(scope);
                    webapi::install_worker_shells(scope, global);
                }
                // (the `guard_native_class_ctors` sweep stood here, as in the window: no global holds a
                // raw `deno_core` class template any more, so it is deleted.)
                // THE SECOND CALL SITE OF THE WASM STREAMING ROWS, and the reason it is
                // a call site and not a module: `WebAssembly.instantiateStreaming` /
                // `compileStreaming` exist because deno_core leaves V8's WASM streaming host callback
                // UNSET, and V8 invoking it does not throw, it ABORTS THE PROCESS. That callback is
                // unset per `JsRuntime`, and this worker is one -- so the defect was never a property
                // of the window realm, only of the fact that `engine.rs` held the only call.
                // Measured on the one corpus row that reached it -- a page whose wasm runtime
                // instantiates inside a worker: korvus died with SIGABRT and "wasm streaming callback
                // invoked before the JS handler was set", so the row read `nodes: null`, a page that
                // produced no result at all. WHICH row is the harness's business, not the engine's.
                //
                // The SAME function the window calls (invariant I8), not a worker twin: it reads
                // `WebAssembly` off whatever context it runs in and never took a realm argument.
                // Chrome 150's two realms are byte-identical here on all 15 shape fields, which is
                // why the table says `both`.
                //
                // No ordering: `WebAssembly` is a V8 intrinsic that exists from context creation, and
                // nothing else in this realm touches either member. Unconditional -- a BARE worker
                // (`host == false`) runs no page script today, but its isolate has the same unset
                // callback, and an install that cannot abort is cheaper than a reason it will not.
                webapi::install::live::install_wasm_streaming_rows(scope);
                // (The geometry families' four `reconcile_*` verifications stood here: now they are
                // OWN classes, built from their tables at the bootstrap's class slot -- `install::live::geometry`.)
                // `Headers` and `FormData`. This realm had NO native class for either
                // -- both were registry shells whose `get` answered null -- so this call is where the
                // worker gets the real one, from the same table the window reads.
                for d in webapi::install::live::install_fetch_values(scope) {
                    eprintln!("[korvus-worker] one-table drift: {}.{}: {}", d.interface, d.member, d.detail);
                }
                // The event model's four tables (Event, EventTarget, Observable, Subscriber)
                // over the worker's own classes -- the same install the window runs, in this realm.
                webapi::install_worker_event_model(scope);
                // An earlier change created `CustomEvent` here, for a realm that has no such class.
                // W4.b's `install_worker_event_family` (called from `install_worker_event_model`
                // above) now creates it from the same table as the window's, with `Exposed` deciding
                // the rows -- which is what invariant I8 asks for, and one install rather than two.
                // The `[Exposed=(Window,Worker)]` list tables (DOMStringList) -- same table as the window's.
                webapi::install::live::install_worker_collections(scope);
                // The twelve IndexedDB interfaces, from the SAME tables the window uses
                // (invariant I8). They left the generated worker registry with the tables, so this is
                // their only source in this realm -- and the members are the window's real bodies now,
                // where the worker shell gave them stubs.
                webapi::install::live::install_worker_indexeddb(scope);
                // The crypto / messaging / FileReader / XHR-base interfaces, from the
                // SAME tables the window uses (invariant I8). They left the generated worker registry
                // with the tables, so this is their only source in this realm -- and it also re-chains
                // the JS-owned worker `XMLHttpRequest.prototype` onto the real
                // `XMLHttpRequestEventTarget.prototype` the table just built.
                webapi::install::live::install_worker_crypto_xhr(scope);
                // `FileList` -- the one interface of that lane the worker capture lists
                // ([Exposed=(Window,Worker)]), from the SAME table the window uses (invariant I8). It
                // left the generated worker registry with the table, so this is its only source here.
                webapi::install::live::install_worker_datatransfer(scope);
                // The eight abort / blob / fetch / notification interfaces the worker
                // capture lists, from the SAME tables the window uses (invariant I8). They left the
                // generated worker registry with the tables, so this is their only source in this
                // realm -- six replace shape-only shells outright, and `Response` /
                // `PerformanceEntry` take the prototypes of the classes shared-globals.js and
                // worker-host.js still provide.
                webapi::install::live::install_worker_abort_style(scope);
                // `Worker`. Both captures list it, so it exists in this realm too, from the
                // SAME table the window uses (invariant I8). This isolate registers the spawn and
                // receive ops (the host extension above), so a worker's `new Worker(...)` starts a
                // nested worker of its own.
                //
                // A DEDICATED worker's only: `Worker` is `[Exposed=(Window,DedicatedWorker)]` in Blink 150
                // (worker.idl:32). Chrome 150, measured (`tests/fixtures/worker-script-loading`): a shared worker
                // answers `typeof Worker` "undefined"; korvus gave every worker kind one.
                if scope_kind_of(scope) == "dedicated" {
                    webapi::install::live::install_worker_workers(scope);
                }
                // `ServiceWorkerRegistration`, from the SAME table the window uses
                // (invariant I8). Both captures list the interface, so taking it out of the generated
                // registry without this line would DELETE it from the worker surface. The install is
                // filtered to the rows this realm exposes, which drops exactly one -- `cookies`, the
                // window-only member on an `[Exposed=(Window,Worker)]` interface, and the row that
                // needed the `interface!` macro's new per-member exposure word.
                //
                // `ServiceWorker` and `ServiceWorkerContainer` are NOT here: `worker.surface.json`
                // lists neither, and the window-only tables say so.
                webapi::install::live::install_worker_service_workers(scope);
                // `SecurityPolicyViolationEvent` -- the one interface of that lane the
                // worker capture lists, from the SAME table the window uses (invariant I8). It left
                // the generated worker registry with the table, so this is its only source here, and
                // its constructor is the real native one rather than the shape-only shell's.
                webapi::install::live::install_worker_small_elements(scope);
                // The six streams interfaces, from the SAME tables the window uses
                // (invariant I8). `worker.surface.json` lists all six with member lists byte-identical
                // to the window's, and all six left the generated worker registry with the tables, so
                // this is their only source in this realm.
                webapi::install::live::install_worker_streams(scope);
                // `MediaCapabilities` -- the one interface of the media family the
                // worker capture lists (`navigator.mediaCapabilities` is `[Exposed=(Window,Worker)]`),
                // from the SAME table the window uses (invariant I8). The inventory is name-based, so
                // the table left BOTH generated registries; without this call the worker would lose
                // the interface outright. The other twelve are window-only.
                webapi::install::live::install_worker_media(scope);
                // `RTCDataChannel` -- the one interface of the WebRTC/media lane the
                // worker capture lists (`[Exposed=(Window,Worker)]`), from the SAME table the window
                // uses (invariant I8). Its worker member list is byte-identical to the window's, and
                // the name-based inventory took it out of BOTH generated registries, so without this
                // call the worker would lose the interface outright. The other twelve are window-only.
                webapi::install::live::install_worker_media_rtc(scope);
                // `PerformanceObserver` + `PerformanceObserverEntryList` -- the two
                // interfaces of that lane the worker capture lists ([Exposed=(Window,Worker)]), from
                // the SAME tables the window uses (invariant I8). They left the generated worker
                // registry with the tables and `worker-host.js`'s duplicate installer is deleted, so
                // this is their only source in this realm; it also binds the two bridge names the
                // worker's own performance timeline reads (`__poTypes` / `__poCb`).
                webapi::install::live::install_worker_observers(scope);
                // `URLSearchParams` (and `URL`) from the SAME tables the window uses
                // (invariant I8). `worker.surface.json` lists both with member lists byte-identical to
                // the window's, and both left the generated worker registry with the tables. Before
                // this call the worker's were the registry's D2 stubs -- measured: every read answered
                // `undefined` -- so this is the first realm build in which they work in a worker.
                webapi::install::live::install_worker_url(scope);
                // `TrustedTypePolicyFactory`, `TrustedTypePolicy`, `TrustedHTML`,
                // `TrustedScript` and `TrustedScriptURL` -- the five interfaces behind
                // `window.trustedTypes`, all `[Exposed=(Window,Worker)]` and all five in every
                // `worker.surface.json` with member lists byte-identical to the window's, from the
                // SAME tables (invariant I8). Their tables took them out of BOTH generated
                // registries, so without this call the worker would lose all five outright. The
                // `trustedTypes` SINGLETON is window-only: no worker capture records it as an own
                // global, because in a worker it is a `WorkerGlobalScope` prototype attribute.
                webapi::install::live::install_worker_trusted_types(scope);
                // `eval(trustedScript)` in a worker runs the script, as in the window (H17):
                // the code-generation hook is per ISOLATE, and a worker has its own.
                webapi::install_codegen_hook(scope);
                // The creator's cross-origin isolation (SharedArrayBuffer, crossOriginIsolated).
                webapi::apply_worker_isolation(scope, boot.isolated);
                // `Scheduler`, `[Exposed=(Window,Worker)]` and in every
                // `worker.surface.json` with the window's two members, from the SAME table. Its
                // `scheduler` SINGLETON is window-only for the same reason `trustedTypes` is.
                webapi::install::live::install_worker_scheduler(scope);
                // `CanvasGradient`, `CanvasPattern` and `Path2D` -- the three
                // interfaces of the canvas lane the worker capture lists ([Exposed=(Window,Worker)];
                // an `OffscreenCanvas` 2D context in a worker hands back gradients and patterns and
                // takes `Path2D`s), from the SAME tables the window uses (invariant I8). Their member
                // lists are byte-identical to the window's, and the name-based inventory took all
                // three out of BOTH generated registries, so without this call the worker would lose
                // them outright. `CanvasRenderingContext2D` is window-only in the capture.
                webapi::install::live::install_worker_canvas(scope);
                // The WebGL half: `WebGLRenderingContext`,
                // `WebGL2RenderingContext`, `WebGLActiveInfo` and `WebGLShaderPrecisionFormat`, from
                // the SAME tables the window uses (invariant I8). All four are in
                // `worker.surface.json` with member lists byte-identical to the window's, and the
                // name-based inventory took them out of BOTH generated registries. The four
                // oracle-backed reads answer nothing here, exactly as their worker shell stubs did:
                // `wire_canvas_oracle` never ran in a worker, so there is no device oracle to
                // read and their `data` is `undefined`.
                webapi::install::live::install_worker_webgl(scope);
                // The device oracle in the worker too, so OffscreenCanvas.getContext("webgl")
                // answers the persona's real VENDOR/RENDERER (ANGLE/SwiftShader) in a worker as the window
                // does -- without it `oracle_has_webgl` is false, the context is null, and a worker WebGL
                // probe reads "NA", a bot tell (deviceandbrowserinfo). The comment above this used to note
                // the worker had no oracle "and that is not a gap this lane opens"; C13 opens it, from the
                // SAME `device_oracle_json` the window realm wires (invariant I8).
                if let Some(p) = &persona {
                    webapi::install::live::wire_worker_canvas_oracle(scope, &p.device_oracle_json());
                }
                // The WebGPU half: the six interfaces of that family this engine can
                // honestly answer, from the SAME tables the window uses (invariant I8). All six are
                // in `worker.surface.json` with member lists byte-identical to the window's, and the
                // name-based inventory took them out of BOTH generated registries. None of the six
                // reads a device oracle, so the worker gets the window's real answers where it used
                // to get shell stubs. After `install_worker_event_model` above, because
                // `GPUUncapturedErrorEvent` splices onto `Event.prototype`.
                webapi::install::live::install_worker_webgpu(scope);
                // The performance TIMELINE, from the SAME tables the window uses
                // (invariant I8) with the WORKER capture's thirteen-row selector for `Performance`.
                // `worker-host.js`'s ~95-line copy of host.js's builders is deleted with it, and this
                // realm gets its OWN `Performance` instance (`worker.performance !== page.performance`
                // is spec, and the `performance-worker` fixture asserts it).
                //
                // AFTER `install_worker_observers`, because a `PerformanceObserver` constructed by a
                // page script binds `globalThis.performance` at construct time and this is what puts
                // it there; and after the whole ESM layer, because `worker-finalize.js` reparents the
                // global and sweeps its own keys during it -- the `performance` accessor this install
                // defines on `WorkerGlobalScope.prototype` is the one that file used to define from a
                // JS object captured before the sweep.
                webapi::install::live::install_worker_performance(scope);
                // THE WORKER GLOBAL SCOPE, from its tables
                // (`core/korvus-webapi/src/core/workers/worker_global_scope.rs`): `WorkerLocation`
                // and this isolate's instance, `WorkerGlobalScope` spliced onto `EventTarget`, the
                // most-derived scope for this isolate's KIND (dedicated / shared / service) with its
                // `[Global]` members on the global object, the CHAIN, and the own-copy sweep.
                //
                // LAST of the interface installs, and it has to be: it reads `EventTarget` off the
                // global (so after `install_worker_event_model`), it publishes nothing itself but
                // exposes what the five singleton installs above published, and it REPARENTS the
                // global — which ruling R9 says happens once, natively, before any script runs.
                // `install_worker_navigator` still runs after it, because the surface AUDIT has to be
                // the last thing this realm does (W7b).
                let global = scope.get_current_context().global(scope);
                webapi::install_worker_global_scope(scope, global);
            }
            // Phase 3 (Part 2) worker-navigator: build the worker `WorkerNavigator` from the one-table
            // NAVIGATOR (worker selector) and point the worker's `navigator` at it -- exactly the
            // window's `install_window_navigator`, in the other realm. ONLY in a real DOM worker: the
            // persona is present (its scalar getters read HostPersona) and `install_worker_bootstrap`
            // created the `navigator` instance. A bare worker (None) has neither, so this is skipped
            // and the worker exposes no navigator, as before.
            if persona.is_some() {
                let ctx = runtime.main_context();
                v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_worker_navigator(scope, global);
            }
            // THE `Deno` STRIP, AND WHY IT IS HERE RATHER THAN IN THE ESM TAIL.
            //
            // `worker-host.js` ended with `op_stash_bridge(KB); op_stash_ops(); delete globalThis.Deno;
            // delete globalThis.__bootstrap;` -- the stash-and-hide the WINDOW has done from Rust since
            // W4.f (`engine.rs` calls the same `webapi::hide_deno`). One function, both realms
            // (invariant I8), and the two stash ops it replaces had no other caller and are deleted.
            //
            // It runs LAST, where the JS ran FIRST relative to every Rust install, and the two are
            // equivalent because `binding::hidden::ops_object` answers from the live `Deno` before this
            // point and from the Private slot after: the installs in between reach the same object
            // either way, and the bridge holder is found by its symbol until it is stashed. Nothing
            // between `JsRuntime::new` and here can observe `Deno`, because nothing but this block
            // runs -- the worker's own script is `execute_script` below, after the strip, exactly as
            // before. What the move BUYS is the line above it in `install_worker_bootstrap`:
            // `registerErrorBuilder` needs `Deno` alive, and that is the whole of the ceiling the JS
            // file claimed for `op_register_dom_exception_builders` -- a three-line wrapper since
            // deleted, its work being `dom_exception::register_error_builders`, which was always Rust.
            //
            // `host` is false for the BARE messaging-only isolate (`spawn_worker`), whose whole source
            // is `Deno.core.ops.…`: it never loaded the ESM layer, so it never had the strip either.
            if host {
                let ctx = runtime.main_context();
                v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
                // THE THREE CROSS-ISOLATE RECEIVE LOOPS, and the two bridge functions the
                // injected boot scripts call. `worker-host.js`'s three `async` IIFEs, in Rust
                // (`worker_recv.rs`), armed here rather than during the ESM evaluation. The moment is
                // equivalent: all three ops are `async(lazy)`, nothing polls their futures until
                // `run_event_loop` below, and the worker's own script is still the `execute_script`
                // after this. BEFORE `hide_deno`, which stashes the bridge holder the hooks are on.
                crate::worker_recv::arm_receive_loops(scope);
                // The SW half of the `korvus-sw-fetch` transport, in the ServiceWorker
                // realm only. It was the first half of `SW_PRELUDE`, a JS string prepended to the
                // worker's own source, so it used to run at the START of `execute_script` below; here
                // it runs a few statements earlier, which cannot be observed -- nothing is delivered
                // on a BroadcastChannel until the event loop runs, and that is after `execute_script`.
                // BEFORE `hide_deno` for the same reason the bridge hooks are: this touches the global.
                if scope_kind_of(scope) == "service" {
                    webapi::install_sw_fetch_transport(scope);
                }
                // HTML's "notify about rejected promises" in this realm too: an unhandled rejection fires
                // `unhandledrejection` at the worker's global and the worker KEEPS RUNNING. Only the window
                // installed the handler, so deno_core's default made a worker's unhandled rejection a fatal
                // event-loop error: the worker died, its timers and message handlers with it (Chrome 150,
                // measured: `Promise.reject(...)` in a worker, listened or not, and a timer after it fires).
                // It is also how a module worker's own throw is REPORTED (`run_worker_module`). BEFORE
                // `hide_deno`: it is set through `Deno.core`.
                webapi::install_rejection_handler(scope);
                webapi::hide_deno(scope);
                // The worker realm's half of the prototype seal. `engine.rs` does this
                // at the end of the window's install block; here the end of the install block IS
                // this `if host`, and `execute_script("worker", source)` two lines down is the
                // worker's own code. Same function, both realms (invariant I8). AFTER `hide_deno`
                // only because that is the last statement that touches the global -- the seal reads
                // it, it does not care what `Deno` was.
                // The same last step of the registry walk the window takes, both realms.
                // The `constructor` seat pass (`seat_constructors_in_capture_order`, which
                // deleted from the window realm too) was never called here: MEASURED, with
                // the worker artifacts recording `ctorIndex` it moved nothing -- the creates already place
                // `constructor` from this realm's capture. The day a worker prototype gains members after
                // its `constructor`, `persona_surface.rs`'s four-bundle live check goes red and names it.
                webapi::install::installer::seat_globals_in_capture_order(scope);
                webapi::seal_interface_protos(scope);
                // What this worker's install recorded, as a blueprint file for the first
                // worker of the next process of this binary (a no-op unless the binary switched the disk
                // cache on, and unless something new was recorded). After the last install, as the
                // window realm's is.
                webapi::persist_templates(scope);
            }
            // a `blob:` / `data:` script's realm holds its creator's policies, before a line of it runs
            if boot.load.is_none() && !boot.csp_headers.is_empty() {
                set_worker_csp(&mut runtime, &boot.csp_headers);
            }
            // A dedicated worker that is a DevTools target takes its session's `Runtime.*` commands on this thread
            // (`worker_devtools`), and its console goes to that session -- from its first statement.
            let devtools = boot.reporter.target.clone().filter(|_| host && boot.destination == "worker");
            let mut commands = devtools.as_deref().map(crate::worker_devtools::register);
            if let Some(t) = &devtools {
                webapi::devtools_log::set_worker_target(Some(t.clone()));
            }
            // however the worker ends, its target takes no more commands
            let _devtools_ends = crate::worker_devtools::Guard(devtools.clone());
            // B3/B4 (ABI 3): the loaded plugins' worker bindings and their `init_scripts(REALM_*_WORKER)`
            // run here, before the worker's own top-level script -- the pre-script point for the worker
            // realms (the page's is in webapi's `run_doc_scripts` and `navigation.rs`). Only a real worker
            // host has a WorkerGlobalScope to install them on; a no-plugin run adds and runs nothing (I9/J2).
            if host {
                install_worker_plugin_env(&mut runtime);
            }
            let top = match loaded {
                // A module worker whose script is in hand (a `blob:`/`data:` URL): evaluated as a module.
                None if boot.module => run_worker_module(&mut runtime, &boot.main_url, source).await,
                // A classic worker's script runs under ITS URL, as Chrome's does: its stack frames read
                // `at f (blob:https://site/<uuid>:1:23)` where they read `at f (worker:1:23)`, and its
                // `import()` resolves against it (Chrome 150, measured, a blob and a network worker).
                None => run_worker_classic(&mut runtime, &script_name(&boot.main_url), &source),
                // A URL-loaded script, fetched before the realm was made: run under its response URL.
                Some(script) => {
                    // its realm's policies are its response's (HTML: a worker's policy container from its script)
                    set_worker_csp(&mut runtime, &script.headers);
                    // A DEDICATED worker's script is its parent document's resource (measured: `w.js`,
                    // `initiatorType` `other`, on the page's timeline); the others are not.
                    if boot.destination == "worker" {
                        if let Ok(bytes) = serde_json::to_vec(&script.timing) {
                            let _ = timing_out_tx.send(bytes);
                            wake_page();
                        }
                    }
                    if boot.module {
                        run_worker_module(&mut runtime, &script.url, script.code).await
                    } else {
                        run_worker_classic(&mut runtime, &script_name(&script.url), &script.code)
                    }
                }
            };
            if top == TopLevel::NotLoaded {
                // Nothing ran, and nothing will: the creator's `Worker` gets its `error` Event and this
                // worker ends.
                send_error_frame(&runtime.op_state().borrow(), &webapi::WorkerErrorFrame::not_loaded());
                return;
            }
            if top == TopLevel::Terminated {
                return;
            }
            after_top_level(&mut runtime, top == TopLevel::Threw);
            report_loaded_surface(&mut runtime);
            if let Some(t) = &devtools {
                crate::worker_devtools::note_script_loaded(t);
            }
            match commands.as_mut() {
                // the loop, with the DevTools session's commands answered between its turns
                Some(rx) => {
                    let mut objects = crate::worker_devtools::Objects::new();
                    loop {
                        tokio::select! {
                            biased;
                            cmd = rx.recv() => match cmd {
                                Some(c) => crate::worker_devtools::handle(&mut runtime, &mut objects, c).await,
                                None => {
                                    let _ = runtime.run_event_loop(PollEventLoopOptions::default()).await;
                                    break;
                                }
                            },
                            _ = runtime.run_event_loop(PollEventLoopOptions::default()) => break,
                        }
                    }
                }
                None => {
                    let _ = runtime.run_event_loop(PollEventLoopOptions::default()).await;
                }
            }
        });
    });
    WorkerHandle {
        outbound: Arc::new(TMutex::new(out_rx)),
        inbound: in_tx,
        bc_outbound: Arc::new(TMutex::new(bc_out_rx)),
        bc_inbound: bc_in_tx,
        port_outbound: Arc::new(TMutex::new(port_out_rx)),
        port_inbound: port_in_tx,
        err_outbound: Arc::new(TMutex::new(err_out_rx)),
        timing_outbound: Arc::new(TMutex::new(timing_out_rx)),
        terminate,
        start,
        ended,
    }
}

/// The name a classic worker's script is compiled under: its URL, or `worker` for a bare isolate that has none.
fn script_name(url: &str) -> String {
    if url.is_empty() { "worker".to_string() } else { url.to_string() }
}

/// How a worker's top-level script ended.
#[derive(PartialEq, Eq, Debug)]
enum TopLevel {
    /// It ran to the end.
    Ran,
    /// It threw while running -- already REPORTED in the worker's own realm ([`run_worker_classic`], or
    /// the realm's rejection handler for a module's throw), and the worker keeps running.
    Threw,
    /// It could not be fetched, parsed or linked: the creator's `Worker` gets a plain `error` `Event`, and
    /// nothing runs. Chrome 150, measured: a 404 classic or module script, a module with a syntax error,
    /// a module whose import 404s -- `Event`, not `ErrorEvent`, `cancelable` true. A classic script's
    /// syntax error is NOT this: it is [`TopLevel::Threw`], an `ErrorEvent` with its line.
    NotLoaded,
    /// `terminate()` interrupted it: nothing is reported, and nothing more runs.
    Terminated,
}

/// Run a classic worker script under its URL. A throw -- a syntax error included -- is REPORTED in the
/// worker's own realm, as HTML's "run a classic script" does for a worker's script: its `onerror` and
/// `error` listeners first, then, when nothing handled it, its creator's `Worker` object; and the worker
/// keeps running. Chrome 150, measured: a timer armed before a top-level throw still fires and a message
/// handler still answers, and a syntax error reaches the page as `ErrorEvent("Uncaught SyntaxError:
/// Unexpected token ';'", <URL>, 1, 9)`. deno_core's `execute_script` handed the error to a caller that
/// dropped it for a `blob:` worker's script and passed its stack text, as a message with no site, for a
/// fetched one -- and neither worker ran another task.
fn run_worker_classic(runtime: &mut JsRuntime, url: &str, code: &str) -> TopLevel {
    let ctx = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
    match crate::script_exec::run_worker_script(scope, code, url) {
        (_, true) => TopLevel::Terminated,
        (Some((err, site)), false) => {
            let err = v8::Local::new(scope, &err);
            crate::script_exec::report(scope, err, site);
            TopLevel::Threw
        }
        (None, false) => TopLevel::Ran,
    }
}

/// Under `KORVUS_TPL_REPORT` (a no-op otherwise): the worker's surface walked again once its
/// top-level script -- and [`after_top_level`] after it -- has run (`worker-loaded`), as the page's is after
/// its `load`.
/// Load and evaluate a module worker's main script under its own URL: its static imports
/// are fetched through the runtime's module loader first, then the module body runs. A failed fetch, a
/// syntax error or a failed link anywhere in the graph is [`TopLevel::NotLoaded`]; a throw while it
/// runs is [`TopLevel::Threw`], reported in the worker's realm.
async fn run_worker_module(runtime: &mut JsRuntime, url: &str, code: String) -> TopLevel {
    let Ok(spec) = deno_core::ModuleSpecifier::parse(url) else { return TopLevel::NotLoaded };
    let Ok(id) = runtime.load_main_es_module_from_code(&spec, code).await else { return TopLevel::NotLoaded };
    // A module's own throw is re-raised by deno_core through a promise it derives here; the hook around this
    // one call is how the realm's rejection handler tells that promise from a page's own rejection and
    // REPORTS the throw (`webapi::core::events::report::ModuleRethrows`, as the window's
    // `run_started_module`). Chrome 150, measured: a module worker's top-level throw and an imported
    // module's both reach the worker's `onerror` and the page's `Worker` as `ErrorEvent`s naming the module
    // that threw, and the worker keeps running; korvus delivered neither.
    {
        let ctx = runtime.main_context();
        v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
        webapi::begin_module_evaluation(scope);
    }
    let evaluated = runtime.mod_evaluate(id);
    {
        let ctx = runtime.main_context();
        v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
        webapi::end_module_evaluation(scope);
    }
    // The evaluation settles only while the event loop turns (a top-level `await`, deno_core's own
    // bookkeeping), so it is polled together with the loop.
    match runtime.with_event_loop_future(Box::pin(evaluated), PollEventLoopOptions::default()).await {
        Ok(()) => TopLevel::Ran,
        Err(_) => TopLevel::Threw,
    }
}

fn report_loaded_surface(runtime: &mut JsRuntime) {
    let ctx = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
    webapi::report_loaded_surfaces(scope);
}

/// Bare worker isolate: `source` runs with ONLY the messaging ops (no worker global, no BroadcastChannel).
/// The low-level messaging unit tests use this; the BC channel ends go unused.
#[cfg(test)]
pub fn spawn_worker(source: String) -> WorkerHandle {
    spawn_on_thread(source, String::new(), None, false, WorkerBoot::default(), |ch, _| vec![korvus_worker_ext::init(ch.out_tx, ch.in_rx)])
}

/// Real Worker isolate: loads `webapi_core_ext` (the SAME native SubtleCrypto/... cppgc classes as the
/// window) + the messaging ops + the `worker-host.js` scope (incl. a native cross-isolate BroadcastChannel),
/// then runs the user `source`. This backs `op_worker_spawn` (the native `class Worker`). `location` is the
/// worker's own script URL (W1.6), backing `HostLocation` for `fetch()`/`importScripts` resolution.
///
/// `#[cfg(test)]` like [`spawn_worker`] above it, and for the same reason: every caller
/// is in this file's own `mod tests`. `op_worker_spawn` — the real `class Worker` — calls
/// [`spawn_real_worker_persona`] directly, because it has a persona to pass.
#[cfg(test)]
pub fn spawn_real_worker(source: String, location: String) -> WorkerHandle {
    spawn_real_worker_persona(source, location, None, WorkerBoot::default())
}

/// As [`spawn_real_worker`], but seeds the worker's `op_state` with the parent's persona so the
/// worker's `navigator` reads the same identity as the window. `op_worker_spawn` passes `Some(...)`;
/// the direct callers (tests) use [`spawn_real_worker`] which passes `None`.
pub(crate) fn spawn_real_worker_persona(source: String, location: String, persona: Option<std::sync::Arc<dyn spec::Persona>>, boot: WorkerBoot) -> WorkerHandle {
    let site_for_cookies = boot.site_for_cookies.clone();
    let top_level_url = boot.top_level_url.clone();
    let name = boot.name.clone();
    spawn_on_thread(source, location, persona, true, boot, move |ch, location| {
        vec![
            webapi::webapi_core_ext::init(),
            korvus_worker_ext::init(ch.out_tx, ch.in_rx),
            korvus_worker_host_ext::init(ch.bc_out_tx, ch.bc_in_rx, ch.port_out_tx, ch.port_in_rx, ch.err_out_tx, location, "dedicated".to_string(), name, String::new(), site_for_cookies, top_level_url),
        ]
    })
}

// ---- W1.4: main-side registry + ops + native Worker methods (kills the LIES) ----

/// The main isolate's table of live workers. Stored in the main `OpState` (`korvus_ext` state); the worker
/// isolates never see it (only bytes cross a channel).
#[derive(Default)]
pub struct WorkerRegistry {
    next: u32,
    /// W3b: monotonic allocator for cross-isolate MessagePort channel ids (main-side; direction main->worker).
    port_next: u32,
    map: HashMap<u32, WorkerHandle>,
}

/// A registry that goes away takes its workers with it: a worker's runtime ending (it closed, was
/// terminated, or its own creator went) ends the workers IT created -- HTML's "terminate a worker" runs
/// for a worker's dependents. Dropping the handles closes their channels, which stops an idle worker;
/// `terminate_execution` stops a busy one (a proof-of-work loop never reads its channel).
impl Drop for WorkerRegistry {
    fn drop(&mut self) {
        for h in self.map.values() {
            if let Ok(g) = h.terminate.lock() {
                if let Some(handle) = g.as_ref() {
                    handle.terminate_execution();
                }
            }
        }
    }
}

impl WorkerRegistry {
    /// Is any worker this page spawned still running? `Engine::pump` asks, because a page with a live
    /// worker is NOT an idle page however empty deno_core's own queues look: the worker's four receive
    /// promises are unref'd, so another thread can still deliver into this realm. An entry leaves this
    /// map when `Worker.terminate()` drops its handle.
    pub fn has_live_worker(&self) -> bool {
        !self.map.is_empty()
    }
}

/// Encode a `Worker`/`self` postMessage frame that may carry transferred MessagePorts: `[u32 n_ports][u32
/// channel_id * n_ports][structured-clone data bytes]`. The receiver splits the header, reconstructs a
/// MessagePort per id (delivered as `MessageEvent.ports`), and deserializes the remaining data bytes.
pub fn msg_encode(data: &[u8], port_ids: &[u32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + port_ids.len() * 4 + data.len());
    v.extend_from_slice(&(port_ids.len() as u32).to_le_bytes());
    for id in port_ids {
        v.extend_from_slice(&id.to_le_bytes());
    }
    v.extend_from_slice(data);
    v
}

/// The BroadcastChannel inboxes of every live worker, so a MAIN (page) BroadcastChannel post can be fanned
/// out to all workers. Held behind a plain `std::sync::Mutex` because the main `BcForward` hook (below) is a
/// `Fn` called synchronously from `cb_bc_post_message` and can't borrow the `OpState` it runs inside.
struct WorkerBcInboxes(Arc<std::sync::Mutex<Vec<tmpsc::UnboundedSender<Vec<u8>>>>>);

/// Install the MAIN side's cross-isolate BroadcastChannel plumbing into the page `OpState`: the shared
/// inbox list + a `webapi::BcForward` hook that fans a page BC post out to every worker. Called from
/// `korvus_ext`'s state init. (`op_worker_spawn` pushes each new worker's inbox here.)
pub fn install_main_bc(state: &mut OpState) {
    let inboxes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let inboxes_fwd = inboxes.clone();
    state.put(WorkerBcInboxes(inboxes));
    state.put(webapi::BcForward(Box::new(move |name, bytes| {
        let frame = bc_encode(&name, &bytes);
        if let Ok(list) = inboxes_fwd.lock() {
            for tx in list.iter() {
                let _ = tx.send(frame.clone());
            }
        }
    })));
}

/// The MAIN page's routing table for cross-isolate MessagePort channels: channel id -> the `port_inbound`
/// of the worker that holds the paired endpoint. Populated when a port is TRANSFERRED to a worker
/// (`cb_worker_post`/`op_worker_self_post`), read by the main `PortForward` to route a remote port's post
/// 1:1 to the right worker. Plain `std::sync::Mutex` because the `Fn` hook can't borrow the `OpState`.
pub struct WorkerPortRoutes(pub Arc<std::sync::Mutex<HashMap<u32, tmpsc::UnboundedSender<Vec<u8>>>>>);

/// Install the MAIN side's cross-isolate MessagePort plumbing: the channel-id -> worker route table + a
/// `webapi::PortForward` hook that routes a page-side remote port's post to the owning worker (1:1 by id).
/// Called lazily on the first worker spawn (mirrors `install_main_bc`).
pub fn install_main_port(state: &mut OpState) {
    let routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let routes_fwd = routes.clone();
    state.put(WorkerPortRoutes(routes));
    state.put(webapi::PortForward(Box::new(move |id, bytes| {
        let frame = port_encode(id, &bytes);
        if let Ok(map) = routes_fwd.lock() {
            if let Some(tx) = map.get(&id) {
                let _ = tx.send(frame);
            }
        }
    })));
}

/// Build a genuine native function (a raw v8 callback) with the correct `fn.name`/`fn.length` set, so it is
/// indistinguishable from an op2-generated binding on the surface (name/length, not just `[native code]`).
/// Mirrors `webapi::build_fn`; engine can't reach that private helper, and it is five lines.
pub(crate) fn build_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    name: &str,
    length: i32,
) -> v8::Local<'s, v8::Function> {
    // ConstructorBehavior::Throw -> a "concise" native function with NO own `.prototype` (real DOM methods
    // carry none; the default Allow would add `.prototype`, a JS-shim tell).
    let f = v8::Function::builder(webapi::binding::refs::f(cb))
        .length(length)
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .build(scope)
        .unwrap();
    let n = v8::String::new(scope, name).unwrap();
    f.set_name(n);
    f
}

/// Read the worker isolate's id off a `Worker` instance receiver.
///
/// A v8 PRIVATE, not the non-enumerable own `__id` the `host.js` class defined. Chrome
/// answers `[]` for `Object.getOwnPropertyNames(new Worker(u))` and korvus answered `["__id"]` --
/// `enumerable: false` hid it from `Object.keys` and from nothing else. The name is the table's
/// (`webapi::core::workers::worker::ID_SLOT`) so the writer and this reader cannot drift.
fn worker_this_id(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    let name = v8::String::new(scope, webapi::worker_id_slot())?;
    let key = v8::Private::for_api(scope, Some(name));
    this.get_private(scope, key).filter(|v| !v.is_undefined()).and_then(|v| v.number_value(scope)).map(|n| n as u32)
}

/// `Worker.prototype.postMessage(data)` — native `[native code]`. Structured-clone-serializes `data` on the
/// main isolate and ships the bytes into the worker's inbound channel.
fn cb_worker_post(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = match worker_this_id(scope, args.this()) {
        Some(id) => id,
        None => return,
    };
    let data = args.get(0);
    let transfer = args.get(1); // W3: transferables (ArrayBuffers detach; MessagePorts re-home)
    // W3b: scan the transfer list for MessagePorts (main -> worker transfer). Each transferred port's
    // same-isolate PEER becomes the main-side cross-isolate endpoint for a fresh channel id (the transferred
    // port itself is neutered); the worker reconstructs a new port for that id, delivered as MessageEvent.ports.
    let mut ports: Vec<v8::Local<v8::Value>> = Vec::new();
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(transfer) {
        for i in 0..arr.length() {
            if let Some(item) = arr.get_index(scope, i) {
                if webapi::is_message_port(scope, item) {
                    ports.push(item);
                }
            }
        }
    }
    // Allocate a channel id per transferred port + fetch the target worker's message + port inboxes.
    let (msg_inbound, port_inbound, port_ids) = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let reg = st.borrow_mut::<WorkerRegistry>();
        let (mi, pi) = match reg.map.get(&id) {
            Some(h) => (Some(h.inbound.clone()), Some(h.port_inbound.clone())),
            None => (None, None),
        };
        let mut ids = Vec::with_capacity(ports.len());
        for _ in &ports {
            reg.port_next += 1;
            ids.push(reg.port_next);
        }
        (mi, pi, ids)
    };
    if !port_ids.is_empty() {
        // Route each channel id -> the target worker's port inbox (so the main endpoint's posts reach it).
        if let Some(pi) = &port_inbound {
            let op_state = JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            if let Some(routes) = st.try_borrow::<WorkerPortRoutes>() {
                if let Ok(mut map) = routes.0.lock() {
                    for cid in &port_ids {
                        map.insert(*cid, pi.clone());
                    }
                }
            }
        }
        // Mark each transferred port's PEER as the main-side endpoint for its channel + neuter the port.
        for (p, cid) in ports.iter().zip(port_ids.iter()) {
            if let Some(peer) = webapi::mp_take_peer(scope, *p) {
                webapi::mp_register_remote(scope, peer, *cid);
            }
        }
    }
    if !webapi::refuse_platform_objects(scope, data, "Worker", "postMessage") {
        return; // a platform object that cannot be cloned
    }
    // The ports' places inside the message are written as their transfer indices: the worker
    // answers each with the port it receives, so `e.data.port` IS a port.
    let port_objs: Vec<v8::Local<v8::Object>> = ports.iter().filter_map(|p| v8::Local::<v8::Object>::try_from(*p).ok()).collect();
    let bytes = webapi::wire_serialize(scope, data, &port_objs);
    let frame = msg_encode(&bytes, &port_ids);
    if let Some(mi) = msg_inbound {
        let _ = mi.send(frame);
    }
    apply_transfer(scope, transfer);
}

/// `Worker.prototype.terminate()` — native `[native code]`. Drops the worker's inbound sender, so the
/// worker's `op_worker_self_recv` resolves empty, its receive loop breaks, its event loop drains, and the
/// isolate/thread exit. Idempotent (a second call finds no entry). (Interrupting a busy-looping worker via
/// `isolate.terminate_execution()` is W1.6.)
fn cb_worker_terminate(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = match worker_this_id(scope, args.this()) {
        Some(id) => id,
        None => return,
    };
    let op_state = JsRuntime::op_state_from(scope);
    let mut state = op_state.borrow_mut();
    if let Some(reg) = state.try_borrow_mut::<WorkerRegistry>() {
        if let Some(h) = reg.map.remove(&id) {
            // Interrupt a BUSY worker (terminate_execution); dropping `h` then closes its channels so an
            // idle worker's receive loops break and its thread exits too.
            if let Ok(g) = h.terminate.lock() {
                if let Some(handle) = g.as_ref() {
                    handle.terminate_execution();
                }
            }
        }
    }
}

/// Spawn a real worker isolate and register it; returns the id the `class Worker` stores as `__id`. The
/// worker runtime loads `worker-host.js` (via `spawn_real_worker`), so the user script sees the full worker
/// scope (`self`/`postMessage`/`onmessage`/`crypto`/...). `source` is the LITERAL script text host.js
/// already resolved (the `__workerScripts`/Blob: seam) -- empty when `url` didn't resolve there, in which
/// case the worker loads `url` itself on its own thread (`load_worker_script`; the JS boot fetch W1.6 injected
/// was deleted). `client` is the creating document's URL when that is not this realm's own
/// `HostLocation` -- a frame's (empty: the realm's own).
#[op2(fast)]
pub fn op_worker_spawn(state: &mut OpState, #[string] source: String, #[string] url: String, #[string] kind: String, isolated: bool, #[string] site_for_cookies: String, #[string] csp: String, #[string] name: String, #[string] client: String) -> f64 {
    // Hand the worker the window's persona so its navigator reads the same identity (persona-in-worker).
    let persona = state.try_borrow::<crate::HostPersona>().map(|h| h.0.clone());
    // A `worker` target; its script is requested by the page and answered on the worker.
    let target = announce_target("worker", &worker_location(&url));
    let mut boot = worker_boot(state, &source, &url, "worker", transport::Reporter { target: None, response_target: Some(target.clone()) }, target);
    boot.module = kind == "module";
    boot.isolated = isolated;
    boot.site_for_cookies = site_for_cookies;
    boot.csp_headers = serde_json::from_str(&csp).unwrap_or_default();
    boot.name = name;
    // the worker begins when its creator's task is over: the creating realm opens the gate ([`op_worker_start`])
    boot.start = Some(Arc::new(WorkerStart::default()));
    // The top-level document: the page's own (a frame's realm shares the page's `HostLocation`), or, for a worker
    // a worker started, the one its creator was given.
    boot.top_level_url = match state.try_borrow::<WorkerScope>() {
        Some(w) => w.top_level_url.clone(),
        None => state.try_borrow::<HostLocation>().map(|l| l.get()).unwrap_or_default(),
    };
    // The script's request is its creator's (Chrome 150, measured): a cross-site frame's worker asks
    // for it as the FRAME -- `Sec-Fetch-Site: same-origin`, the frame's `Referer` -- in the frame's network isolation
    // and cookie context, so it is read from the HTTP cache the frame's own fetch filled. Here it asked as the page
    // (`cross-site`, the page's Referer) under the page's partition, and downloaded it again.
    if let Some(load) = boot.load.as_mut() {
        if !client.is_empty() {
            (load.initiator, load.no_referrer) = request_client(&client);
        }
        load.top_level_url = Some(boot.top_level_url.clone()).filter(|u| !u.is_empty());
        let page = load.initiator.clone().unwrap_or_default();
        load.cookie_cross_site = transport::cookie_cross_site(&boot.site_for_cookies, &page, &url);
        // a module script is checked strictly, whoever made the worker (`ModuleScriptFetcher`)
        if boot.module {
            load.check = Some(webapi::page::ScriptFlavor::Module);
        }
    }
    let handle = spawn_real_worker_persona(source, worker_location(&url), persona, boot);
    // A NESTED worker (spawned by a worker) keeps its creator's own BroadcastChannel and MessagePort
    // plumbing: the MAIN relays below are the page's, and installing them in a worker would replace the
    // forward hooks that carry that worker's own traffic to the page.
    let in_worker = state.try_borrow::<WorkerScope>().is_some();
    // Lazily install the MAIN cross-isolate BC plumbing on the FIRST worker spawn, so a pure-window session
    // (no workers) keeps a BroadcastChannel path with no forward hook (byte-identical to before).
    if !in_worker && state.try_borrow::<WorkerBcInboxes>().is_none() {
        install_main_bc(state);
    }
    // W3b: install the MAIN MessagePort routing table + PortForward on the first spawn too (routes stay empty
    // until a port is actually transferred to a worker).
    if !in_worker && state.try_borrow::<WorkerPortRoutes>().is_none() {
        install_main_port(state);
    }
    // Register the worker's BroadcastChannel inbox so a page BC post reaches it.
    if let Some(inboxes) = state.try_borrow::<WorkerBcInboxes>() {
        if let Ok(mut list) = inboxes.0.lock() {
            list.push(handle.bc_inbound.clone());
        }
    }
    let reg = state.borrow_mut::<WorkerRegistry>();
    reg.next += 1;
    let id = reg.next;
    reg.map.insert(id, handle);
    id as f64
}

/// Let worker `id` begin ([`WorkerStart`]): its creator's task is over.
#[op2(fast)]
pub fn op_worker_start(state: &mut OpState, #[smi] id: u32) {
    if let Some(gate) = state.try_borrow::<WorkerRegistry>().and_then(|r| r.map.get(&id)).and_then(|h| h.start.clone()) {
        gate.open();
    }
}

// ---- W4: SharedWorker (a keyed singleton worker isolate + a `connect` event per connection) ----

/// The MAIN page's table of live SharedWorkers, keyed by `name\0url` -> the worker id in `WorkerRegistry`.
/// A repeated `new SharedWorker(url, name)` with the same key reuses the one isolate (in a single-page engine
/// "shared across contexts" means "shared across repeated `new SharedWorker` in this page"). Keyed by `url`
/// (not the resolved literal source): W1.6 URL-loaded SharedWorkers all resolve to an EMPTY literal source,
/// so keying by source would wrongly collapse every unregistered url+name pair onto one isolate.
#[derive(Default)]
pub struct SharedWorkerRegistry {
    map: HashMap<String, u32>,
}

/// THE SharedWorker realm's "script still loading" phase: every inbound `connect` is buffered until the worker's top-level script has been evaluated (which is when its `onconnect` /
/// `addEventListener("connect")` registration exists), then dispatches the buffer in arrival order and
/// delivers every later connect immediately -- [`after_top_level`], right after the literal or fetched
/// source has run.
///
/// This is the spec ordering, not a workaround: a `connect` is a queued task, and the event loop does not
/// run tasks until the worker's script has finished evaluating. It matters here because the connect marker
/// is a near-instant local channel send while the script load is a real network round trip, so the connect
/// essentially always wins the race.

/// Spawn a SharedWorker isolate: the SharedWorkerGlobalScope realm (`scope_kind = "shared"`, which is what
/// makes `worker-finalize.js` build the shared -- not dedicated -- global scope) under `name`. The parent
/// `persona` (from the connect op's op_state) is seeded into the worker so its `navigator` reads the page
/// identity; pass `None` for a persona-less worker.
pub(crate) fn spawn_shared_worker_persona(source: String, location: String, name: String, persona: Option<std::sync::Arc<dyn spec::Persona>>, boot: WorkerBoot) -> WorkerHandle {
    spawn_on_thread(source, location, persona, true, boot, move |ch, location| {
        vec![
            webapi::webapi_core_ext::init(),
            korvus_worker_ext::init(ch.out_tx, ch.in_rx),
            korvus_worker_host_ext::init(ch.bc_out_tx, ch.bc_in_rx, ch.port_out_tx, ch.port_in_rx, ch.err_out_tx, location, "shared".to_string(), name, String::new(), String::new(), String::new()),
        ]
    })
}

/// `__sharedWorkerConnect(source, name, url)` (native): spawn-or-reuse the keyed SharedWorker isolate, open a
/// fresh cross-isolate MessagePort channel, fire the worker's `connect` event with the worker-side port, and
/// return the page-side channel id (host.js reconstructs the page's `.port` for it). Reuses the W3b port plane.
fn cb_shared_worker_connect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let source = args.get(0).to_rust_string_lossy(scope);
    let name = args.get(1).to_rust_string_lossy(scope);
    let url = args.get(2).to_rust_string_lossy(scope);
    let key = format!("{name}\0{url}");
    let op_state = JsRuntime::op_state_from(scope);
    // 1. Resolve the worker id (spawn a new isolate on the first connect for this key).
    let existing = op_state.borrow().try_borrow::<SharedWorkerRegistry>().and_then(|r| r.map.get(&key).copied());
    let is_new = existing.is_none();
    let worker_id = match existing {
        Some(id) => id,
        None => {
            // A shared worker is not auto-attached from a page, so its script is the page's
            // request and what it fetches itself is reported on a target no page client attaches to.
            let target = announce_target("shared_worker", &worker_location(&url));
            let boot = {
                let st = op_state.borrow();
                worker_boot(&st, &source, &url, "sharedworker", transport::Reporter::default(), target)
            };
            // Hand the SharedWorker the page's persona (Part 1 seam), so its `navigator`/WorkerNavigator
            // read the same identity as the window and install exactly as a dedicated worker's do. Read
            // before the mutable borrow below.
            let persona = op_state.borrow().try_borrow::<crate::HostPersona>().map(|h| h.0.clone());
            let handle = spawn_shared_worker_persona(source.clone(), worker_location(&url), name.clone(), persona, boot);
            let mut st = op_state.borrow_mut();
            if st.try_borrow::<WorkerBcInboxes>().is_none() {
                install_main_bc(&mut st);
            }
            if st.try_borrow::<WorkerPortRoutes>().is_none() {
                install_main_port(&mut st);
            }
            if let Some(inboxes) = st.try_borrow::<WorkerBcInboxes>() {
                if let Ok(mut l) = inboxes.0.lock() {
                    l.push(handle.bc_inbound.clone());
                }
            }
            let id = {
                let reg = st.borrow_mut::<WorkerRegistry>();
                reg.next += 1;
                let id = reg.next;
                reg.map.insert(id, handle);
                id
            };
            st.borrow_mut::<SharedWorkerRegistry>().map.insert(key, id);
            id
        }
    };
    // 2-4. Open a cross-isolate MessagePort channel into the worker and hand it the worker-side end.
    let Some(cid) = open_worker_port(scope, worker_id, "__korvus_connect__") else { return };
    // Return [channel_id, worker_id, isNew] so host.js reconstructs `.port` for cid and, on the FIRST
    // connection to a new isolate, starts the main-side pumps (worker->main port/bc/error) for worker_id.
    let arr = v8::Array::new(scope, 3);
    let c = v8::Number::new(scope, cid as f64);
    let w = v8::Number::new(scope, worker_id as f64);
    let n = v8::Number::new(scope, if is_new { 1.0 } else { 0.0 });
    arr.set_index(scope, 0, c.into());
    arr.set_index(scope, 1, w.into());
    arr.set_index(scope, 2, n.into());
    rv.set(arr.into());
}

/// Open ONE cross-isolate MessagePort channel into `worker_id` and hand the worker the worker-side end,
/// wrapped in a `marker` postMessage frame. Returns the channel id the PAGE side reconstructs its own
/// endpoint from (`KB.__mpSetRemote(port, cid)`), or `None` when the worker id is unknown.
///
/// This is the whole cross-isolate connection primitive, and both realms that need one use it: a
/// SharedWorker connection (`__korvus_connect__` -> the `connect` event) and a ServiceWorker client
/// channel (`__korvus_sw_connect__` -> the port every `client.postMessage` rides). The port plane itself
/// (`WorkerPortRoutes` + `PortForward`) is W3b's and is unchanged.
fn open_worker_port(scope: &mut v8::PinScope, worker_id: u32, marker: &str) -> Option<u32> {
    let op_state = JsRuntime::op_state_from(scope);
    let (cid, port_inbound, msg_inbound) = {
        let mut st = op_state.borrow_mut();
        let reg = st.borrow_mut::<WorkerRegistry>();
        reg.port_next += 1;
        let cid = reg.port_next;
        let (pi, mi) = match reg.map.get(&worker_id) {
            Some(h) => (Some(h.port_inbound.clone()), Some(h.inbound.clone())),
            None => (None, None),
        };
        (cid, pi, mi)
    };
    // Route cid -> the worker's port inbox, so the page port's posts reach the worker's paired port.
    if let Some(pi) = &port_inbound {
        let st = op_state.borrow();
        if let Some(routes) = st.try_borrow::<WorkerPortRoutes>() {
            if let Ok(mut m) = routes.0.lock() {
                m.insert(cid, pi.clone());
            }
        }
    }
    // Ship the marker frame (the worker-side port rides as a transferred port) down the worker's inbound
    // postMessage plane; worker-host.js's receive loop recognises the marker and never surfaces it as a
    // message.
    let mi = msg_inbound?;
    let marker = v8::String::new(scope, marker)?;
    let bytes = serialize(scope, marker.into());
    let _ = mi.send(msg_encode(&bytes, &[cid]));
    Some(cid)
}

/// Build the native `__sharedWorkerConnect` function (host.js installs it as `KB.__sharedWorkerConnect`).
#[op2]
pub fn op_shared_worker_connect_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[string] name: &str,
    #[smi] length: i32,
) -> v8::Local<'s, v8::Function> {
    build_fn(scope, cb_shared_worker_connect, name, length)
}

// ---- W5.1: ServiceWorker (registration + install/activate lifecycle) ----

/// The MAIN page's ServiceWorker registrations, keyed by source -> the SW isolate id. A repeated
/// `register()` of the same script reuses the isolate.
#[derive(Default)]
pub struct ServiceWorkerRegistry {
    map: HashMap<String, u32>,
}

// `SW_PRELUDE` IS GONE, AND WITH IT THE LAST FAKE WEB API IN THIS FILE.
//
// It was a ~20-line JS string prepended to every ServiceWorker's own source, and it did three
// things, none of which a kernel may do (invariant I9):
//
// 1. **It defined `FetchEvent` in JavaScript.** `new globalThis.Event('fetch')` with
//    `ev.request = { url, method: 'GET' }`, `ev.respondWith = (x) => …` and (in the lifecycle half)
//    `ev.waitUntil = () => {}` written on as OWN properties — three instance tells on an event
//    object that a real browser leaves owning `isTrusted` and nothing else, with an object literal
//    standing in for a `Request`, on an interface that did not exist. I1, I3 and I9 together.
//    `FetchEvent`, `ExtendableEvent` and `InstallEvent` are `interface!` tables now
//    (`core/korvus-webapi/src/core/events/extendable_event.rs`), installed in the ServiceWorker
//    realm only, and the events are minted in Rust.
// 2. **It ran the install -> activate lifecycle**, with `waitUntil` a no-op, so an `install`
//    handler's promise was thrown away and `activate` fired while it was still pending. That is
//    `service_worker_realm::script_loaded`, and `waitUntil` now holds `activate` back.
// 3. **It held the SW half of the fetch transport** — the two `BroadcastChannel`s. THE TRANSPORT IS
//    UNCHANGED: same names (`korvus-sw-fetch`, `korvus-sw-fetch-resp`), same `{id, url}` request and
//    `{id, body}` reply frames, because the page half (`modules/fetch/fetch_global.rs`) speaks
//    exactly that. Only the side that was JS moved; see `install_sw_fetch_transport`.
//
// The lifecycle's start, `script_loaded`, is called by `after_top_level` the moment the script has been
// evaluated (it was `__swScriptLoaded` on a `Symbol.for("korvus.bridge")` holder, called
// by a JS string appended to the script).

/// Spawn a ServiceWorker isolate: the ServiceWorkerGlobalScope realm (`scope_kind = "service"`). Its
/// source is the user script and NOTHING ELSE -- the lifecycle, the fetch transport and
/// the scope's own globals are all Rust. `location` is the registered script's own
/// URL -- what `importScripts` and a relative `fetch` inside the worker resolve against. The parent
/// `persona` (from the register op's op_state) is seeded into the worker so its `navigator` reads the page
/// identity; pass `None` for a persona-less worker.
pub(crate) fn spawn_service_worker_persona(source: String, location: String, client_url: String, persona: Option<std::sync::Arc<dyn spec::Persona>>, boot: WorkerBoot) -> WorkerHandle {
    spawn_on_thread(source, location, persona, true, boot, move |ch, location| {
        vec![
            webapi::webapi_core_ext::init(),
            korvus_worker_ext::init(ch.out_tx, ch.in_rx),
            korvus_worker_host_ext::init(ch.bc_out_tx, ch.bc_in_rx, ch.port_out_tx, ch.port_in_rx, ch.err_out_tx, location, "service".to_string(), String::new(), client_url, String::new(), String::new()),
        ]
    })
}

/// `__serviceWorkerRegister(source, url)` (native): spawn-or-reuse the SW isolate for this script, run its
/// install->activate lifecycle, and return [worker_id, isNew] (host.js starts the worker->main pumps on a
/// new isolate so the SW's BroadcastChannel/MessagePort traffic reaches the page).
///
/// `source` is the literal script when the caller already has one (the `__workerScripts` test seam);
/// otherwise it is empty and `url` is fetched on the worker's own thread, exactly as `new Worker(url)` and
/// `new SharedWorker(url)` do -- registration used to be source-only, which is why every
/// `.any.serviceworker.html` file had nothing to run.
fn cb_service_worker_register(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let source = args.get(0).to_rust_string_lossy(scope);
    let url = args.get(1).to_rust_string_lossy(scope);
    let (worker_id, is_new) = register_service_worker(scope, source, url);
    let arr = v8::Array::new(scope, 2);
    let w = v8::Number::new(scope, worker_id as f64);
    let n = v8::Number::new(scope, if is_new { 1.0 } else { 0.0 });
    arr.set_index(scope, 0, w.into());
    arr.set_index(scope, 1, n.into());
    rv.set(arr.into());
}

/// Spawn-or-reuse the ServiceWorker isolate for `source`/`url` and return `(worker_id, is_new)`. The registration
/// core shared by `navigator.serviceWorker.register` (page JS, via [`cb_service_worker_register`]) and the ABI-3
/// `host->spawn_background` host call (a WebExtension MV3 background, via [`crate::engine::Engine::spawn_background`]).
/// Keys by url when present, else by source, so a re-register reuses the isolate.
pub(crate) fn register_service_worker(scope: &mut v8::PinScope, source: String, url: String) -> (u32, bool) {
    // Key by url when there is one (every URL-loaded registration resolves to an EMPTY literal source, so
    // keying those by source would collapse them all onto one isolate -- the same trap SharedWorker hit).
    let key = if url.is_empty() { format!("src\0{source}") } else { format!("url\0{url}") };
    let op_state = JsRuntime::op_state_from(scope);
    let existing = op_state.borrow().try_borrow::<ServiceWorkerRegistry>().and_then(|r| r.map.get(&key).copied());
    let is_new = existing.is_none();
    let worker_id = match existing {
        Some(id) => id,
        None => {
            // Hand the ServiceWorker the page's persona (Part 1 seam) so its `navigator`/WorkerNavigator
            // read the window's identity and install as a dedicated worker's do. Read before the borrow.
            let persona = op_state.borrow().try_borrow::<crate::HostPersona>().map(|h| h.0.clone());
            // A `service_worker` target reports its own script request, start to end.
            let target = announce_target("service_worker", &worker_location(&url));
            let boot = {
                let st = op_state.borrow();
                let own = transport::Reporter { target: Some(target.clone()), response_target: None };
                worker_boot(&st, &source, &url, "serviceworker", own, target)
            };
            // The registering page, its `Client.url` in the worker (B22).
            let client_url = op_state.borrow().try_borrow::<HostLocation>().map(|l| l.get()).unwrap_or_default();
            let handle = spawn_service_worker_persona(source.clone(), worker_location(&url), client_url, persona, boot);
            let mut st = op_state.borrow_mut();
            if st.try_borrow::<WorkerBcInboxes>().is_none() {
                install_main_bc(&mut st);
            }
            if st.try_borrow::<WorkerPortRoutes>().is_none() {
                install_main_port(&mut st);
            }
            if let Some(inboxes) = st.try_borrow::<WorkerBcInboxes>() {
                if let Ok(mut l) = inboxes.0.lock() {
                    l.push(handle.bc_inbound.clone());
                }
            }
            let id = {
                let reg = st.borrow_mut::<WorkerRegistry>();
                reg.next += 1;
                let id = reg.next;
                reg.map.insert(id, handle);
                id
            };
            st.borrow_mut::<ServiceWorkerRegistry>().map.insert(key, id);
            id
        }
    };
    (worker_id, is_new)
}

/// Build the native `__serviceWorkerRegister` function (host.js installs it on the ServiceWorkerContainer).
#[op2]
pub fn op_service_worker_register_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[string] name: &str,
    #[smi] length: i32,
) -> v8::Local<'s, v8::Function> {
    build_fn(scope, cb_service_worker_register, name, length)
}

/// `__serviceWorkerConnect(workerId)` (native): open the page's client MessagePort channel to a registered
/// ServiceWorker and return its channel id. Every `serviceWorker.postMessage(...)` from the page rides this
/// port, and the worker side re-dispatches each arrival as a `message` event whose `source` is the paired
/// port -- which is what a ServiceWorkerGlobalScope's `event.source` (a Client) is for, and what
/// testharness.js's ServiceWorkerTestEnvironment reports its results back through.
fn cb_service_worker_connect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let worker_id = args.get(0).number_value(scope).unwrap_or(0.0) as u32;
    match open_worker_port(scope, worker_id, "__korvus_sw_connect__") {
        Some(cid) => rv.set(v8::Number::new(scope, cid as f64).into()),
        None => rv.set(v8::Number::new(scope, 0.0).into()),
    }
}

/// Build the native `__serviceWorkerConnect` function (host.js installs it as `KB.__serviceWorkerConnect`).
#[op2]
pub fn op_service_worker_connect_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[string] name: &str,
    #[smi] length: i32,
) -> v8::Local<'s, v8::Function> {
    build_fn(scope, cb_service_worker_connect, name, length)
}

/// Await the next message FROM worker `id` (worker -> main). Returns empty bytes when the worker is gone.
/// The `class Worker` receive loop unrefs this op's promise so a parked receive never keeps the page's
/// event loop alive by itself.
#[op2(async(lazy), fast)]
#[buffer]
pub async fn op_worker_recv(state: Rc<RefCell<OpState>>, id: f64) -> Vec<u8> {
    let id = id as u32;
    let rx = {
        let s = state.borrow();
        let reg = s.borrow::<WorkerRegistry>();
        match reg.map.get(&id) {
            Some(h) => h.outbound.clone(),
            None => return Vec::new(),
        }
    };
    let mut guard = rx.lock().await;
    let frame = guard.recv().await.unwrap_or_default();
    still_running(&state, id, frame)
}

/// A frame from worker `id`, or nothing once `terminate()` has run. HTML's "terminate a worker"
/// DISCARDS what the worker posted and the page has not yet received; this future holds its own clone
/// of the channel, so without the check every message still buffered at `terminate()` was delivered
/// afterwards (WPT workers/constructors/Worker/terminate.html: the count moves after terminate).
/// An empty frame is also what stops the pump.
fn still_running(state: &Rc<RefCell<OpState>>, id: u32, frame: Vec<u8>) -> Vec<u8> {
    let live = state.borrow().try_borrow::<WorkerRegistry>().is_some_and(|r| r.map.contains_key(&id));
    if live { frame } else { Vec::new() }
}

/// Await the next cross-isolate BroadcastChannel frame FROM worker `id` (worker BC -> main). The `class
/// Worker` BC receive loop decodes it and calls `op_bc_deliver_external` to deliver to the page's local
/// BroadcastChannels. Empty when the worker is gone. Unrefed like `op_worker_recv`.
#[op2(async(lazy), fast)]
#[buffer]
pub async fn op_worker_bc_recv(state: Rc<RefCell<OpState>>, id: f64) -> Vec<u8> {
    let id = id as u32;
    let rx = {
        let s = state.borrow();
        let reg = s.borrow::<WorkerRegistry>();
        match reg.map.get(&id) {
            Some(h) => h.bc_outbound.clone(),
            None => return Vec::new(),
        }
    };
    let mut guard = rx.lock().await;
    guard.recv().await.unwrap_or_default()
}

/// Await the next cross-isolate MessagePort frame FROM worker `id` (a remote port's post leaving the worker).
/// The `class Worker` port receive loop decodes `[u32 channel_id][bytes]` and calls `op_port_deliver_external`
/// to deliver to the page's paired port. Empty when the worker is gone. Unrefed like `op_worker_recv`.
#[op2(async(lazy), fast)]
#[buffer]
pub async fn op_worker_port_recv(state: Rc<RefCell<OpState>>, id: f64) -> Vec<u8> {
    let id = id as u32;
    let rx = {
        let s = state.borrow();
        let reg = s.borrow::<WorkerRegistry>();
        match reg.map.get(&id) {
            Some(h) => h.port_outbound.clone(),
            None => return Vec::new(),
        }
    };
    let mut guard = rx.lock().await;
    guard.recv().await.unwrap_or_default()
}

/// Await worker `id`'s script timing (worker -> main, one JSON `spec::FetchTiming`), which the parent's
/// `Worker` pump turns into the document's resource-timing entry. Empty when the worker is
/// gone. Unrefed like the other pumps.
#[op2(async(lazy), fast)]
#[buffer]
pub async fn op_worker_timing_recv(state: Rc<RefCell<OpState>>, id: f64) -> Vec<u8> {
    let id = id as u32;
    let rx = {
        let s = state.borrow();
        let reg = s.borrow::<WorkerRegistry>();
        match reg.map.get(&id) {
            Some(h) => h.timing_outbound.clone(),
            None => return Vec::new(),
        }
    };
    let mut guard = rx.lock().await;
    guard.recv().await.unwrap_or_default()
}

/// Await the next uncaught error FROM worker `id` (worker -> main), as a UTF-8 message. The `class Worker`
/// error pump fires `onerror`. Empty when the worker is gone. Unrefed like the other pumps.
#[op2(async(lazy), fast)]
#[buffer]
pub async fn op_worker_err_recv(state: Rc<RefCell<OpState>>, id: f64) -> Vec<u8> {
    let id = id as u32;
    let rx = {
        let s = state.borrow();
        let reg = s.borrow::<WorkerRegistry>();
        match reg.map.get(&id) {
            Some(h) => h.err_outbound.clone(),
            None => return Vec::new(),
        }
    };
    let mut guard = rx.lock().await;
    let frame = guard.recv().await.unwrap_or_default();
    still_running(&state, id, frame)
}

/// Return the native `postMessage` function to install on `Worker.prototype` (`[native code]`, name/length
/// set at the install site so the surface matches Chrome exactly).
#[op2]
pub fn op_worker_post_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[string] name: &str,
    #[smi] length: i32,
) -> v8::Local<'s, v8::Function> {
    // webapi's builder, not this file's copy: it RECORDS what it builds while a template capture runs,
    // so `Worker`'s interface takes the template step like every other.
    webapi::binding::protos::build_fn(scope, cb_worker_post, name, length)
}

/// Return the native `terminate` function to install on `Worker.prototype`.
#[op2]
pub fn op_worker_terminate_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[string] name: &str,
    #[smi] length: i32,
) -> v8::Local<'s, v8::Function> {
    webapi::binding::protos::build_fn(scope, cb_worker_terminate, name, length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::{JsRuntime, RuntimeOptions};

    /// A structured-clone-aware deep-equal over two LIVE V8 values.
    ///
    /// IT WAS A JS STRING. `const DEQ: &str` held a recursive `function d(a, b)` that the test
    /// assigned onto the isolate's global under the name `__deq`, run in with `execute_script`, with
    /// the two values planted on the global as `__o` and `__c` so the injected function could reach
    /// them. That string was the LAST hit in `scripts/freeze.sh`'s `global_assign_rs_strings`
    /// ratchet, which counts textually and cannot see that the string was inside
    /// `#[cfg(test)] mod tests` and so was never compiled into the library at all.
    ///
    /// (This comment does not SPELL that assignment, for the same reason `worker_recv.rs`'s
    /// `__swScriptLoaded` note does not spell its own: the ratchet greps whole `.rs` files without
    /// stripping comments, so documenting a removal in its own syntax puts the count straight back
    /// up. The prose bends; the counter does not.)
    ///
    /// THE RULING, recorded because the alternative was tempting. Narrowing the ratchet to
    /// non-test code would have been the one-line fix, and it is the wrong one: `freeze.sh`'s
    /// `ratchets()` is a `grep` over whole files, so "skip the test module" means teaching shell to
    /// find `#[cfg(test)]` regions in Rust — a fragile rule, on a counter whose entire value is that
    /// its meaning is unambiguous, in a project whose own audit records being bitten by counters that
    /// quietly changed meaning. And the helper is not deleted either: the comparison it performed is
    /// still performed, on the same fourteen cases, to the same depth. Only the language changed, and
    /// with it the ratchet reaches 0 without the counter meaning anything new.
    ///
    /// WHAT IS COMPARED, unchanged from the JS: `Date` by its time value, `RegExp` by its source
    /// text, `Map` and `Set` by size then element, `ArrayBuffer`/views by their bytes, and everything
    /// else by own enumerable string keys, recursively. VALUES are compared and not serialized bytes,
    /// for the reason the old comment gave: V8 may re-represent a deserialized array (dense -> sparse),
    /// so re-serialization is not byte-stable even when the value is faithful.
    ///
    /// ONE THING IS STRICTER, deliberately. The JS compared a typed array against its WHOLE backing
    /// buffer (`new Uint8Array(a.buffer)`), ignoring `byteOffset`/`byteLength`; this compares the
    /// view's own bytes. Every case below is a whole-buffer view, so no case changes answer — but a
    /// future case with an offset view would be judged correctly rather than leniently.
    fn deep_eq(scope: &mut v8::PinScope, a: v8::Local<v8::Value>, b: v8::Local<v8::Value>) -> bool {
        if a.strict_equals(b) {
            return true;
        }
        if !a.is_object() || !b.is_object() {
            return false;
        }
        if a.is_date() || b.is_date() {
            let (Ok(da), Ok(db)) = (v8::Local::<v8::Date>::try_from(a), v8::Local::<v8::Date>::try_from(b)) else {
                return false;
            };
            return da.value_of() == db.value_of() || (da.value_of().is_nan() && db.value_of().is_nan());
        }
        if a.is_reg_exp() || b.is_reg_exp() {
            if !a.is_reg_exp() || !b.is_reg_exp() {
                return false;
            }
            let (Some(sa), Some(sb)) = (a.to_string(scope), b.to_string(scope)) else { return false };
            return sa.to_rust_string_lossy(scope) == sb.to_rust_string_lossy(scope);
        }
        if a.is_map() || b.is_map() {
            let (Ok(ma), Ok(mb)) = (v8::Local::<v8::Map>::try_from(a), v8::Local::<v8::Map>::try_from(b)) else {
                return false;
            };
            if ma.size() != mb.size() {
                return false;
            }
            // `as_array` is `[k0, v0, k1, v1, …]` in INSERTION order, which structured clone
            // preserves — so comparing position by position is what the JS's `b.has(k)` + `d(v,
            // b.get(k))` meant, plus the order guarantee the JS could not express.
            return arrays_deep_eq(scope, ma.as_array(scope), mb.as_array(scope));
        }
        if a.is_set() || b.is_set() {
            let (Ok(sa), Ok(sb)) = (v8::Local::<v8::Set>::try_from(a), v8::Local::<v8::Set>::try_from(b)) else {
                return false;
            };
            if sa.size() != sb.size() {
                return false;
            }
            return arrays_deep_eq(scope, sa.as_array(scope), sb.as_array(scope));
        }
        if a.is_array_buffer() || a.is_array_buffer_view() || b.is_array_buffer() || b.is_array_buffer_view() {
            let (Some(ba), Some(bb)) = (bytes_of(scope, a), bytes_of(scope, b)) else { return false };
            return ba == bb;
        }
        let (Ok(oa), Ok(ob)) = (v8::Local::<v8::Object>::try_from(a), v8::Local::<v8::Object>::try_from(b)) else {
            return false;
        };
        // `Object.keys`: own, enumerable, string-keyed, indices included — the default
        // `GetPropertyNamesArgs` with `OwnOnly`.
        let own = || v8::GetPropertyNamesArgs { mode: v8::KeyCollectionMode::OwnOnly, ..Default::default() };
        let (Some(ka), Some(kb)) = (oa.get_own_property_names(scope, own()), ob.get_own_property_names(scope, own())) else {
            return false;
        };
        if ka.length() != kb.length() {
            return false;
        }
        for i in 0..ka.length() {
            let Some(k) = ka.get_index(scope, i) else { return false };
            let (Some(va), Some(vb)) = (oa.get(scope, k), ob.get(scope, k)) else { return false };
            if !deep_eq(scope, va, vb) {
                return false;
            }
        }
        true
    }

    /// Element-wise [`deep_eq`] over two arrays of the same length.
    fn arrays_deep_eq(scope: &mut v8::PinScope, a: v8::Local<v8::Array>, b: v8::Local<v8::Array>) -> bool {
        if a.length() != b.length() {
            return false;
        }
        for i in 0..a.length() {
            let (Some(x), Some(y)) = (a.get_index(scope, i), b.get_index(scope, i)) else { return false };
            if !deep_eq(scope, x, y) {
                return false;
            }
        }
        true
    }

    /// The bytes an `ArrayBuffer` or an `ArrayBufferView` actually covers.
    fn bytes_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
        if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
            let buf = view.buffer(scope)?;
            let store = buf.get_backing_store();
            let off = view.byte_offset();
            let len = view.byte_length();
            let data = store.data()?;
            // SAFETY: the backing store is alive for as long as `buf`, which this scope holds, and
            // `[off, off + len)` is inside it by construction of the view.
            let all = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, store.byte_length()) };
            return all.get(off..off + len).map(|s| s.to_vec());
        }
        let buf = v8::Local::<v8::ArrayBuffer>::try_from(v).ok()?;
        let store = buf.get_backing_store();
        let len = store.byte_length();
        if len == 0 {
            return Some(Vec::new());
        }
        let data = store.data()?;
        // SAFETY: as above.
        let all = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, len) };
        Some(all.to_vec())
    }

    /// Clone a value THROUGH the byte channel (serialize -> deserialize) — the exact path a message
    /// takes to a worker isolate — and return the clone as a fresh Global.
    fn clone_via_bytes(rt: &mut JsRuntime, g: &v8::Global<v8::Value>) -> v8::Global<v8::Value> {
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let v = v8::Local::new(scope, g);
        let bytes = serialize(scope, v);
        assert!(!bytes.is_empty(), "serialize produced no bytes");
        let back = deserialize(scope, &bytes);
        v8::Global::new(scope, back)
    }

    #[test]
    fn structured_clone_roundtrip() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let cases = [
            "42", "-3.5", "'hello'", "true", "false", "null",
            "[1, 2, 3]",
            "({ a: 1, b: 'x', c: [true, null, 3.5], d: { nested: 'y' } })",
            "new Date(1234567890)",
            "/ab+c/gi",
            "new Map([['k', 1], ['j', [2, 3]]])",
            "new Set([1, 'a', 2])",
            "new Uint8Array([1, 2, 3, 255])",
            "new ArrayBuffer(8)",
        ];
        for src in cases {
            let orig = rt.execute_script("orig", format!("({src})")).expect("eval orig");
            let copy = clone_via_bytes(&mut rt, &orig);
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let (o, c) = (v8::Local::new(scope, &orig), v8::Local::new(scope, &copy));
            // The comparison is Rust's now, and the test isolate is left exactly as `JsRuntime::new`
            // made it: no `__deq`, no `__o`, no `__c`. The scaffolding this test used to plant on the
            // global it is measuring is gone with the helper.
            assert!(deep_eq(scope, o, c), "structured-clone round-trip not deep-equal for `{src}`");
        }
    }

    /// [`deep_eq`] SAYS NO.
    ///
    /// `structured_clone_roundtrip` only ever asks the comparison to say YES, so a `deep_eq` that
    /// returned `true` unconditionally would pass it — and the JS helper it replaces was never
    /// checked this way either. Each pair below differs in exactly one place, one per branch of the
    /// comparison, so a branch that silently stopped discriminating is a failure here rather than a
    /// green round trip that proves nothing.
    #[test]
    fn deep_eq_discriminates() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let pairs = [
            ("1", "2"),                                                   // primitives
            ("1", "'1'"),                                                 // type, not value
            ("[1, 2, 3]", "[1, 2, 4]"),                                   // array element
            ("[1, 2, 3]", "[1, 2]"),                                      // array length
            ("({a: 1})", "({a: 2})"),                                     // object value
            ("({a: 1})", "({b: 1})"),                                     // object key
            ("({a: 1})", "({a: 1, b: 2})"),                               // key count
            ("({a: {b: 1}})", "({a: {b: 2}})"),                           // nested
            ("new Date(1)", "new Date(2)"),                               // time value
            ("new Date(1)", "1"),                                         // date vs number
            ("/a/g", "/a/i"),                                             // regexp flags
            ("/a/g", "/b/g"),                                             // regexp source
            ("new Map([['k', 1]])", "new Map([['k', 2]])"),               // map value
            ("new Map([['k', 1]])", "new Map([['j', 1]])"),               // map key
            ("new Map([['k', 1]])", "new Map()"),                         // map size
            ("new Set([1, 2])", "new Set([1, 3])"),                       // set element
            ("new Set([1, 2])", "new Set([1])"),                          // set size
            ("new Uint8Array([1, 2])", "new Uint8Array([1, 3])"),         // bytes
            ("new Uint8Array([1, 2])", "new Uint8Array([1])"),            // byte length
            ("new ArrayBuffer(4)", "new ArrayBuffer(8)"),                 // buffer length
            ("new Uint8Array([1, 2])", "[1, 2]"),                         // view vs array
        ];
        for (l, r) in pairs {
            let gl = rt.execute_script("l", format!("({l})")).expect("eval l");
            let gr = rt.execute_script("r", format!("({r})")).expect("eval r");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let (a, b) = (v8::Local::new(scope, &gl), v8::Local::new(scope, &gr));
            assert!(!deep_eq(scope, a, b), "deep_eq wrongly equated `{l}` and `{r}`");
            // ...and it is symmetric, which the round-trip test never exercises either.
            assert!(!deep_eq(scope, b, a), "deep_eq wrongly equated `{r}` and `{l}` (reversed)");
        }
        // A control: an offset view compares its OWN bytes, not the whole buffer. The JS helper this
        // replaces compared `new Uint8Array(a.buffer)` and would have called these two equal.
        let gl = rt.execute_script("l", "(new Uint8Array([9, 1, 2, 9]).subarray(1, 3))").expect("eval l");
        let gr = rt.execute_script("r", "(new Uint8Array([8, 1, 2, 8]).subarray(1, 3))").expect("eval r");
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let (a, b) = (v8::Local::new(scope, &gl), v8::Local::new(scope, &gr));
        assert!(deep_eq(scope, a, b), "two views over the same two bytes must be equal");
    }

    /// A REAL worker isolate on its own thread runs a script, structured-clone-serializes a value via
    /// `op_worker_self_post`, and ships it to the main thread; the main deserializes it and checks the
    /// value survived the isolate boundary. Proves the whole spawn/op/channel/serialize path.
    #[tokio::test]
    async fn worker_isolate_posts_a_message() {
        // Create the main runtime first so the V8 platform is initialised before the worker.
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_worker(
            "Deno.core.ops.op_worker_self_post({ n: 42, s: 'hi', a: [1, 2, 3] });".to_string(),
        );
        let bytes = {
            let mut guard = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(10), guard.recv())
                .await
                .expect("worker posted within 10s")
                .expect("worker posted a message")
        };
        assert!(!bytes.is_empty(), "worker sent an empty message");

        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let val = deserialize(scope, &bytes);
        let obj = v8::Local::<v8::Object>::try_from(val).expect("message is an object");
        let key = v8::String::new(scope, "n").unwrap();
        let n = obj
            .get(scope, key.into())
            .and_then(|v| v.number_value(scope))
            .expect("n");
        assert_eq!(n, 42.0, "worker message value did not survive the isolate boundary");
    }

    /// Full main->worker->main round trip. The main posts a structured value INTO a real worker isolate;
    /// the worker awaits it (`op_worker_self_recv`), deserializes it (`op_msg_deserialize`), and echoes it
    /// back (`op_worker_self_post`); the main receives the echo and checks the value survived both isolate
    /// crossings. Proves BIDIRECTIONAL messaging with the async receive path.
    #[tokio::test]
    async fn worker_bidirectional_echo() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_worker(
            "Deno.core.ops.op_worker_self_recv().then((b) => \
               Deno.core.ops.op_worker_self_post(Deno.core.ops.op_msg_deserialize(b)));"
                .to_string(),
        );
        // Serialize a value in the MAIN isolate and post it into the worker.
        let msg = {
            let g = rt.execute_script("v", "({ x: 7, y: ['a', 'b'] })").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.inbound.send(msg).expect("post into worker");
        // Receive the echo and verify the value round-tripped.
        let echoed = {
            let mut guard = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(10), guard.recv())
                .await
                .expect("worker echoed within 10s")
                .expect("worker echoed the message")
        };
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let val = deserialize(scope, &echoed);
        let obj = v8::Local::<v8::Object>::try_from(val).expect("echo is an object");
        let key = v8::String::new(scope, "x").unwrap();
        let x = obj
            .get(scope, key.into())
            .and_then(|v| v.number_value(scope))
            .expect("x");
        assert_eq!(x, 7.0, "echoed value did not survive the round trip");
    }

    /// W1.5b — a REAL worker isolate loaded with the full worker scope (`worker-host.js` via
    /// `spawn_real_worker`) echoes `data + 1`. Proves the worker-host `self`/`onmessage`/`postMessage` +
    /// receive loop work on a real isolate that ALSO has webapi_core loaded.
    #[tokio::test]
    async fn worker_scope_echo() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker(
            "self.onmessage = (e) => { self.postMessage(e.data + 1); };".to_string(),
            "about:blank".to_string(),
        );
        let msg = {
            let g = rt.execute_script("v", "41").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.inbound.send(msg_encode(&msg, &[])).expect("post into worker");
        let echoed = {
            let mut guard = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), guard.recv())
                .await
                .expect("worker echoed within 15s")
                .expect("worker echoed")
        };
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let val = deserialize(scope, &echoed);
        let n = val.number_value(scope).expect("number");
        assert_eq!(n, 42.0, "worker-host.js echo failed");
    }

    /// W3b.0 -- a REAL worker isolate creates a same-isolate `MessageChannel`, port2 posts to port1, port1's
    /// onmessage relays to main. Proves the native cppgc MessagePort + JS MessageChannel are functional
    /// inside the worker (not just the surface-parity shells).
    #[tokio::test]
    async fn worker_scope_message_channel() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker(
            "const mc = new MessageChannel(); \
             mc.port1.onmessage = (e) => self.postMessage('port-got:' + e.data); \
             mc.port2.postMessage('hello-port');"
                .to_string(),
            "about:blank".to_string(),
        );
        let msg = {
            let mut guard = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), guard.recv())
                .await
                .expect("worker's port1 relayed within 15s")
                .expect("worker's port1 relayed")
        };
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let val = deserialize(scope, &msg);
        assert_eq!(val.to_rust_string_lossy(scope), "port-got:hello-port", "same-isolate MessageChannel in worker failed");
    }

    /// W1.5b — `crypto.subtle.digest` runs NATIVELY inside a real worker isolate (the SAME webapi
    /// SubtleCrypto cppgc the window uses, via `webapi_core_ext`) and `TextEncoder` encodes. The worker
    /// digests 'over 9000' and posts the hex; the main checks it is a real 32-byte (64-hex) SHA-256.
    #[tokio::test]
    async fn worker_scope_crypto_digest() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker(
            "self.onmessage = async () => { \
               const buf = await crypto.subtle.digest('SHA-256', new TextEncoder().encode('over 9000')); \
               const hex = [...new Uint8Array(buf)].map((x) => x.toString(16).padStart(2, '0')).join(''); \
               self.postMessage(hex); };"
                .to_string(),
            "about:blank".to_string(),
        );
        let trigger = {
            let g = rt.execute_script("v", "1").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.inbound.send(msg_encode(&trigger, &[])).expect("post trigger");
        let echoed = {
            let mut guard = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), guard.recv())
                .await
                .expect("worker digested within 15s")
                .expect("worker posted the digest")
        };
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let val = deserialize(scope, &echoed);
        let hex = val.to_rust_string_lossy(scope);
        assert_eq!(hex.len(), 64, "SHA-256 hex should be 64 chars (got {hex:?})");
        assert!(
            hex.chars().all(|c| c.is_ascii_hexdigit()),
            "digest should be hex (got {hex:?})"
        );
    }

    /// W1.5b — the worker's OWN Performance timeline + a PerformanceObserver whose callback fires in the
    /// WORKER's scheduler (the hardest performance path). The worker marks two entries, awaits the observer
    /// callback, and posts a sentinel only if EVERYTHING holds: observer fired with both entries, the
    /// observer identity is stable, `performance.now()` is a number, `timeOrigin > 0`, and the window-only
    /// `eventCounts` partial is absent. Proves performance + the native PerformanceObserver work on a real
    /// worker isolate driven by the worker's own event loop.
    #[tokio::test]
    async fn worker_scope_performance_observer() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker(
            "self.onmessage = async () => { \
               performance.clearMarks(); \
               let resolveCb; const fired = new Promise((r) => { resolveCb = r; }); \
               const observer = new PerformanceObserver((list, obs) => { \
                 resolveCb({ n: list.getEntries().length, first: list.getEntries()[0] && list.getEntries()[0].name, same: obs === observer }); \
               }); \
               observer.observe({ entryTypes: ['mark'] }); \
               performance.mark('observed-1'); performance.mark('observed-2'); \
               const r = await fired; observer.disconnect(); \
               const ok = r.n === 2 && r.same === true && (observer instanceof PerformanceObserver) \
                 && !('eventCounts' in performance) && typeof performance.now() === 'number' \
                 && performance.timeOrigin > 0; \
               self.postMessage(ok ? ('PERF_OK:' + r.first) : 'PERF_FAIL'); };"
                .to_string(),
            "about:blank".to_string(),
        );
        let trigger = {
            let g = rt.execute_script("v", "1").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.inbound.send(msg_encode(&trigger, &[])).expect("post trigger");
        let echoed = {
            let mut guard = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), guard.recv())
                .await
                .expect("worker perf result within 15s")
                .expect("worker posted the perf result")
        };
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let val = deserialize(scope, &echoed);
        let s = val.to_rust_string_lossy(scope);
        assert_eq!(s, "PERF_OK:observed-1", "worker performance/observer path failed");
    }

    /// W1 — cross-isolate BroadcastChannel BOTH directions on a real worker isolate. The worker owns a
    /// native `BroadcastChannel('room')`. (A) worker->main: a `postMessage` on it is forwarded (BcForward)
    /// as an encoded frame on `bc_outbound` with the right name + structured-clone payload. (B) main->worker:
    /// an encoded frame posted to `bc_inbound` is delivered (op_bc_self_recv -> op_bc_deliver_external) to the
    /// worker's channel, whose `onmessage` relays it back over `Worker.postMessage`.
    #[tokio::test]
    async fn worker_scope_broadcast_channel() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker(
            "const c = new BroadcastChannel('room'); \
             c.onmessage = (e) => self.postMessage('got:' + e.data); \
             self.onmessage = () => c.postMessage('hi-from-worker');"
                .to_string(),
            "about:blank".to_string(),
        );
        // (A) worker -> main: trigger a BC post from the worker; read the forwarded frame off bc_outbound.
        let trigger = {
            let g = rt.execute_script("v", "1").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.inbound.send(msg_encode(&trigger, &[])).expect("trigger the worker");
        let frame = {
            let mut guard = handle.bc_outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), guard.recv())
                .await
                .expect("worker forwarded a BC post within 15s")
                .expect("worker forwarded a BC post")
        };
        assert!(frame.len() >= 4, "BC frame too short");
        let name_len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        let name = std::str::from_utf8(&frame[4..4 + name_len]).expect("utf8 name");
        assert_eq!(name, "room", "forwarded BC channel name");
        {
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let val = deserialize(scope, &frame[4 + name_len..]);
            assert_eq!(val.to_rust_string_lossy(scope), "hi-from-worker", "forwarded BC payload");
        }

        // (B) main -> worker: post an encoded frame for 'room'; the worker's BC onmessage relays it back.
        let payload = {
            let g = rt.execute_script("p", "'ping'").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.bc_inbound.send(bc_encode("room", &payload)).expect("post BC into worker");
        let echoed = {
            let mut guard = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), guard.recv())
                .await
                .expect("worker relayed the inbound BC within 15s")
                .expect("worker relayed the inbound BC")
        };
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let val = deserialize(scope, &echoed);
        assert_eq!(val.to_rust_string_lossy(scope), "got:ping", "inbound BC delivery/relay");
    }

    /// W1.6 — `terminate()` stops a BUSY worker. The worker posts 'go' then spins in a tight sync loop that
    /// no channel-drop could interrupt; `isolate.terminate_execution()` (via the published handle) unwinds
    /// it, the thread exits, and its sender drops -> the main's channel closes. Proves no hang/thread leak.
    #[tokio::test]
    async fn worker_terminate_stops_busy_worker() {
        let mut _rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker("self.postMessage('go'); while (true) {}".to_string(), "about:blank".to_string());
        // Wait for 'go' -- the worker has started and is about to spin.
        let go = {
            let mut g = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), g.recv())
                .await
                .expect("worker signalled start within 15s")
        };
        assert!(go.is_some(), "worker should post 'go' before spinning");
        // Interrupt the busy worker via its thread-safe isolate handle.
        let isolate = handle.terminate.lock().unwrap().as_ref().cloned();
        isolate.expect("isolate handle published").terminate_execution();
        // The worker thread should exit, dropping its sender -> the channel closes (recv -> None).
        let after = {
            let mut g = handle.outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(10), g.recv()).await
        };
        match after {
            Ok(None) => {} // channel closed = worker thread exited: PASS
            Ok(Some(_)) => panic!("worker sent an unexpected message after 'go'"),
            Err(_) => panic!("busy worker did not stop within 10s after terminate_execution"),
        }
    }

    /// W1.6 — an uncaught error in the worker's `onmessage` propagates to the main thread (the message that
    /// drives `Worker.onerror`). The worker throws on receipt; the main reads the error off err_outbound.
    #[tokio::test]
    async fn worker_onmessage_error_propagates() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker(
            "self.onmessage = () => { throw new Error('boom'); };".to_string(),
            "about:blank".to_string(),
        );
        let trigger = {
            let g = rt.execute_script("v", "1").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.inbound.send(msg_encode(&trigger, &[])).expect("trigger the worker");
        let err = {
            let mut g = handle.err_outbound.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), g.recv())
                .await
                .expect("worker reported an error within 15s")
                .expect("worker reported an error")
        };
        let s = String::from_utf8_lossy(&err);
        assert!(s.contains("boom"), "worker onerror message should mention 'boom' (got {s:?})");
    }

    /// W3 — ArrayBuffer transfer. The worker fills an 8-byte ArrayBuffer, `postMessage(ab, [ab])`s it, then
    /// reports the SOURCE's byteLength. The main checks the receiver got an 8-byte buffer AND the source was
    /// detached (byteLength -> 0) -- the observable transfer semantics.
    #[tokio::test]
    async fn worker_transfer_arraybuffer() {
        let mut rt = crate::new_runtime(RuntimeOptions::default());
        let handle = spawn_real_worker(
            "self.onmessage = () => { \
               const ab = new ArrayBuffer(8); new Uint8Array(ab).set([1,2,3,4,5,6,7,8]); \
               self.postMessage(ab, [ab]); \
               self.postMessage({ detached: ab.byteLength }); };"
                .to_string(),
            "about:blank".to_string(),
        );
        let trigger = {
            let g = rt.execute_script("v", "1").expect("eval");
            let ctx = rt.main_context();
            v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
            let v = v8::Local::new(scope, &g);
            serialize(scope, v)
        };
        handle.inbound.send(msg_encode(&trigger, &[])).expect("trigger");
        let recv = |h: Arc<TMutex<tmpsc::UnboundedReceiver<Vec<u8>>>>| async move {
            let mut g = h.lock().await;
            tokio::time::timeout(std::time::Duration::from_secs(15), g.recv())
                .await
                .expect("message within 15s")
                .expect("message")
        };
        let msg1 = recv(handle.outbound.clone()).await;
        let msg2 = recv(handle.outbound.clone()).await;
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        // msg1: the transferred ArrayBuffer arrived with its 8 bytes.
        let ab_val = deserialize(scope, &msg1);
        let ab = v8::Local::<v8::ArrayBuffer>::try_from(ab_val).expect("msg1 is an ArrayBuffer");
        assert_eq!(ab.byte_length(), 8, "transferred ArrayBuffer should be 8 bytes");
        // msg2: the SOURCE was detached (byteLength 0).
        let obj = v8::Local::<v8::Object>::try_from(deserialize(scope, &msg2)).expect("msg2 object");
        let key = v8::String::new(scope, "detached").unwrap();
        let detached = obj.get(scope, key.into()).and_then(|v| v.number_value(scope)).expect("detached");
        assert_eq!(detached, 0.0, "source ArrayBuffer should be detached after transfer");
    }
}
