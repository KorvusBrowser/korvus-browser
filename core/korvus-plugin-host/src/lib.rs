//! The plugin host: korvus's run-time plugins (goal 1020 step 2, D13-D15).
//!
//! A plugin is a shared library built standalone against ONE header, `plugin-sdk/korvus_plugin.h`, which includes
//! nothing of korvus's. This crate is that header's other side: the `#[repr(C)]` mirror of its types, the loader
//! (`dlopen`/`LoadLibrary` through `libloading`), and the dispatch of each slot in the order the header sets. It holds
//! no V8 and no network code: the crate that owns a point -- webapi a member's exit, transport a request's hop, the
//! router a context's making -- calls in here with its side of the call behind a trait ([`CallCtx`],
//! [`RequestCtx`], [`ResponseCtx`]), and this crate turns each into the opaque handle and the host functions a plugin
//! sees.
//!
//! **Nothing loaded, nothing changed (invariant I9).** Every entry point starts from [`any`]: with no plugin it
//! answers at once, and a Web API member no plugin claims is built exactly as before ([`claims`], invariant I7).
//!
//! A plugin is trusted code, run with korvus's own rights (D14): a crash in it is a crash of its process.
#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// The ABI this host speaks (`KORVUS_PLUGIN_ABI`): 2 adds readbacks, a context's exit IP and the worn persona; 3 adds
/// the metric sink (`metric`), the page-eval primitive (`page_eval`), plugin CDP domains and the response-body/
/// connection host calls, then wave 1 (the RNG seed, viewport, input injection and screenshot host calls, and the
/// on_call_pre, verify_cert, init_script, frame-tap, dialog and download plugin slots), then step 5 (the storage
/// slot, `storage_load` / `storage_save`), then step 9 (virtual time: `virtual_time_freeze` /
/// `virtual_time_advance` / `virtual_time_resume`), then step 8 (multi-tenancy: the `on_connect` / `context_allow`
/// / `queue_order` plugin slots, and the `hibernate_context` / `resume_context` / `kv_get` / `kv_set` router host
/// calls), then step 10 (the `navigate` / `reload` / `add_binding` host calls -- B5 drive the page, B4 page
/// bindings -- and the `on_binding` / `on_file_chooser` / `choose_proxy` plugin slots -- B4, 7.3 file chooser,
/// D2 a new context's proxy), then step 12 / Frontier S (the `inject_touch` / `inject_ime` / `clipboard_write`
/// host calls -- B6 touch+IME, C5 clipboard write -- and the `on_proxy_auth` / `on_cookie` / `on_storage_change`
/// / `on_clipboard_read` / `on_permission` / `geolocation` / `on_cursor` / `on_focus_state` plugin slots -- D3,
/// D9, E2, C5, permission/geo, C3 cursor/focus), then step 13 / Frontier M (the `resolve` / `on_target_open`
/// plugin slots -- D4 DNS resolution and popup/target control; no new host call), then step 14 / Frontier L (the
/// `req_hold_id` / `resume_request` / `modify_and_resume` host calls -- D7 async on_request hold, with the new
/// `KORVUS_HOLD` verdict -- and the `cache_get` / `cache_put` plugin slots -- D10 a plugin-backed response cache).
/// 4 (D110) adds the `return_json` / `call_stack` host calls, so an `on_call` tracer reads a claimed member's whole
/// call -- its return value as JSON and the JS stack it was called from -- not only a per-member tally.
pub const ABI: u32 = 4;

pub const READBACK_CANVAS: i32 = 0;
pub const READBACK_WEBGL: i32 = 1;
pub const READBACK_AUDIO: i32 = 2;
pub const READBACK_TEXT: i32 = 3;

pub const REALM_PAGE: i32 = 0;
pub const REALM_DEDICATED_WORKER: i32 = 1;
pub const REALM_SHARED_WORKER: i32 = 2;
pub const REALM_SERVICE_WORKER: i32 = 3;
pub const REALM_OTHER: i32 = 4;

const CONTINUE: c_int = 0;
const BLOCK: c_int = 1;
const FULFILL: c_int = 2;
/// `on_request`'s HOLD verdict (`KORVUS_HOLD`, ABI 3 step 14, D7): PARK the hop and release it later with
/// `host->resume_request` / `modify_and_resume`. Ends the chain, like `BLOCK` / `FULFILL`.
const HOLD: c_int = 3;

/// `on_call_pre`'s result (`KORVUS_SKIP`, ABI 3 wave 1): skip the claimed member's own body and use what the plugin
/// set with `set_*`. Any other value lets the body run.
const SKIP: c_int = 1;

// ── the header's types ─────────────────────────────────────────────────────────────────────────────

#[repr(C)]
pub struct KorvusContextInfo {
    pub context_id: *const c_char,
    pub name: *const c_char,
    pub host_os: *const c_char,
    pub proxy: *const c_char,
    pub identity: *const c_char,
    pub config_json: *const c_char,
    /// ABI 2.
    pub exit_ip: *const c_char,
}

/// ABI 2: a readback's pixels or samples, perturbed in place by the plugins.
#[repr(C)]
pub struct KorvusReadback {
    pub kind: c_int,
    pub width: u32,
    pub height: u32,
    pub rgba: *mut u8,
    pub samples: *mut f32,
    pub count: usize,
    // ABI 3 append (`KORVUS_READBACK_TEXT`): the measured string and its metrics (perturbed in place),
    // null / dangling for the ABI-2 kinds. Appending keeps the ABI-2 prefix byte-identical (ABI stays 3).
    pub text: *const c_char,
    pub metrics: *mut f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KorvusPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KorvusMotionSample {
    pub x: i32,
    pub y: i32,
    pub t_us: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KorvusKeyEvent {
    pub codepoint: u32,
    pub down_us: f64,
    pub up_us: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KorvusWheelEvent {
    pub dy: f64,
    pub t_us: f64,
}

#[repr(C)]
pub struct KorvusEvent {
    pub kind: *const c_char,
    pub json: *const c_char,
}

/// ABI 3 (wave 1): a painted frame handed to `on_frame` (C1, the D29 streaming core). `rgba` is width * height * 4
/// bytes (top-left origin), `len` its length, `t_us` the timestamp in microseconds; valid only for the call.
#[repr(C)]
pub struct KorvusFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: *const u8,
    pub len: usize,
    pub t_us: f64,
}

/// One Web API call at its exit, as a plugin holds it: opaque to C.
pub struct KorvusCall<'a> {
    label: &'a CStr,
    ctx: &'a mut dyn CallCtx,
}

/// One request hop before it is sent, as a plugin holds it: opaque to C.
pub struct KorvusRequest<'a> {
    ctx: &'a mut dyn RequestCtx,
    /// The strings handed to C during this callback, kept alive until it returns.
    scratch: Vec<CString>,
    /// ABI 3 (step 14, D7): the hold id `host->req_hold_id` assigned this hop, once a plugin asked for one; the
    /// dispatch reads it when the plugin returns `KORVUS_HOLD`.
    hold_id: Option<u64>,
}

/// One response as it arrives: opaque to C.
pub struct KorvusResponse<'a> {
    ctx: &'a mut dyn ResponseCtx,
    scratch: Vec<CString>,
    want_bodies: bool,
}

type CallPtr = *mut c_void;

#[repr(C)]
pub struct KorvusHost {
    pub abi: u32,
    pub size: u32,
    pub log: unsafe extern "C" fn(c_int, *const c_char),
    pub context_id: unsafe extern "C" fn() -> *const c_char,
    pub persona_name: unsafe extern "C" fn() -> *const c_char,
    pub seed: unsafe extern "C" fn(*const c_char) -> u32,
    // on_call
    pub label: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub argc: unsafe extern "C" fn(CallPtr) -> c_int,
    pub arg_is_number: unsafe extern "C" fn(CallPtr, c_int) -> c_int,
    pub arg_number: unsafe extern "C" fn(CallPtr, c_int) -> f64,
    pub arg_is_string: unsafe extern "C" fn(CallPtr, c_int) -> c_int,
    pub arg_string: unsafe extern "C" fn(CallPtr, c_int, *mut c_char, usize) -> usize,
    pub set_number: unsafe extern "C" fn(CallPtr, f64),
    pub set_bool: unsafe extern "C" fn(CallPtr, c_int),
    pub set_string: unsafe extern "C" fn(CallPtr, *const c_char),
    pub set_json: unsafe extern "C" fn(CallPtr, *const c_char),
    pub return_bytes: unsafe extern "C" fn(CallPtr, *mut *mut u8, *mut usize) -> c_int,
    pub return_floats: unsafe extern "C" fn(CallPtr, *mut *mut f32, *mut usize) -> c_int,
    pub realm: unsafe extern "C" fn(CallPtr) -> c_int,
    // on_request
    pub req_url: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub req_method: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub req_header: unsafe extern "C" fn(CallPtr, *const c_char) -> *const c_char,
    pub req_headers_json: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub req_destination: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub req_top_level_url: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub req_set_url: unsafe extern "C" fn(CallPtr, *const c_char),
    pub req_set_method: unsafe extern "C" fn(CallPtr, *const c_char),
    pub req_set_header: unsafe extern "C" fn(CallPtr, *const c_char, *const c_char),
    pub req_set_body: unsafe extern "C" fn(CallPtr, *const u8, usize),
    pub req_set_proxy: unsafe extern "C" fn(CallPtr, *const c_char),
    pub req_fulfill: unsafe extern "C" fn(CallPtr, c_int, *const c_char, *const u8, usize),
    // on_response
    pub res_status: unsafe extern "C" fn(CallPtr) -> c_int,
    pub res_url: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub res_header: unsafe extern "C" fn(CallPtr, *const c_char) -> *const c_char,
    pub res_headers_json: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub res_body: unsafe extern "C" fn(CallPtr, *mut *const u8, *mut usize) -> c_int,
    pub res_set_header: unsafe extern "C" fn(CallPtr, *const c_char, *const c_char),
    /// ABI 2.
    pub persona_json: unsafe extern "C" fn() -> *const c_char,
    /// ABI 3.
    pub metric: unsafe extern "C" fn(*const c_char, f64, *const c_char),
    pub page_eval: unsafe extern "C" fn(*const c_char, *const c_char, *mut c_char, usize) -> usize,
    pub emit_cdp_event: unsafe extern "C" fn(*const c_char, *const c_char),
    pub res_set_body: unsafe extern "C" fn(CallPtr, *const u8, usize),
    pub res_remote_ip: unsafe extern "C" fn(CallPtr) -> *const c_char,
    pub res_http_version: unsafe extern "C" fn(CallPtr) -> *const c_char,
    // ABI 3 (wave 1).
    pub set_rng_seed: unsafe extern "C" fn(u64),
    pub set_viewport: unsafe extern "C" fn(u32, u32, f64),
    pub inject_mouse: unsafe extern "C" fn(*const c_char, f64, f64, c_int),
    pub inject_key: unsafe extern "C" fn(*const c_char, *const c_char),
    pub inject_wheel: unsafe extern "C" fn(f64, f64, f64),
    pub capture_png: unsafe extern "C" fn(*mut *mut u8, *mut usize) -> c_int,
    // ABI 3 (step 9): virtual time (F5).
    pub virtual_time_freeze: unsafe extern "C" fn(),
    pub virtual_time_advance: unsafe extern "C" fn(f64),
    pub virtual_time_resume: unsafe extern "C" fn(),
    // ABI 3 (step 8 / step 9 G5): tenancy, the router's side.
    pub hibernate_context: unsafe extern "C" fn(*const c_char),
    pub resume_context: unsafe extern "C" fn(*const c_char),
    pub kv_get: unsafe extern "C" fn(*const c_char, *const c_char) -> *const c_char,
    pub kv_set: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char),
    // ABI 3 (step 10): drive the page (B5), page bindings (B4). Like the other host calls, they take no plugin
    // self. TODO(1021-verify): the task sketched `navigate(void* self, ...)`, but every host call in this ABI is
    // self-less (the plugin's self reaches its own slots, not the host); kept self-less to match.
    pub navigate: unsafe extern "C" fn(*const c_char),
    pub reload: unsafe extern "C" fn(),
    pub add_binding: unsafe extern "C" fn(*const c_char),
    // ABI 3 (step 12, Frontier S): touch/IME injection (B6), clipboard write (C5). Self-less like the other host
    // calls -- see the navigate TODO above (the task sketched `void* self` on these, but every host call in this
    // ABI is self-less, so they match inject_mouse / inject_wheel / navigate). TODO(1021-verify).
    pub inject_touch: unsafe extern "C" fn(*const c_char, f64, f64),
    pub inject_ime: unsafe extern "C" fn(*const c_char),
    pub clipboard_write: unsafe extern "C" fn(*const c_char),
    // ABI 3 (step 14, Frontier L): async on_request hold (D7). Self-less like the other host calls -- see the
    // navigate TODO above (the task sketched `void* self` on resume_request / modify_and_resume, but every host
    // call in this ABI is self-less: the id names the parked hop, and req_hold_id takes the request handle as the
    // other req_* accessors do). TODO(1021-verify).
    pub req_hold_id: unsafe extern "C" fn(CallPtr) -> c_ulong,
    pub resume_request: unsafe extern "C" fn(c_ulong, c_int),
    pub modify_and_resume: unsafe extern "C" fn(c_ulong, c_int, *const c_char, *const c_char, *const c_char, *const u8, usize),
    /// ABI 3 (fake-media SESSION 2, D-media): feed one RGBA frame for a synthetic video track (self-less, like
    /// every host call here). korvus copies the bytes into the per-track frame store; the paint path draws the
    /// latest frame into any `<video>` whose `srcObject` carries that track.
    pub push_video_frame: unsafe extern "C" fn(*const c_char, u32, u32, *const u8, usize),
    /// ABI 3 (WebExtension message bus, P1b): install a global function `name` in NAMED ISOLATED world `world`
    /// (not the main world), e.g. an extension's message-channel binding. Self-less like every host call here.
    pub add_world_binding: unsafe extern "C" fn(*const c_char, *const c_char),
    /// ABI 3 (WebExtension MV3 background): spawn a background realm of `kind` ("service": a ServiceWorker realm)
    /// that runs `src` at `url` (a `chrome-extension://` base), e.g. a manifest `background.service_worker`.
    /// Returns the realm's worker id (> 0) or 0 (no page yet / unsupported kind / failed). Self-less; appended at
    /// the struct tail so older plugins/hosts keep every prior offset (the loader zero-fills absent trailing members).
    pub spawn_background: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> c_int,
    // ABI 4 (D110): read a claimed member's whole call at `on_call` -- its return value as JSON and the JS stack it
    // was called from -- so a tracer plugin records args + result + stack, not just a per-member tally. Each writes
    // up to `cap-1` bytes + a NUL into `buf` and returns the full byte length (as `arg_string` does); 0 is "none"
    // (undefined/function/symbol result, or no stack). Appended at the struct tail: older plugins keep every prior
    // offset and a host built before ABI 4 zero-fills these, so a plugin checks `host->abi >= 4` before calling them.
    pub return_json: unsafe extern "C" fn(CallPtr, *mut c_char, usize) -> usize,
    pub call_stack: unsafe extern "C" fn(CallPtr, c_int, *mut c_char, usize) -> usize,
}

type RotationFn = unsafe extern "C" fn(*mut c_void, *const KorvusContextInfo, *mut u64, *mut c_char, usize) -> c_int;
type DeviceFn = unsafe extern "C" fn(*mut c_void, *const KorvusContextInfo, u64) -> *const c_char;
type IdentityFn = unsafe extern "C" fn(*mut c_void, *const KorvusContextInfo, *const c_char) -> *const c_char;
type OnCallFn = unsafe extern "C" fn(*mut c_void, CallPtr);
type OnRequestFn = unsafe extern "C" fn(*mut c_void, CallPtr) -> c_int;
type OnResponseFn = unsafe extern "C" fn(*mut c_void, CallPtr);
type PlanMoveFn = unsafe extern "C" fn(*mut c_void, KorvusPoint, KorvusPoint, f64, *mut KorvusMotionSample, usize) -> usize;
type TypeTextFn = unsafe extern "C" fn(*mut c_void, *const c_char, *mut KorvusKeyEvent, usize) -> usize;
type ScrollFn = unsafe extern "C" fn(*mut c_void, f64, *mut KorvusWheelEvent, usize) -> usize;
type DwellFn = unsafe extern "C" fn(*mut c_void) -> f64;
type OnEventFn = unsafe extern "C" fn(*mut c_void, *const KorvusEvent);
type FreeFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type ShutdownFn = unsafe extern "C" fn(*mut c_void);
type ReadbackFn = unsafe extern "C" fn(*mut c_void, *mut KorvusReadback);
type KindsFn = unsafe extern "C" fn(*mut c_void) -> u32;
type OnCdpCallFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *const c_char;
// ABI 3 (wave 1).
type OnCallPreFn = unsafe extern "C" fn(*mut c_void, CallPtr) -> c_int;
type VerifyCertFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type InitScriptFn = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char;
type OnFrameFn = unsafe extern "C" fn(*mut c_void, *const KorvusFrame);
type OnDialogFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut c_char, usize) -> c_int;
type OnDownloadFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *const u8, usize);
// ABI 3 (step 5).
type StorageLoadFn = unsafe extern "C" fn(*mut c_void, *const KorvusContextInfo) -> *const c_char;
type StorageSaveFn = unsafe extern "C" fn(*mut c_void, *const KorvusContextInfo, *const c_char);
// ABI 3 (step 8): tenancy. C `long` is mirrored as `c_long` (the header's `long`); the dispatch fns convert to/from
// the `i64` / `usize` the router speaks.
type OnConnectFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_long;
type ContextAllowFn = unsafe extern "C" fn(*mut c_void, c_long, *const c_char) -> c_int;
type QueueOrderFn = unsafe extern "C" fn(*mut c_void, *const c_long, c_uint) -> c_long;
// ABI 3 (step 10).
type OnBindingFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char);
type OnFileChooserFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char;
type ChooseProxyFn = unsafe extern "C" fn(*mut c_void, *const KorvusContextInfo) -> *const c_char;
// ABI 3 (step 12, Frontier S).
type OnProxyAuthFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char;
type OnCookieFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char, *const c_char);
type OnStorageChangeFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char);
type OnClipboardReadFn = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type OnPermissionFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type GeolocationFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char;
type OnCursorFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type OnFocusStateFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
// ABI 3 (step 13, Frontier M).
type ResolveFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char;
type OnTargetOpenFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
// ABI 3 (step 14, Frontier L).
type CacheGetFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char;
type CachePutFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char);
type OnUserMediaFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *const c_char;
// ABI 3 (WebExtension MV3 compat, B3 isolated worlds): (world name, init script) pairs per realm.
type InitWorldCountFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type InitWorldScriptFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut c_char, usize) -> *const c_char;
// ABI 3 (WebExtension message bus, P1b): a binding call that RETURNS a response string.
type OnBindingCallFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *const c_char;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KorvusPlugin {
    pub abi: u32,
    pub size: u32,
    pub name: *const c_char,
    pub version: *const c_char,
    pub self_: *mut c_void,
    pub labels: *const *const c_char,
    pub want_bodies: c_int,
    pub rotation: Option<RotationFn>,
    pub device: Option<DeviceFn>,
    pub identity: Option<IdentityFn>,
    pub on_call: Option<OnCallFn>,
    pub on_request: Option<OnRequestFn>,
    pub on_response: Option<OnResponseFn>,
    pub plan_move: Option<PlanMoveFn>,
    pub type_text: Option<TypeTextFn>,
    pub scroll: Option<ScrollFn>,
    pub press_dwell_us: Option<DwellFn>,
    pub on_event: Option<OnEventFn>,
    pub free_string: Option<FreeFn>,
    pub shutdown: Option<ShutdownFn>,
    /// ABI 2.
    pub readback_kinds: Option<KindsFn>,
    pub on_readback: Option<ReadbackFn>,
    /// ABI 3.
    pub cdp_domains: *const *const c_char,
    pub on_cdp_call: Option<OnCdpCallFn>,
    /// ABI 3 (wave 1).
    pub on_call_pre: Option<OnCallPreFn>,
    pub verify_cert: Option<VerifyCertFn>,
    pub init_script: Option<InitScriptFn>,
    pub want_frames: c_int,
    pub on_frame: Option<OnFrameFn>,
    pub on_dialog: Option<OnDialogFn>,
    pub on_download: Option<OnDownloadFn>,
    /// ABI 3 (step 5).
    pub storage_load: Option<StorageLoadFn>,
    pub storage_save: Option<StorageSaveFn>,
    /// ABI 3 (step 8): multi-tenancy (G1 / G2 / G4).
    pub on_connect: Option<OnConnectFn>,
    pub context_allow: Option<ContextAllowFn>,
    pub queue_order: Option<QueueOrderFn>,
    /// ABI 3 (step 10): page bindings (B4), file chooser (7.3), a new context's proxy (D2).
    pub on_binding: Option<OnBindingFn>,
    pub on_file_chooser: Option<OnFileChooserFn>,
    pub choose_proxy: Option<ChooseProxyFn>,
    /// ABI 3 (step 12, Frontier S): proxy auth (D3), cookie events (D9), storage-change events (E2), clipboard
    /// read (C5), permission and geolocation, cursor/focus (C3).
    pub on_proxy_auth: Option<OnProxyAuthFn>,
    pub on_cookie: Option<OnCookieFn>,
    pub on_storage_change: Option<OnStorageChangeFn>,
    pub on_clipboard_read: Option<OnClipboardReadFn>,
    pub on_permission: Option<OnPermissionFn>,
    pub geolocation: Option<GeolocationFn>,
    pub on_cursor: Option<OnCursorFn>,
    pub on_focus_state: Option<OnFocusStateFn>,
    /// ABI 3 (step 13, Frontier M): DNS resolution (D4), popup/target control.
    pub resolve: Option<ResolveFn>,
    pub on_target_open: Option<OnTargetOpenFn>,
    /// ABI 3 (step 14, Frontier L): a plugin-backed response cache (D10).
    pub cache_get: Option<CacheGetFn>,
    pub cache_put: Option<CachePutFn>,
    /// ABI 3 (step 14, Frontier L / fake-media giant, D-media): a synthetic getUserMedia / getDisplayMedia source.
    pub on_user_media: Option<OnUserMediaFn>,
    /// ABI 3 (WebExtension MV3 compat, B3 isolated worlds): init scripts for NAMED isolated worlds, beside
    /// `init_script` (the realm's own world). Count + indexed getter; gathered by `init_world_scripts`.
    pub init_world_count: Option<InitWorldCountFn>,
    pub init_world_script: Option<InitWorldScriptFn>,
    /// ABI 3 (WebExtension message bus, P1b): a binding call that RETURNS a response string (unlike the void
    /// `on_binding`), so a bound global's call -- e.g. chrome.runtime.sendMessage -- resolves to the plugin's answer.
    pub on_binding_call: Option<OnBindingCallFn>,
}

/// An ABI-1 descriptor's size: the fields up to `shutdown`. A plugin built against that header is loaded, its later
/// callbacks absent.
const ABI1_SIZE: usize = std::mem::offset_of!(KorvusPlugin, readback_kinds);

/// `korvus_plugin_init`'s type.
pub type InitFn = unsafe extern "C" fn(*const KorvusHost, *const c_char) -> *const KorvusPlugin;

// ── what the owning crates implement ───────────────────────────────────────────────────────────────

/// A Web API call at its exit, as the crate that made it (webapi) can read and answer it.
pub trait CallCtx {
    fn argc(&mut self) -> i32;
    fn arg_number(&mut self, index: i32) -> Option<f64>;
    fn arg_string(&mut self, index: i32) -> Option<String>;
    fn set_number(&mut self, v: f64);
    fn set_bool(&mut self, v: bool);
    fn set_string(&mut self, v: &str);
    fn set_json(&mut self, v: &str);
    /// The returned bytes to perturb in place (an RGBA buffer, a byte view).
    fn return_bytes(&mut self) -> Option<(*mut u8, usize)>;
    fn return_floats(&mut self) -> Option<(*mut f32, usize)>;
    fn realm(&mut self) -> i32;
    /// ABI 4 (D110): the member's return value as JSON (`undefined`, a function or a symbol -> `None`), so a tracer
    /// records the result and not only that the member was called.
    fn return_json(&mut self) -> Option<String>;
    /// ABI 4 (D110): the JS call stack at this call, up to `depth` frames, deepest first, one `func url:line:col` per
    /// line -- so a tracer records WHERE a member was called from, not just a per-member tally. `None` for no stack.
    fn stack(&mut self, depth: usize) -> Option<String>;
}

/// A request hop before it is sent (transport).
pub trait RequestCtx {
    fn url(&self) -> String;
    fn method(&self) -> String;
    fn headers(&self) -> Vec<(String, String)>;
    fn destination(&self) -> String;
    fn top_level_url(&self) -> String;
    fn set_url(&mut self, url: &str);
    fn set_method(&mut self, method: &str);
    fn set_header(&mut self, name: &str, value: Option<&str>);
    fn set_body(&mut self, body: &[u8]);
    fn set_proxy(&mut self, proxy: &str);
    fn fulfill(&mut self, status: u16, headers: Vec<(String, String)>, body: Vec<u8>);
}

/// A response as it arrives (transport).
pub trait ResponseCtx {
    fn status(&self) -> u16;
    fn url(&self) -> String;
    fn headers(&self) -> Vec<(String, String)>;
    fn body(&self) -> Option<&[u8]>;
    fn set_header(&mut self, name: &str, value: Option<&str>);
    fn set_body(&mut self, body: &[u8]);
    /// The address the terminal hop connected to (`ip:port`), or "" when there was no live connection.
    fn remote_ip(&self) -> String;
    /// The HTTP version the terminal hop spoke (`http/1.1`, `h2`, `h3`), or "".
    fn http_version(&self) -> String;
}

/// What the network chain decided for a hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Continue,
    Block,
    Fulfill,
    /// ABI 3 (step 14, D7): the hop is PARKED -- a plugin returned `KORVUS_HOLD`, and will release it later with
    /// `host->resume_request` / `modify_and_resume`. The `u64` is the hold id (`host->req_hold_id`); the transport
    /// awaits [`hold_gate`] with it and applies the [`HoldResolution`] the resume sends.
    Hold(u64),
}

// ── loading ────────────────────────────────────────────────────────────────────────────────────────

/// A loaded plugin.
pub struct Loaded {
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    /// The configuration it was loaded with (JSON).
    pub config: String,
    /// The Web API members it claims (`*`: every member).
    pub labels: Vec<String>,
    /// The CDP domains it serves (`Korvus_<name>`; ABI 3).
    pub cdp_domains: Vec<String>,
    desc: KorvusPlugin,
    /// `None` for a plugin linked into this process ([`load_static`]).
    _lib: Option<libloading::Library>,
}

// SAFETY: a plugin is thread-safe by its contract (the header's THREADS paragraph); its descriptor is read-only here.
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

impl Loaded {
    /// The slots this plugin fills, as `plugin ls` and the load line name them.
    pub fn slots(&self) -> Vec<&'static str> {
        let d = &self.desc;
        let mut s = Vec::new();
        if d.rotation.is_some() {
            s.push("rotation");
        }
        if d.device.is_some() {
            s.push("device");
        }
        if d.identity.is_some() {
            s.push("identity");
        }
        if (d.on_call.is_some() || d.on_call_pre.is_some()) && !self.labels.is_empty() {
            s.push("webapi");
        }
        if d.on_request.is_some() || d.on_response.is_some() {
            s.push("network");
        }
        if d.plan_move.is_some() || d.type_text.is_some() || d.scroll.is_some() || d.press_dwell_us.is_some() {
            s.push("input");
        }
        if d.on_event.is_some() {
            s.push("observer");
        }
        if d.on_readback.is_some() {
            s.push("readback");
        }
        if d.init_world_count.is_some() && d.init_world_script.is_some() {
            s.push("initworld");
        }
        if !self.cdp_domains.is_empty() {
            s.push("cdp");
        }
        if d.verify_cert.is_some() {
            s.push("tls");
        }
        if d.init_script.is_some() {
            s.push("init");
        }
        if d.want_frames != 0 && d.on_frame.is_some() {
            s.push("frame");
        }
        if d.on_dialog.is_some() {
            s.push("dialog");
        }
        if d.on_download.is_some() {
            s.push("download");
        }
        if d.storage_load.is_some() || d.storage_save.is_some() {
            s.push("storage");
        }
        if d.on_connect.is_some() || d.context_allow.is_some() || d.queue_order.is_some() {
            s.push("tenancy");
        }
        if d.on_binding.is_some() {
            s.push("binding");
        }
        if d.on_file_chooser.is_some() {
            s.push("filechooser");
        }
        if d.choose_proxy.is_some() {
            s.push("proxy");
        }
        if d.on_proxy_auth.is_some() {
            s.push("proxyauth");
        }
        if d.on_cookie.is_some() {
            s.push("cookies");
        }
        if d.on_storage_change.is_some() {
            s.push("storagewatch");
        }
        if d.on_clipboard_read.is_some() {
            s.push("clipboard");
        }
        if d.on_permission.is_some() {
            s.push("permission");
        }
        if d.geolocation.is_some() {
            s.push("geolocation");
        }
        if d.on_cursor.is_some() || d.on_focus_state.is_some() {
            s.push("cursor");
        }
        if d.resolve.is_some() {
            s.push("dns");
        }
        if d.on_target_open.is_some() {
            s.push("target");
        }
        if d.cache_get.is_some() || d.cache_put.is_some() {
            s.push("cache");
        }
        s
    }

    /// The plugin ABI its descriptor declared (`KorvusPlugin.abi`): the version it was built against.
    pub fn abi(&self) -> u32 {
        self.desc.abi
    }

    /// The size of its descriptor in bytes (`KorvusPlugin.size`): how much of the struct the plugin filled.
    pub fn size(&self) -> u32 {
        self.desc.size
    }
}

/// Why a library was not taken. Each is said on stderr as `[korvus] plugin: refused <path>: <why>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Open(String),
    NoInit(String),
    Declined,
    /// Built for a later ABI than this korvus speaks.
    Abi { plugin: u32, host: u32 },
    /// Its descriptor is smaller than ABI 1's: not built against korvus_plugin.h.
    Size { plugin: u32, host: u32 },
    BadConfig(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Open(e) => write!(f, "cannot load it: {e}"),
            Refusal::NoInit(e) => write!(f, "it exports no korvus_plugin_init: {e}"),
            Refusal::Declined => write!(f, "its korvus_plugin_init declined to load (returned NULL)"),
            Refusal::Abi { plugin, host } => write!(f, "it was built for plugin ABI {plugin}, and this korvus speaks ABI {host}"),
            Refusal::Size { plugin, host } => write!(f, "its KorvusPlugin is {plugin} bytes, smaller than ABI 1's {host}: it was not built against korvus_plugin.h"),
            Refusal::BadConfig(e) => write!(f, "its configuration is not JSON: {e}"),
        }
    }
}

fn plugins() -> &'static RwLock<Vec<Arc<Loaded>>> {
    static P: OnceLock<RwLock<Vec<Arc<Loaded>>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// The plugins loaded so far, in load order.
pub fn loaded() -> Vec<Arc<Loaded>> {
    plugins().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether any plugin is loaded: every entry point's first question.
pub fn any() -> bool {
    !plugins().read().unwrap_or_else(|e| e.into_inner()).is_empty()
}

/// A `--plugin` argument: `<path>` or `<path>:<config JSON>` (the config begins at the first `:{` or `:[`, so a
/// Windows drive letter's colon stays the path's).
pub fn split_arg(arg: &str) -> (String, String) {
    for (i, _) in arg.match_indices(':') {
        let rest = &arg[i + 1..];
        if rest.starts_with('{') || rest.starts_with('[') {
            return (arg[..i].to_string(), rest.to_string());
        }
    }
    (arg.to_string(), "{}".to_string())
}

/// Load the library at `path` and hand it `config` (JSON). On success it joins the loaded plugins, after every one
/// loaded before it. Says what happened on stderr either way.
pub fn load(path: &Path, config: &str) -> Result<Arc<Loaded>, Refusal> {
    let r = load_inner(path, config);
    match &r {
        // a browser endpoint's page process loads the endpoint's plugins again; the endpoint said so once already
        Ok(_) if std::env::var_os("KORVUS_PAGE_PROCESS").is_some() => {}
        Ok(p) => eprintln!("[korvus] plugin: loaded {} {} ({})", p.name, p.version, p.slots().join(", ")),
        Err(why) => eprintln!("[korvus] plugin: refused {}: {why}", path.display()),
    }
    r
}

fn load_inner(path: &Path, config: &str) -> Result<Arc<Loaded>, Refusal> {
    let config = if config.trim().is_empty() { "{}" } else { config };
    serde_json::from_str::<serde_json::Value>(config).map_err(|e| Refusal::BadConfig(e.to_string()))?;
    // SAFETY: loading a library runs its initializers; a plugin is trusted code by the header's contract.
    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| Refusal::Open(e.to_string()))?;
    // SAFETY: the symbol's type is the header's `korvus_plugin_init`.
    let init: InitFn = unsafe { lib.get::<InitFn>(b"korvus_plugin_init\0") }.map(|s| *s).map_err(|e| Refusal::NoInit(e.to_string()))?;
    init_plugin(path, config, init, Some(lib))
}

/// A plugin linked into this process -- its `korvus_plugin_init` called directly, then checked and dispatched exactly
/// as a loaded library's. For a plugin's own tests, which drive an engine with the plugin in it; `name` stands where a
/// library's path would.
///
/// # Safety
/// `init` is a `korvus_plugin_init` as the header defines it.
pub unsafe fn load_static(name: &str, init: InitFn, config: &str) -> Result<Arc<Loaded>, Refusal> {
    let config = if config.trim().is_empty() { "{}" } else { config };
    serde_json::from_str::<serde_json::Value>(config).map_err(|e| Refusal::BadConfig(e.to_string()))?;
    let r = init_plugin(Path::new(name), config, init, None);
    match &r {
        Ok(p) => eprintln!("[korvus] plugin: loaded {} {} ({})", p.name, p.version, p.slots().join(", ")),
        Err(why) => eprintln!("[korvus] plugin: refused {name}: {why}"),
    }
    r
}

fn init_plugin(path: &Path, config: &str, init: InitFn, lib: Option<libloading::Library>) -> Result<Arc<Loaded>, Refusal> {
    let cfg = CString::new(config).map_err(|e| Refusal::BadConfig(e.to_string()))?;
    // SAFETY: the host table is 'static and the config lives across the call, as the header promises.
    let desc = unsafe { init(host_table(), cfg.as_ptr()) };
    if desc.is_null() {
        return Err(Refusal::Declined);
    }
    // SAFETY: a non-NULL answer points at the plugin's descriptor, valid for the library's life; only its first two
    // words are read before its size is known.
    let (abi, size) = unsafe { ((*desc).abi, (*desc).size) };
    if abi > ABI || abi == 0 {
        return Err(Refusal::Abi { plugin: abi, host: ABI });
    }
    if (size as usize) < ABI1_SIZE {
        return Err(Refusal::Size { plugin: size, host: ABI1_SIZE as u32 });
    }
    // The fields the plugin's header has, and none after: a plugin built for an earlier ABI leaves later callbacks
    // absent (zero, which is `None`).
    let ours = std::mem::size_of::<KorvusPlugin>();
    let mut d: KorvusPlugin = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is at least `min(size, ours)` bytes (its own size, checked at least ABI 1's).
    unsafe { std::ptr::copy_nonoverlapping(desc as *const u8, &mut d as *mut KorvusPlugin as *mut u8, (size as usize).min(ours)) };
    let text = |p: *const c_char| if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() };
    let mut labels = Vec::new();
    if !d.labels.is_null() {
        let mut i = 0;
        loop {
            // SAFETY: a NULL-terminated array of C strings, as the header defines `labels`.
            let p = unsafe { *d.labels.add(i) };
            if p.is_null() {
                break;
            }
            labels.push(text(p));
            i += 1;
        }
    }
    let mut cdp_domains = Vec::new();
    if !d.cdp_domains.is_null() {
        let mut i = 0;
        loop {
            // SAFETY: a NULL-terminated array of C strings, as the header defines `cdp_domains`.
            let p = unsafe { *d.cdp_domains.add(i) };
            if p.is_null() {
                break;
            }
            cdp_domains.push(text(p));
            i += 1;
        }
    }
    let name = text(d.name);
    let loaded = Arc::new(Loaded {
        name: if name.is_empty() { path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { name },
        version: text(d.version),
        path: path.to_path_buf(),
        config: config.to_string(),
        labels,
        cdp_domains,
        desc: d,
        _lib: lib,
    });
    let mut all = plugins().write().unwrap_or_else(|e| e.into_inner());
    all.push(loaded.clone());
    claim_set_reset();
    Ok(loaded)
}

/// Every loaded plugin's `shutdown`, last loaded first (at the process's end).
pub fn shutdown() {
    for p in loaded().iter().rev() {
        if let Some(f) = p.desc.shutdown {
            // SAFETY: the plugin's own callback with its own self.
            unsafe { f(p.desc.self_) };
        }
    }
}

// ── the process's context, for the host functions ──────────────────────────────────────────────────

struct Process {
    context: CString,
    persona: CString,
    secret: String,
    /// The persona document this process wears (`persona_json`), "" with korvus's own identity.
    document: CString,
}

fn process() -> &'static RwLock<Process> {
    static P: OnceLock<RwLock<Process>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(Process { context: CString::default(), persona: CString::default(), secret: String::new(), document: CString::default() }))
}

/// This process's browser context and persona, as `context_id()` and `persona_name()` answer them, and the secret
/// `seed(domain)` is keyed by (the license's, once licensing holds one; until then the context's own id).
pub fn set_context(context: &str, persona: &str, secret: &str) {
    let mut p = process().write().unwrap_or_else(|e| e.into_inner());
    p.context = CString::new(context).unwrap_or_default();
    p.persona = CString::new(persona).unwrap_or_default();
    p.secret = secret.to_string();
}

/// The persona document this process wears (JSON), for `persona_json()`; "" for korvus's own identity.
pub fn set_persona_document(json: &str) {
    process().write().unwrap_or_else(|e| e.into_inner()).document = CString::new(json).unwrap_or_default();
}

// ── ABI 3: the metric sink (A6) ──────────────────────────────────────────────────────────────────

/// One accumulated measurement a plugin recorded (`host->metric`): a key, its running total, and the labels it was
/// recorded under (an object as JSON, `""` for none). The router sums these per context and serves them.
#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub labels: String,
}

fn metrics() -> &'static RwLock<Vec<Metric>> {
    static M: OnceLock<RwLock<Vec<Metric>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// Add `value` to the total for `(name, labels)` in this process. Plugins reach this through `host->metric`; the
/// router (and its tests) read the totals with [`drain_metrics`].
pub fn record_metric(name: &str, value: f64, labels: &str) {
    if name.is_empty() {
        return;
    }
    let mut m = metrics().write().unwrap_or_else(|e| e.into_inner());
    match m.iter_mut().find(|x| x.name == name && x.labels == labels) {
        Some(x) => x.value += value,
        None => m.push(Metric { name: name.to_string(), value, labels: labels.to_string() }),
    }
}

/// Take and clear this process's accumulated metrics. The endpoint calls it to serve `GET /korvus/metrics`; a page
/// process's own totals reach the router over the plugin channel (goal 1021 step 1b).
pub fn drain_metrics() -> Vec<Metric> {
    std::mem::take(&mut *metrics().write().unwrap_or_else(|e| e.into_inner()))
}

// ── ABI 3: the page-eval primitive (B1) ──────────────────────────────────────────────────────────

/// Runs a script on the page's own thread. The runtime installs one with [`set_page_executor`]; a callback reaches it
/// through `host->page_eval`. A router-side process (no page) installs none, and `page_eval` then answers nothing.
pub trait PageExecutor: Send + Sync {
    /// Run `script` in `world` (`""`: the page's own world) at the next task boundary of the realm's thread, and
    /// return its result as JSON. `None` when there is no such page, the world is unknown, or the script threw.
    fn eval(&self, world: &str, script: &str) -> Option<String>;
    /// ABI 3 (WebExtension MV3 background): spawn a background realm of `kind` ("service": a ServiceWorker realm)
    /// running `src` at `url`, at the page thread's next task boundary like [`PageExecutor::eval`], and return its
    /// worker id (> 0) or 0 (no page, an unsupported kind, or the spawn failed).
    fn spawn_background(&self, kind: &str, url: &str, src: &str) -> u32;
}

fn page_executor() -> &'static RwLock<Option<Arc<dyn PageExecutor>>> {
    static E: OnceLock<RwLock<Option<Arc<dyn PageExecutor>>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

/// Install the executor `host->page_eval` runs scripts through (the page process does this once its engine exists).
pub fn set_page_executor(e: Arc<dyn PageExecutor>) {
    *page_executor().write().unwrap_or_else(|err| err.into_inner()) = Some(e);
}

fn page_eval_now(world: &str, script: &str) -> Option<String> {
    let e = page_executor().read().unwrap_or_else(|err| err.into_inner()).clone()?;
    e.eval(world, script)
}

fn spawn_background_now(kind: &str, url: &str, src: &str) -> u32 {
    let Some(e) = page_executor().read().unwrap_or_else(|err| err.into_inner()).clone() else {
        return 0; // a router-side process (no page executor) cannot spawn a realm -> no-op
    };
    e.spawn_background(kind, url, src)
}

// ── ABI 3: plugin CDP domains (A1) ─────────────────────────────────────────────────────────────────

/// Where `host->emit_cdp_event` sends an event: the router installs one with [`set_cdp_event_sink`] to broadcast
/// it to the connected clients. A process with no clients (a page process) installs none.
type CdpEventSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

fn cdp_event_sink() -> &'static RwLock<Option<CdpEventSink>> {
    static S: OnceLock<RwLock<Option<CdpEventSink>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Install the sink `host->emit_cdp_event` forwards to (the endpoint does this as it starts serving).
pub fn set_cdp_event_sink(sink: CdpEventSink) {
    *cdp_event_sink().write().unwrap_or_else(|e| e.into_inner()) = Some(sink);
}

/// Every CDP domain any loaded plugin serves (`Korvus_<name>`). The router asks this to know which methods to hand
/// to a plugin rather than the engine.
pub fn cdp_domains() -> Vec<String> {
    loaded().iter().flat_map(|p| p.cdp_domains.clone()).collect()
}

/// Answer one CDP call on a plugin-served domain: the first plugin whose `cdp_domains` holds `method`'s domain
/// handles it. `Some(json)` is the result, `None` means no plugin served it or the plugin failed the call.
pub fn cdp_call(method: &str, params_json: &str) -> Option<String> {
    traced("cdp_call", method);
    let domain = method.split('.').next().unwrap_or("");
    for p in loaded() {
        if !p.cdp_domains.iter().any(|d| d == domain) {
            continue;
        }
        let f = p.desc.on_cdp_call?;
        let (m, pj) = (CString::new(method).ok()?, CString::new(params_json).ok()?);
        // SAFETY: the plugin's own callback; the strings live across the call.
        let s = unsafe { f(p.desc.self_, m.as_ptr(), pj.as_ptr()) };
        return take_string(&p, s);
    }
    None
}

// ── ABI 3 (wave 1): the page RNG seed (F6) ─────────────────────────────────────────────────────────

fn rng_seed() -> &'static RwLock<Option<u64>> {
    static S: OnceLock<RwLock<Option<u64>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// The RNG seed a plugin forced with `host->set_rng_seed`, or `None` until one did. The runtime reads it to seed the
/// page's pseudo-random sources so a persona's randomness is reproducible.
pub fn rng_seed_override() -> Option<u64> {
    *rng_seed().read().unwrap_or_else(|e| e.into_inner())
}

// ── ABI 3 (wave 1): the viewport override (C2) ─────────────────────────────────────────────────────

fn viewport() -> &'static RwLock<Option<(u32, u32, f64)>> {
    static V: OnceLock<RwLock<Option<(u32, u32, f64)>>> = OnceLock::new();
    V.get_or_init(Default::default)
}

/// The viewport a plugin forced with `host->set_viewport` as `(width, height, dpr)`, or `None` until one did. The
/// engine lays out and reports to the page accordingly.
pub fn viewport_override() -> Option<(u32, u32, f64)> {
    *viewport().read().unwrap_or_else(|e| e.into_inner())
}

// ── ABI 3 (step 9): virtual time (F5) ──────────────────────────────────────────────────────────────

/// A plugin's virtual-time policy (`host->virtual_time_*`, F5): the clock is PINNED and steps only when
/// the plugin advances it. `base` is the real instant the clock was pinned at -- the anchor the virtual
/// "now" is measured from -- and `advanced_ms` is how far past it `virtual_time_advance` has moved.
struct VirtualPolicy {
    base: std::time::Instant,
    advanced_ms: f64,
}

/// The virtual clock in force (F5), as [`virtual_time`] hands it to the engine. The engine reads its
/// "now" as the pinned `base` plus everything advance has added, translated onto whatever timeline the
/// reader keeps (the realm's `performance.now()`, the event loop's timer deadlines).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualClock {
    base: std::time::Instant,
    advanced_ms: f64,
}

impl VirtualClock {
    /// The virtual "now" as a real [`std::time::Instant`]: the pinned base plus the advance budget. The
    /// reading clock reports this moment on the realm's own timeline, and the event-loop timer queue
    /// treats a deadline at or before it as due.
    pub fn now_instant(&self) -> std::time::Instant {
        self.base + std::time::Duration::from_secs_f64(self.advanced_ms.max(0.0) / 1000.0)
    }
    /// How far past the base the clock has been advanced, in milliseconds -- the sum of every
    /// `virtual_time_advance`, which is the timer queue's advance budget.
    pub fn advanced_ms(&self) -> f64 {
        self.advanced_ms
    }
}

fn virtual_policy() -> &'static RwLock<Option<VirtualPolicy>> {
    static V: OnceLock<RwLock<Option<VirtualPolicy>>> = OnceLock::new();
    V.get_or_init(Default::default)
}

/// Fast "is the clock virtual?" gate for the hot path. `performance.now()` / `Event.timeStamp` and the
/// timer queue call [`virtual_time`] constantly; when no plugin has touched the clock -- every no-plugin
/// run (invariant I9/J2) -- this flag is false and [`virtual_time`] returns `None` from one relaxed
/// atomic load, never taking the lock. freeze/advance set it true, resume false; the lock holds the
/// policy itself.
static VIRTUAL_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The virtual-time policy a plugin set (F5), or `None` while none has. `None` is the default and what
/// every no-plugin run reads: the engine then reads the real OS clock, byte-for-byte as with no plugin
/// loaded (invariant I9/J2). `Some` means a plugin froze the clock, and the engine follows the returned
/// [`VirtualClock`] instead of the OS clock. The common `None` is a single atomic load (see
/// [`VIRTUAL_ON`]); only a set policy pays the lock.
pub fn virtual_time() -> Option<VirtualClock> {
    if !VIRTUAL_ON.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    virtual_policy()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|p| VirtualClock { base: p.base, advanced_ms: p.advanced_ms })
}

// ── ABI 3 (wave 1): injected input (B6, D29 interaction) ───────────────────────────────────────────

/// One synthetic input event a plugin queued (`host->inject_mouse` / `inject_key` / `inject_wheel`, and ABI 3
/// step 12's `inject_touch` / `inject_ime`). The runtime drains the queue with [`take_injected_input`] and
/// delivers each on the page's own thread, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum InjectedInput {
    Mouse { kind: String, x: f64, y: f64, button: i32 },
    Key { kind: String, key: String },
    Wheel { x: f64, y: f64, dy: f64 },
    /// ABI 3 (step 12, B6): a single-point touch; `kind` is "start"|"move"|"end", `x`,`y` in CSS pixels.
    Touch { kind: String, x: f64, y: f64 },
    /// ABI 3 (step 12, B6): an IME commit -- `text` composed and committed as one input, not key presses.
    Ime { text: String },
}

fn injected_input() -> &'static RwLock<Vec<InjectedInput>> {
    static Q: OnceLock<RwLock<Vec<InjectedInput>>> = OnceLock::new();
    Q.get_or_init(Default::default)
}

fn push_injected_input(ev: InjectedInput) {
    injected_input().write().unwrap_or_else(|e| e.into_inner()).push(ev);
}

/// Take and clear the queued injected input events, oldest first.
pub fn take_injected_input() -> Vec<InjectedInput> {
    std::mem::take(&mut *injected_input().write().unwrap_or_else(|e| e.into_inner()))
}

/// ABI 3 (fake-media SESSION 2, D-media): one fed RGBA frame for a synthetic video track.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The latest fed frame per track id, written by `push_video_frame` and read by the paint path (korvus-webapi's
/// `<video>` source arm). Process-global, keyed by the plugin-chosen track id. Empty on every no-plugin / no-fed
/// run, so the paint arm is a strict no-op then (I9/J2).
fn video_frames() -> &'static RwLock<HashMap<String, Frame>> {
    static F: OnceLock<RwLock<HashMap<String, Frame>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// Bumped once per ACCEPTED fed frame (fake-media SESSION 2b-repaint). The event loop / serve loop compare it
/// against their own cursor to force exactly ONE paint per new frame on the default no-compositor path, so a fed
/// `<video>` is live without a screenshot or a `want_frames` plugin. Starts at 0 and advances ONLY inside
/// `push_video_frame`, so it stays 0 on every no-plugin / no-fed run -> no forced paint (I9/J2). Edge (the ACT of
/// feeding), never level (`has_fed_frames()` would pin a repaint loop); read non-destructively (two readers).
static FED_FRAME_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Cheap "is any synthetic video frame fed?" gate for the paint hot path -- false on every no-plugin / no-fed run,
/// so the `<video>` source arm returns before any v8 traversal (I9/J2).
pub fn has_fed_frames() -> bool {
    !video_frames().read().unwrap_or_else(|e| e.into_inner()).is_empty()
}

/// The fed-frame generation (see [`FED_FRAME_GEN`]): a monotonic count of accepted fed frames. A reader keeps its
/// own last-seen cursor and forces one paint when this differs. Non-destructive (both the engine paint gate and the
/// serve-loop un-settle read it independently); a relaxed-ish `Acquire` load, cheaper than the frame-store lock.
pub fn fed_frame_generation() -> u64 {
    FED_FRAME_GEN.load(std::sync::atomic::Ordering::Acquire)
}

/// The latest fed frame for `track_id`, cloned, or `None`.
pub fn fed_video_frame(track_id: &str) -> Option<Frame> {
    video_frames().read().unwrap_or_else(|e| e.into_inner()).get(track_id).cloned()
}

/// The latest fed frame's dimensions for `track_id` (no RGBA copy), or `None`. The `<video>` state getters
/// (`videoWidth`/`videoHeight`/`readyState`) need only the size, not the pixels, and are polled, so they must not
/// clone the frame buffer on every read.
pub fn fed_video_dims(track_id: &str) -> Option<(u32, u32)> {
    video_frames().read().unwrap_or_else(|e| e.into_inner()).get(track_id).map(|f| (f.width, f.height))
}

/// Drop a track's fed frame -- its `stop()` -- so a stopped track never paints and the store does not grow.
pub fn clear_video_frame(track_id: &str) {
    video_frames().write().unwrap_or_else(|e| e.into_inner()).remove(track_id);
}

// ── ABI 3 (wave 1): the screenshotter (C6) ─────────────────────────────────────────────────────────

/// Renders the current page to a PNG. The runtime installs one with [`set_screenshotter`]; a plugin reaches it
/// through `host->capture_png`. A process with no page (the router) installs none, and `capture_png` then answers 0.
pub trait Screenshotter: Send + Sync {
    /// The page as PNG bytes, or `None` when there is nothing to capture.
    fn png(&self) -> Option<Vec<u8>>;
}

fn screenshotter() -> &'static RwLock<Option<Arc<dyn Screenshotter>>> {
    static S: OnceLock<RwLock<Option<Arc<dyn Screenshotter>>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Install the screenshotter `host->capture_png` captures through (the page process does this once its engine exists).
pub fn set_screenshotter(s: Arc<dyn Screenshotter>) {
    *screenshotter().write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

thread_local! {
    /// The bytes the last `capture_png` on this thread returned: the host owns them and keeps them valid until the
    /// thread's next `capture_png` (the ABI's contract for the pointer it hands back).
    static LAST_PNG: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

// ── ABI 3 (step 8 / step 9 G5): tenancy, the router's side ─────────────────────────────────────────

/// What the router services for a tenancy plugin's router-side host calls (`host->hibernate_context` /
/// `resume_context` / `kv_get` / `kv_set`). The endpoint installs one with [`set_tenancy`]; a plugin reaches it
/// through those host calls. A process with no router (a page process) installs none, and the host calls are then
/// no-ops with `kv_get` answering `None`. `ctx` is a context's CDP id throughout.
pub trait Tenancy: Send + Sync {
    /// Idle `ctx`: tear its page process down but keep its saved state (G5).
    fn hibernate(&self, ctx: &str);
    /// Bring a hibernated `ctx` back (G5).
    fn resume(&self, ctx: &str);
    /// The value stored under `(ctx, key)`, or `None` when absent (A4).
    fn kv_get(&self, ctx: &str, key: &str) -> Option<String>;
    /// Store `val` under `(ctx, key)` (A4).
    fn kv_set(&self, ctx: &str, key: &str, val: &str);
}

fn tenancy() -> &'static RwLock<Option<Arc<dyn Tenancy>>> {
    static T: OnceLock<RwLock<Option<Arc<dyn Tenancy>>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

/// Install the handler the tenancy host calls dispatch to (the endpoint does this as it starts serving).
pub fn set_tenancy(t: Arc<dyn Tenancy>) {
    *tenancy().write().unwrap_or_else(|e| e.into_inner()) = Some(t);
}

thread_local! {
    /// The string the last `kv_get` on this thread returned: the host owns it and keeps it valid until the thread's
    /// next `kv_get` (the ABI's contract for the pointer it hands back -- the plugin does not free it).
    static LAST_KV: RefCell<CString> = RefCell::new(CString::default());
}

// ── ABI 3 (step 10): drive the page (B5) ───────────────────────────────────────────────────────────

/// One navigation command a plugin queued (`host->navigate` / `host->reload`). The page loop drains the queue
/// with [`take_navigations`] and carries each out on the page's own thread, in order. A process with no page (the
/// router) never drains it, so a command queued there is simply never acted on.
#[derive(Debug, Clone, PartialEq)]
pub enum NavCmd {
    /// Load `url` in the current page.
    Navigate { url: String },
    /// Reload the current page.
    Reload,
}

fn navigations() -> &'static RwLock<Vec<NavCmd>> {
    static Q: OnceLock<RwLock<Vec<NavCmd>>> = OnceLock::new();
    Q.get_or_init(Default::default)
}

fn push_navigation(cmd: NavCmd) {
    navigations().write().unwrap_or_else(|e| e.into_inner()).push(cmd);
}

/// Take and clear the queued navigation commands, oldest first. The page loop drains them each turn.
pub fn take_navigations() -> Vec<NavCmd> {
    std::mem::take(&mut *navigations().write().unwrap_or_else(|e| e.into_inner()))
}

// ── ABI 3 (step 10): page bindings (B4) ────────────────────────────────────────────────────────────

fn bindings() -> &'static RwLock<Vec<String>> {
    static B: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn add_binding_name(name: &str) {
    if name.is_empty() {
        return;
    }
    let mut b = bindings().write().unwrap_or_else(|e| e.into_inner());
    if !b.iter().any(|n| n == name) {
        b.push(name.to_string());
    }
}

/// The global function names plugins asked the engine to install with `host->add_binding`, in the order added (a
/// name added twice is kept once). The runtime installs one global per name on each page realm, each calling
/// [`on_binding`]; empty until a plugin adds one, so a no-plugin run installs no binding (invariant I9/J2).
pub fn binding_names() -> Vec<String> {
    bindings().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// ABI 3 (WebExtension message bus, P1b): global function names to install in a NAMED ISOLATED world, keyed by
/// world name (`host->add_world_binding(world, name)`). Kept SEPARATE from [`binding_names`] so an isolated
/// world's bindings (an extension's message channel) never leak into any page's MAIN world. Empty until a plugin
/// adds one, so a no-plugin / stealth-only run installs nothing in any world (invariant I9/J2).
fn world_bindings() -> &'static RwLock<HashMap<String, Vec<String>>> {
    static W: OnceLock<RwLock<HashMap<String, Vec<String>>>> = OnceLock::new();
    W.get_or_init(Default::default)
}

fn add_world_binding_name(world: &str, name: &str) {
    if world.is_empty() || name.is_empty() {
        return;
    }
    let mut w = world_bindings().write().unwrap_or_else(|e| e.into_inner());
    let list = w.entry(world.to_string()).or_default();
    if !list.iter().any(|n| n == name) {
        list.push(name.to_string());
    }
}

/// The binding names to install in isolated world `world`, in add order (empty for a world no plugin bound).
pub fn world_binding_names(world: &str) -> Vec<String> {
    world_bindings().read().unwrap_or_else(|e| e.into_inner()).get(world).cloned().unwrap_or_default()
}

/// Cheap "did any plugin bind an isolated world?" gate for the per-world installer -- false on every no-plugin /
/// no-webext run, so the isolated-world binding install never fires then (I9/J2).
pub fn has_world_bindings() -> bool {
    !world_bindings().read().unwrap_or_else(|e| e.into_inner()).is_empty()
}

// ── ABI 3 (step 12): clipboard writes (C5) ─────────────────────────────────────────────────────────

/// One clipboard write a plugin queued (`host->clipboard_write`, C5). The page loop drains the queue with
/// [`take_clipboard_writes`] and writes each to the page's clipboard on the page's own thread, in order. A
/// process with no page (the router) never drains it, so a write queued there is simply never acted on; empty
/// until a plugin writes one, so a no-plugin run touches no clipboard (invariant I9/J2).
fn clipboard_writes() -> &'static RwLock<Vec<String>> {
    static Q: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    Q.get_or_init(Default::default)
}

fn push_clipboard_write(text: String) {
    clipboard_writes().write().unwrap_or_else(|e| e.into_inner()).push(text);
}

/// Take and clear the queued clipboard writes, oldest first. The page loop drains them each turn.
pub fn take_clipboard_writes() -> Vec<String> {
    std::mem::take(&mut *clipboard_writes().write().unwrap_or_else(|e| e.into_inner()))
}

// ── ABI 3 (step 14, Frontier L): async on_request hold (D7) ─────────────────────────────────────────

/// A parked hop's resolution, carried from `resume_request` / `modify_and_resume` to the awaiting transport
/// future. `verdict` is what to do with the released hop -- [`Verdict::Continue`] (allow, send it) or
/// [`Verdict::Block`] (abort it) -- and `modify`, when set, the edits to apply to the hop first.
#[derive(Debug, Clone, PartialEq)]
pub struct HoldResolution {
    pub verdict: Verdict,
    pub modify: Option<HoldModify>,
}

/// The edits `modify_and_resume` applies to a held hop before it is released: each `Some` replaces that part
/// of the request, each `None` leaves it unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoldModify {
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<Vec<(String, String)>>,
    pub body: Option<Vec<u8>>,
}

#[derive(Default)]
struct HoldState {
    resolution: Option<HoldResolution>,
    waker: Option<std::task::Waker>,
}

/// One parked hop: the slot a resume writes its resolution into, then wakes the awaiting future.
#[derive(Default)]
struct HoldSlot {
    state: Mutex<HoldState>,
}

/// The awaitable a parked hop hands the transport ([`hold_gate`]): awaiting it yields the [`HoldResolution`]
/// a later `resume_request` / `modify_and_resume` sends, from whatever thread the plugin resumes on. Dropping
/// it drops the hop's registry entry.
pub struct HoldToken {
    id: u64,
    slot: Arc<HoldSlot>,
}

impl HoldToken {
    /// The hold id (what the plugin learned from `host->req_hold_id` and passes back to `resume_request`).
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl std::future::Future for HoldToken {
    type Output = HoldResolution;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<HoldResolution> {
        // HoldToken is Unpin (a u64 and an Arc), so reading through the pin is sound; the wake-up state lives
        // behind the slot's own Mutex, not in the pinned struct.
        let mut st = self.slot.state.lock().unwrap_or_else(|e| e.into_inner());
        match st.resolution.take() {
            Some(r) => std::task::Poll::Ready(r),
            None => {
                st.waker = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        }
    }
}

impl Drop for HoldToken {
    fn drop(&mut self) {
        holds().write().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// Fast "does anything park requests?" gate for the request hot path. False on every run whose plugins never
/// hold -- every no-plugin run too (invariant I9/J2) -- so [`hold_gate`] returns `None` from one relaxed
/// atomic load, never touching the registry lock. The first `req_hold_id` raises it; it never clears (a hold
/// is rare, and the flag only guards skipping the lock).
static HOLD_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The next hold id to hand out: monotonic, and never 0 so a caller may keep 0 for "no hold".
static NEXT_HOLD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn holds() -> &'static RwLock<HashMap<u64, Arc<HoldSlot>>> {
    static H: OnceLock<RwLock<HashMap<u64, Arc<HoldSlot>>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

/// Allocate a hold id, make its slot, and raise the gate. Called when a plugin parks a hop -- through
/// `host->req_hold_id`, or on a bare `KORVUS_HOLD` with no id taken yet. The slot exists from this moment, so
/// a `resume_request` that races ahead of [`hold_gate`] still lands.
fn hold_register() -> u64 {
    let id = NEXT_HOLD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    holds().write().unwrap_or_else(|e| e.into_inner()).insert(id, Arc::new(HoldSlot::default()));
    HOLD_ON.store(true, std::sync::atomic::Ordering::Release);
    id
}

/// Register the awaitable for a parked hop `id`: the transport calls this after [`on_request`] returned
/// [`Verdict::Hold`] and awaits the returned token for the resume. `None` when nothing parks requests (the
/// cheap gate) or `id` is unknown (already resolved and dropped, or never held). The slot was made when the
/// id was assigned, so a resume that arrived before this call is already waiting in it and the token resolves
/// on its first poll.
pub fn hold_gate(id: u64) -> Option<HoldToken> {
    if !HOLD_ON.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let slot = holds().read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()?;
    Some(HoldToken { id, slot })
}

/// Is the plugin that serves CDP `domain` (a `Korvus_<name>`) one that can PARK requests -- i.e. has `on_request`
/// (ABI 3, step 14, D7)? The page process's socket-reader task answers such a domain OFF the page loop, because
/// only a holding plugin can block that loop inside a parked navigation (`begin_navigate` awaiting the very
/// document it holds), leaving no one on the loop to answer the `Korvus_hold.*` call that would release it. A
/// domain whose plugin never parks (e.g. a pure virtual-time domain, `Korvus_time`) is left to the in-order page
/// loop, so its calls are never reordered -- even in a session that also loads a holding plugin. The FIRST
/// plugin owning the domain decides, matching [`cdp_call`]'s dispatch. False with no plugin loaded and for every
/// stealth-only (no `on_request`) session, so the off-path branch is invisible then (I9/J2); the caller gates on
/// a cheap `"Korvus_` prefix test first, so engine traffic never reaches this.
pub fn cdp_domain_can_hold(domain: &str) -> bool {
    loaded()
        .iter()
        .find(|p| p.cdp_domains.iter().any(|d| d == domain))
        .is_some_and(|p| p.desc.on_request.is_some())
}

/// Write `resolution` into hop `id`'s slot and wake its awaiter, if the hop is still parked.
fn resolve_hold(id: u64, resolution: HoldResolution) {
    let slot = holds().read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
    if let Some(slot) = slot {
        let mut st = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        st.resolution = Some(resolution);
        if let Some(w) = st.waker.take() {
            w.wake();
        }
    }
}

/// A resume verdict's effect on the hop: `KORVUS_BLOCK` aborts it, anything else releases it (sends it).
fn resume_verdict(verdict: i32) -> Verdict {
    if verdict == BLOCK {
        Verdict::Block
    } else {
        Verdict::Continue
    }
}

/// ABI 3 (step 14, D7, `resume_request`): release a parked hop `id`. `verdict` is `KORVUS_CONTINUE` (allow,
/// send it) or `KORVUS_BLOCK` (abort it); any other value releases it. A plugin reaches this through
/// `host->resume_request`, from any thread, after its `on_request` returned `KORVUS_HOLD` and read the id
/// with `host->req_hold_id`. An id korvus no longer holds (already released, or never held) is ignored.
pub fn resume_request(id: u64, verdict: i32) {
    traced("resume_request", "");
    resolve_hold(id, HoldResolution { verdict: resume_verdict(verdict), modify: None });
}

/// ABI 3 (step 14, D7, `modify_and_resume`): release a parked hop `id` with `modify`'s set fields applied to
/// the hop (url / method / headers / body) before the `verdict` takes effect. Like [`resume_request`]
/// otherwise; reached through `host->modify_and_resume`.
pub fn modify_and_resume(id: u64, verdict: i32, modify: HoldModify) {
    traced("modify_and_resume", "");
    resolve_hold(id, HoldResolution { verdict: resume_verdict(verdict), modify: Some(modify) });
}

/// FNV-1a over the secret, the persona and the domain: a plugin's noise seed, stable for one persona.
pub fn seed_for(domain: &str) -> u32 {
    let p = process().read().unwrap_or_else(|e| e.into_inner());
    let mut h: u32 = 0x811c_9dc5;
    for b in p.secret.bytes().chain([0u8]).chain(p.persona.as_bytes().iter().copied()).chain([0u8]).chain(domain.bytes()) {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ── the host table ─────────────────────────────────────────────────────────────────────────────────

fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        // SAFETY: a plugin hands NUL-terminated strings, as the header's types say.
        unsafe { CStr::from_ptr(p) }.to_str().ok()
    }
}

unsafe fn call<'a>(c: CallPtr) -> &'a mut KorvusCall<'a> {
    &mut *(c as *mut KorvusCall<'a>)
}
unsafe fn req<'a>(c: CallPtr) -> &'a mut KorvusRequest<'a> {
    &mut *(c as *mut KorvusRequest<'a>)
}
unsafe fn res<'a>(c: CallPtr) -> &'a mut KorvusResponse<'a> {
    &mut *(c as *mut KorvusResponse<'a>)
}

fn keep(scratch: &mut Vec<CString>, s: String) -> *const c_char {
    scratch.push(CString::new(s).unwrap_or_default());
    scratch.last().map_or(std::ptr::null(), |c| c.as_ptr())
}

unsafe extern "C" fn h_log(level: c_int, msg: *const c_char) {
    let tag = match level {
        1 => "warn",
        2 => "error",
        _ => "info",
    };
    eprintln!("[korvus] plugin {tag}: {}", cstr(msg).unwrap_or(""));
}
unsafe extern "C" fn h_context_id() -> *const c_char {
    // the CString lives as long as the process's context does
    process().read().unwrap_or_else(|e| e.into_inner()).context.as_ptr()
}
unsafe extern "C" fn h_persona_name() -> *const c_char {
    process().read().unwrap_or_else(|e| e.into_inner()).persona.as_ptr()
}
unsafe extern "C" fn h_seed(domain: *const c_char) -> u32 {
    seed_for(cstr(domain).unwrap_or(""))
}
unsafe extern "C" fn h_persona_json() -> *const c_char {
    process().read().unwrap_or_else(|e| e.into_inner()).document.as_ptr()
}
unsafe extern "C" fn h_metric(name: *const c_char, value: f64, labels: *const c_char) {
    if let Some(n) = cstr(name) {
        record_metric(n, value, cstr(labels).unwrap_or(""));
    }
}
unsafe extern "C" fn h_page_eval(world: *const c_char, script: *const c_char, out: *mut c_char, cap: usize) -> usize {
    let Some(s) = cstr(script) else { return 0 };
    let Some(res) = page_eval_now(cstr(world).unwrap_or(""), s) else { return 0 };
    if !out.is_null() && cap > 0 {
        let mut n = res.len().min(cap - 1);
        while n > 0 && !res.is_char_boundary(n) {
            n -= 1;
        }
        std::ptr::copy_nonoverlapping(res.as_ptr(), out as *mut u8, n);
        *out.add(n) = 0;
    }
    res.len()
}
unsafe extern "C" fn h_emit_cdp_event(method: *const c_char, params: *const c_char) {
    let Some(m) = cstr(method) else { return };
    let p = cstr(params).unwrap_or("");
    if let Some(sink) = cdp_event_sink().read().unwrap_or_else(|e| e.into_inner()).clone() {
        sink(m, p);
    }
}
unsafe extern "C" fn h_label(c: CallPtr) -> *const c_char {
    call(c).label.as_ptr()
}
unsafe extern "C" fn h_argc(c: CallPtr) -> c_int {
    call(c).ctx.argc()
}
unsafe extern "C" fn h_arg_is_number(c: CallPtr, i: c_int) -> c_int {
    call(c).ctx.arg_number(i).is_some() as c_int
}
unsafe extern "C" fn h_arg_number(c: CallPtr, i: c_int) -> f64 {
    call(c).ctx.arg_number(i).unwrap_or(0.0)
}
unsafe extern "C" fn h_arg_is_string(c: CallPtr, i: c_int) -> c_int {
    call(c).ctx.arg_string(i).is_some() as c_int
}
unsafe extern "C" fn h_arg_string(c: CallPtr, i: c_int, buf: *mut c_char, cap: usize) -> usize {
    let Some(s) = call(c).ctx.arg_string(i) else { return 0 };
    if !buf.is_null() && cap > 0 {
        let mut n = s.len().min(cap - 1);
        while n > 0 && !s.is_char_boundary(n) {
            n -= 1;
        }
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    s.len()
}
unsafe extern "C" fn h_set_number(c: CallPtr, v: f64) {
    call(c).ctx.set_number(v)
}
unsafe extern "C" fn h_set_bool(c: CallPtr, v: c_int) {
    call(c).ctx.set_bool(v != 0)
}
unsafe extern "C" fn h_set_string(c: CallPtr, v: *const c_char) {
    if let Some(s) = cstr(v) {
        call(c).ctx.set_string(s)
    }
}
unsafe extern "C" fn h_set_json(c: CallPtr, v: *const c_char) {
    if let Some(s) = cstr(v) {
        if serde_json::from_str::<serde_json::Value>(s).is_ok() {
            call(c).ctx.set_json(s)
        }
    }
}
/// Copy `s` into the plugin's `buf` as `arg_string` does: up to `cap-1` bytes on a char boundary, then a NUL; return
/// the full byte length so the plugin can grow its buffer and call again.
unsafe fn write_into(s: &str, buf: *mut c_char, cap: usize) -> usize {
    if !buf.is_null() && cap > 0 {
        let mut n = s.len().min(cap - 1);
        while n > 0 && !s.is_char_boundary(n) {
            n -= 1;
        }
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    s.len()
}
unsafe extern "C" fn h_return_json(c: CallPtr, buf: *mut c_char, cap: usize) -> usize {
    match call(c).ctx.return_json() {
        Some(s) => write_into(&s, buf, cap),
        None => 0,
    }
}
unsafe extern "C" fn h_call_stack(c: CallPtr, depth: c_int, buf: *mut c_char, cap: usize) -> usize {
    match call(c).ctx.stack(depth.max(0) as usize) {
        Some(s) => write_into(&s, buf, cap),
        None => 0,
    }
}
unsafe extern "C" fn h_return_bytes(c: CallPtr, ptr: *mut *mut u8, len: *mut usize) -> c_int {
    match call(c).ctx.return_bytes() {
        Some((p, n)) if !ptr.is_null() && !len.is_null() => {
            *ptr = p;
            *len = n;
            1
        }
        _ => 0,
    }
}
unsafe extern "C" fn h_return_floats(c: CallPtr, ptr: *mut *mut f32, count: *mut usize) -> c_int {
    match call(c).ctx.return_floats() {
        Some((p, n)) if !ptr.is_null() && !count.is_null() => {
            *ptr = p;
            *count = n;
            1
        }
        _ => 0,
    }
}
unsafe extern "C" fn h_realm(c: CallPtr) -> c_int {
    call(c).ctx.realm()
}
unsafe extern "C" fn h_req_url(c: CallPtr) -> *const c_char {
    let r = req(c);
    let s = r.ctx.url();
    keep(&mut r.scratch, s)
}
unsafe extern "C" fn h_req_method(c: CallPtr) -> *const c_char {
    let r = req(c);
    let s = r.ctx.method();
    keep(&mut r.scratch, s)
}
unsafe extern "C" fn h_req_header(c: CallPtr, name: *const c_char) -> *const c_char {
    let r = req(c);
    let Some(n) = cstr(name) else { return std::ptr::null() };
    match r.ctx.headers().into_iter().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
        Some((_, v)) => keep(&mut r.scratch, v),
        None => std::ptr::null(),
    }
}
unsafe extern "C" fn h_req_headers_json(c: CallPtr) -> *const c_char {
    let r = req(c);
    let j = serde_json::to_string(&r.ctx.headers()).unwrap_or_else(|_| "[]".into());
    keep(&mut r.scratch, j)
}
unsafe extern "C" fn h_req_destination(c: CallPtr) -> *const c_char {
    let r = req(c);
    let s = r.ctx.destination();
    keep(&mut r.scratch, s)
}
unsafe extern "C" fn h_req_top_level_url(c: CallPtr) -> *const c_char {
    let r = req(c);
    let s = r.ctx.top_level_url();
    keep(&mut r.scratch, s)
}
unsafe extern "C" fn h_req_set_url(c: CallPtr, v: *const c_char) {
    if let Some(s) = cstr(v) {
        req(c).ctx.set_url(s)
    }
}
unsafe extern "C" fn h_req_set_method(c: CallPtr, v: *const c_char) {
    if let Some(s) = cstr(v) {
        req(c).ctx.set_method(s)
    }
}
unsafe extern "C" fn h_req_set_header(c: CallPtr, name: *const c_char, value: *const c_char) {
    if let Some(n) = cstr(name) {
        req(c).ctx.set_header(n, cstr(value))
    }
}
unsafe extern "C" fn h_req_set_body(c: CallPtr, data: *const u8, len: usize) {
    let body = if data.is_null() { &[][..] } else { std::slice::from_raw_parts(data, len) };
    req(c).ctx.set_body(body)
}
unsafe extern "C" fn h_req_set_proxy(c: CallPtr, v: *const c_char) {
    if let Some(s) = cstr(v) {
        req(c).ctx.set_proxy(s)
    }
}
unsafe extern "C" fn h_req_fulfill(c: CallPtr, status: c_int, headers: *const c_char, body: *const u8, len: usize) {
    let hs: Vec<(String, String)> = cstr(headers).and_then(|h| serde_json::from_str(h).ok()).unwrap_or_default();
    let b = if body.is_null() { Vec::new() } else { std::slice::from_raw_parts(body, len).to_vec() };
    req(c).ctx.fulfill(status.clamp(100, 999) as u16, hs, b)
}
unsafe extern "C" fn h_res_status(c: CallPtr) -> c_int {
    res(c).ctx.status() as c_int
}
unsafe extern "C" fn h_res_url(c: CallPtr) -> *const c_char {
    let r = res(c);
    let s = r.ctx.url();
    keep(&mut r.scratch, s)
}
unsafe extern "C" fn h_res_header(c: CallPtr, name: *const c_char) -> *const c_char {
    let r = res(c);
    let Some(n) = cstr(name) else { return std::ptr::null() };
    match r.ctx.headers().into_iter().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
        Some((_, v)) => keep(&mut r.scratch, v),
        None => std::ptr::null(),
    }
}
unsafe extern "C" fn h_res_headers_json(c: CallPtr) -> *const c_char {
    let r = res(c);
    let j = serde_json::to_string(&r.ctx.headers()).unwrap_or_else(|_| "[]".into());
    keep(&mut r.scratch, j)
}
unsafe extern "C" fn h_res_body(c: CallPtr, ptr: *mut *const u8, len: *mut usize) -> c_int {
    let r = res(c);
    if !r.want_bodies || ptr.is_null() || len.is_null() {
        return 0;
    }
    match r.ctx.body() {
        Some(b) => {
            *ptr = b.as_ptr();
            *len = b.len();
            1
        }
        None => 0,
    }
}
unsafe extern "C" fn h_res_set_header(c: CallPtr, name: *const c_char, value: *const c_char) {
    if let Some(n) = cstr(name) {
        res(c).ctx.set_header(n, cstr(value))
    }
}
unsafe extern "C" fn h_res_set_body(c: CallPtr, data: *const u8, len: usize) {
    let body = if data.is_null() { &[][..] } else { std::slice::from_raw_parts(data, len) };
    res(c).ctx.set_body(body)
}
unsafe extern "C" fn h_res_remote_ip(c: CallPtr) -> *const c_char {
    let r = res(c);
    let s = r.ctx.remote_ip();
    keep(&mut r.scratch, s)
}
unsafe extern "C" fn h_res_http_version(c: CallPtr) -> *const c_char {
    let r = res(c);
    let s = r.ctx.http_version();
    keep(&mut r.scratch, s)
}
// ── ABI 3 (wave 1) host calls ──
unsafe extern "C" fn h_set_rng_seed(seed: u64) {
    *rng_seed().write().unwrap_or_else(|e| e.into_inner()) = Some(seed);
}
unsafe extern "C" fn h_set_viewport(width: u32, height: u32, dpr: f64) {
    *viewport().write().unwrap_or_else(|e| e.into_inner()) = Some((width, height, dpr));
}
unsafe extern "C" fn h_inject_mouse(kind: *const c_char, x: f64, y: f64, button: c_int) {
    push_injected_input(InjectedInput::Mouse { kind: cstr(kind).unwrap_or("").to_string(), x, y, button });
}
unsafe extern "C" fn h_inject_key(kind: *const c_char, key: *const c_char) {
    push_injected_input(InjectedInput::Key { kind: cstr(kind).unwrap_or("").to_string(), key: cstr(key).unwrap_or("").to_string() });
}
unsafe extern "C" fn h_inject_wheel(x: f64, y: f64, dy: f64) {
    push_injected_input(InjectedInput::Wheel { x, y, dy });
}
unsafe extern "C" fn h_capture_png(out: *mut *mut u8, len: *mut usize) -> c_int {
    let Some(s) = screenshotter().read().unwrap_or_else(|e| e.into_inner()).clone() else { return 0 };
    let Some(png) = s.png() else { return 0 };
    // The bytes live in this thread's LAST_PNG and stay valid until the thread's next capture_png (the host owns
    // them, as the header promises): the pointer outlives the borrow because the thread-local keeps the Vec.
    let (ptr, n) = LAST_PNG.with(|cell| {
        let mut b = cell.borrow_mut();
        *b = png;
        (b.as_mut_ptr(), b.len())
    });
    if !out.is_null() {
        *out = ptr;
    }
    if !len.is_null() {
        *len = n;
    }
    1
}
// ── ABI 3 (fake-media SESSION 2) host call: feed a synthetic video frame ──
unsafe extern "C" fn h_push_video_frame(track_id: *const c_char, width: u32, height: u32, rgba: *const u8, len: usize) {
    let Some(id) = cstr(track_id) else { return };
    // Reject a null or mis-sized buffer: width * height * 4 (RGBA) must equal len, with no overflow.
    let expect = (width as usize).checked_mul(height as usize).and_then(|p| p.checked_mul(4));
    if rgba.is_null() || expect != Some(len) {
        return;
    }
    // SAFETY: the plugin promises `len` valid bytes at `rgba` for the duration of the call; copy them out so the
    // frame outlives it.
    let data = unsafe { std::slice::from_raw_parts(rgba, len) }.to_vec();
    video_frames().write().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), Frame { width, height, rgba: data });
    // Signal one paint for this new frame (SESSION 2b-repaint). Bump AFTER the insert (Release) so the frame is
    // visible before a reader observes the new generation; only here, so no-feed runs never advance it (I9/J2).
    FED_FRAME_GEN.fetch_add(1, std::sync::atomic::Ordering::Release);
}
// ── ABI 3 (step 9) host calls: virtual time (F5) ──
unsafe extern "C" fn h_virtual_time_freeze() {
    // Pin the clock. The first freeze captures the anchor the virtual now is measured from; re-freezing
    // keeps the advance already applied (the clock does not jump back).
    {
        let mut g = virtual_policy().write().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(VirtualPolicy { base: std::time::Instant::now(), advanced_ms: 0.0 });
        }
    }
    VIRTUAL_ON.store(true, std::sync::atomic::Ordering::Release);
}
unsafe extern "C" fn h_virtual_time_advance(ms: f64) {
    if !ms.is_finite() || ms <= 0.0 {
        return;
    }
    // Advancing implies virtual time is on: a freeze is taken now if the plugin skipped one.
    {
        let mut g = virtual_policy().write().unwrap_or_else(|e| e.into_inner());
        let p = g.get_or_insert_with(|| VirtualPolicy { base: std::time::Instant::now(), advanced_ms: 0.0 });
        p.advanced_ms += ms;
    }
    VIRTUAL_ON.store(true, std::sync::atomic::Ordering::Release);
}
unsafe extern "C" fn h_virtual_time_resume() {
    VIRTUAL_ON.store(false, std::sync::atomic::Ordering::Release);
    *virtual_policy().write().unwrap_or_else(|e| e.into_inner()) = None;
}
// ── ABI 3 (step 8 / step 9 G5) host calls: tenancy ──
unsafe extern "C" fn h_hibernate_context(context: *const c_char) {
    let Some(ctx) = cstr(context) else { return };
    if let Some(t) = tenancy().read().unwrap_or_else(|e| e.into_inner()).clone() {
        t.hibernate(ctx);
    }
}
unsafe extern "C" fn h_resume_context(context: *const c_char) {
    let Some(ctx) = cstr(context) else { return };
    if let Some(t) = tenancy().read().unwrap_or_else(|e| e.into_inner()).clone() {
        t.resume(ctx);
    }
}
unsafe extern "C" fn h_kv_get(context: *const c_char, key: *const c_char) -> *const c_char {
    let (Some(ctx), Some(k)) = (cstr(context), cstr(key)) else { return std::ptr::null() };
    let Some(t) = tenancy().read().unwrap_or_else(|e| e.into_inner()).clone() else { return std::ptr::null() };
    let Some(val) = t.kv_get(ctx, k) else { return std::ptr::null() };
    // The string lives in this thread's LAST_KV and stays valid until the thread's next kv_get (the host owns it, as
    // the header promises): the pointer outlives the borrow because the thread-local keeps the CString.
    LAST_KV.with(|cell| {
        let mut c = cell.borrow_mut();
        *c = CString::new(val).unwrap_or_default();
        c.as_ptr()
    })
}
unsafe extern "C" fn h_kv_set(context: *const c_char, key: *const c_char, value: *const c_char) {
    let (Some(ctx), Some(k)) = (cstr(context), cstr(key)) else { return };
    let v = cstr(value).unwrap_or("");
    if let Some(t) = tenancy().read().unwrap_or_else(|e| e.into_inner()).clone() {
        t.kv_set(ctx, k, v);
    }
}
// ── ABI 3 (step 10) host calls: drive the page (B5), page bindings (B4) ──
unsafe extern "C" fn h_navigate(url: *const c_char) {
    push_navigation(NavCmd::Navigate { url: cstr(url).unwrap_or("").to_string() });
}
unsafe extern "C" fn h_reload() {
    push_navigation(NavCmd::Reload);
}
unsafe extern "C" fn h_add_binding(name: *const c_char) {
    if let Some(n) = cstr(name) {
        add_binding_name(n);
    }
}
unsafe extern "C" fn h_add_world_binding(world: *const c_char, name: *const c_char) {
    if let (Some(w), Some(n)) = (cstr(world), cstr(name)) {
        add_world_binding_name(w, n);
    }
}
unsafe extern "C" fn h_spawn_background(kind: *const c_char, url: *const c_char, src: *const c_char) -> c_int {
    let (Some(k), Some(u), Some(s)) = (cstr(kind), cstr(url), cstr(src)) else {
        return 0;
    };
    spawn_background_now(k, u, s) as c_int
}
// ── ABI 3 (step 12) host calls: touch/IME injection (B6), clipboard write (C5) ──
unsafe extern "C" fn h_inject_touch(kind: *const c_char, x: f64, y: f64) {
    push_injected_input(InjectedInput::Touch { kind: cstr(kind).unwrap_or("").to_string(), x, y });
}
unsafe extern "C" fn h_inject_ime(text: *const c_char) {
    push_injected_input(InjectedInput::Ime { text: cstr(text).unwrap_or("").to_string() });
}
unsafe extern "C" fn h_clipboard_write(text: *const c_char) {
    push_clipboard_write(cstr(text).unwrap_or("").to_string());
}
// ── ABI 3 (step 14) host calls: async on_request hold (D7) ──
unsafe extern "C" fn h_req_hold_id(c: CallPtr) -> c_ulong {
    // Assign this hop a hold id (and make its slot) the first time it is asked, then return the same id. The
    // plugin calls this during on_request, keeps the id, and returns KORVUS_HOLD.
    let r = req(c);
    (*r.hold_id.get_or_insert_with(hold_register)) as c_ulong
}
unsafe extern "C" fn h_resume_request(id: c_ulong, verdict: c_int) {
    resume_request(id as u64, verdict as i32);
}
unsafe extern "C" fn h_modify_and_resume(
    id: c_ulong,
    verdict: c_int,
    url: *const c_char,
    method: *const c_char,
    headers: *const c_char,
    body: *const u8,
    len: usize,
) {
    let modify = HoldModify {
        url: cstr(url).map(str::to_string),
        method: cstr(method).map(str::to_string),
        // [[name, value], ...] as req_headers_json / req_fulfill speak it; an unparseable value changes nothing.
        headers: cstr(headers).and_then(|h| serde_json::from_str::<Vec<(String, String)>>(h).ok()),
        body: if body.is_null() { None } else { Some(std::slice::from_raw_parts(body, len).to_vec()) },
    };
    modify_and_resume(id as u64, verdict as i32, modify);
}

fn host_table() -> &'static KorvusHost {
    static HOST: KorvusHost = KorvusHost {
        abi: ABI,
        size: std::mem::size_of::<KorvusHost>() as u32,
        log: h_log,
        context_id: h_context_id,
        persona_name: h_persona_name,
        seed: h_seed,
        label: h_label,
        argc: h_argc,
        arg_is_number: h_arg_is_number,
        arg_number: h_arg_number,
        arg_is_string: h_arg_is_string,
        arg_string: h_arg_string,
        set_number: h_set_number,
        set_bool: h_set_bool,
        set_string: h_set_string,
        set_json: h_set_json,
        return_bytes: h_return_bytes,
        return_floats: h_return_floats,
        realm: h_realm,
        req_url: h_req_url,
        req_method: h_req_method,
        req_header: h_req_header,
        req_headers_json: h_req_headers_json,
        req_destination: h_req_destination,
        req_top_level_url: h_req_top_level_url,
        req_set_url: h_req_set_url,
        req_set_method: h_req_set_method,
        req_set_header: h_req_set_header,
        req_set_body: h_req_set_body,
        req_set_proxy: h_req_set_proxy,
        req_fulfill: h_req_fulfill,
        res_status: h_res_status,
        res_url: h_res_url,
        res_header: h_res_header,
        res_headers_json: h_res_headers_json,
        res_body: h_res_body,
        res_set_header: h_res_set_header,
        persona_json: h_persona_json,
        metric: h_metric,
        page_eval: h_page_eval,
        emit_cdp_event: h_emit_cdp_event,
        res_set_body: h_res_set_body,
        res_remote_ip: h_res_remote_ip,
        res_http_version: h_res_http_version,
        set_rng_seed: h_set_rng_seed,
        set_viewport: h_set_viewport,
        inject_mouse: h_inject_mouse,
        inject_key: h_inject_key,
        inject_wheel: h_inject_wheel,
        capture_png: h_capture_png,
        virtual_time_freeze: h_virtual_time_freeze,
        virtual_time_advance: h_virtual_time_advance,
        virtual_time_resume: h_virtual_time_resume,
        hibernate_context: h_hibernate_context,
        resume_context: h_resume_context,
        kv_get: h_kv_get,
        kv_set: h_kv_set,
        navigate: h_navigate,
        reload: h_reload,
        add_binding: h_add_binding,
        inject_touch: h_inject_touch,
        inject_ime: h_inject_ime,
        clipboard_write: h_clipboard_write,
        req_hold_id: h_req_hold_id,
        resume_request: h_resume_request,
        modify_and_resume: h_modify_and_resume,
        push_video_frame: h_push_video_frame,
        add_world_binding: h_add_world_binding,
        spawn_background: h_spawn_background,
        return_json: h_return_json,
        call_stack: h_call_stack,
    };
    &HOST
}

// ── the plugin trace (debug) ─────────────────────────────────────────────────────────────────────

/// Whether the plugin trace is on, read once from the environment (`KORVUS_PLUGIN_TRACE=1`). When on, each dispatch
/// fn logs one stderr line per invocation (see [`traced`]); off, it is one bool load and nothing else.
fn trace_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| matches!(std::env::var("KORVUS_PLUGIN_TRACE").as_deref(), Ok("1")))
}

/// Log one plugin-trace line (`[korvus] plugin trace: <slot> <detail>`) when the trace is on; `detail` is the call's
/// name or label. Gated on [`trace_on`], so it costs one bool load when off.
fn traced(slot: &str, detail: &str) {
    if trace_on() {
        eprintln!("[korvus] plugin trace: {slot} {detail}");
    }
}

// ── dispatch ───────────────────────────────────────────────────────────────────────────────────────

struct Claims {
    all: bool,
    labels: HashSet<String>,
}

fn claim_set() -> &'static RwLock<Option<Arc<Claims>>> {
    static C: OnceLock<RwLock<Option<Arc<Claims>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn claim_set_reset() {
    *claim_set().write().unwrap_or_else(|e| e.into_inner()) = None;
}

fn claims_now() -> Arc<Claims> {
    if let Some(c) = claim_set().read().unwrap_or_else(|e| e.into_inner()).clone() {
        return c;
    }
    let mut c = Claims { all: false, labels: HashSet::new() };
    for p in loaded() {
        // A plugin that fills on_call or on_call_pre (ABI 3 wave 1) claims its labels: the member must be built with
        // the plugin seam for either to run.
        if p.desc.on_call.is_none() && p.desc.on_call_pre.is_none() {
            continue;
        }
        for l in &p.labels {
            if l == "*" {
                c.all = true;
            } else {
                c.labels.insert(l.clone());
            }
        }
    }
    let c = Arc::new(c);
    *claim_set().write().unwrap_or_else(|e| e.into_inner()) = Some(c.clone());
    c
}

/// Does any loaded plugin answer Web API member `label`? Asked when a member is BUILT, so an unclaimed member is built
/// exactly as with no plugin, and pays nothing at its calls (invariant I7).
pub fn claims(label: &str) -> bool {
    if !any() {
        return false;
    }
    let c = claims_now();
    c.all || c.labels.contains(label)
}

/// Whether any loaded plugin claims any Web API member at all.
pub fn claims_any() -> bool {
    if !any() {
        return false;
    }
    let c = claims_now();
    c.all || !c.labels.is_empty()
}

/// ABI 3 (wave 1, F1): BEFORE a claimed member's body. Every plugin that claims `label` (the same claim set as
/// [`on_call`]) runs its `on_call_pre` in load order and may set the answer with `set_*`. Returns `true` when any
/// plugin returned `KORVUS_SKIP` -- the member then skips its own body and uses what the plugins set; `false` to run
/// the body as normal (with on_call still to follow at the exit).
pub fn on_call_pre(label: &CStr, ctx: &mut dyn CallCtx) -> bool {
    let text = label.to_str().unwrap_or("");
    traced("on_call_pre", text);
    let mut skip = false;
    for p in loaded() {
        let Some(f) = p.desc.on_call_pre else { continue };
        if !p.labels.iter().any(|l| l == "*" || l == text) {
            continue;
        }
        let mut c = KorvusCall { label, ctx: &mut *ctx };
        // SAFETY: the handle lives across the callback; the plugin reads it only through the host table.
        let v = unsafe { f(p.desc.self_, &mut c as *mut KorvusCall as CallPtr) };
        if v == SKIP {
            skip = true;
        }
    }
    skip
}

/// The exit of a claimed member's call: every plugin that claims `label`, in load order, may read the call and
/// replace or perturb its answer; the last `set_*` wins.
pub fn on_call(label: &CStr, ctx: &mut dyn CallCtx) {
    let text = label.to_str().unwrap_or("");
    traced("on_call", text);
    for p in loaded() {
        let Some(f) = p.desc.on_call else { continue };
        if !p.labels.iter().any(|l| l == "*" || l == text) {
            continue;
        }
        let mut c = KorvusCall { label, ctx: &mut *ctx };
        // SAFETY: the handle lives across the callback; the plugin reads it only through the host table.
        unsafe { f(p.desc.self_, &mut c as *mut KorvusCall as CallPtr) };
    }
}

/// Is any plugin on the network chain?
pub fn has_network() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_request.is_some() || p.desc.on_response.is_some())
}

/// A request hop, before it is sent: every network plugin in load order; the first that blocks or fulfills ends it.
pub fn on_request(ctx: &mut dyn RequestCtx) -> Verdict {
    if trace_on() {
        traced("on_request", &ctx.url());
    }
    for p in loaded() {
        let Some(f) = p.desc.on_request else { continue };
        let mut r = KorvusRequest { ctx: &mut *ctx, scratch: Vec::new(), hold_id: None };
        // SAFETY: as `on_call`.
        let v = unsafe { f(p.desc.self_, &mut r as *mut KorvusRequest as CallPtr) };
        match v {
            BLOCK => return Verdict::Block,
            FULFILL => return Verdict::Fulfill,
            // HOLD parks the hop and ends the chain (D7). The plugin learned its id from `host->req_hold_id`; if
            // it returned HOLD without asking, assign one now so the hop still parks under a known id.
            HOLD => return Verdict::Hold(r.hold_id.unwrap_or_else(hold_register)),
            CONTINUE => {}
            _ => {}
        }
    }
    Verdict::Continue
}

/// A response as it arrives: every network plugin in load order.
pub fn on_response(ctx: &mut dyn ResponseCtx) {
    if trace_on() {
        traced("on_response", &ctx.url());
    }
    for p in loaded() {
        let Some(f) = p.desc.on_response else { continue };
        let mut r = KorvusResponse { ctx: &mut *ctx, scratch: Vec::new(), want_bodies: p.desc.want_bodies != 0 };
        // SAFETY: as `on_call`.
        unsafe { f(p.desc.self_, &mut r as *mut KorvusResponse as CallPtr) };
    }
}

/// Does any plugin read response bodies? (Transport buffers a body for them only then.)
pub fn wants_bodies() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_response.is_some() && p.desc.want_bodies != 0)
}

/// Is any plugin an observer? (A caller builds an event's JSON only then.)
pub fn has_observer() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_event.is_some())
}

/// An observer's event, to every plugin that observes.
pub fn event(kind: &str, json: &str) {
    traced("on_event", kind);
    if !any() {
        return;
    }
    let (Ok(k), Ok(j)) = (CString::new(kind), CString::new(json)) else { return };
    let ev = KorvusEvent { kind: k.as_ptr(), json: j.as_ptr() };
    for p in loaded() {
        if let Some(f) = p.desc.on_event {
            // SAFETY: the event's strings live across the callback.
            unsafe { f(p.desc.self_, &ev) };
        }
    }
}

/// A context being made, as the answer slots read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextInfo {
    pub context_id: String,
    pub name: String,
    pub host_os: String,
    pub proxy: String,
    pub identity: String,
    pub config_json: String,
    pub exit_ip: String,
}

struct InfoC {
    _strings: Vec<CString>,
    info: KorvusContextInfo,
}

fn info_c(i: &ContextInfo) -> InfoC {
    let strings: Vec<CString> =
        [&i.context_id, &i.name, &i.host_os, &i.proxy, &i.identity, &i.config_json, &i.exit_ip].iter().map(|s| CString::new(s.as_str()).unwrap_or_default()).collect();
    let info = KorvusContextInfo {
        context_id: strings[0].as_ptr(),
        name: strings[1].as_ptr(),
        host_os: strings[2].as_ptr(),
        proxy: strings[3].as_ptr(),
        identity: strings[4].as_ptr(),
        config_json: if i.config_json.is_empty() { c"{}".as_ptr() } else { strings[5].as_ptr() },
        exit_ip: strings[6].as_ptr(),
    };
    InfoC { _strings: strings, info }
}

/// The answer slots ask the last loaded plugin first.
fn answerers() -> impl Iterator<Item = Arc<Loaded>> {
    loaded().into_iter().rev()
}

fn take_string(p: &Loaded, s: *const c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the plugin returned a NUL-terminated string, valid until it frees it.
    let out = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    if let Some(free) = p.desc.free_string {
        // SAFETY: handing the plugin back its own string.
        unsafe { free(p.desc.self_, s) };
    }
    Some(out)
}

/// `rotation`: the seed and stable name a new context wears, from the first plugin that answers.
pub fn rotation(i: &ContextInfo) -> Option<(u64, String)> {
    traced("rotation", &i.context_id);
    let c = info_c(i);
    for p in answerers() {
        let Some(f) = p.desc.rotation else { continue };
        let mut seed = 0u64;
        let mut name = vec![0 as c_char; 256];
        // SAFETY: the info and the name buffer live across the call.
        if unsafe { f(p.desc.self_, &c.info, &mut seed, name.as_mut_ptr(), name.len()) } != 0 {
            let n = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
            return Some((seed, n));
        }
    }
    None
}

/// `device`: a new context's device facts (JSON), from the first plugin that answers.
pub fn device(i: &ContextInfo, seed: u64) -> Option<String> {
    traced("device", &i.context_id);
    let c = info_c(i);
    for p in answerers() {
        let Some(f) = p.desc.device else { continue };
        // SAFETY: as `rotation`.
        let s = unsafe { f(p.desc.self_, &c.info, seed) };
        if let Some(j) = take_string(&p, s) {
            return Some(j);
        }
    }
    None
}

/// `identity`: a new context's persona document (JSON), from the first plugin that answers.
pub fn identity(i: &ContextInfo, device_json: &str) -> Option<String> {
    traced("identity", &i.context_id);
    let c = info_c(i);
    let d = CString::new(device_json).unwrap_or_default();
    for p in answerers() {
        let Some(f) = p.desc.identity else { continue };
        // SAFETY: as `rotation`.
        let s = unsafe { f(p.desc.self_, &c.info, d.as_ptr()) };
        if let Some(j) = take_string(&p, s) {
            return Some(j);
        }
    }
    None
}

/// Does this plugin perturb readbacks of `kind` now (`readback_kinds`)?
fn takes(p: &Loaded, kind: i32) -> bool {
    match (p.desc.on_readback, p.desc.readback_kinds) {
        (None, _) => false,
        // A plugin with on_readback but no readback_kinds took "every kind" as of ABI 2 -- meaning the kinds that
        // existed then (CANVAS, WEBGL, AUDIO). A kind added later (TEXT, ABI 3) is NOT auto-delivered to it; it must
        // opt in via readback_kinds. So an older plugin's on_readback is never handed a readback shape (null rgba
        // AND null samples) it was never written for -- the ABI-2 contract is preserved exactly.
        (Some(_), None) => kind == READBACK_CANVAS || kind == READBACK_WEBGL || kind == READBACK_AUDIO,
        // SAFETY: the plugin's own callback with its own self.
        (Some(_), Some(k)) => (unsafe { k(p.desc.self_) } & (1u32 << kind)) != 0,
    }
}

/// Does any plugin perturb readbacks of `kind` now? A readback none takes is read as with no plugin loaded.
pub fn has_readback(kind: i32) -> bool {
    any() && loaded().iter().any(|p| takes(p, kind))
}

/// A readback's pixels (`READBACK_CANVAS`, `READBACK_WEBGL`; width * height * 4 bytes, top-left origin), through every
/// plugin that perturbs readbacks of that kind, in load order.
pub fn readback_rgba(kind: i32, width: u32, height: u32, rgba: &mut [u8]) {
    traced("readback", match kind {
        READBACK_CANVAS => "canvas",
        READBACK_WEBGL => "webgl",
        READBACK_AUDIO => "audio",
        _ => "",
    });
    for p in loaded() {
        let Some(f) = p.desc.on_readback.filter(|_| takes(&p, kind)) else { continue };
        let mut rb = KorvusReadback { kind, width, height, rgba: rgba.as_mut_ptr(), samples: std::ptr::null_mut(), count: rgba.len(), text: std::ptr::null(), metrics: std::ptr::null_mut() };
        // SAFETY: the buffer lives across the callback, `count` long.
        unsafe { f(p.desc.self_, &mut rb) };
    }
}

/// An offline audio render's samples (`READBACK_AUDIO`), through every plugin that perturbs readbacks.
pub fn readback_samples(samples: &mut [f32]) {
    traced("readback", "audio");
    for p in loaded() {
        let Some(f) = p.desc.on_readback.filter(|_| takes(&p, READBACK_AUDIO)) else { continue };
        let mut rb = KorvusReadback { kind: READBACK_AUDIO, width: 0, height: 0, rgba: std::ptr::null_mut(), samples: samples.as_mut_ptr(), count: samples.len(), text: std::ptr::null(), metrics: std::ptr::null_mut() };
        // SAFETY: as `readback_rgba`.
        unsafe { f(p.desc.self_, &mut rb) };
    }
}

/// A text measurement (`READBACK_TEXT`, ABI 3): the fingerprint tap on `measureText` / `TextMetrics`. `metrics`
/// is perturbed IN PLACE by every plugin that opted into `READBACK_TEXT`, in load order, in the fixed order
/// `[width, fontBoundingBoxAscent, fontBoundingBoxDescent]` (CSS px); the caller derives the full `TextMetrics`
/// from the perturbed values so a shift stays self-consistent. `text` is the measured string the plugin may read
/// to key its noise. Called only behind [`has_readback`]`(READBACK_TEXT)`, so a run with no TEXT-tapping plugin
/// -- every no-plugin and stealth-only run -- never reaches here and the metrics are byte-for-byte the engine's
/// (I9/J2). The perturbation and its model are the plugin's; korvus passes only the point and the raw numbers (J6).
pub fn readback_text(text: &str, metrics: &mut [f64]) {
    traced("readback", "text");
    let c = CString::new(text).unwrap_or_default();
    for p in loaded() {
        let Some(f) = p.desc.on_readback.filter(|_| takes(&p, READBACK_TEXT)) else { continue };
        let mut rb = KorvusReadback {
            kind: READBACK_TEXT, width: 0, height: 0, rgba: std::ptr::null_mut(), samples: std::ptr::null_mut(),
            count: metrics.len(), text: c.as_ptr(), metrics: metrics.as_mut_ptr(),
        };
        // SAFETY: `metrics` lives across the callback, `count` long; `text` is a valid NUL-terminated C string
        // owned by `c` for the duration of the loop.
        unsafe { f(p.desc.self_, &mut rb) };
    }
}

/// ABI 3 (fake-media / D-media): what `getUserMedia(kind, constraints)` resolves to -- a JSON array of track
/// descriptors, or `None` to pass. Asked of the loaded plugins from LAST to first; the first non-NULL answer wins
/// (a later plugin overrides an earlier one), its string freed via the plugin's `free_string`. `None` with no
/// plugin loaded and when no plugin answers, so the engine then rejects as device-absent -- exactly as with no
/// plugin (I9/J2). `kind` is "user" (getUserMedia) or "display" (getDisplayMedia).
pub fn on_user_media(kind: &str, constraints_json: &str) -> Option<String> {
    if !any() {
        return None;
    }
    let (k, c) = (CString::new(kind).ok()?, CString::new(constraints_json).ok()?);
    let plugins = loaded();
    for p in plugins.iter().rev() {
        let Some(f) = p.desc.on_user_media else { continue };
        // SAFETY: the plugin's own callback; the two strings live across the call.
        let s = unsafe { f(p.desc.self_, k.as_ptr(), c.as_ptr()) };
        if let Some(out) = take_string(p, s) {
            return Some(out);
        }
    }
    None
}

/// Does any plugin fill an input slot?
pub fn has_input() -> bool {
    any() && loaded().iter().any(|p| p.desc.plan_move.is_some() || p.desc.type_text.is_some() || p.desc.scroll.is_some() || p.desc.press_dwell_us.is_some())
}

fn grow<T: Copy + Default>(call: impl Fn(*mut T, usize) -> usize) -> Option<Vec<T>> {
    let mut buf = vec![T::default(); 256];
    let n = call(buf.as_mut_ptr(), buf.len());
    if n == 0 {
        return None;
    }
    if n > buf.len() {
        buf = vec![T::default(); n];
        let m = call(buf.as_mut_ptr(), buf.len());
        buf.truncate(m.min(n));
    } else {
        buf.truncate(n);
    }
    Some(buf)
}

/// `plan_move`: a mouse path from the first plugin that answers.
pub fn plan_move(from: KorvusPoint, to: KorvusPoint, target_width: f64) -> Option<Vec<KorvusMotionSample>> {
    traced("plan_move", "");
    for p in answerers() {
        let Some(f) = p.desc.plan_move else { continue };
        // SAFETY: the buffer is `cap` samples long.
        if let Some(v) = grow(|out, cap| unsafe { f(p.desc.self_, from, to, target_width, out, cap) }) {
            return Some(v);
        }
    }
    None
}

/// `type_text`: the key events for `text` from the first plugin that answers.
pub fn type_text(text: &str) -> Option<Vec<KorvusKeyEvent>> {
    traced("type_text", "");
    let t = CString::new(text).ok()?;
    for p in answerers() {
        let Some(f) = p.desc.type_text else { continue };
        // SAFETY: as `plan_move`.
        if let Some(v) = grow(|out, cap| unsafe { f(p.desc.self_, t.as_ptr(), out, cap) }) {
            return Some(v);
        }
    }
    None
}

/// `scroll`: the wheel steps for `dy` from the first plugin that answers.
pub fn scroll(dy: f64) -> Option<Vec<KorvusWheelEvent>> {
    traced("scroll", "");
    for p in answerers() {
        let Some(f) = p.desc.scroll else { continue };
        // SAFETY: as `plan_move`.
        if let Some(v) = grow(|out, cap| unsafe { f(p.desc.self_, dy, out, cap) }) {
            return Some(v);
        }
    }
    None
}

/// `press_dwell_us`: how long a click's button stays down, from the first plugin that answers.
pub fn press_dwell_us() -> Option<f64> {
    traced("press_dwell_us", "");
    for p in answerers() {
        let Some(f) = p.desc.press_dwell_us else { continue };
        // SAFETY: the plugin's own callback.
        let v = unsafe { f(p.desc.self_) };
        if v >= 0.0 {
            return Some(v);
        }
    }
    None
}

// ── ABI 3 (wave 1) dispatch ──────────────────────────────────────────────────────────────────────

/// ABI 3 (wave 1, 4.5, `verify_cert`): verify a TLS server certificate for `host` (`leaf_der_b64` is its leaf cert's
/// DER, base64). Every plugin is asked in load order; the first that returns 1 (allow) or 0 (deny) decides, and one
/// that returns -1 defers. `Some(true)` allow, `Some(false)` deny, `None` when no plugin decided (korvus's own
/// verification then stands).
pub fn verify_cert(host: &str, leaf_der_b64: &str) -> Option<bool> {
    traced("verify_cert", host);
    if !any() {
        return None;
    }
    let (Ok(h), Ok(d)) = (CString::new(host), CString::new(leaf_der_b64)) else { return None };
    for p in loaded() {
        let Some(f) = p.desc.verify_cert else { continue };
        // SAFETY: the strings live across the call; the plugin's own callback with its own self.
        match unsafe { f(p.desc.self_, h.as_ptr(), d.as_ptr()) } {
            1 => return Some(true),
            0 => return Some(false),
            _ => {}
        }
    }
    None
}

/// ABI 3 (wave 1, B3, `init_script`): the scripts to run before the page's own scripts in a realm of `realm_kind`
/// (`REALM_*`), from every plugin that fills the slot, in load order. Each is JavaScript source; the runtime injects
/// them in this order, each before any page script.
pub fn init_scripts(realm_kind: i32) -> Vec<String> {
    traced("init_script", match realm_kind {
        REALM_PAGE => "page",
        REALM_DEDICATED_WORKER => "dedicated-worker",
        REALM_SHARED_WORKER => "shared-worker",
        REALM_SERVICE_WORKER => "service-worker",
        _ => "other",
    });
    let mut out = Vec::new();
    for p in loaded() {
        let Some(f) = p.desc.init_script else { continue };
        // SAFETY: the plugin's own callback; its returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, realm_kind) };
        if let Some(js) = take_string(&p, s) {
            out.push(js);
        }
    }
    out
}

/// ABI 3 (WebExtension MV3 compat, B3 isolated worlds): the init scripts for the NAMED ISOLATED worlds of a realm
/// of `realm_kind` (`REALM_*`), gathered across every plugin that fills the slot, in load order. Mirrors
/// [`init_scripts`] but keyed by world: each entry is `(world name, scripts in order)`. The consumer feeds these
/// to named isolated worlds (`Engine::push_document_worlds`), appended AFTER any CDP-named worlds. Empty with no
/// plugin loaded and when no plugin fills the slot, so the no-plugin page gets no extra world (I9/J2).
pub fn init_world_scripts(realm_kind: i32) -> Vec<(String, Vec<String>)> {
    traced("init_world_script", match realm_kind {
        REALM_PAGE => "page",
        REALM_DEDICATED_WORKER => "dedicated-worker",
        REALM_SHARED_WORKER => "shared-worker",
        REALM_SERVICE_WORKER => "service-worker",
        _ => "other",
    });
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for p in loaded() {
        let (Some(count), Some(script)) = (p.desc.init_world_count, p.desc.init_world_script) else { continue };
        // SAFETY: the plugin's own callbacks with its own self_.
        let n = unsafe { count(p.desc.self_, realm_kind) };
        for i in 0..n.max(0) {
            let mut name = vec![0 as c_char; 256];
            // SAFETY: `name` lives across the call; the plugin writes a NUL-terminated world name into it (cut to
            // fit), and the returned source is freed via `free_string` in `take_string`.
            let s = unsafe { script(p.desc.self_, realm_kind, i, name.as_mut_ptr(), name.len()) };
            let Some(src) = take_string(&p, s) else { continue }; // NULL skips this index
            let world = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
            match out.iter_mut().find(|(w, _)| *w == world) {
                Some((_, scripts)) => scripts.push(src),
                None => out.push((world, vec![src])),
            }
        }
    }
    out
}

/// ABI 3 (wave 1, C1): does anything want painted frames -- a plugin (`want_frames` non-zero with `on_frame` set), or
/// the engine's own sink (a CDP screencast, [`set_frame_sink`])? Frames are captured only then.
pub fn has_frames() -> bool {
    frame_sink().read().unwrap_or_else(|e| e.into_inner()).is_some() || plugins_want_frames()
}

/// Does any PLUGIN want painted frames? [`on_frame`] is called only then.
pub fn plugins_want_frames() -> bool {
    any() && loaded().iter().any(|p| p.desc.want_frames != 0 && p.desc.on_frame.is_some())
}

/// A frame for the engine's own sink: the frame tap's pixels (RGBA, unpremultiplied, top-left origin), its
/// timestamp in microseconds from the page's start, and the document's scroll offset when it was painted.
pub struct SinkFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    pub t_us: f64,
    pub scroll: (f64, f64),
}

/// The engine's own consumer of painted frames, beside the plugins' `on_frame`: a CDP screencast
/// (`Page.startScreencast`, D74 step 3). Called on the page's thread with each frame the frame tap hands on.
pub type FrameSink = Arc<dyn Fn(&SinkFrame) + Send + Sync>;

fn frame_sink() -> &'static RwLock<Option<FrameSink>> {
    static S: OnceLock<RwLock<Option<FrameSink>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Install (or, with `None`, remove) the engine's frame sink. While one is installed the page paints for it as it
/// would for a frame-wanting plugin ([`has_frames`]).
pub fn set_frame_sink(sink: Option<FrameSink>) {
    *frame_sink().write().unwrap_or_else(|e| e.into_inner()) = sink;
}

/// Hand a painted frame to the engine's frame sink, if one is installed.
pub fn sink_frame(frame: &SinkFrame) {
    let sink = frame_sink().read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(s) = sink {
        s(frame);
    }
}

/// ABI 3 (wave 1, C1, `on_frame`): a painted frame (RGBA, top-left origin, `width` * `height` * 4 bytes) to every
/// plugin that wants frames, in load order. `t_us` is the frame's timestamp in microseconds.
pub fn on_frame(width: u32, height: u32, rgba: &[u8], t_us: f64) {
    if trace_on() {
        traced("on_frame", &format!("{width}x{height}"));
    }
    if !any() {
        return;
    }
    let frame = KorvusFrame { width, height, rgba: rgba.as_ptr(), len: rgba.len(), t_us };
    for p in loaded() {
        if p.desc.want_frames == 0 {
            continue;
        }
        if let Some(f) = p.desc.on_frame {
            // SAFETY: the frame and its pixels live across the callback, `len` long.
            unsafe { f(p.desc.self_, &frame) };
        }
    }
}

/// ABI 3 (wave 1, step 7, `on_dialog`): a JavaScript dialog (`kind` is "alert", "confirm" or "prompt"; `message` its
/// text). Every plugin is asked in load order; the first that returns 1 (accept) or 0 (dismiss) decides. `Some((true,
/// text))` accepts it (`text` is a prompt's reply, else ""), `Some((false, _))` dismisses it, `None` when no plugin
/// decided (korvus's default then stands).
pub fn on_dialog(kind: &str, message: &str) -> Option<(bool, String)> {
    traced("on_dialog", kind);
    if !any() {
        return None;
    }
    let (Ok(k), Ok(m)) = (CString::new(kind), CString::new(message)) else { return None };
    for p in loaded() {
        let Some(f) = p.desc.on_dialog else { continue };
        let mut out = vec![0 as c_char; 4096];
        // SAFETY: the strings and the out buffer live across the call; the plugin's own callback.
        match unsafe { f(p.desc.self_, k.as_ptr(), m.as_ptr(), out.as_mut_ptr(), out.len()) } {
            1 => {
                // SAFETY: the plugin wrote a NUL-terminated reply into `out`.
                let text = unsafe { CStr::from_ptr(out.as_ptr()) }.to_string_lossy().into_owned();
                return Some((true, text));
            }
            0 => return Some((false, String::new())),
            _ => {}
        }
    }
    None
}

/// ABI 3 (wave 1, step 7): does any plugin take downloads (`on_download` set)?
pub fn wants_downloads() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_download.is_some())
}

/// ABI 3 (wave 1, step 7, `on_download`): a finished download (`url`, the page's `name`, and `data` its bytes) to
/// every plugin that takes downloads, in load order.
pub fn on_download(url: &str, name: &str, data: &[u8]) {
    traced("on_download", url);
    if !any() {
        return;
    }
    let (Ok(u), Ok(n)) = (CString::new(url), CString::new(name)) else { return };
    for p in loaded() {
        if let Some(f) = p.desc.on_download {
            // SAFETY: the strings and `data` live across the callback, `data.len()` long.
            unsafe { f(p.desc.self_, u.as_ptr(), n.as_ptr(), data.as_ptr(), data.len()) };
        }
    }
}

// ── ABI 3 (step 5): storage (E1 profiles) ──────────────────────────────────────────────────────────

/// ABI 3 (step 5, E1): does any plugin persist or restore a context's state (`storage_load` or `storage_save` set)?
pub fn has_storage() -> bool {
    any() && loaded().iter().any(|p| p.desc.storage_load.is_some() || p.desc.storage_save.is_some())
}

/// ABI 3 (step 5, E1, `storage_load`): a context's previously-saved state blob to seed a new context, from the first
/// plugin that returns non-NULL (the answer slots' order, last loaded first). The blob is the opaque UTF-8 string a
/// plugin stored (e.g. base64); `None` when no plugin has one.
pub fn storage_load(info: &ContextInfo) -> Option<String> {
    traced("storage_load", &info.context_id);
    let c = info_c(info);
    for p in answerers() {
        let Some(f) = p.desc.storage_load else { continue };
        // SAFETY: the info lives across the call; the returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, &c.info) };
        if let Some(blob) = take_string(&p, s) {
            return Some(blob);
        }
    }
    None
}

/// ABI 3 (step 5, E1, `storage_save`): persist a context's state `blob` at teardown, to every plugin with
/// `storage_save` set, in load order.
pub fn storage_save(info: &ContextInfo, blob: &str) {
    traced("storage_save", &info.context_id);
    if !any() {
        return;
    }
    let c = info_c(info);
    let Ok(b) = CString::new(blob) else { return };
    for p in loaded() {
        if let Some(f) = p.desc.storage_save {
            // SAFETY: the info and blob live across the callback; the plugin's own callback with its own self.
            unsafe { f(p.desc.self_, &c.info, b.as_ptr()) };
        }
    }
}

// ── ABI 3 (step 8): tenancy slots (G1 / G2 / G4) ───────────────────────────────────────────────────

/// ABI 3 (step 8, G1): does any plugin gate connections (`on_connect` set)? The router asks a client's connection
/// through [`on_connect`] only then; with none it admits every client as before.
pub fn has_connect_auth() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_connect.is_some())
}

/// ABI 3 (step 8, G1, `on_connect`): a CDP/HTTP client is connecting -- `auth` is its bearer token ("" when none),
/// `peer` its address. Every plugin with `on_connect` is asked in load order; the first that returns a value < 0
/// REFUSES the connection (`None`), else the first non-negative value is the admitted tenant id (`Some(tenant)`).
/// `None` with no plugin answering too (the router calls this only when [`has_connect_auth`]).
pub fn on_connect(auth: &str, peer: &str) -> Option<i64> {
    traced("on_connect", peer);
    let (Ok(a), Ok(p)) = (CString::new(auth), CString::new(peer)) else { return None };
    let mut tenant: Option<i64> = None;
    for pl in loaded() {
        let Some(f) = pl.desc.on_connect else { continue };
        // SAFETY: the strings live across the call; the plugin's own callback with its own self.
        let v = unsafe { f(pl.desc.self_, a.as_ptr(), p.as_ptr()) };
        if v < 0 {
            return None; // the first plugin that refuses wins
        }
        // TODO(1021-verify): with several admitting plugins the FIRST non-negative id is the tenant; confirm the owner
        // wants first-admitter (not last) to attach the tenant.
        tenant.get_or_insert(v as i64);
    }
    tenant
}

/// ABI 3 (step 8, G2, `context_allow`): may `tenant` create `context` now (a quota decision)? Every plugin with
/// `context_allow` is asked and all must allow; `true` when every one does (and when none fills the slot), `false`
/// as soon as one refuses. A refusal blocks only this new context, never one already running (invariant I3).
pub fn context_allow(tenant: i64, context: &str) -> bool {
    traced("context_allow", context);
    let Ok(c) = CString::new(context) else { return false };
    for p in loaded() {
        let Some(f) = p.desc.context_allow else { continue };
        // SAFETY: the string lives across the call; the plugin's own callback with its own self.
        if unsafe { f(p.desc.self_, tenant as c_long, c.as_ptr()) } == 0 {
            return false;
        }
    }
    true
}

/// ABI 3 (step 8, G4, `queue_order`): the sessions waiting under the license cap belong to `tenants` -- the first
/// plugin that fills the slot (the answer slots' order, last loaded first) returns the INDEX into `tenants` of the
/// one to admit next. `None` when no plugin fills it or a plugin returns an out-of-range index (korvus's own order
/// then stands); this is fairness within the cap and cannot change it.
pub fn queue_order(tenants: &[i64]) -> Option<usize> {
    traced("queue_order", "");
    if tenants.is_empty() {
        return None;
    }
    let ids: Vec<c_long> = tenants.iter().map(|&t| t as c_long).collect();
    for p in answerers() {
        let Some(f) = p.desc.queue_order else { continue };
        // SAFETY: the slice lives across the call, `len` long; the plugin's own callback with its own self.
        let idx = unsafe { f(p.desc.self_, ids.as_ptr(), ids.len() as c_uint) };
        if idx >= 0 && (idx as u64) < tenants.len() as u64 {
            return Some(idx as usize);
        }
    }
    None
}

// ── ABI 3 (step 10): page bindings (B4), file chooser (7.3), a new context's proxy (D2) ────────────

/// ABI 3 (step 10, B4, `on_binding`): a page called a bound global (one of [`binding_names`], installed from
/// `host->add_binding`). `name` is the binding and `payload` the single string argument the page passed. Every
/// plugin with `on_binding` set is told, in load order.
/// ABI 3 (WebExtension message bus, P1b): a binding call that RETURNS a response. The FIRST loaded plugin whose
/// `on_binding_call` answers non-NULL wins; its string is freed via `free_string`. `None` when no plugin fills the
/// slot (the caller then falls back to the void [`on_binding`]). Runs on the calling realm's thread, like `on_binding`.
pub fn on_binding_call(name: &str, payload: &str) -> Option<String> {
    if !any() {
        return None;
    }
    let (n, p) = (CString::new(name).ok()?, CString::new(payload).ok()?);
    for pl in loaded() {
        let Some(f) = pl.desc.on_binding_call else { continue };
        // SAFETY: the strings live across the call; the plugin's own callback with its own self.
        let s = unsafe { f(pl.desc.self_, n.as_ptr(), p.as_ptr()) };
        if let Some(out) = take_string(&pl, s) {
            return Some(out);
        }
    }
    None
}

pub fn on_binding(name: &str, payload: &str) {
    traced("on_binding", name);
    if !any() {
        return;
    }
    let (Ok(n), Ok(p)) = (CString::new(name), CString::new(payload)) else { return };
    for pl in loaded() {
        if let Some(f) = pl.desc.on_binding {
            // SAFETY: the strings live across the callback; the plugin's own callback with its own self.
            unsafe { f(pl.desc.self_, n.as_ptr(), p.as_ptr()) };
        }
    }
}

/// ABI 3 (step 10, 7.3, `on_file_chooser`): a page opened a file chooser; `accept` is its accept hint ("" when
/// none). The first plugin with `on_file_chooser` set decides (like `on_dialog`): `Some(paths)` supplies those
/// absolute file paths to the `<input type=file>`, `None` cancels it (the plugin returned NULL / "" or only
/// blanks, or no plugin fills the slot). The plugin's returned string is a newline-separated path list, freed via
/// `free_string`.
pub fn on_file_chooser(accept: &str) -> Option<Vec<String>> {
    traced("on_file_chooser", accept);
    if !any() {
        return None;
    }
    let a = CString::new(accept).unwrap_or_default();
    for p in loaded() {
        let Some(f) = p.desc.on_file_chooser else { continue };
        // SAFETY: the string lives across the call; the returned list is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, a.as_ptr()) };
        // The first plugin with the slot decides -- NULL / "" / only-blanks cancels (empty list -> None).
        let files: Vec<String> = match take_string(&p, s) {
            Some(list) => list.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect(),
            None => return None,
        };
        return if files.is_empty() { None } else { Some(files) };
    }
    None
}

/// ABI 3 (step 10, D2, `choose_proxy`): does any plugin choose a new context's proxy (`choose_proxy` set)? The
/// router asks [`choose_proxy`] only then; with none a context keeps its own configured proxy.
pub fn has_choose_proxy() -> bool {
    any() && loaded().iter().any(|p| p.desc.choose_proxy.is_some())
}

/// ABI 3 (step 10, D2, `choose_proxy`): the proxy URL a new context should exit through, from the first plugin
/// that answers (the answer slots' order, last loaded first). `Some(url)` routes the context through `url`;
/// `None` leaves the context's own configured proxy (every plugin passed with NULL / "", or none fills the slot).
pub fn choose_proxy(info: &ContextInfo) -> Option<String> {
    traced("choose_proxy", &info.context_id);
    let c = info_c(info);
    for p in answerers() {
        let Some(f) = p.desc.choose_proxy else { continue };
        // SAFETY: the info lives across the call; the returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, &c.info) };
        if let Some(url) = take_string(&p, s) {
            if !url.is_empty() {
                return Some(url);
            }
        }
    }
    None
}

// ── ABI 3 (step 12, Frontier S): proxy auth (D3), cookie / storage events (D9 / E2), clipboard read (C5), ───
// ── permission / geolocation, cursor / focus (C3) ──────────────────────────────────────────────────────────

/// ABI 3 (step 12, D3, `on_proxy_auth`): the credentials for a proxy that challenges (a 407). `proxy` is the
/// proxy URL being connected to. Asked from the first plugin that answers (the answer slots' order, last loaded
/// first); `Some("user:pass")` authenticates, `None` when every plugin passed with NULL / "" or none fills the
/// slot. The returned string is freed via `free_string`.
pub fn on_proxy_auth(proxy: &str) -> Option<String> {
    traced("on_proxy_auth", proxy);
    if !any() {
        return None;
    }
    let u = CString::new(proxy).ok()?;
    for p in answerers() {
        let Some(f) = p.desc.on_proxy_auth else { continue };
        // SAFETY: the string lives across the call; the returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, u.as_ptr()) };
        if let Some(creds) = take_string(&p, s) {
            if !creds.is_empty() {
                return Some(creds);
            }
        }
    }
    None
}

/// ABI 3 (step 12, D9): does any plugin watch cookie changes (`on_cookie` set)? The cookie jar emits [`on_cookie`]
/// only then; with none a cookie change costs nothing.
pub fn wants_cookies() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_cookie.is_some())
}

/// ABI 3 (step 12, D9, `on_cookie`): a cookie was set or deleted -- `ctx` the context's CDP id, `name` and
/// `domain` the cookie's, `op` "set" or "delete". Every plugin with `on_cookie` set is told, in load order.
pub fn on_cookie(ctx: &str, name: &str, domain: &str, op: &str) {
    traced("on_cookie", name);
    if !any() {
        return;
    }
    let (Ok(c), Ok(n), Ok(d), Ok(o)) = (CString::new(ctx), CString::new(name), CString::new(domain), CString::new(op)) else { return };
    for p in loaded() {
        if let Some(f) = p.desc.on_cookie {
            // SAFETY: the strings live across the callback; the plugin's own callback with its own self.
            unsafe { f(p.desc.self_, c.as_ptr(), n.as_ptr(), d.as_ptr(), o.as_ptr()) };
        }
    }
}

/// ABI 3 (step 12, E2): does any plugin watch storage changes (`on_storage_change` set)? The context store emits
/// [`on_storage_change`] only then.
pub fn wants_storage_changes() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_storage_change.is_some())
}

/// ABI 3 (step 12, E2, `on_storage_change`): a storage area changed -- `ctx` the context's CDP id, `slot` the
/// area touched ("local", "session", "indexeddb", ...). Every plugin with `on_storage_change` set is told, in
/// load order.
pub fn on_storage_change(ctx: &str, slot: &str) {
    traced("on_storage_change", slot);
    if !any() {
        return;
    }
    let (Ok(c), Ok(s)) = (CString::new(ctx), CString::new(slot)) else { return };
    for p in loaded() {
        if let Some(f) = p.desc.on_storage_change {
            // SAFETY: the strings live across the callback; the plugin's own callback with its own self.
            unsafe { f(p.desc.self_, c.as_ptr(), s.as_ptr()) };
        }
    }
}

/// ABI 3 (step 12, C5, `on_clipboard_read`): the text a page reads from the clipboard (a paste). Asked from the
/// first plugin that answers (the answer slots' order, last loaded first); `Some(text)` is what the page pastes,
/// `None` when every plugin passed with NULL or none fills the slot (the page reads the real clipboard). The
/// returned string is freed via `free_string`. Pair with `host->clipboard_write` ([`take_clipboard_writes`]).
pub fn on_clipboard_read() -> Option<String> {
    traced("on_clipboard_read", "");
    if !any() {
        return None;
    }
    for p in answerers() {
        let Some(f) = p.desc.on_clipboard_read else { continue };
        // SAFETY: the plugin's own callback; its returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_) };
        if let Some(text) = take_string(&p, s) {
            return Some(text);
        }
    }
    None
}

/// ABI 3 (step 12, `on_permission`): a permission prompt for `name` ("geolocation", "notifications", ...). Every
/// plugin is asked in load order; the first that returns 1 (grant) or 0 (deny) decides, and one that returns -1
/// defers. `Some(true)` grant, `Some(false)` deny, `None` when no plugin decided (korvus's default then stands).
/// Mirrors [`on_dialog`].
pub fn on_permission(name: &str) -> Option<bool> {
    traced("on_permission", name);
    if !any() {
        return None;
    }
    let n = CString::new(name).ok()?;
    for p in loaded() {
        let Some(f) = p.desc.on_permission else { continue };
        // SAFETY: the string lives across the call; the plugin's own callback with its own self.
        match unsafe { f(p.desc.self_, n.as_ptr()) } {
            1 => return Some(true),
            0 => return Some(false),
            _ => {}
        }
    }
    None
}

/// ABI 3 (step 12, `geolocation`): the position a context reports for `navigator.geolocation`, as a JSON object
/// {"lat":.., "lng":.., "acc":..}. `ctx` is the context's CDP id. Asked from the first plugin that answers (the
/// answer slots' order, last loaded first); `Some(json)` supplies a position, `None` when every plugin passed
/// with NULL / "" or none fills the slot. The returned string is freed via `free_string`.
pub fn geolocation(ctx: &str) -> Option<String> {
    traced("geolocation", ctx);
    if !any() {
        return None;
    }
    let c = CString::new(ctx).ok()?;
    for p in answerers() {
        let Some(f) = p.desc.geolocation else { continue };
        // SAFETY: the string lives across the call; the returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, c.as_ptr()) };
        if let Some(j) = take_string(&p, s) {
            if !j.is_empty() {
                return Some(j);
            }
        }
    }
    None
}

/// ABI 3 (step 12, C3): does any plugin watch the cursor or focus state (`on_cursor` or `on_focus_state` set)?
/// The engine emits [`on_cursor`] / [`on_focus_state`] only then.
pub fn wants_cursor() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_cursor.is_some() || p.desc.on_focus_state.is_some())
}

/// ABI 3 (step 12, C3, `on_cursor`): the cursor's shape changed -- `shape` is a CSS cursor name ("default",
/// "pointer", "text", ...). Every plugin with `on_cursor` set is told, in load order.
pub fn on_cursor(shape: &str) {
    traced("on_cursor", shape);
    if !any() {
        return;
    }
    let Ok(s) = CString::new(shape) else { return };
    for p in loaded() {
        if let Some(f) = p.desc.on_cursor {
            // SAFETY: the string lives across the callback; the plugin's own callback with its own self.
            unsafe { f(p.desc.self_, s.as_ptr()) };
        }
    }
}

/// ABI 3 (step 12, C3, `on_focus_state`): the page's focus state changed -- `focused` whether the page has focus,
/// `editable` whether the focused node takes text input. Every plugin with `on_focus_state` set is told, in load
/// order.
pub fn on_focus_state(focused: bool, editable: bool) {
    traced("on_focus_state", "");
    if !any() {
        return;
    }
    for p in loaded() {
        if let Some(f) = p.desc.on_focus_state {
            // SAFETY: the plugin's own callback with its own self.
            unsafe { f(p.desc.self_, focused as c_int, editable as c_int) };
        }
    }
}

// ── ABI 3 (step 13, Frontier M): DNS resolution (D4), popup/target control ───────────────────────────

/// ABI 3 (step 13, D4): what a plugin's `resolve` answered for a host. `Ips` are the addresses to use for it (as
/// the plugin listed them, left to right), `Block` refuses the lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsAnswer {
    Ips(Vec<String>),
    Block,
}

/// ABI 3 (step 13, D4, `resolve`): does any plugin resolve host names (`resolve` set)? The resolver asks
/// [`resolve`] only then; with none korvus resolves every host normally.
pub fn has_resolver() -> bool {
    any() && loaded().iter().any(|p| p.desc.resolve.is_some())
}

/// ABI 3 (step 13, D4, `resolve`): resolve `host`, from the first plugin that answers (the answer slots' order,
/// last loaded first). The plugin returns a comma-separated IP list, the literal "block", or NULL to pass;
/// `Some(DnsAnswer::Ips(..))` gives the addresses to use, `Some(DnsAnswer::Block)` refuses the lookup, and `None`
/// when every plugin passed with NULL (or an empty list) or none fills the slot (korvus resolves it normally).
/// The returned string is freed via `free_string`.
pub fn resolve(host: &str) -> Option<DnsAnswer> {
    traced("resolve", host);
    if !any() {
        return None;
    }
    let h = CString::new(host).ok()?;
    for p in answerers() {
        let Some(f) = p.desc.resolve else { continue };
        // SAFETY: the string lives across the call; the returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, h.as_ptr()) };
        if let Some(ans) = take_string(&p, s) {
            if ans.trim() == "block" {
                return Some(DnsAnswer::Block);
            }
            let ips: Vec<String> = ans.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
            if !ips.is_empty() {
                return Some(DnsAnswer::Ips(ips));
            }
            // a non-NULL but empty / all-blank answer passes to the next plugin (like choose_proxy)
        }
    }
    None
}

/// ABI 3 (step 13, `on_target_open`): does any plugin gate new targets (`on_target_open` set)? The engine asks
/// [`on_target_open`] only then; with none every new tab / window / popup opens as before.
pub fn has_target_gate() -> bool {
    any() && loaded().iter().any(|p| p.desc.on_target_open.is_some())
}

/// ABI 3 (step 13, `on_target_open`): a page is opening a new browsing target -- `url` the target's URL,
/// `disposition` how it would open ("newTab", "newWindow", "popup"). Every plugin with `on_target_open` set is
/// asked in load order and all must allow; `true` when every one does (and when none fills the slot), `false` as
/// soon as one returns 0 (block). Gates only a NEW target, never the page's own navigation. Mirrors
/// [`context_allow`].
pub fn on_target_open(url: &str, disposition: &str) -> bool {
    traced("on_target_open", url);
    let (Ok(u), Ok(d)) = (CString::new(url), CString::new(disposition)) else { return true };
    for p in loaded() {
        let Some(f) = p.desc.on_target_open else { continue };
        // SAFETY: the strings live across the call; the plugin's own callback with its own self.
        if unsafe { f(p.desc.self_, u.as_ptr(), d.as_ptr()) } == 0 {
            return false;
        }
    }
    true
}

// ── ABI 3 (step 14, Frontier L): a plugin-backed response cache (D10) ───────────────────────────────

/// Standard base64 (RFC 4648) for the cache slots: korvus and the plugin trade response bytes as base64 over
/// the C ABI (a plain C string), and korvus owns the encode/decode so the plugin stores exactly what it is
/// handed. These are kept here (no new crate dependency) and used only by [`cache_get`] / [`cache_put`].
fn b64_encode(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Decode standard base64, ignoring padding and ASCII whitespace; `None` on any other invalid character.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut bits = 0u32;
    let mut nbits = 0u32;
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        bits = (bits << 6) | val(c)?;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

/// ABI 3 (step 14, D10, `cache_get`): does any plugin back a response cache (`cache_get` set)? korvus
/// consults [`cache_get`] only then; with none every request is fetched as before (invariant I9/J2).
pub fn has_cache() -> bool {
    any() && loaded().iter().any(|p| p.desc.cache_get.is_some())
}

/// ABI 3 (step 14, D10, `cache_get`): a cached response for `key`, from the first plugin that answers (the
/// answer slots' order, last loaded first). The plugin returns base64 of the stored response bytes, or NULL
/// for a miss; korvus decodes the base64 and hands back the bytes. `None` when every plugin missed (NULL, or
/// an answer that is not valid base64 -- treated as a miss and passed to the next plugin) or none fills the
/// slot. The returned string is freed via `free_string`.
pub fn cache_get(key: &str) -> Option<Vec<u8>> {
    traced("cache_get", key);
    if !any() {
        return None;
    }
    let k = CString::new(key).ok()?;
    for p in answerers() {
        let Some(f) = p.desc.cache_get else { continue };
        // SAFETY: the key lives across the call; the returned string is freed via `free_string` in `take_string`.
        let s = unsafe { f(p.desc.self_, k.as_ptr()) };
        if let Some(b64) = take_string(&p, s) {
            if let Some(bytes) = b64_decode(&b64) {
                return Some(bytes);
            }
        }
    }
    None
}

/// ABI 3 (step 14, D10, `cache_put`): store `body` for `key`, to every plugin with `cache_put` set, in load
/// order. korvus base64-encodes the bytes and hands the plugin the key and the base64; the plugin backs the
/// store with Redis or anything. A no-op when no plugin fills the slot.
pub fn cache_put(key: &str, body: &[u8]) {
    traced("cache_put", key);
    if !any() {
        return;
    }
    let (Ok(k), Ok(v)) = (CString::new(key), CString::new(b64_encode(body))) else { return };
    for p in loaded() {
        if let Some(f) = p.desc.cache_put {
            // SAFETY: the strings live across the callback; the plugin's own callback with its own self.
            unsafe { f(p.desc.self_, k.as_ptr(), v.as_ptr()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_argument_keeps_a_drive_letter_in_its_path() {
        assert_eq!(split_arg("/a/b.so"), ("/a/b.so".to_string(), "{}".to_string()));
        assert_eq!(split_arg("/a/b.so:{\"cores\":12}"), ("/a/b.so".to_string(), "{\"cores\":12}".to_string()));
        assert_eq!(split_arg("C:\\p\\b.dll:{\"x\":1}"), ("C:\\p\\b.dll".to_string(), "{\"x\":1}".to_string()));
        assert_eq!(split_arg("C:\\p\\b.dll"), ("C:\\p\\b.dll".to_string(), "{}".to_string()));
    }

    #[test]
    fn nothing_loaded_claims_nothing() {
        assert!(!claims("Navigator.get platform"));
        assert!(!claims_any());
    }

    #[test]
    fn metrics_sum_per_key_and_labels_then_drain_clears() {
        let _ = drain_metrics();
        record_metric("requests", 1.0, "");
        record_metric("requests", 2.0, "");
        record_metric("requests", 1.0, r#"{"host":"a"}"#);
        record_metric("", 9.0, ""); // an empty key is dropped
        let mut got = drain_metrics();
        got.sort_by(|a, b| a.labels.cmp(&b.labels));
        assert_eq!(got, vec![Metric { name: "requests".into(), value: 3.0, labels: "".into() }, Metric { name: "requests".into(), value: 1.0, labels: r#"{"host":"a"}"#.into() }]);
        assert!(drain_metrics().is_empty());
    }

    #[test]
    fn page_eval_runs_through_the_installed_executor_and_truncates_at_a_char_boundary() {
        struct Echo;
        impl PageExecutor for Echo {
            fn eval(&self, world: &str, script: &str) -> Option<String> {
                if world.is_empty() && script == "document.title" {
                    Some("\"héllo\"".to_string())
                } else {
                    None
                }
            }
            fn spawn_background(&self, _kind: &str, _url: &str, _src: &str) -> u32 {
                0
            }
        }
        set_page_executor(Arc::new(Echo));
        let want = "\"héllo\""; // 8 bytes: the é is two (bytes: " h é é l l o ")
        // a cap of 6 leaves 5 bytes' room, which would split the second l's neighbour -- back off to a boundary,
        // still NUL-terminated, and report the full length
        let mut buf = vec![0 as c_char; 6];
        let n = unsafe { h_page_eval(std::ptr::null(), c"document.title".as_ptr(), buf.as_mut_ptr(), buf.len()) };
        assert_eq!(n, want.len());
        let got = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
        assert_eq!(got, "\"hél"); // 5 bytes: ", h, é(2), l -- all whole
        // a script the executor does not answer yields 0
        assert_eq!(unsafe { h_page_eval(std::ptr::null(), c"1+1".as_ptr(), std::ptr::null_mut(), 0) }, 0);
    }

    #[test]
    fn a_seed_is_stable_for_a_persona_and_differs_for_another() {
        set_context("C", "shop", "s");
        let a = seed_for("canvas");
        assert_eq!(a, seed_for("canvas"));
        assert_ne!(a, seed_for("audio"));
        set_context("C", "other", "s");
        assert_ne!(a, seed_for("canvas"));
    }

    #[test]
    fn base64_roundtrips_and_rejects_junk() {
        for case in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0u8, 255, 16, 200, 1]] {
            assert_eq!(b64_decode(&b64_encode(case)).as_deref(), Some(case));
        }
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64_decode("Zm9vYmFy").as_deref(), Some(&b"foobar"[..]));
        // padding and ASCII whitespace are ignored; a stray non-alphabet byte is rejected
        assert_eq!(b64_decode("Zm9v\n YmFy").as_deref(), Some(&b"foobar"[..]));
        assert!(b64_decode("not_base64!").is_none());
    }

    #[test]
    fn a_hold_resolves_when_resumed_then_its_token_drops_the_slot() {
        use std::future::Future; // `HoldToken::poll` is the Future-trait method
        use std::task::{Context, Poll, Wake, Waker};
        struct Noop;
        impl Wake for Noop {
            fn wake(self: Arc<Self>) {}
        }
        let waker = Waker::from(Arc::new(Noop));
        let mut cx = Context::from_waker(&waker);

        // A plugin parks a hop (what h_req_hold_id does): a slot exists, and the gate is now up.
        let id = hold_register();
        assert!(hold_gate(u64::MAX).is_none()); // an id nothing holds misses
        let mut token = hold_gate(id).expect("the parked hop's slot exists");

        // Pending until a resume arrives.
        assert_eq!(std::pin::Pin::new(&mut token).poll(&mut cx), Poll::Pending);

        // resume_request releases it (CONTINUE), and the next poll is Ready with that verdict.
        resume_request(id, CONTINUE);
        match std::pin::Pin::new(&mut token).poll(&mut cx) {
            Poll::Ready(r) => {
                assert_eq!(r.verdict, Verdict::Continue);
                assert!(r.modify.is_none());
            }
            Poll::Pending => panic!("a resumed hop must poll Ready"),
        }

        // Dropping the token drops the registry entry, so a fresh gate for the same id now misses.
        drop(token);
        assert!(hold_gate(id).is_none());
    }

    #[test]
    fn a_resume_that_races_ahead_of_the_gate_still_lands() {
        // The slot is made when the id is assigned, so a resume before hold_gate is not lost: the token
        // resolves on its first poll. (BLOCK aborts the hop; modify rides along on modify_and_resume.)
        use std::future::Future; // `HoldToken::poll` is the Future-trait method
        use std::task::{Context, Poll, Wake, Waker};
        struct Noop;
        impl Wake for Noop {
            fn wake(self: Arc<Self>) {}
        }
        let waker = Waker::from(Arc::new(Noop));
        let mut cx = Context::from_waker(&waker);

        let id = hold_register();
        modify_and_resume(id, BLOCK, HoldModify { url: Some("https://x/".into()), ..Default::default() });
        let mut token = hold_gate(id).expect("slot still present until the token drops");
        match std::pin::Pin::new(&mut token).poll(&mut cx) {
            Poll::Ready(r) => {
                assert_eq!(r.verdict, Verdict::Block);
                assert_eq!(r.modify.unwrap().url.as_deref(), Some("https://x/"));
            }
            Poll::Pending => panic!("a resume that preceded the gate must already be waiting"),
        }
    }
}
