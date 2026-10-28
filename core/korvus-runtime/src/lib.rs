//! `engine` — the V8 adapter. The **only** crate permitted to import `rusty_v8`/`deno_core`.
//!
//! G1.1: wraps `deno_core::JsRuntime` — runs JS, exposes a native `print` op, owns HTML's timer TASK
//! SOURCE ([`scheduler`]), and drives the event loop (promises/microtasks/timers).
//!
//! Confining V8 here is what lets v3 swap in an instrumented V8 fork without touching
//! `webapi`/`persona` (see WORKSPACE.md, "Engine / RE swap").


use deno_core::{
    extension, op2, OpState,
};
use spec::Persona;

// HTML's timer TASK SOURCE. `setTimeout` used to be a promise chain, so two timers due
// together ran inside one microtask checkpoint and a microtask the first queued ran after the second.
// The queue, the deadlines, the insertion order and the nesting clamp are Rust now, and the pump runs
// exactly one callback per turn of the event loop. Goal §W4.f's `Scheduler` service is this.
/// The navigation pipeline in Rust: document install, parser-induced loads, `<body>`
/// handler attributes, document readiness, the lifecycle events.
/// The module loader and the deferred module queue.
/// CDP evaluation and the remote-object table.
mod cdp_eval;
/// The engine itself: JsRuntime assembly, the persona, the plugin loader.
mod engine;
pub use engine::{engine_name, force_host_audit, set_locale_override, set_timezone_override, Engine};
/// A page-driving BINARY builds its interface templates from the blueprint file an earlier
/// run of the same binary wrote (`webapi`'s disk blueprint). Off unless a binary calls this: a test run
/// must never depend on a file an earlier run left.
pub use webapi::enable_template_disk_cache;
pub use cdp_eval::{Awaited, CallFrame, CdpArg, DevtoolsEntry, EvalOutcome, ExceptionInfo, RemoteResult};
/// The event loop: the pump, the module drain, the deadlines.
mod event_loop;
mod ice; // ICE gathering's network half -- the UDP sockets and STUN an RTCPeerConnection uses.
mod rtc_media; // The WebRTC send path's DTLS-SRTP handshake, over the socket `ice.rs` gathered on.
#[cfg(feature = "webrtc-srtp")]
mod rtc_srtp; // The WebRTC send path's SRTP_AES128_CM_SHA1_80 protect (goal A, Slice 1c).
#[cfg(feature = "webrtc-srtp")]
mod rtc_rtp; // The WebRTC send path's RTP (RFC 3550/7741 VP8) packetizer (goal A, Slice 1c).
#[cfg(feature = "webrtc-media")]
mod rtc_vp8; // The WebRTC send path's VP8 encoder over the vendored libvpx (goal A, Slice 1c).
pub use event_loop::{IsolateInterrupt, PumpEnd};
/// The behaviour layer's input dispatch.
mod input;
pub mod screenshot; // The screenshot primitive (plugin ABI 3 wave 1, C6): the page-process side of `host->capture_png`.
mod modules;
use modules::{KorvusModuleLoader, ModuleFetchState, PendingModule, PendingModules, MODULE_DRAIN_BUDGET};
mod location; // The window's `Location` members and the navigations they start.
pub mod decode; // A document's bytes to its text (the HTML encoding sniff).
mod navigation;
mod idleness; // Blink's network-quiet points (`networkAlmostIdle` / `networkIdle`), replayed from the ledger.
pub use idleness::NetworkIdle;
mod cdp_dom; // What a DevTools client reads about objects and the DOM, from the arena and the layout.
pub use cdp_dom::{NodeInfo, ObjectMeta, PropValue, PropertyRow};
mod oopif; // A cross-site frame's document in its own engine on its own thread.
pub(crate) use webapi::text_viewer; // in korvus-webapi, where a frame's load reads it too.
/// What a top-level XML document with no style information becomes: Chrome's XML tree viewer.
mod xml_viewer;
pub use spec::navtrace; // Where a navigation's time goes, phase by phase.
pub use navigation::{arrival_for, navigation_request, parse_declarative_refresh, Arrival, NavEntry, NavigationReason, PageNavigation};
mod scheduler;
mod scope_timers; // The five WindowOrWorkerGlobalScope timer bodies, shared by BOTH realms' tables.
/// Running a page's `<script>`s in Rust: the one driver for inline, external and module
/// scripts.
mod script_exec;
mod worker; // Real multi-isolate Web Worker platform (cross-isolate clone, isolate spawn, messaging).
mod worker_recv; // The worker realm's three cross-isolate receive loops (was worker-host.js's three async IIFEs).
pub mod worker_devtools; // A dedicated worker's DevTools channel: `Runtime.*` on its session, run on its thread.
mod worker_scope; // The WorkerGlobalScope tables' kernel-service bodies (timers, importScripts, close, postMessage).
/// The one V8 initialisation, and the two JS features Chrome does not ship.
mod v8_features;
mod websocket; // the WebSocket kernel service: the ops the `WebSocket` table calls (transport::websocket underneath).
pub use v8_features::{init_platform, new_runtime};
#[cfg(not(feature = "analysis"))]
pub(crate) use v8_features::new_snapshot_runtime;
/// The context snapshot (slice 3): the page realm made from this build's snapshot of it.
#[cfg(not(feature = "analysis"))]
pub mod snapshot;
/// (an analysis build makes none)
#[cfg(feature = "analysis")]
#[path = "snapshot_off.rs"]
pub mod snapshot;
// `mod natives` WAS HERE: raw v8 callbacks for the host-surface members op2 could not express.
// Its whole contents were the four `URL` statics and the blob-URL serial; all four
// are `static_method` rows of the `URL` table in korvus-webapi now and the registry is Rust there, so
// the file is deleted. Nothing in the kernel defines a Web-API member any more (invariant I9).

/// Holds the active persona in `OpState` so host ops can resolve persona-visible values.
/// This is the native-side of the `ctx.persona()` seam: Web-API values are sourced here, never
/// hardcoded in the binding classes.
struct HostPersona(std::sync::Arc<dyn Persona>);

pub mod singletons;

/// Adapter plugin: provides the injected persona as a `PersonaHandle` service on the kernel Context, so
/// swapping the registered persona plugin swaps the browser's identity. The instance comes from
/// `Engine::new`; a community plugin could provide a different or per-page-rotating one.
struct PersonaPlugin(std::sync::Arc<dyn Persona>);
impl spec::kernel::Plugin for PersonaPlugin {
    fn name(&self) -> &str {
        "persona"
    }
    fn provides(&self) -> &[&str] {
        &["persona"]
    }
    fn apply(&self, ctx: &spec::kernel::Context, _c: &spec::kernel::PluginConfig) -> spec::kernel::Result<()> {
        ctx.provide(spec::PersonaHandle(self.0.clone()));
        Ok(())
    }
}


// `JsNavigator` WAS HERE: moved to korvus-webapi's `core/navigator/navigator.rs` as an own
// class (the cppgc object `navigator` IS; its prototype is the `Navigator` table's).

/// The current page URL, held in `OpState`; backs `window.location`. Default `about:blank`.
///
/// ONE storage, two holders. `OpState` carries this handle, and so does the subresource loader the
/// kernel registers on the `spec::kernel::Context` -- a `kernel::Service` must be
/// `Send + Sync`, so it cannot hold the `!Send` `OpState`, and a *copy* of the base URL would be a
/// second truth that a navigation could leave stale. The `Arc` makes it the same string in both
/// places: `set` moves the page, and every reader -- `window.location`, `fetch()`'s relative-URL
/// resolution, an element-induced load -- moves with it.
#[derive(Clone)]
struct HostLocation(std::sync::Arc<std::sync::RwLock<String>>);
impl HostLocation {
    fn new(url: impl Into<String>) -> Self {
        HostLocation(std::sync::Arc::new(std::sync::RwLock::new(url.into())))
    }
    /// The current page URL.
    fn get(&self) -> String {
        self.0.read().unwrap().clone()
    }
    /// Move the page. Takes `&self` (not `&mut`): the storage is shared, not owned by `OpState`.
    fn set(&self, url: impl Into<String>) {
        *self.0.write().unwrap() = url.into();
    }
}
impl Default for HostLocation {
    fn default() -> Self {
        HostLocation::new("about:blank")
    }
}

// `NavRequest` WAS HERE. It was this crate's private `Option<String>`, which meant the
// two things that ALSO ask for a top-level navigation -- `window.open(url, "_self")` and a form
// submission whose target is the page -- could not reach it from `korvus-webapi`, and that a POST's
// body had nowhere to travel. The slot is `webapi::PendingNavigation` now, holding a whole
// `webapi::PageNavigation`; one slot, in the crate both callers already link.

// `JsLocation` WAS HERE: a `deno_core` class whose twelve `#[op2]` members
// `singletons::reshape_location` copied onto the instance. The window's `Location` is minted by that
// function now, from a template whose instances have an IMMUTABLE PROTOTYPE (the class could not give
// it one), and every member is `location.rs`'s.



// `JsScreen` WAS HERE: Web API in the kernel, moved to korvus-webapi's
// `core/html/screen.rs` as an own class with its table.

/// Captures `print(...)` output so callers/tests can observe it deterministically.
#[derive(Default)]
struct PrintBuffer(Vec<String>);

/// Has the page fired its `load` event for the current navigation?
///
/// HOST STATE, IN `OpState`, NOT A SLOT ON THE PAGE (it was `KB.__loadFired`). Written by
/// `navigation.rs::set_load_fired` at the two moments the pipeline knows the answer, read by
/// `event_loop.rs::pump_until_loaded` to return a navigation early and by
/// [`Engine::load_fired`] for every reader outside this process. Absent means "not yet": a realm that
/// has never navigated has no record, which is the same answer `false` gives.
#[derive(Default)]
pub(crate) struct LoadFired(pub(crate) bool);

/// Where a host is told the instant the page's `load` event fired, while the load it awaits still holds the engine
/// ([`Engine::mark_load_in`]).
pub type LoadMark = std::rc::Rc<std::cell::Cell<Option<std::time::Instant>>>;

/// The [`LoadMark`] a host gave this engine.
pub(crate) struct LoadMarkSlot(pub(crate) LoadMark);



/// Native `print` — appends to the [`PrintBuffer`] in `OpState`.
#[op2(fast)]
fn op_kprint(state: &mut OpState, #[string] s: String) {
    state.borrow_mut::<PrintBuffer>().0.push(s);
}












// `op_device_pixel_ratio` AND `op_window_geometry` WERE HERE, with the six-number
// FALLBACK CHAIN the second existed to keep in one place. `window.devicePixelRatio` and the other
// twelve geometry attributes are rows of the `Window` table now
// (`core/korvus-webapi/src/core/html/window_geometry.rs`), reading the SAME persona through
// `spec::PersonaHandle` on the kernel `Context` -- the route `performance.memory` took in W4.d. The
// kernel owned them only because it owned `HostPersona` (invariant I9), and their one JS caller was
// `host.js`'s block of `Object.defineProperty` calls on `globalThis`, which is gone with them.
//
// THE CHAIN IS BETTER OFF FOR THE MOVE, not merely relocated: `core/dom/geometry_queries.rs`
// carried a HAND COPY of the inner-size half, because "the cascade cannot call an op", and a comment
// admitting it. Both now call one Rust function, so `@media (height: 982px)` and
// `window.innerHeight` cannot disagree.

// `op_perf_memory` WAS HERE. `performance.memory` is a row of the `Performance` table now
// (`core/korvus-webapi/src/core/timing/performance.rs`), reading the SAME persona through
// `spec::PersonaHandle` on the kernel `Context` -- the runtime owned it only because it owned
// `HostPersona`, and it still leaves the member OFF ENTIRELY for a profile that captured no heap
// numbers (invariant I9).

// `PerfClock`, `op_perf_now` and `op_time_origin` WERE HERE. The clock is `spec::PerfClock` now --
// the kernel's, because two crates need the SAME one: this crate creates one per realm (below, and in
// `worker.rs`), and the `Performance` table in `korvus-webapi` reads it out of `OpState` at CALL time,
// which is what keeps `timeOrigin` snapshot-safe. The 100 us quantization moved with it, comment and
// all: it is a fingerprinting decision, not a resolution bug.

// `op_persona_oracle` WAS HERE. It handed the persona's device-oracle replay data to the
// one JS line that read it -- `js/generated_shells.js`'s `wire_canvas_oracle(op_persona_oracle())`.
// Both the file and both ops are deleted: `Engine::assemble` reads `persona.device_oracle_json()`
// before it moves the persona into the extension and passes it to
// `webapi::install::live::install_window_bootstrap`, so the oracle never crosses the JS boundary at all.

/// Update the current URL (backs `history.pushState`/`replaceState`), resolving relative to the
/// current location.
#[op2(fast)]
fn op_nav_to(state: &mut OpState, #[string] url: String) {
    let current = state.borrow::<HostLocation>().get();
    let joined = url::Url::parse(&current).and_then(|base| base.join(&url));
    let resolved = match joined {
        Ok(u) => u.to_string(),
        // The current location is a cannot-be-a-base URL (e.g. `about:blank`) so a relative
        // path won't resolve. Anchor absolute-path/relative navigations to a default origin so
        // `location.pathname`/`search`/`hash` stay meaningful (matches a real page having an origin).
        Err(_) => url::Url::parse("http://localhost/")
            .and_then(|base| base.join(&url))
            .map(|u| u.to_string())
            .unwrap_or(url),
    };
    state.borrow::<HostLocation>().set(resolved);
}

/// WebCrypto `crypto.getRandomValues` backing — `len` random bytes.
#[op2]
#[buffer]
fn op_random_bytes(#[smi] len: i32) -> Vec<u8> {
    let mut buf = vec![0u8; len.max(0) as usize];
    let _ = getrandom::getrandom(&mut buf);
    buf
}

/// `crypto.randomUUID` backing — a v4 UUID string.
#[op2]
#[string]
fn op_uuid() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// `crypto.subtle.digest("SHA-256", …)` backing.
#[op2]
#[buffer]
fn op_sha256(#[buffer] data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).to_vec()
}

/// `crypto.subtle.digest` backing — SHA-1/256/384/512 by canonical (upper-cased) name.
/// Unknown names return an empty buffer; the JS wrapper rejects those.
#[op2]
#[buffer]
fn op_digest(#[string] algo: String, #[buffer] data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256, Sha384, Sha512};
    match algo.to_ascii_uppercase().as_str() {
        "SHA-1" => sha1::Sha1::digest(data).to_vec(),
        "SHA-256" => Sha256::digest(data).to_vec(),
        "SHA-384" => Sha384::digest(data).to_vec(),
        "SHA-512" => Sha512::digest(data).to_vec(),
        _ => Vec::new(),
    }
}

/// HMAC sign backing (`crypto.subtle.sign`/`verify` for HMAC). Hash selects the digest.
#[op2]
#[buffer]
fn op_hmac_sign(#[string] hash: String, #[buffer] key: &[u8], #[buffer] data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::{Sha256, Sha384, Sha512};
    macro_rules! mac {
        ($d:ty) => {{
            let mut m = <Hmac<$d> as Mac>::new_from_slice(key).expect("hmac takes any key length");
            m.update(data);
            m.finalize().into_bytes().to_vec()
        }};
    }
    match hash.to_ascii_uppercase().as_str() {
        "SHA-1" => mac!(sha1::Sha1),
        "SHA-256" => mac!(Sha256),
        "SHA-384" => mac!(Sha384),
        "SHA-512" => mac!(Sha512),
        _ => Vec::new(),
    }
}

#[derive(serde::Serialize)]
struct CtrOut {
    ok: bool,
    data: Vec<u8>,
}

/// AES-CTR encrypt/decrypt (`crypto.subtle.encrypt`/`decrypt`). `length` is the number of
/// low-order counter bits that increment (the nonce bits above stay fixed and wrap within the
/// sub-counter). Rejects (ok=false) when the message needs more blocks than the counter space
/// (2^length), matching the WebCrypto OperationError. Key length picks AES-128/192/256.
#[op2]
#[serde]
fn op_aes_ctr(
    #[buffer] key: &[u8],
    #[buffer] counter: &[u8],
    #[smi] length: i32,
    #[buffer] data: &[u8],
) -> CtrOut {
    use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
    if counter.len() != 16 || !(1..=128).contains(&length) {
        return CtrOut { ok: false, data: Vec::new() };
    }
    let length = length as u32;
    let num_blocks = data.len().div_ceil(16) as u128;
    if length < 128 && num_blocks > (1u128 << length) {
        return CtrOut { ok: false, data: Vec::new() };
    }
    // One boxed closure per key size so the three AES variants share the CTR loop.
    let encrypt_block: Box<dyn Fn(&mut [u8; 16])> = match key.len() {
        16 => { let c = aes::Aes128::new(GenericArray::from_slice(key)); Box::new(move |b| c.encrypt_block(GenericArray::from_mut_slice(b))) }
        24 => { let c = aes::Aes192::new(GenericArray::from_slice(key)); Box::new(move |b| c.encrypt_block(GenericArray::from_mut_slice(b))) }
        32 => { let c = aes::Aes256::new(GenericArray::from_slice(key)); Box::new(move |b| c.encrypt_block(GenericArray::from_mut_slice(b))) }
        _ => return CtrOut { ok: false, data: Vec::new() },
    };
    let mask: u128 = if length >= 128 { u128::MAX } else { (1u128 << length) - 1 };
    let mut ctr = [0u8; 16];
    ctr.copy_from_slice(counter);
    let mut out = Vec::with_capacity(data.len());
    for block in data.chunks(16) {
        let mut ks = ctr;
        encrypt_block(&mut ks);
        for (i, &b) in block.iter().enumerate() {
            out.push(b ^ ks[i]);
        }
        // Increment only the low `length` bits; high (nonce) bits stay fixed.
        let c = u128::from_be_bytes(ctr);
        let low = (c & mask).wrapping_add(1) & mask;
        ctr = ((c & !mask) | low).to_be_bytes();
    }
    CtrOut { ok: true, data: out }
}

#[derive(serde::Serialize)]
struct X25519KeyPair {
    private: Vec<u8>,
    public: Vec<u8>,
}

/// X25519 key generation (`crypto.subtle.generateKey({name:"X25519"})`).
#[op2]
#[serde]
fn op_x25519_keypair() -> X25519KeyPair {
    use x25519_dalek::{PublicKey, StaticSecret};
    let mut seed = [0u8; 32];
    let _ = getrandom::getrandom(&mut seed);
    let secret = StaticSecret::from(seed);
    let public = PublicKey::from(&secret);
    X25519KeyPair { private: secret.to_bytes().to_vec(), public: public.to_bytes().to_vec() }
}

/// X25519 ECDH (`crypto.subtle.deriveBits`). Returns the low `bits/8` bytes of the shared secret.
#[op2]
#[buffer]
fn op_x25519_derive(#[buffer] private: &[u8], #[buffer] public: &[u8], #[smi] bits: i32) -> Vec<u8> {
    use x25519_dalek::{PublicKey, StaticSecret};
    if private.len() != 32 || public.len() != 32 {
        return Vec::new();
    }
    let mut pk = [0u8; 32];
    let mut sk = [0u8; 32];
    sk.copy_from_slice(private);
    pk.copy_from_slice(public);
    let secret = StaticSecret::from(sk);
    let shared = secret.diffie_hellman(&PublicKey::from(pk));
    let n = ((bits.max(0) as usize) / 8).min(32);
    shared.as_bytes()[..n].to_vec()
}

/// `btoa` — base64-encode a binary (latin1) string.
#[op2]
#[string]
fn op_btoa(#[string] input: String, #[string] iface: String) -> Result<String, deno_error::JsErrorBox> {
    use base64::Engine as _;
    // `btoa` takes a BINARY string: every code point must fit in a byte, and one that does not is an
    // InvalidCharacterError. `c as u8` truncated instead, so `btoa("\u{20ac}")` quietly encoded the
    // wrong byte where Chrome throws.
    let mut bytes = Vec::with_capacity(input.len());
    for c in input.chars() {
        if (c as u32) > 0xFF {
            // The REALM names itself. Chrome says "on 'Window'" in a window and "on
            // 'WorkerGlobalScope'" in a worker (measured over a loopback page that posts the probe
            // into a real dedicated worker); this op is called from both and used to say 'Window' in
            // both.
            return Err(deno_error::JsErrorBox::new(
                "InvalidCharacterError",
                format!("Failed to execute 'btoa' on '{iface}': The string to be encoded contains characters outside of the Latin1 range."),
            ));
        }
        bytes.push(c as u8);
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// `atob` — base64-decode to a binary (latin1) string.
#[op2]
#[string]
fn op_atob(#[string] input: String, #[string] iface: String) -> Result<String, deno_error::JsErrorBox> {
    use base64::Engine as _;
    // The infra spec's "forgiving-base64 decode", which differs from a plain decode in three ways
    // that all showed up as wrong answers: ASCII whitespace is stripped from ANYWHERE, not just the
    // ends (`.trim()` only did the ends); padding may be omitted; and a failure is an
    // InvalidCharacterError, not a TypeError -- `atob("x")` was answering the wrong error class.
    let cleaned: String = input
        .chars()
        .filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}'))
        .collect();
    let bad = || {
        deno_error::JsErrorBox::new(
            "InvalidCharacterError",
            format!("Failed to execute 'atob' on '{iface}': The string to be decoded is not correctly encoded."),
        )
    };
    // The rest of the infra algorithm, step by step: only a length of 4n may shed
    // padding, and at most two `=`; then 4n+1 fails; then any `=` left is a character outside the
    // alphabet; and the leftover bits of a final partial group are DISCARDED, whatever they are.
    // Chrome 150: `atob("YQ=")`, `atob("YWJjZA====")` and `atob("Y Q =")` are InvalidCharacterError, and
    // `atob("YR")` / `atob("YWK")` are "a" / "ab". This trimmed every trailing `=` and decoded with
    // the base64 crate's canonical-bits check, so it accepted the first three and rejected the last
    // two -- strict exactly where real data (a truncated or non-canonical token) is decoded.
    let mut data = cleaned.as_str();
    if data.len() % 4 == 0 {
        data = data.strip_suffix("==").or_else(|| data.strip_suffix('=')).unwrap_or(data);
    }
    if data.len() % 4 == 1 || !data.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
        return Err(bad());
    }
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone),
    );
    engine.decode(data).map(|bytes| bytes.iter().map(|&b| b as char).collect()).map_err(|_| bad())
}

// `op_sleep` WAS HERE: the async sleep the old promise-chain `setTimeout` awaited. The timer task
// source (`scheduler.rs`) owns the deadlines now -- `op_timer_next_due` sleeps to the EARLIEST of
// them, once, instead of one sleep per armed timer -- and no JS in either realm called this any more.

/// What `op_fetch` / `op_fetch_sync` hand JS: the transport's response with its body as a
/// `Uint8Array` over the EXACT bytes (`transport::FetchResponse::bytes`).
///
/// It is a separate type rather than `transport::FetchResponse` itself because the transport crate
/// cannot name `deno_core`'s buffer type, and serialising `Vec<u8>` through serde would hand JS an
/// ARRAY OF NUMBERS -- a 10 MB wasm module as ten million boxed doubles. The text view
/// (`FetchResponse::body`) is not sent at all: every JS consumer now decodes the bytes itself, the way
/// the platform says it must (a Response's `text()` is always UTF-8; XHR's `responseText` follows the
/// charset; `arrayBuffer()` does not decode).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct JsFetchResponse {
    status: u16,
    status_text: String,
    url: String,
    headers: Vec<(String, String)>,
    /// `null` for an opaque or opaque-redirect response: the page may not read it.
    body: Option<deno_core::ToJsBuffer>,
    /// `Response.type`: `basic`, `cors`, `opaque` or `opaqueredirect`.
    #[serde(rename = "type")]
    response_type: &'static str,
    redirected: bool,
    /// The transport's timing of the request, for the realm's resource-timing entry.
    timing: Option<spec::FetchTiming>,
    /// Set when this is only the response's HEAD: the body is still arriving, and
    /// `op_fetch_body(body_id)` answers the whole response once it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    body_id: Option<u32>,
}

impl From<transport::FetchResponse> for JsFetchResponse {
    /// The response AS THE PAGE MAY SEE IT: filtered for its type (`transport::cors::filtered_for_page`).
    /// The ledger keeps the unfiltered one, which is what the network did.
    fn from(r: transport::FetchResponse) -> Self {
        let (status, status_text, url, headers, body) = transport::cors::filtered_for_page(&r);
        JsFetchResponse {
            status,
            status_text,
            url,
            headers,
            body: body.map(Into::into),
            response_type: r.response_type.token(),
            redirected: r.redirected && r.response_type != transport::ResponseType::Opaque,
            timing: Some(r.timing),
            body_id: None,
        }
    }
}

/// Every way a page's fetch fails is ONE rejection to the page: `TypeError: Failed to fetch` (Chrome
/// 150, measured for a refused connection, a CORS failure, a refused preflight, a same-origin-mode
/// cross-origin url, a mixed-content block and `redirect: 'error'`). The transport's own message is
/// what `KORVUS_TRACE_FETCH` prints; the page is not told which of those it was.
fn page_fetch_error(_detail: impl std::fmt::Display) -> deno_error::JsErrorBox {
    deno_error::JsErrorBox::type_error("Failed to fetch")
}

/// Async HTTP fetch backing `globalThis.fetch` (G1.7). Functional; a later layer swaps the client for a
/// fingerprint-matched one behind the same `transport::fetch` API.
// `reentrant`: the FetchRequest dictionary is deserialised by reading properties, and a page may pass
// an init object whose properties are native accessors (`fetch(u, {get headers(){...}})`). Without it
// deno_core's guard panics inside an op frame, which cannot unwind -> the process aborts.
//
// `deferred`: polled in the call that made it, so the request leaves before the page's task
// ends, and settled on a later turn whatever that poll found. A lazy op waited for the event loop's next turn:
// Chrome 150's `fetch()` at the head of a 200 ms task has `fetchStart` 0-2 ms and its response at 52 ms; korvus's
// were 201 and 253.
#[op2(async(deferred), reentrant)]
#[serde]
async fn op_fetch(
    state: std::rc::Rc<std::cell::RefCell<OpState>>,
    #[serde] req: transport::FetchRequest,
    // The requesting document when it is a nested context's; "" for the realm's own.
    // An argument and not a `FetchRequest` member, for the reason `initiator` is `#[serde(skip)]`.
    #[string] client: String,
    // The requesting document's site for cookies: "" the page's own, "null" under a cross-site
    // ancestor, else the top-level URL -- `webapi`'s `realm_site_for_cookies`.
    #[string] site_for_cookies: String,
) -> Result<JsFetchResponse, deno_error::JsErrorBox> {
    page_fetch_whole(state, req, client, site_for_cookies).await
}

/// A `blob:` URL's response from the blob URL store, for the async and the synchronous fetch alike (a
/// synchronous XHR to a blob URL threw NetworkError; Chrome 150 answers it 200 with the Blob's bytes).
fn blob_url_response(state: &OpState, url: &str) -> Option<Result<JsFetchResponse, deno_error::JsErrorBox>> {
    if !url.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("blob:")) {
        return None;
    }
    Some(match webapi::blob_url_bytes(state, url) {
        Some((ty, bytes)) => Ok(JsFetchResponse {
            status: 200,
            status_text: "OK".into(),
            url: url.to_string(),
            headers: if ty.is_empty() { vec![("content-length".into(), bytes.len().to_string())] } else { vec![("content-type".into(), ty), ("content-length".into(), bytes.len().to_string())] },
            body: Some(bytes.as_ref().clone().into()),
            response_type: "basic",
            redirected: false,
            timing: None,
            body_id: None,
        }),
        None => Err(deno_error::JsErrorBox::type_error("Failed to fetch")),
    })
}

/// [`op_fetch`]'s body: the whole response, body and all. [`op_fetch_head`] answers through it too when
/// the url never reaches the network (`data:`, `blob:`).
async fn page_fetch_whole(
    state: std::rc::Rc<std::cell::RefCell<OpState>>,
    mut req: transport::FetchRequest,
    client: String,
    site_for_cookies: String,
) -> Result<JsFetchResponse, deno_error::JsErrorBox> {
    if let Some(r) = data_url_response(&req.url) {
        return r;
    }
    // A `blob:` URL is answered from the blob URL store (Fetch §4.2); the network client refused it, so every
    // `fetch(URL.createObjectURL(blob))` rejected with "Failed to fetch".
    if let Some(r) = blob_url_response(&state.borrow(), &req.url) {
        return r;
    }
    prepare_page_request(&state.borrow(), &mut req, Some(client.as_str()).filter(|c| !c.is_empty()));
    {
        let (site, page_url) = request_site_context(&state.borrow(), site_for_cookies);
        req.cookie_cross_site = transport::cookie_cross_site(&site, &page_url, &req.url);
    }
    apply_persona_headers(&state, &mut req);
    send_through_transport(&state, req, "fetch").await.map(JsFetchResponse::from).map_err(page_fetch_error)
}

/// The whole-response futures of `fetch()` calls answered at their HEAD, and the cancel switch of each
/// body still being read, by body id. The future is parked here half-polled -- the
/// network work runs on the transport's own runtime meanwhile -- until `op_fetch_body` awaits it.
#[derive(Default)]
struct PendingBodies {
    next: u32,
    futures: std::collections::HashMap<u32, std::pin::Pin<Box<dyn std::future::Future<Output = Result<transport::FetchResponse, deno_error::JsErrorBox>>>>>,
    cancels: std::collections::HashMap<u32, std::sync::Arc<tokio::sync::Notify>>,
    /// The body's chunks as they arrive, taken by `op_fetch_body_chunk`.
    chunks: std::collections::HashMap<u32, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
}

/// `op_fetch` that answers at the response's HEAD: Chrome resolves a page's `fetch()` the
/// moment the headers are in and streams the body after -- measured 2026-10-01 (c8/repro/abort): a body
/// trickled over a second resolves `fetch()` at 0 ms, where this engine resolved it at 1000. A page
/// that reads a response's headers and aborts its body (a bot-detection endpoint's loader does, on costco.com) never
/// waited for that body in Chrome, and waited it out here. A response whose body is already complete
/// when the head is noticed (a `data:` or `blob:` url, a small body, an error) comes back whole.
// `deferred`, as [`op_fetch`] is: an XMLHttpRequest's `send()` and a `fetch()` answered at its head go out in the call.
#[op2(async(deferred), reentrant)]
#[serde]
async fn op_fetch_head(
    state: std::rc::Rc<std::cell::RefCell<OpState>>,
    #[serde] mut req: transport::FetchRequest,
    #[string] client: String,
    #[string] site_for_cookies: String,
) -> Result<JsFetchResponse, deno_error::JsErrorBox> {
    let scheme = req.url.get(..5).map(|p| p.to_ascii_lowercase()).unwrap_or_default();
    if scheme == "data:" || scheme == "blob:" {
        return page_fetch_whole(state, req, client, site_for_cookies).await;
    }
    // A plugin that reads or rewrites response bodies (ABI 3, D6) needs the whole body before the page does, so it
    // cannot be streamed chunk by chunk from the network: serve it whole, where `on_response` sees and can replace
    // `resp.bytes` and the page reads the result.
    if transport::plugins_want_bodies() {
        return page_fetch_whole(state, req, client, site_for_cookies).await;
    }
    prepare_page_request(&state.borrow(), &mut req, Some(client.as_str()).filter(|c| !c.is_empty()));
    {
        let (site, page_url) = request_site_context(&state.borrow(), site_for_cookies);
        req.cookie_cross_site = transport::cookie_cross_site(&site, &page_url, &req.url);
    }
    apply_persona_headers(&state, &mut req);
    let (head_tx, mut head_rx) = tokio::sync::oneshot::channel();
    req.on_head = Some(std::sync::Arc::new(std::sync::Mutex::new(Some(head_tx))));
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    req.cancel = Some(cancel.clone());
    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    req.on_chunk = Some(chunk_tx);
    let st = state.clone();
    let mut full: std::pin::Pin<Box<dyn std::future::Future<Output = Result<transport::FetchResponse, deno_error::JsErrorBox>>>> =
        Box::pin(async move { send_through_transport(&st, req, "fetch").await });
    enum First {
        Whole(Result<transport::FetchResponse, deno_error::JsErrorBox>),
        Head(transport::FetchResponse),
        NoHead,
    }
    let first = std::future::poll_fn(|cx| {
        use std::future::Future as _;
        if let std::task::Poll::Ready(r) = full.as_mut().poll(cx) {
            return std::task::Poll::Ready(First::Whole(r));
        }
        match std::pin::Pin::new(&mut head_rx).poll(cx) {
            std::task::Poll::Ready(Ok(h)) => std::task::Poll::Ready(First::Head(h)),
            std::task::Poll::Ready(Err(_)) => std::task::Poll::Ready(First::NoHead),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    })
    .await;
    match first {
        First::Whole(r) => r.map(JsFetchResponse::from).map_err(page_fetch_error),
        // The head channel closed unsent: a transport that answers only whole responses, or a failure
        // before the headers. The whole response (or its error) is the answer.
        First::NoHead => full.await.map(JsFetchResponse::from).map_err(page_fetch_error),
        First::Head(h) => {
            let id = {
                let mut s = state.borrow_mut();
                if !s.has::<PendingBodies>() {
                    s.put(PendingBodies::default());
                }
                let p = s.borrow_mut::<PendingBodies>();
                p.next = p.next.wrapping_add(1);
                let id = p.next;
                p.futures.insert(id, full);
                p.cancels.insert(id, cancel);
                p.chunks.insert(id, chunk_rx);
                id
            };
            let mut head = JsFetchResponse::from(h);
            head.body = None;
            head.timing = None;
            head.body_id = Some(id);
            Ok(head)
        }
    }
}

/// The whole response behind a head `op_fetch_head` answered: its body bytes, and its timing for the
/// resource-timing entry. A body cut short (cancelled, or the connection failed) rejects.
#[op2(async(lazy), fast)]
#[serde]
async fn op_fetch_body(state: std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Result<JsFetchResponse, deno_error::JsErrorBox> {
    let fut = state.borrow_mut().try_borrow_mut::<PendingBodies>().and_then(|p| p.futures.remove(&id));
    let Some(fut) = fut else { return Err(deno_error::JsErrorBox::type_error("network error")) };
    let out = fut.await;
    // the body's end came in with its last bytes: read right after them, it is their arrival's rest
    deno_core::op_continues_arrival();
    if let Some(p) = state.borrow_mut().try_borrow_mut::<PendingBodies>() {
        p.cancels.remove(&id);
    }
    out.map(JsFetchResponse::from).map_err(|_| deno_error::JsErrorBox::type_error("network error"))
}

/// The next chunk of the body behind `id` as it arrives, or `null` once there is no more --
/// the body ended, failed or was cancelled; `op_fetch_body` then says which.
#[op2(async(lazy), fast)]
#[serde]
async fn op_fetch_body_chunk(state: std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<deno_core::ToJsBuffer> {
    let rx = state.borrow_mut().try_borrow_mut::<PendingBodies>().and_then(|p| p.chunks.remove(&id));
    let mut rx = rx?;
    let next = rx.recv().await;
    if next.is_some() {
        if let Some(p) = state.borrow_mut().try_borrow_mut::<PendingBodies>() {
            p.chunks.insert(id, rx);
        }
    } else {
        // the end of the body came in with its last bytes (`deno_core::op_continues_arrival`); a further chunk that is
        // waiting may have come in long after the one before it, and is queued when the loop looks
        deno_core::op_continues_arrival();
    }
    next.map(Into::into)
}

/// Stop reading the body behind `id` (an abort after the headers, or the page cancelled the body).
#[op2(fast)]
fn op_fetch_body_cancel(state: &mut OpState, id: u32) {
    if let Some(p) = state.try_borrow_mut::<PendingBodies>() {
        if let Some(c) = p.cancels.remove(&id) {
            c.notify_one();
        }
        p.futures.remove(&id);
        p.chunks.remove(&id);
    }
}

/// Fetch's scheme fetch for `data:` -- the WHATWG "data: URL processor": the bytes after the first
/// comma, percent-decoded, then forgiving-base64-decoded when the media type ends in `;base64`; the
/// media type (default `text/plain;charset=US-ASCII`) becomes the Content-Type. A `data:` URL never
/// reaches the network, so it never reaches the transport either (which cannot fetch one). `None` for
/// any other scheme.
///
/// WHY IT IS HERE: synchronous XHR made a `data:` request observable -- WPT's
/// `xhr/xmlhttprequest-sync-block-scripts.html` does `open("GET", "data:,", false); send()`, which
/// Chrome answers with status 200 and this engine answered with a thrown NetworkError. The async path
/// takes the same answer rather than a transport error.
fn data_url_response(url: &str) -> Option<Result<JsFetchResponse, deno_error::JsErrorBox>> {
    let rest = url.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).map(|_| &url[5..])?;
    let rest = rest.split('#').next().unwrap_or("");
    let Some((meta, data)) = rest.split_once(',') else {
        return Some(Err(deno_error::JsErrorBox::type_error("Failed to fetch")));
    };
    let mut bytes: Vec<u8> = percent_encoding::percent_decode_str(data).collect();
    let meta = meta.trim_matches(|c: char| c.is_ascii_whitespace());
    let (mime, is_b64) = match meta.len().checked_sub(7).map(|i| (&meta[..i], &meta[i..])) {
        Some((head, tail)) if tail.eq_ignore_ascii_case(";base64") => (head.trim_end_matches([' ', '\t']).to_string(), true),
        _ => (meta.to_string(), false),
    };
    if is_b64 {
        use base64::Engine as _;
        let cleaned: Vec<u8> = bytes.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
        let cleaned = if cleaned.len() % 4 == 0 { cleaned.strip_suffix(b"==").or_else(|| cleaned.strip_suffix(b"=")).map(<[u8]>::to_vec).unwrap_or(cleaned) } else { cleaned };
        match base64::engine::general_purpose::STANDARD_NO_PAD.decode(&cleaned) {
            Ok(b) => bytes = b,
            Err(_) => return Some(Err(deno_error::JsErrorBox::type_error("Failed to fetch"))),
        }
    }
    let mime = if mime.is_empty() || mime.starts_with(';') { format!("text/plain{}", if mime.is_empty() { ";charset=US-ASCII".to_string() } else { mime }) } else { mime };
    Some(Ok(JsFetchResponse {
        status: 200,
        status_text: "OK".into(),
        url: url.to_string(),
        headers: vec![("content-type".into(), mime)],
        body: Some(bytes.into()),
        response_type: "basic",
        redirected: false,
        timing: None,
        body_id: None,
    }))
}

/// The SYNCHRONOUS fetch a synchronous `XMLHttpRequest` (`open(m, u, false)`) blocks on.
///
/// MEASURED on `google-chrome` 150.0.7871.46 before this op existed: a sync XHR of a 256-byte file
/// answers `readyState 4, status 200, responseText.length 256` when `send()` returns, and fires
/// `readystatechange(4)`, `load`, `loadend` inside it. This engine had no synchronous path at all --
/// `open`'s third argument was ignored, `send()` went out asynchronously and returned at once, so the
/// page read `readyState 1` and `responseText.length 0` on the next line. Not a corrupted body: no body.
///
/// The request is the page's (`prepare_page_request`, the same one `op_fetch` runs) and goes out on
/// the SAME transport service; only the wait differs. The transport future is `!Send`, so it is built
/// and driven on a short-lived OS thread with its own current-thread runtime (the request itself hops
/// onto `transport`'s always-running `net_runtime`), and the isolate thread blocks on a std channel --
/// an OS-level wait, safe inside any tokio runtime, which is exactly what a sync XHR is.
///
/// Bounded by `worker::BLOCKING_FETCH_TIMEOUT_MS`, the bound `importScripts` (the engine's other
/// blocking fetch) already uses, for the reason given there: a thread parked in a std recv cannot be
/// cancelled, so an unbounded wait on a server that never answers would freeze the isolate for good.
/// A timeout surfaces as the network error it is.
#[op2(reentrant)]
#[serde]
fn op_fetch_sync(state: &mut OpState, #[serde] mut req: transport::FetchRequest, #[string] client: String) -> Result<JsFetchResponse, deno_error::JsErrorBox> {
    if let Some(r) = data_url_response(&req.url) {
        return r;
    }
    if let Some(r) = blob_url_response(state, &req.url) {
        return r;
    }
    prepare_page_request(state, &mut req, (!client.is_empty()).then_some(client.as_str()));
    if let Some(hp) = state.try_borrow::<HostPersona>() {
        transport::apply_persona_headers(&*hp.0, &mut req);
    }
    let svc = state
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<transport::TransportHandle>())
        .map(|h| h.0.clone());
    let trace = std::env::var_os("KORVUS_TRACE_FETCH").is_some();
    if trace {
        eprintln!("[fetch-sync] -> {} {}", req.method, req.url);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(Err(anyhow::anyhow!("sync fetch runtime: {e}")));
                return;
            }
        };
        let out = rt.block_on(async move {
            match svc {
                Some(svc) => svc.fetch(req).await,
                None => transport::fetch(req).await,
            }
        });
        let _ = tx.send(out);
    });
    let out = rx
        .recv_timeout(std::time::Duration::from_millis(worker::BLOCKING_FETCH_TIMEOUT_MS))
        .map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => {
                anyhow::anyhow!("timed out after {}ms", worker::BLOCKING_FETCH_TIMEOUT_MS)
            }
            std::sync::mpsc::RecvTimeoutError::Disconnected => anyhow::anyhow!("sync fetch thread dropped"),
        })
        .and_then(|r| r)
        .map_err(|e| {
            if trace {
                eprintln!("[fetch-sync] <- ERR {e}");
            }
            page_fetch_error(e)
        });
    if trace {
        if let Ok(r) = &out {
            eprintln!("[fetch-sync] <- {} ({} bytes)", r.status, r.bytes.len());
        }
    }
    out.map(JsFetchResponse::from)
}

/// What every PAGE-initiated request (`fetch()`, XHR -- async or sync) gets before it leaves: its URL
/// resolved against the document base, its initiator, the empty destination, and the `Origin` header
/// Fetch appends. One function so the async and sync ops cannot drift.
///
/// `client` is the URL of the document that made the request when that is NOT this realm's own
/// document -- a nested context's `fetch` (`webapi`'s `fetch_global.rs`), whose request
/// the caller has already resolved against the frame's base. `None` is the realm's own document.
fn prepare_page_request(state: &OpState, req: &mut transport::FetchRequest, client: Option<&str>) {
    // A request made from a WORKER realm is reported on that worker's DevTools target.
    // The window has no `WorkerReporter`, so its requests keep the default -- the page.
    if let Some(r) = state.try_borrow::<worker::WorkerReporter>() {
        req.reporter = r.0.clone();
    }
    // Resolve a relative request URL (fetch("/api"), fetch("path"), protocol-relative "//h/p") against
    // the page location, as a browser does — otherwise wreq rejects it ("relative URL without a base")
    // and a sensor's same-origin telemetry POST never leaves.
    //
    // The page's URL is also this request's INITIATOR, which is what transport computes
    // `Sec-Fetch-Site` and `Referer` from. It is handed over as a field rather than as a pre-baked
    // `referer` header because the referrer policy is per-HOP (a cross-origin redirect trims the
    // referrer to an origin; an https → http hop drops it entirely) and only transport sees the hops.
    // A client in an OPAQUE origin -- a sandboxed frame without `allow-same-origin` -- is marked by webapi
    // (`browsing_context::request_client`): its requests carry `Origin: null` (Chrome 150, measured --
    // a bot-detection script's risk frame `sandbox=allow-scripts srcdoc` fetches with `Origin: null`; korvus sent the page's).
    let (client, opaque) = match client.and_then(|c| c.strip_prefix(webapi::OPAQUE_CLIENT)) {
        Some(c) => (Some(c), true),
        None => (client, false),
    };
    let base = match client {
        Some(c) => c.to_string(),
        None => state.borrow::<HostLocation>().get(),
    };
    // A `blob:` worker's environment URL is its blob URL, which no referrer may name and no site
    // matches: its requests are initiated by the blob's ORIGIN, with no `Referer`
    // (Brave 151 and Chrome 150: `Sec-Fetch-Site: same-origin`, no Referer; korvus said `none`).
    let (base, blob_client) = match blob_origin(&base) {
        Some(origin) => (origin, true),
        None => (base, false),
    };
    // A blob minted in an opaque origin (`blob:null/<uuid>`: a sandboxed frame's worker) is opaque too -- its requests
    // carry `Origin: null` and are cross-site to everything (Chrome 150, measured; korvus sent them as the browser's
    // own, `Sec-Fetch-Site: none`, no `Origin`).
    let opaque = opaque || url::Url::parse(&base).is_ok_and(|u| u.scheme() == "blob" && !u.origin().is_tuple());
    // ... and no request from an opaque origin names a referrer (Referrer Policy "determine request's referrer":
    // "If document's origin is an opaque origin, return no referrer"; Chrome 150, measured).
    req.no_referrer |= blob_client || opaque;
    req.opaque_origin |= opaque;
    // A dedicated worker's requests carry no Client Hints; a window's know their top-level document.
    match state.try_borrow::<worker::WorkerScope>() {
        Some(w) => {
            req.no_client_hints |= w.kind == "dedicated";
            // A dedicated worker's requests are its creator's, network isolation and all
            // (`transport::partition`): they key by the top-level document that started it.
            if !w.top_level_url.is_empty() {
                req.top_level_url = Some(w.top_level_url.clone());
            }
        }
        None => req.top_level_url = Some(state.borrow::<HostLocation>().get()),
    }
    // A relative URL resolves against the document's BASE url, not its location: `<base href>`
    // moves `fetch("x")` and `xhr.open(m, "x")` in Chrome exactly as it moves `a.href`.
    // The RULE lives once, in `webapi::document_base_url`; this is only the plumbing that
    // reaches this realm's DOM, and a realm that has none (a worker) keeps its location. A frame's
    // request arrives already resolved against the frame's base, so its client URL is the base.
    let resolve_base = match (client, state.try_borrow::<webapi::HostDom>()) {
        (None, Some(h)) => webapi::document_base_url(&h.0, &base),
        _ => base.clone(),
    };
    if let Ok(b) = url::Url::parse(&base) {
        if url::Url::parse(&req.url).is_err() {
            if let Ok(joined) = url::Url::parse(&resolve_base).ok().map_or_else(|| b.join(&req.url), |rb| rb.join(&req.url)) {
                req.url = joined.to_string();
            }
        }
        if b.scheme().starts_with("http") {
            req.initiator = Some(base.clone());
        }
        // A page `fetch()`/XHR has the EMPTY destination; `Sec-Fetch-Dest` serialises that as
        // `empty`. It is set here rather than trusted from JS (see FetchRequest::destination).
        req.destination = transport::Destination::default();
        // A request from an http(s) document or worker is a PAGE request, which the
        // transport taints and CORS-checks (`transport/src/cors.rs`) -- and whose `Origin` it writes
        // per redirect hop, since only it sees the hops. The author's headers are captured BEFORE the
        // persona's are added, because they are what decides a preflight.
        req.method = transport::cors::normalize_method(&req.method);
        if b.scheme() == "http" || b.scheme() == "https" || opaque {
            req.page = Some(transport::PageRequest {
                origin: if opaque { "null".to_string() } else { b.origin().ascii_serialization() },
                author_headers: req.headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("origin")).cloned().collect(),
                // FALSE for `fetch()` and XHR, measured: a same-origin `fetch(url)` is mode `cors`
                // and Chrome sends it NO `Origin`. Only a loader that asked for CORS itself does --
                // see `transport::PageRequest::cors_preconfigured`, and `modules.rs` for the one
                // call site in this engine that sets it.
                cors_preconfigured: false,
            });
            // THE CACHE MODE'S HEADERS (Fetch "HTTP-network-or-cache fetch"), added AFTER
            // the author headers were captured: they are the browser's, so they never make a request
            // non-simple. `no-cache` adds `cache-control: max-age=0`; `no-store` and `reload` add
            // `pragma: no-cache` and `cache-control: no-cache` -- each only when the page set none.
            // Chrome 150, measured on the wire; the transport's order table puts both after `connection`.
            let has = |req: &transport::FetchRequest, n: &str| req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
            match req.cache.as_deref() {
                Some("no-cache") if !has(req, "cache-control") => req.headers.push(("cache-control".into(), "max-age=0".into())),
                Some("no-store") | Some("reload") => {
                    if !has(req, "pragma") {
                        req.headers.push(("pragma".into(), "no-cache".into()));
                    }
                    if !has(req, "cache-control") {
                        req.headers.push(("cache-control".into(), "no-cache".into()));
                    }
                }
                _ => {}
            }
            return;
        }
        // Origin, per Fetch's "append the Origin header": always for a method other than
        // GET/HEAD, and for a CORS request whose target is not same-origin with the initiator.
        let has_origin = req.headers.iter().any(|(h, _)| h.eq_ignore_ascii_case("origin"));
        let non_get = !req.method.eq_ignore_ascii_case("GET") && !req.method.eq_ignore_ascii_case("HEAD");
        let cross_origin_cors = req.mode == transport::Mode::Cors
            && url::Url::parse(&req.url).map(|t| t.origin() != b.origin()).unwrap_or(false);
        if !has_origin && (non_get || cross_origin_cors) {
            if let Some(origin) = b.origin().is_tuple().then(|| b.origin().ascii_serialization()) {
                req.headers.push(("origin".into(), origin));
            }
        }
    }
}

/// The origin of a `blob:` URL, as an origin URL (`http://h:p/`), or `None` for any other URL.
pub(crate) fn blob_origin(u: &str) -> Option<String> {
    let parsed = url::Url::parse(u).ok().filter(|p| p.scheme() == "blob")?;
    let o = parsed.origin();
    o.is_tuple().then(|| format!("{}/", o.ascii_serialization()))
}

/// The site context a page request is judged from for cookies: the realm's own
/// `site_for_cookies` (`""` = its own document), a worker's inherited from the document that created it
/// (a worker started by a cross-site frame is in that frame's cross-site context), and the
/// document URL -- a blob worker's ORIGIN, not the blob URL no site can match.
fn request_site_context(state: &OpState, site_for_cookies: String) -> (String, String) {
    let page_url = state.borrow::<HostLocation>().get();
    let page_url = blob_origin(&page_url).unwrap_or(page_url);
    let site = match (site_for_cookies.is_empty(), state.try_borrow::<worker::WorkerScope>()) {
        (true, Some(w)) => w.site_for_cookies.clone(),
        _ => site_for_cookies,
    };
    (site, page_url)
}

/// Put the PERSONA's identity headers on the WIRE, reading the persona out of `OpState`.
///
/// The headers themselves are built by [`transport::apply_persona_headers`], which is their ONE
/// builder (W9.4). This wrapper exists only because the op path reaches its persona through the
/// `!Send` `OpState` while the subresource loader is a `Send + Sync` `kernel::Service` holding one
/// directly — two ways to reach the same identity, not two statements of what it is.
fn apply_persona_headers(state: &std::rc::Rc<std::cell::RefCell<OpState>>, req: &mut transport::FetchRequest) {
    let st = state.borrow();
    let Some(hp) = st.try_borrow::<HostPersona>() else { return };
    transport::apply_persona_headers(&*hp.0, req);
}

/// Route `req` through the transport service on the kernel Context (a transport plugin provided it), so
/// a proxy/alternate transport can be swapped in. Falls back to the default fetch if unset (e.g.
/// workers). `tag` names the caller in `KORVUS_TRACE_FETCH` output.
async fn send_through_transport(
    state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    req: transport::FetchRequest,
    tag: &str,
) -> Result<transport::FetchResponse, deno_error::JsErrorBox> {
    let trace = std::env::var_os("KORVUS_TRACE_FETCH").is_some();
    if trace {
        eprintln!("[{tag}] -> {} {} (dest={} mode={})", req.method, req.url, req.destination.token(), req.mode.token());
    }
    let transport_svc = state
        .borrow()
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<transport::TransportHandle>())
        .map(|h| h.0.clone());
    let out = match transport_svc {
        Some(svc) => svc.fetch(req).await,
        None => transport::fetch(req).await,
    }
    .map_err(|e| deno_error::JsErrorBox::type_error(e.to_string()));
    if trace {
        match &out {
            Ok(r) => eprintln!("[{tag}] <- {} ({} bytes)", r.status, r.bytes.len()),
            Err(e) => eprintln!("[{tag}] <- ERR {e}"),
        }
    }
    out
}

/// The engine's OWN subresource loader: fetch `url` as a `destination`/`mode` request initiated by the
/// current document. Backs the element-induced loads (`<img src>`, `<script src>`, `<link href>`, …)
/// that `core/html/subresource.rs` (korvus-webapi) starts when an element's source attribute is set or
/// the element is inserted, and the document fetch a browsing-context navigation issues.
///
/// Separate from `op_fetch` on purpose. `op_fetch` is the PAGE-facing op and deliberately refuses to
/// take a destination from JS (a page that could name its own `Sec-Fetch-Dest` would defeat the header);
/// this one is not an op at all -- it is a kernel SERVICE, and its only caller is the engine's own
/// element-loading code, which knows what kind of resource it is loading because it read the tag.
///
/// Everything it needs is held directly, because a `kernel::Service` is `Send + Sync` and `OpState` is
/// not: the page location (the SAME `Arc` `OpState` carries, so a navigation moves both), the worn
/// identity, and the transport the plugin loader resolved.
struct RuntimeSubresourceLoader {
    location: HostLocation,
    persona: std::sync::Arc<dyn Persona>,
    transport: Option<std::sync::Arc<dyn transport::TransportService>>,
    /// Image bodies, kept for the canvas; the same store the kernel `Context` provides.
    images: spec::FetchedImages,
    /// Media headers, read here from each `video`/`audio` body; the same store the
    /// kernel `Context` provides to the media element.
    media: webapi::MediaProbes,
    /// Whether each `font` body parses as a font, for `FontFace.load()`; the body itself
    /// never reaches the page's realm.
    fonts: webapi::FontProbes,
}

impl spec::SubresourceLoader for RuntimeSubresourceLoader {
    fn fetch(&self, req: spec::SubresourceRequest) -> spec::SubresourceFuture {
        let base = self.location.get();
        let persona = self.persona.clone();
        let transport_svc = self.transport.clone();
        let images = self.images.clone();
        let media = self.media.clone();
        let fonts = self.fonts.clone();
        Box::pin(async move {
            // The page's URL is the top-level document's; the initiator is the requesting
            // document's -- a frame's own -- and its URL already resolved against its base.
            let top_level = base.clone();
            // a client in an opaque origin is marked by webapi (`browsing_context::request_client`)
            let (client_url, opaque) = match req.client_url.strip_prefix(webapi::OPAQUE_CLIENT) {
                Some(c) => (c.to_string(), true),
                None => (req.client_url.clone(), false),
            };
            let base = if client_url.is_empty() { base } else { client_url };
            let b = url::Url::parse(&base).map_err(|_| "no document base URL".to_string())?;
            let resolved =
                b.join(&req.url).map_err(|_| format!("cannot resolve subresource URL: {}", req.url))?;
            let mode = match req.mode.as_str() {
                "cors" => transport::Mode::Cors,
                "same-origin" => transport::Mode::SameOrigin,
                // A browsing context's own document is fetched with mode `navigate` — the one mode a
                // page cannot ask for and the engine's popup/frame loader must. Without it a
                // `window.open` went out as `Sec-Fetch-Mode: no-cors`, which no browser ever sends for
                // a document.
                "navigate" => transport::Mode::Navigate,
                _ => transport::Mode::NoCors,
            };
            // A same-origin request for another origin is a network error before anything is sent (Fetch
            // "main fetch"): a `<track>` whose media element has no `crossorigin`, pointed elsewhere.
            if mode == transport::Mode::SameOrigin && resolved.origin() != b.origin() {
                return Err(format!("request mode same-origin: {resolved} is another origin"));
            }
            // Cross-site from the requesting document's site for cookies; a frame's own navigation is
            // judged from its parent's.
            let cross = transport::cookie_cross_site(&req.site_for_cookies, &base, resolved.as_str());
            let mut freq = transport::FetchRequest {
                headers: Vec::new(),
                ..transport::FetchRequest::subresource(resolved.to_string(), &req.destination, mode, base)
            };
            freq.cookie_cross_site = cross;
            freq.top_level_url = Some(top_level);
            freq.referrer_policy = transport::ReferrerPolicy::from_token(&req.referrer_policy);
            // an opaque-origin document's load names no referrer and is cross-site to every URL
            freq.no_referrer |= opaque;
            freq.opaque_origin = opaque;
            // A `method=post` form submission into a nested context. The body is the
            // form's own serialisation and travels under the encoding the form chose; every other
            // element-induced load leaves the request's default `GET` and no body.
            if !req.method.is_empty() && !req.method.eq_ignore_ascii_case("get") {
                freq.method = req.method.to_ascii_uppercase();
            }
            if let Some(body) = req.body {
                freq.body = Some(body.into_bytes());
            }
            if let Some(ct) = req.content_type {
                freq.headers.push(("content-type".into(), ct));
            }
            // A load its element made CORS -- `crossorigin`, a module script, a font -- carries `Origin`
            // whatever its target, same-origin too: Blink's `SetCrossOriginAccessControl` puts it in the
            // request's header map (`fetch_parameters.cc`), which is also why the transport writes it first
            // there. Measured (Chrome 150): a same-origin `<img crossorigin>`, a
            // script-inserted module script and a same-origin `@font-face` all carry it; korvus sent it
            // only across origins. The credentials mode is the element's (`use-credentials` includes them).
            // A MEDIA element's load is the exception: Blink's WebMediaPlayer fetches through its own loader, which
            // sets the CORS mode and not the header, so Fetch's rule holds -- `Origin` only across origins. Chrome 150
            // (`U-video-source-requests`, three runs alike): a same-origin `<video crossorigin>` source asks in mode
            // cors with no `Origin`; korvus sent the page's.
            let media_element = matches!(req.destination.as_str(), "video" | "audio");
            if mode == transport::Mode::Cors && (opaque || b.origin().is_tuple()) && (!media_element || opaque || resolved.origin() != b.origin()) {
                let origin = if opaque { "null".to_string() } else { b.origin().ascii_serialization() };
                freq.headers.push(("origin".into(), origin));
            }
            if req.credentials == "include" {
                freq.credentials = transport::CredentialsMode::Include;
            }
            // Blink's load priority for it, as its loader computed it (`korvus-webapi`'s `load_priority`).
            freq.priority = req.priority.clone();
            // a navigation's own start: when it was initiated (its document's time origin)
            freq.navigation_start = req.navigation_start;
            transport::apply_persona_headers(&*persona, &mut freq);
            let trace = std::env::var_os("KORVUS_TRACE_FETCH").is_some();
            if trace {
                eprintln!(
                    "[subresource] -> {} {} (dest={} mode={})",
                    freq.method,
                    freq.url,
                    freq.destination.token(),
                    freq.mode.token()
                );
            }
            let out = match &transport_svc {
                Some(svc) => svc.fetch(freq).await,
                None => transport::fetch(freq).await,
            };
            if trace {
                match &out {
                    Ok(r) => eprintln!("[subresource] <- {} ({} bytes)", r.status, r.bytes.len()),
                    Err(e) => eprintln!("[subresource] <- ERR {e}"),
                }
            }
            // AN IMAGE'S BODY IS KEPT for the canvas: `drawImage(img)` paints these bytes
            // and taints on these headers, long after the element's `load` event consumed the status.
            // A network failure is kept too, as status 0, because a failed image is BROKEN (drawImage
            // throws) where an image never requested is merely unavailable (drawImage draws nothing).
            if req.destination == "image" {
                let (status, final_url, headers, bytes) = match &out {
                    Ok(r) => (r.status, r.url.clone(), r.headers.clone(), r.bytes.clone()),
                    Err(_) => (0, resolved.to_string(), Vec::new(), Vec::new()),
                };
                images.insert(
                    resolved.as_str(),
                    &req.mode,
                    spec::FetchedImage { status, final_url, headers, bytes: std::sync::Arc::new(bytes) },
                );
            }
            // A MEDIA BODY STOPS HERE. The element needs its header -- the container,
            // the duration, the frame size -- the header is read now, the answer is stored for the element,
            // and the page's realm gets no body. A video's body goes on to the element's decoder.
            let is_media = matches!(req.destination.as_str(), "video" | "audio");
            if is_media {
                if let Ok(r) = &out {
                    if (200..300).contains(&r.status) {
                        // a video's body is kept for its element to decode (`webapi`'s `video_pipe`)
                        let probe = webapi::media_probe(&r.bytes);
                        media.insert(&req.url, probe.clone());
                        if req.destination == "video" {
                            media.keep_body(&req.url, probe.as_ref(), &r.bytes);
                        }
                    }
                }
            }
            // A FONT body stops here too: the probe is the answer `FontFace.load()` needs.
            let is_font = req.destination == "font";
            if is_font {
                if let Ok(r) = &out {
                    let ok = (200..300).contains(&r.status) && webapi::font_probe(&r.bytes);
                    fonts.insert(&req.url, ok);
                    if ok {
                        fonts.keep_body(&req.url, &r.bytes);
                    }
                }
            }
            // A BROWSING CONTEXT'S DOCUMENT IS DECODED AS ITS TOP-LEVEL TWIN IS: BOM, the network stack's
            // charset, the `<meta>` prescan, the persona's locale (`decode.rs`, as `Engine::load_page_delivered`). It
            // reached the frame as the transport's UTF-8 view, so an EUC-KR or Shift_JIS frame read as mojibake and
            // reported `characterSet` "UTF-8" -- Chrome 150 reads the frame's own (`~/bench-a reference engine/obj/site/krf.html`:
            // `가|EUC-KR`), and WPT's `encoding/legacy-mb-*` reads every character through such a frame.
            let navigate = mode == transport::Mode::Navigate;
            let locale = persona.navigator().languages.first().cloned().unwrap_or_default();
            let parent = req.parent_encoding.clone().zip(req.parent_origin.clone());
            out.map(|r| {
                let (body, encoding) = if navigate {
                    let ct = transport::mime::mime_type_and_charset(&r.headers);
                    let is_xml = crate::navigation::xml_document(&ct.mime, &r.url);
                    // AN IMAGE'S NAVIGATION KEEPS ITS BYTES AS AN IMAGE'S, for its image document's `<img>`
                    // (`webapi`'s `subresource::ImageDocumentContent`): Blink feeds that element from this response, and
                    // korvus's asked the network a second time. Status 200: that image has no loader to fail an HTTP
                    // error status; a frame shows a 2xx image only (`frame_document_source`).
                    if (200..300).contains(&r.status) && !is_xml && crate::text_viewer::viewer_for(&ct.mime) == Some(crate::text_viewer::Viewer::Image) {
                        let url = url::Url::parse(&r.url).map(|u| u.to_string()).unwrap_or_else(|_| r.url.clone());
                        let img = spec::FetchedImage { status: 200, final_url: url.clone(), headers: r.headers.clone(), bytes: std::sync::Arc::new(r.bytes.clone()) };
                        images.insert(&url, "no-cors", img);
                    }
                    let (mut encoding, confidence, bom) = crate::decode::sniff(&r.bytes, Some(&ct.charset), is_xml, &locale);
                    // THE PARENT'S ENCODING, before the locale's default (Blink `TextResourceDecoderBuilder`,
                    // `CanReferToParentFrameEncoding` -> `kEncodingFromParentFrame`): no BOM, no charset on the wire, no
                    // usable `<meta>`, and a parent that can access the frame (same origin) -- the frame decodes as its
                    // parent does. Without it an unlabelled frame of a UTF-8 page read as windows-1252 (WPT
                    // `encoding/unsupported-labels.window.js`; `content_type_live` frames c-k: Chrome 150 "UTF-8"). XML
                    // keeps its UTF-8 default, as Blink's XML decoder does.
                    if !is_xml && matches!(confidence, crate::decode::Confidence::Detected) {
                        if let Some((parent_enc, parent_origin)) = &parent {
                            let same_origin = url::Url::parse(&r.url).ok().map(|u| u.origin()).filter(|o| o.is_tuple()).map(|o| o.ascii_serialization());
                            if same_origin.as_deref() == Some(parent_origin.as_str()) {
                                if let Some(e) = encoding_rs::Encoding::for_label(parent_enc.as_bytes()) {
                                    encoding = e;
                                }
                            }
                        }
                    }
                    let (text, _) = encoding.decode_without_bom_handling(&r.bytes[bom..]);
                    (text.into_owned(), Some(encoding.name().to_string()))
                } else if is_media || is_font {
                    (String::new(), None)
                } else {
                    (r.body, None)
                };
                spec::SubresourceResponse { status: r.status, url: r.url, headers: r.headers, body, timing: Some(r.timing), encoding }
            })
            .map_err(|e| e.to_string())
        })
    }
}

// `JsUrl` WAS HERE. The `URL` class -- a cppgc `#[op2] impl` with twelve accessors,
// whose `new` reached the page only through a `Proxy` in `src/js/host.js` because op2's `#[string]`
// parameter does not coerce a JS object -- moved to `core/korvus-webapi/src/modules/url/url.rs`,
// where it is an `interface!` table with a REAL Rust constructor that does the WHATWG `USVString`
// conversion itself. The kernel defines no Web API (invariant I9); it keeps the `url` crate only for
// its own navigation/base resolution.

// `JsTextEncoder` / `JsTextDecoder` WERE HERE: Web API in the kernel, moved to
// korvus-webapi's `core/encoding/text_codec.rs` as own classes with their tables.

extension!(
    korvus_ext,
    ops = [modules::op_queue_module, oopif::op_oopif_post_parent, oopif::op_oopif_parent_recv, op_kprint, op_fetch, op_fetch_head, op_fetch_body, op_fetch_body_chunk, op_fetch_body_cancel, op_fetch_sync, op_nav_to, op_random_bytes, op_uuid, op_sha256, op_digest, op_hmac_sign, op_aes_ctr, op_x25519_keypair, op_x25519_derive, op_btoa, op_atob,
        // The timer task source (scheduler.rs). `op_timer_next_due` is the task boundary
        // -- one callback per event-loop turn, with the microtask checkpoint in between.
        scheduler::op_timer_set, scheduler::op_timer_clear, scheduler::op_timer_pump_claim, scheduler::op_timer_next_due, scheduler::op_timer_run,
        // The FACTORY the `Window` table's five timer rows call, the same shape
        // `op_worker_scope_fn` serves the two worker scope tables. It replaces the five native-mask
        // call sites in `engine.rs`'s `BOOTSTRAP`, which presented JS arrows as `[native code]`.
        scope_timers::op_window_scope_fn,
        // ICE gathering's sockets and STUN (ice.rs), which `core/webrtc/peer_connection.rs` drives.
        ice::op_ice_open, ice::op_ice_routable, ice::op_ice_stun, ice::op_ice_turn, ice::op_ice_close,
        // The WebRTC send path's DTLS-SRTP handshake over a gathered socket + its outbound-rtp counters (rtc_media.rs).
        rtc_media::op_rtc_dtls_connect, rtc_media::op_rtc_outbound,
        // The WebSocket kernel service (websocket.rs): the `WebSocket` table's mojo pipe to
        // `transport::websocket` -- open, the receive pump's next event, send, close, abort.
        websocket::op_ws_open, websocket::op_ws_next, websocket::op_ws_send_text, websocket::op_ws_send_binary, websocket::op_ws_close, websocket::op_ws_abort,
        // Real multi-isolate Worker (spawn/recv are engine-side; post/terminate are native
        // functions installed on Worker.prototype -> [native code], killing the last surface_parity LIES).
        worker::op_worker_spawn, worker::op_worker_start, worker::op_worker_recv, worker::op_worker_bc_recv, worker::op_worker_port_recv, worker::op_worker_err_recv, worker::op_worker_timing_recv, worker::op_worker_post_fn, worker::op_worker_terminate_fn, worker::op_msg_deserialize, worker::op_worker_msg_in, worker::op_shared_worker_connect_fn, worker::op_service_worker_register_fn, worker::op_service_worker_connect_fn,
        // natives.rs's four URL statics (op_url_can_parse / op_url_parse / op_url_create_object_url /
        // op_url_revoke_object_url) WERE HERE. They are now `static_method` rows of the
        // `URL` table in korvus-webapi, and the file is gone with them.
        // THE CSSOM / MEDIA-QUERY / COMPUTED-STYLE OPS ARE NOT HERE ANY MORE. The whole
        // of `cssom.rs` moved to `core/korvus-webapi/src/core/css/cssom.rs` -- the kernel defines no
        // Web API (invariant I9), and the CSSOM tables that call these live in that crate. They are
        // registered by its own `webapi_ops!` under the SAME names, so `host.js`'s
        // `op_css_computed_all` / `op_media_matches` / `op_media_serialize` reach them unchanged.
        ],
    // `objects = [JsNavigator, JsScreen, JsTextEncoder, JsTextDecoder]` WAS HERE: the four `deno_core` classes
    // the kernel registered -- Web API, against invariant I9 -- are korvus-webapi's own classes since
    // Templates (`core/encoding/text_codec.rs`, `core/html/screen.rs`, `core/navigator/navigator.rs`).
    // `esm_entry_point` AND `esm` ARE GONE, and so is `core/korvus-runtime/src/js/`.
    // `host.js` was the last file; what it still did at the end was mint the bridge HOLDER, which is
    // `webapi::binding::hidden::mint_bridge_holder` now -- the first statement of
    // `install::live::install_window_bootstrap`, which is the statement immediately after the
    // `JsRuntime::new` that used to evaluate this module. The kernel ships no JavaScript
    // (invariant I9): it has ops, cppgc objects and state, and nothing that defines a Web API member.
    options = { persona: std::sync::Arc<dyn Persona> },
    // Runs at extension init — BEFORE esm_entry_point evaluates — so the persona is available to
    // op_nav_user_agent when host.js constructs the native navigator.
    state = |state, options| {
        state.put(PrintBuffer::default());
        state.put(scheduler::Scheduler::default()); // This realm's timer task source.
        // its rendering opportunity after every task (`scheduler::run_due_frame`)
        state.put(webapi::AfterTaskHook(scheduler::run_due_frame));
        let location = HostLocation::default();
        state.put(location.clone());
        state.put(webapi::PendingNavigation::default());
        // The realm's high-resolution clock, created HERE so `performance.timeOrigin` is when the realm
        // started rather than when a page first asked. `core/timing/performance.rs` reads it.
        state.put(spec::PerfClock::default());
        // Plugin-core bridge (A3): one kernel Context in OpState; core plugins register services on it.
        // render → RenderHandle (canvas reads ctx.get); persona → PersonaHandle. Swapping a plugin here
        // swaps that capability without touching consumers.
        let persona: std::sync::Arc<dyn Persona> = options.persona;
        let ctx = spec::kernel::Context::root();
        // this thread's fonts and documents are the worn persona's (`page::font::worn_font_map`): its standard font is the
        // default of every document made from here on, the page plugin's first one included
        page::font::set_worn_font_map(persona.theme().map_or(0, |t| t.font_map));
        let mut loader = spec::kernel::Loader::new();
        // and render is one too: `KORVUS_RENDER=off` leaves it out (`render::wanted`)
        if render::wanted() {
            loader.register(render::RenderPlugin);
        }
        // The page pipeline is a *plugin*, and the design's whole point is that a consumer who does
        // not want it can leave it out: an embedder that only wants `fetch` should not pay for a
        // cascade. `KORVUS_NO_PAGE=1` is how that path is exercised -- without it, every consumer's
        // "what if the service is absent" branch is code nothing ever runs.
        //
        // Leaving it out means no CSS at all: no computed styles and zero geometry, which is what
        // this engine did before the pipeline existed. Nothing throws, which is the contract.
        if std::env::var("KORVUS_NO_PAGE").as_deref() != Ok("1") {
            loader.register(page::PagePlugin);
        }
        loader.register(PersonaPlugin(persona.clone()));
        loader.register(transport::TransportPlugin);
        let seed = engine::motor_seed(&*persona);
        let wheel = behavior::WheelKind::from_platform(&persona.navigator().platform);
        loader.register(behavior::BehaviorPlugin::new(seed, wheel));
        // korvus-analysis Layer 1: the call recorder's writer, when KORVUS_ANALYSIS_OUT is set.
        #[cfg(feature = "analysis")]
        loader.register(analysis::AnalysisPlugin);
        loader
            .apply(&ctx, &spec::kernel::PluginConfig::default())
            .expect("core plugins apply");
        // The element-induced load is a kernel SERVICE, not an op. `korvus-webapi` owns
        // the algorithm (which element/attribute induces a request, with what destination and mode)
        // and reaches the network only through this; the kernel owns the page location, the worn
        // identity and the transport. Registered AFTER `apply` so the transport the plugin loader
        // resolved is the one it carries -- and so the service does not hold the `Context` that holds
        // it.
        // The image bodies those loads fetch, readable by the canvas: ONE store, held by
        // the loader that writes it and provided on the Context the canvas reads.
        let images = spec::FetchedImages::default();
        ctx.provide(images.clone());
        let media = webapi::MediaProbes::default();
        ctx.provide(media.clone());
        let fonts = webapi::FontProbes::default();
        ctx.provide(fonts.clone());
        ctx.provide(spec::SubresourceFetch(std::sync::Arc::new(RuntimeSubresourceLoader {
            location,
            persona: persona.clone(),
            transport: ctx.get::<transport::TransportHandle>().map(|h| h.0.clone()),
            images,
            media,
            fonts,
        })));
        // The host of this engine's remote frames (each child engine gets its own, for frames
        // nested in it).
        ctx.provide(spec::RemoteFrames(std::sync::Arc::new(oopif::Host::new(persona.clone()))));
        state.put(ctx);
        // Bridge the plugin's persona into OpState for the ~30 navigator/screen/UA-CH ops that still read
        // HostPersona directly (migrating those reads to ctx.get::<PersonaHandle>() is a follow-up).
        state.put(HostPersona(persona));
        state.put(worker::WorkerRegistry::default()); // Live Worker isolates by id.
        state.put(worker::SharedWorkerRegistry::default()); // Keyed SharedWorker isolates.
        state.put(worker::ServiceWorkerRegistry::default()); // ServiceWorker registrations.
    },
);

// THE PAGE PIPELINE'S FIVE JS DRIVERS ARE NOT HERE ANY MORE. `INSTALL_DOCUMENT`,
// `START_PARSED_SUBRESOURCES`, `RUN_PAGE_SCRIPTS`, `LIFECYCLE_EVENTS` and `ACTIVATE_BODY_HANDLERS`
// were ~200 lines of JavaScript, `execute_script`'d into the page's own realm, reaching every Web API
// they needed by NAME off the hidden bridge's `__ops` holder inside a `try {} catch (e) {}`. They are
// `navigation.rs` and `script_exec.rs` now -- read those two files for what each step does, why its
// position in the sequence was measured rather than chosen, and what the string shape cost.
//
// Two page-visible slots went with them: `KB.__kx` (the staged external script bodies) and
// `KB.__xmlDoc` (which scan the driver should use) are arguments to a Rust call now
// (`script_exec::Staged`) rather than state on a holder a page could reach.
