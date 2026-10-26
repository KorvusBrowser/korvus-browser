//! `core/media/media_source.rs` -- `MediaSource` and `SourceBufferList` as two `interface!` tables.
//!
//! Rows are VERBATIM from the two blocks of `install/generated_worker_registry.rs` they replace --
//! the WORKER registry, not the window one, because `MediaSource` is this tree's first UNION TABLE
//! (see below). Both files' rows are otherwise identical key for key.
//!
//! A `MediaSource` is `closed` until a media element attaches it through its object URL; then it is `open`, and
//! its `SourceBuffer`s take bytes (`mse.rs`, which holds every source's state, its buffers and the element). The
//! refusals here are the spec's first steps for a source that is not `open` -- `duration`, `endOfStream`,
//! `setLiveSeekableRange` and `clearLiveSeekableRange` each throw `InvalidStateError` -- which the bundle's oracle
//! recorded off a real Brave 151 for a source never attached.
//!
//! THE UNION TABLE, and why this interface needed one. A table's NAME is what removes an interface
//! from BOTH generated registries, so one table has to serve both realms -- and the two captures
//! DISAGREE about this one: `worker.surface.json` records a 13th prototype member, `handle`, that
//! `surface.json` does not. The table therefore carries the WORKER's list (the superset, in the
//! worker's order) and the `worker` word on the `handle` row is the whole difference; it was
//! `WINDOW_MEMBERS`, a hand list `install_window_media_rtc` passed to
//! `installer::overwrite_prototype_selected` through `build_newable`'s new `selector` parameter. The
//! worker install passes `None` and gets all thirteen.
//!
//! `handle` IS A REAL ANSWER, NOT A STUB. Asked of headless Chrome 150.0.7871.46 over a loopback
//! server (`file://` is refused), inside a dedicated worker: `new MediaSource().handle` is
//! `[object MediaSourceHandle]`, `o.handle === o.handle` is true, its `[[Prototype]]` is
//! `MediaSourceHandle.prototype`, `Object.getOwnPropertyNames(o.handle)` is `[]`, and the getter on a
//! foreign receiver throws `TypeError: Illegal invocation`. `MediaSourceHandle` has ZERO prototype
//! members in the capture, so there is nothing about that object this engine has to invent: it is
//! minted on first read onto the (still registry-owned) `MediaSourceHandle.prototype` and kept, which
//! is all the recorded object is.
//!
//! INSTANCE STATE is a v8 private slot (`binding::instance_state`), not a `WeakMap` and not an own
//! property: `Object.getOwnPropertyNames(new MediaSource())` is `[]` -- the answer Chrome gives and
//! the one the quality gate counts -- and the slot is also the BRAND, so a member reached on
//! `Object.create(MediaSource.prototype)` is Chrome's `Illegal invocation`.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::core::media::mse;
use crate::binding::protos::{build_fn_data, make_dom_exc, shell_proto, str_val, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;

pub(crate) const MSRC: &str = "MediaSource";
pub(crate) const SBL: &str = "SourceBufferList";
const HANDLE: &str = "MediaSourceHandle";

// `WINDOW_MEMBERS` WAS HERE: the window capture's twelve prototype members and two
// statics, handed to `build_newable` as the window selector. See the `worker` word on `handle`.

// ---- minting -------------------------------------------------------------------------------------

/// One fresh object on `iface`'s prototype with no own property -- the shape `Object.create(P)` had in
/// the JS this replaces.
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    Some(o)
}

/// An empty `SourceBufferList`. A `MediaSource` mints two (`sourceBuffers` and
/// `activeSourceBuffers`), and each is STABLE for the life of the source -- reading twice gives the
/// same object, as it does in Chrome.
fn mint_buffer_list<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, SBL)?;
    let s = st::attach(scope, o, SBL);
    let empty = st::array(scope, &[]);
    st::set(scope, s, "buffers", empty.into());
    Some(o)
}

// ---- SourceBufferList ----------------------------------------------------------------------------

fn cb_sbl_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), SBL) else { return };
    let n = st::get_array(scope, s, "buffers").map(|a| a.length()).unwrap_or(0);
    rv.set(v8::Integer::new_from_unsigned(scope, n).into());
}

pub fn install_source_buffer_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => {
            let g = crate::binding::protos::build_fn(scope, cb_sbl_length, "get length", 0);
            define_accessor(scope, proto, member, g, None);
        }
        other => debug_assert!(false, "install_source_buffer_list: unexpected member {other}"),
    }
}

// ---- MediaSource ---------------------------------------------------------------------------------

fn throw_dom(scope: &mut v8::PinScope, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        scope.throw_exception(e);
    }
}

/// The source a receiver's state names (`mse.rs`).
fn source_id(scope: &mut v8::PinScope, s: v8::Local<v8::Object>) -> u32 {
    st::get_num(scope, s, "id") as u32
}

/// The spec's first step for `duration`, `endOfStream`, `setLiveSeekableRange` and
/// `clearLiveSeekableRange`: a source that is not `open` refuses. `prefix` is Blink's own wording for
/// the member (an attribute says "set the 'x' property on", an operation "execute 'x' on"). Returns
/// `true` when it refused.
fn refuse_unless_open(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, prefix: &str) -> bool {
    let id = source_id(scope, s);
    if mse::source_state(scope, id) == mse::ReadyState::Open {
        return false;
    }
    throw_dom(
        scope,
        &format!("Failed to {prefix} 'MediaSource': The MediaSource's readyState is not 'open'."),
        "InvalidStateError",
    );
    true
}

pub(crate) fn cb_media_source_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("MediaSource");
        throw_type_error(scope, &msg);
        return;
    }
    let Some(buffers) = mint_buffer_list(scope) else { return };
    let Some(active) = mint_buffer_list(scope) else { return };
    let id = mse::new_source(scope, args.this(), buffers, active);
    let s = st::attach(scope, args.this(), MSRC);
    st::set_num(scope, s, "id", id as f64);
    st::set(scope, s, "sourceBuffers", buffers.into());
    st::set(scope, s, "activeSourceBuffers", active.into());
    st::set_null(scope, s, "liveSeekableRange");
    st::set_null(scope, s, "handle");
}

/// `sourceBuffers` and `activeSourceBuffers`: the stored list, by name; `readyState`: the source's.
fn cb_ms_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if name == "readyState" {
        let id = source_id(scope, s);
        let state = mse::source_state(scope, id).name();
        let v = str_val(scope, state);
        rv.set(v);
        return;
    }
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// `handle` (WORKER ONLY -- the window capture has no such member, and the window install's selector
/// leaves the row out). The handle is created on first read and kept, so `o.handle === o.handle`, and
/// it is a bare object on `MediaSourceHandle.prototype`, which is every observable thing the recorded
/// one is: that interface has no prototype members at all.
fn cb_ms_handle(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    let existing = st::get(scope, s, "handle");
    if existing.is_object() {
        rv.set(existing);
        return;
    }
    let Some(h) = mint(scope, HANDLE) else { return };
    st::set(scope, s, "handle", h.into());
    rv.set(h.into());
}

/// `duration`: `NaN` for a source never attached -- Chrome's answer, which serialises to the `null` the bundle's
/// oracle recorded -- else the source's.
fn cb_ms_duration_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    let id = source_id(scope, s);
    let d = mse::source_duration(scope, id);
    rv.set(v8::Number::new(scope, d).into());
}

/// The algorithm's ORDER matters and is the reason both throws are recorded: a negative or non-numeric
/// duration is a `TypeError` BEFORE the readyState check, and a legal number on a closed source is the
/// `InvalidStateError`. `o.duration = "1"` therefore reaches the second throw, not the first.
fn cb_ms_duration_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    let raw = args.get(0);
    let d = raw.number_value(scope).unwrap_or(f64::NAN);
    if d.is_nan() || d < 0.0 {
        let Some(text) = st::to_str(scope, raw) else { return };
        throw_type_error(scope, &format!("Failed to set the 'duration' property on 'MediaSource': The value provided ('{text}') is negative or NaN."));
        return;
    }
    if refuse_unless_open(scope, s, "set the 'duration' property on") {
        return;
    }
    let id = source_id(scope, s);
    mse::set_duration(scope, id, d);
}

fn cb_ms_add_source_buffer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    let Some(t) = st::to_str(scope, args.get(0)) else { return };
    // Blink's `MediaSource::addSourceBuffer`, in its order (Chrome 150 measured):
    // an empty type is a TypeError, an unsupported one NotSupportedError -- the same answer
    // `isTypeSupported` gives -- and a supported type on a source that is not `open` InvalidStateError.
    if t.is_empty() {
        throw_type_error(scope, "Failed to execute 'addSourceBuffer' on 'MediaSource': The type provided is empty");
        return;
    }
    if !crate::core::dom::media_codecs::media_source_supported(scope, &t) {
        throw_dom(
            scope,
            &format!("Failed to execute 'addSourceBuffer' on 'MediaSource': The type provided ('{t}') is unsupported."),
            "NotSupportedError",
        );
        return;
    }
    if refuse_unless_open(scope, s, "execute 'addSourceBuffer' on") {
        return;
    }
    let id = source_id(scope, s);
    if let Some(b) = mse::add_source_buffer(scope, id, &t) {
        rv.set(b.into());
    }
}

fn cb_ms_remove_source_buffer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    // `removeSourceBuffer(SourceBuffer buffer)`: anything else is the binding's TypeError first.
    if !crate::binding::protos::implements(scope, args.get(0), "SourceBuffer") {
        throw_type_error(scope, "Failed to execute 'removeSourceBuffer' on 'MediaSource': parameter 1 is not of type 'SourceBuffer'.");
        return;
    }
    let id = source_id(scope, s);
    if mse::remove_source_buffer(scope, id, args.get(0)) {
        return;
    }
    throw_dom(
        scope,
        "Failed to execute 'removeSourceBuffer' on 'MediaSource': The SourceBuffer provided is not contained in this MediaSource.",
        "NotFoundError",
    );
}

fn cb_ms_end_of_stream(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    // `optional EndOfStreamError error`: the enumeration is converted first
    let error = if args.get(0).is_undefined() {
        None
    } else {
        let Some(e) = st::to_str(scope, args.get(0)) else { return };
        if e != "network" && e != "decode" {
            throw_type_error(scope, &format!("Failed to execute 'endOfStream' on 'MediaSource': The provided value '{e}' is not a valid enum value of type EndOfStreamError."));
            return;
        }
        Some(e)
    };
    if refuse_unless_open(scope, s, "execute 'endOfStream' on") {
        return;
    }
    let id = source_id(scope, s);
    mse::end_of_stream(scope, id, error.as_deref());
}

fn cb_ms_set_live_seekable_range(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    if refuse_unless_open(scope, s, "execute 'setLiveSeekableRange' on") {
        return;
    }
    let a = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    let b = args.get(1).number_value(scope).unwrap_or(f64::NAN);
    if !(a >= 0.0) || !(a <= b) {
        throw_type_error(scope, "Failed to execute 'setLiveSeekableRange' on 'MediaSource': The start value provided is invalid.");
        return;
    }
    let range = st::array(scope, &[v8::Number::new(scope, a).into(), v8::Number::new(scope, b).into()]);
    st::set(scope, s, "liveSeekableRange", range.into());
}

fn cb_ms_clear_live_seekable_range(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), MSRC) else { return };
    if refuse_unless_open(scope, s, "execute 'clearLiveSeekableRange' on") {
        return;
    }
    st::set_null(scope, s, "liveSeekableRange");
}

/// The static. It answers from the recorded byte-stream table (`core/dom/media_codecs.rs`), which is
/// also what `MediaRecorder.isTypeSupported` and `HTMLMediaElement.canPlayType` read, so the three
/// cannot disagree. A constant `false` passed the corpus's one case while being wrong for every type
/// Brave 151 actually supports; the bundle's oracle records 27 of them.
fn cb_ms_is_type_supported(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = st::to_str(scope, args.get(0)) else { return };
    rv.set_bool(crate::core::dom::media_codecs::media_source_supported(scope, &t));
}

/// The other static: a BUILD-TIME capability, and the capture records it true.
fn cb_ms_can_construct_in_worker(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = scope;
    rv.set_bool(true);
}

pub fn install_media_source(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    let field = |scope: &mut v8::PinScope| {
        let d = str_val(scope, member.name);
        let g = build_fn_data(scope, cb_ms_field, &format!("get {}", member.name), 0, d);
        define_accessor(scope, target, member, g, None);
    };
    match member.name {
        "sourceBuffers" | "activeSourceBuffers" | "readyState" => field(scope),
        "handle" => {
            let g = crate::binding::protos::build_fn(scope, cb_ms_handle, "get handle", 0);
            define_accessor(scope, target, member, g, None);
        }
        "duration" => {
            let g = crate::binding::protos::build_fn(scope, cb_ms_duration_get, "get duration", 0);
            let s = crate::binding::protos::build_fn(scope, cb_ms_duration_set, "set duration", 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        "addSourceBuffer" => method_row(scope, target, member, cb_ms_add_source_buffer),
        "removeSourceBuffer" => method_row(scope, target, member, cb_ms_remove_source_buffer),
        "endOfStream" => method_row(scope, target, member, cb_ms_end_of_stream),
        "setLiveSeekableRange" => method_row(scope, target, member, cb_ms_set_live_seekable_range),
        "clearLiveSeekableRange" => method_row(scope, target, member, cb_ms_clear_live_seekable_range),
        "isTypeSupported" => method_row(scope, target, member, cb_ms_is_type_supported),
        "canConstructInDedicatedWorker" => {
            let g = crate::binding::protos::build_fn(scope, cb_ms_can_construct_in_worker, "get canConstructInDedicatedWorker", 0);
            define_accessor(scope, target, member, g, None);
        }
        other => debug_assert!(false, "install_media_source: unexpected member {other}"),
    }
}

// ---- the tables ----------------------------------------------------------------------------------

webapi::interface! {
    "SourceBufferList" : "EventTarget" [interface, both] {
    ctor    "SourceBufferList" len 0                     = stub,
    attr    "length" : Object readonly                   = install_source_buffer_list,
    handler "onaddsourcebuffer" nullable                 = event_handler,
    handler "onremovesourcebuffer" nullable              = event_handler,
    method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

// THE UNION TABLE: the WORKER's thirteen prototype members, in the worker capture's order. The window
// the window install leaves `handle` off (the `worker` word on its row, and the window capture's own
// member list); the worker install takes them all.
webapi::interface! {
    "MediaSource" : "EventTarget" [value, both] {
    ctor    "MediaSource" len 0                          = stub,
    attr    "sourceBuffers" : Object readonly            = install_media_source,
    attr    "activeSourceBuffers" : Object readonly      = install_media_source,
    attr    "duration" : Object                          = install_media_source,
    handler "onsourceopen" nullable                      = event_handler,
    handler "onsourceended" nullable                     = event_handler,
    handler "onsourceclose" nullable                     = event_handler,
    attr    "readyState" : Object readonly               = install_media_source,
    method  "addSourceBuffer" : Object len 1 required 1  = install_media_source,
    method  "clearLiveSeekableRange" : Object            = install_media_source,
    method  "endOfStream" : Object                       = install_media_source,
    method  "removeSourceBuffer" : Object len 1 required 1 = install_media_source,
    method  "setLiveSeekableRange" : Object len 2 required 2 = install_media_source,
    // THE ROW SAYS WHICH REALM: `handle` is the entire difference between this
    // interface's two captures, in both bundles -- `worker.surface.json` records a thirteenth
    // prototype member the window's does not. It was `WINDOW_MEMBERS`, a hand-written copy of the
    // window's twelve handed to `build_newable`; it is one word on the row that differs.
    attr    "handle" : Object worker readonly            = install_media_source,
    static_attr   "canConstructInDedicatedWorker" : Object readonly = install_media_source,
    static_method "isTypeSupported" : Object len 1 required 1       = install_media_source
    }
}
