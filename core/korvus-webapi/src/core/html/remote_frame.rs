//! `core/html/remote_frame.rs` -- a cross-site `<iframe>` in its own engine on its own thread.
//! Opt-in: `KORVUS_OOPIF=1`.
//!
//! WHY. Chrome runs a cross-site frame in its own renderer process (site isolation); korvus ran every frame
//! as one more V8 context on the page's thread, so a frame's computation stalled the page. Measured
//! 2026-10-04: during a 1.5 s busy loop in a cross-site frame the top page draws 90 frames in stock Chrome
//! and 0 in korvus -- and stock Chrome with site isolation switched OFF (2 frames) failed a challenge provider's
//! managed challenge on 5 of 5 sites it otherwise passes, exactly as korvus fails it.
//!
//! TWO HALVES, one file:
//! - the PARENT keeps the frame's browsing context, its `<iframe>` element and its cross-origin
//!   WindowProxy (`core/html/browsing_context.rs`'s `xo_*`, the only surface a page has onto a
//!   cross-origin frame), and hands the committed response to a child engine instead of loading it;
//!   `postMessage` crosses as V8 structured-clone bytes, the child's messages and its `load` come back
//!   as engine tasks;
//! - the CHILD is a whole window engine whose state carries a [`FrameEmbedding`]: its `parent`/`top`
//!   are a remote proxy, its viewport is the `<iframe>`'s content box.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::{op2, v8, OpState};

use crate::binding::protos::{build_fn, build_fn_data, get_prop, set_prop, str_val};

// ── the child side ──────────────────────────────────────────────────────────────────────────────────

/// Present in the state of a child engine that runs a remote frame's document.
#[derive(Clone, Default, Debug)]
pub struct FrameEmbedding {
    /// The origin of the document holding the `<iframe>`.
    pub parent_origin: String,
    /// The top-level document's URL.
    pub top_url: String,
    /// The `<iframe>`'s content box, in CSS px -- this document's viewport.
    pub viewport: (f64, f64),
    /// The `<iframe>`'s `scrolling` turns this viewport's scrollbars off.
    pub scrollbars_off: bool,
    /// `location.ancestorOrigins`, parent first.
    pub ancestor_origins: Vec<String>,
    /// The policy-controlled features this frame inherits disabled from its container.
    pub disabled_features: Vec<String>,
    /// Whether its agent cluster -- the top document's -- is cross-origin isolated.
    pub cluster_isolated: bool,
}

/// Is `feature` disabled for this remote frame by what it inherited from its container?
pub(crate) fn inherited_disabled(state: &OpState, feature: &str) -> bool {
    state.try_borrow::<FrameEmbedding>().is_some_and(|e| e.disabled_features.iter().any(|f| f == feature))
}

/// `location.ancestorOrigins` of a remote frame's document, as a `DOMStringList`.
pub fn ancestor_origins_list<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let list = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<FrameEmbedding>().map(|e| e.ancestor_origins.clone())
    }?;
    let items: Vec<v8::Local<v8::Value>> = list.iter().map(|o| str_val(scope, o)).collect();
    let arr = v8::Array::new_with_elements(scope, &items);
    Some(crate::core::dom::collections::make_static(scope, "DOMStringList", arr))
}

/// The frame navigation's timing, handed over with the response, for the child's document install.
#[derive(Default)]
pub(crate) struct PendingNavTiming(pub Option<spec::FetchTiming>);

/// Hand the child's first document its navigation timing (the parent fetched it).
pub fn set_pending_navigation_timing(state: &mut OpState, t: Option<spec::FetchTiming>) {
    state.put(PendingNavTiming(t));
}

/// The embedding of this engine's document, when it is a remote frame.
pub fn embedding(state: &OpState) -> Option<FrameEmbedding> {
    state.try_borrow::<FrameEmbedding>().cloned()
}

/// The viewport a remote frame's document lays out in.
/// Whether this remote frame's `<iframe>` turned its viewport's scrollbars off (`scrolling=no`).
pub(crate) fn embedded_scrollbars_off(state: &OpState) -> bool {
    state.try_borrow::<FrameEmbedding>().is_some_and(|e| e.scrollbars_off)
}

/// Does an `<iframe>`'s (or `<frame>`'s) `scrolling` attribute turn its viewport's scrollbars off? Blink's
/// `HTMLFrameElementBase`: `no`, `off` and `noscroll`, in any case; anything else is `auto`. reCAPTCHA's frames are
/// `scrolling=no`: Chrome draws its 400x580 challenge, two pixels taller than its frame, with no scrollbar, where
/// korvus drew both and cut the tile grid's last column.
pub(crate) fn scrolling_off(dom: &korvus_dom::Dom, el: u32) -> bool {
    dom.get_attribute(el, "scrolling").is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "no" | "off" | "noscroll"))
}

pub(crate) fn embedded_viewport(state: &OpState) -> Option<(f64, f64)> {
    state.try_borrow::<FrameEmbedding>().map(|e| e.viewport).filter(|v| v.0 > 0.0 && v.1 > 0.0)
}

const PARENT_SLOT: &str = "korvus_remote_parent";

/// The remote frame's `parent` and `top`: the embedder's window as a CROSS-ORIGIN WindowProxy -- the same
/// surface `browsing_context.rs`'s `xo_*` gives a page onto a cross-origin frame (HTML 7.2.3): the allowed
/// members read, `then` and three symbols read `undefined`, everything else throws `SecurityError` with
/// Chrome's sentences. `None` in an engine that is not a remote frame.
pub(crate) fn parent_proxy<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let has = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.has::<FrameEmbedding>()
    };
    if !has {
        return None;
    }
    remote_proxy(scope, PARENT)
}

/// A popup's `opener` when the opener runs in another process (`core/html/popup.rs`, D74 step 3): the opener's window
/// as a CROSS-ORIGIN WindowProxy, the surface [`parent_proxy`] gives a remote frame onto its parent, its
/// `postMessage` reaching the opener over the browser endpoint. `None` in a window no script opened that way.
pub(crate) fn opener_proxy<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    crate::core::html::popup::opener_origin()?;
    // a remote frame's own `opener` is its own business: only the top-level document's window has this one
    let framed = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.has::<FrameEmbedding>()
    };
    if framed {
        return None;
    }
    remote_proxy(scope, OPENER)
}

/// Which window a cross-origin proxy of this file stands for, bound as its traps' data.
const PARENT: i32 = 0;
const OPENER: i32 = 1;
const OPENER_SLOT: &str = "korvus_remote_opener";

fn proxy_kind(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> i32 {
    crate::binding::protos::body_data(scope, args).int32_value(scope).unwrap_or(PARENT)
}

fn remote_proxy<'s>(scope: &mut v8::PinScope<'s, '_>, kind: i32) -> Option<v8::Local<'s, v8::Object>> {
    let slot = if kind == OPENER { OPENER_SLOT } else { PARENT_SLOT };
    if let Some(o) = crate::binding::protos::realm_slot(scope, slot).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(o);
    }
    let target = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    target.set_prototype(scope, null);
    let handler = v8::Object::new(scope);
    macro_rules! trap {
        ($name:literal, $cb:expr) => {{
            let d: v8::Local<v8::Value> = v8::Integer::new(scope, kind).into();
            let f = build_fn_data(scope, $cb, $name, 0, d);
            set_prop(scope, handler, $name, f.into());
        }};
    }
    trap!("get", cb_xp_get);
    trap!("set", cb_xp_set);
    trap!("has", cb_xp_has);
    trap!("ownKeys", cb_xp_own_keys);
    trap!("getOwnPropertyDescriptor", cb_xp_gopd);
    trap!("defineProperty", cb_xp_blocked);
    trap!("deleteProperty", cb_xp_blocked);
    trap!("setPrototypeOf", cb_xp_blocked);
    trap!("preventExtensions", cb_xp_blocked);
    trap!("getPrototypeOf", cb_xp_null);
    trap!("isExtensible", cb_xp_true);
    let proxy = v8::Proxy::new(scope, target, handler)?;
    let o: v8::Local<v8::Object> = proxy.into();
    crate::binding::protos::set_realm_slot(scope, slot, o.into());
    Some(o)
}

/// The members a cross-origin WindowProxy exposes (HTML CrossOriginProperties), in Chrome's `ownKeys` order.
const XP_KEYS: [&str; 13] =
    ["window", "self", "location", "closed", "frames", "length", "top", "opener", "parent", "blur", "close", "focus", "postMessage"];

fn xp_blocked_msg(scope: &mut v8::PinScope) -> String {
    let own = crate::core::feature_policy::document_origin(scope);
    format!("Blocked a frame with origin \"{own}\" from accessing a cross-origin frame.")
}

fn xp_throw(scope: &mut v8::PinScope, msg: String) {
    crate::binding::protos::throw_dom(scope, &msg, "SecurityError");
}

fn xp_fallback(scope: &mut v8::PinScope, key: v8::Local<v8::Value>) -> bool {
    if key.is_string() {
        return key.to_rust_string_lossy(scope) == "then";
    }
    let Ok(sym) = v8::Local::<v8::Symbol>::try_from(key) else { return false };
    let s: v8::Local<v8::Value> = sym.into();
    let tag: v8::Local<v8::Value> = v8::Symbol::get_to_string_tag(scope).into();
    let inst: v8::Local<v8::Value> = v8::Symbol::get_has_instance(scope).into();
    let spread: v8::Local<v8::Value> = v8::Symbol::get_is_concat_spreadable(scope).into();
    s == tag || s == inst || s == spread
}

fn xp_label(scope: &mut v8::PinScope, key: v8::Local<v8::Value>) -> String {
    if let Ok(sym) = v8::Local::<v8::Symbol>::try_from(key) {
        return format!("[{}]", sym.description(scope).to_rust_string_lossy(scope));
    }
    key.to_rust_string_lossy(scope)
}

/// The value of an allowed member of the parent's (or the opener's) cross-origin view.
fn xp_member<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, kind: i32) -> v8::Local<'s, v8::Value> {
    let me = if kind == OPENER { opener_proxy(scope) } else { parent_proxy(scope) };
    let me = me.map(Into::into).unwrap_or_else(|| v8::undefined(scope).into());
    match name {
        // P1: the embedder is one level up and is the top (a remote frame nested deeper reports its
        // parent for both until P2 mirrors the chain). An opener in another process is a top-level window: its own
        // `top` and `parent`.
        "window" | "self" | "frames" | "top" | "parent" => me,
        "closed" => v8::Boolean::new(scope, kind == OPENER && crate::core::html::popup::opener_closed()).into(),
        "length" => v8::Integer::new(scope, if kind == OPENER { 0 } else { 1 }).into(),
        "opener" => v8::null(scope).into(),
        "location" => xp_location(scope),
        "postMessage" if kind == OPENER => xp_fn(scope, "opener_postMessage", "postMessage", cb_opener_post_message, 1),
        "postMessage" => xp_fn(scope, "postMessage", "postMessage", cb_parent_post_message, 1),
        "blur" | "close" | "focus" => xp_fn(scope, name, name, cb_xp_noop, 0),
        _ => v8::undefined(scope).into(),
    }
}

fn xp_fn<'s>(scope: &mut v8::PinScope<'s, '_>, slot: &str, name: &str, cb: impl v8::MapFnTo<v8::FunctionCallback>, len: i32) -> v8::Local<'s, v8::Value> {
    let key = format!("korvus_remote_parent_fn_{slot}");
    if let Some(f) = crate::binding::protos::realm_slot(scope, &key) {
        return f;
    }
    let f = build_fn(scope, cb, name, len);
    crate::binding::protos::set_realm_slot(scope, &key, f.into());
    f.into()
}

fn cb_xp_noop(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// The parent's cross-origin `Location`: every read throws, `href`'s setter and `replace` would navigate the
/// parent (not crossed in P1 -- they throw nothing and do nothing).
fn xp_location<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    if let Some(l) = crate::binding::protos::realm_slot(scope, "korvus_remote_parent_location") {
        return l;
    }
    let target = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    target.set_prototype(scope, null);
    let handler = v8::Object::new(scope);
    let g = build_fn(scope, cb_xp_loc_get, "get", 0);
    set_prop(scope, handler, "get", g.into());
    let p = v8::Proxy::new(scope, target, handler).map(Into::into).unwrap_or_else(|| v8::undefined(scope).into());
    crate::binding::protos::set_realm_slot(scope, "korvus_remote_parent_location", p);
    p
}

fn cb_xp_loc_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = args.get(1);
    if key.is_string() {
        let k = key.to_rust_string_lossy(scope);
        if k == "replace" {
            let f = xp_fn(scope, "replace", "replace", cb_xp_noop, 1);
            rv.set(f);
            return;
        }
    }
    if xp_fallback(scope, key) {
        rv.set_undefined();
        return;
    }
    let label = xp_label(scope, key);
    let msg = xp_blocked_msg(scope);
    xp_throw(scope, format!("Failed to read a named property '{label}' from 'Location': {msg}"));
}

fn cb_xp_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = args.get(1);
    if key.is_string() {
        let name = key.to_rust_string_lossy(scope);
        if XP_KEYS.contains(&name.as_str()) {
            let kind = proxy_kind(scope, &args);
            let v = xp_member(scope, &name, kind);
            rv.set(v);
            return;
        }
        if let Ok(n) = name.parse::<u32>() {
            if n.to_string() == name {
                let msg = xp_blocked_msg(scope);
                xp_throw(scope, format!("Failed to read an indexed property [{n}] from 'Window': {msg}"));
                return;
            }
        }
    }
    if xp_fallback(scope, key) {
        rv.set_undefined();
        return;
    }
    let label = xp_label(scope, key);
    let msg = xp_blocked_msg(scope);
    xp_throw(scope, format!("Failed to read a named property '{label}' from 'Window': {msg}"));
}

fn cb_xp_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = args.get(1);
    if key.is_string() && key.to_rust_string_lossy(scope) == "location" {
        rv.set_bool(true);
        return;
    }
    let label = xp_label(scope, key);
    let msg = xp_blocked_msg(scope);
    xp_throw(scope, format!("Failed to set a named property '{label}' on 'Window': {msg}"));
}

fn cb_xp_has(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = args.get(1);
    let ok = key.is_string() && XP_KEYS.contains(&key.to_rust_string_lossy(scope).as_str());
    if ok || xp_fallback(scope, key) {
        rv.set_bool(ok);
        return;
    }
    let label = xp_label(scope, key);
    let msg = xp_blocked_msg(scope);
    xp_throw(scope, format!("Failed to read a named property '{label}' from 'Window': {msg}"));
}

fn cb_xp_own_keys(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut items: Vec<v8::Local<v8::Value>> = vec![str_val(scope, "0")];
    for k in XP_KEYS {
        items.push(str_val(scope, k));
    }
    for s in [v8::Symbol::get_to_string_tag(scope), v8::Symbol::get_has_instance(scope), v8::Symbol::get_is_concat_spreadable(scope)] {
        items.push(s.into());
    }
    let arr = v8::Array::new_with_elements(scope, &items);
    rv.set(arr.into());
}

fn cb_xp_gopd(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = args.get(1);
    if key.is_string() {
        let name = key.to_rust_string_lossy(scope);
        if XP_KEYS.contains(&name.as_str()) {
            let d = v8::Object::new(scope);
            let kind = proxy_kind(scope, &args);
            let v = xp_member(scope, &name, kind);
            set_prop(scope, d, "value", v);
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
            set_prop(scope, d, "enumerable", f);
            set_prop(scope, d, "configurable", t);
            set_prop(scope, d, "writable", f);
            rv.set(d.into());
            return;
        }
    }
    if xp_fallback(scope, key) {
        let d = v8::Object::new(scope);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        set_prop(scope, d, "value", u);
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
        set_prop(scope, d, "enumerable", f);
        set_prop(scope, d, "configurable", t);
        set_prop(scope, d, "writable", f);
        rv.set(d.into());
        return;
    }
    let label = xp_label(scope, key);
    let msg = xp_blocked_msg(scope);
    xp_throw(scope, format!("Failed to read a named property '{label}' from 'Window': {msg}"));
}

fn cb_xp_blocked(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let label = xp_label(scope, args.get(1));
    let msg = xp_blocked_msg(scope);
    xp_throw(scope, format!("Failed to define a named property '{label}' on 'Window': {msg}"));
}

fn cb_xp_null(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_null();
}

fn cb_xp_true(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(true);
}

/// `parent.postMessage(message, targetOrigin | options)` from the remote frame.
fn cb_parent_post_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(emb) = ({
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        embedding(&st)
    }) else {
        return;
    };
    let mut target = "/".to_string();
    let a = args.get(1);
    // the `WindowPostMessageOptions` overload, converted, as the page's `Window` body does
    if args.length() <= 2 && (a.is_null_or_undefined() || a.is_object()) {
        let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, a, "WindowPostMessageOptions", "Failed to execute 'postMessage' on 'Window': ") else { return };
        if let Some(t) = get_prop(scope, o, "targetOrigin").filter(|t| t.is_string()) {
            target = t.to_rust_string_lossy(scope);
        }
    } else {
        target = a.to_rust_string_lossy(scope);
    }
    // HTML: a message whose targetOrigin does not match the receiver's origin is dropped silently; "/"
    // means the sender's own origin, which a cross-site parent never is.
    let own = crate::core::feature_policy::document_origin(scope);
    let receiver = emb.parent_origin.clone();
    let ok = match target.as_str() {
        "*" => true,
        "/" => own == receiver,
        t => url::Url::parse(t).ok().map(|u| u.origin().ascii_serialization()).as_deref() == Some(receiver.as_str()),
    };
    if !ok {
        return;
    }
    let bytes = crate::modules::clone_state::wire_serialize(scope, args.get(0), &[]);
    let Some(op) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_oopif_post_parent") else { return };
    let len = bytes.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    let Some(arr) = v8::Uint8Array::new(scope, ab, 0, len) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    op.call(scope, undef, &[arr.into()]);
}

/// `opener.postMessage(message, targetOrigin | options)` from a popup whose opener runs in another process.
fn cb_opener_post_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(receiver) = crate::core::html::popup::opener_origin() else { return };
    let mut target = "/".to_string();
    let a = args.get(1);
    // the `WindowPostMessageOptions` overload, converted, as the page's `Window` body does
    if args.length() <= 2 && (a.is_null_or_undefined() || a.is_object()) {
        let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, a, "WindowPostMessageOptions", "Failed to execute 'postMessage' on 'Window': ") else { return };
        if let Some(t) = get_prop(scope, o, "targetOrigin").filter(|t| t.is_string()) {
            target = t.to_rust_string_lossy(scope);
        }
    } else {
        target = a.to_rust_string_lossy(scope);
    }
    // HTML: a message whose targetOrigin does not match the receiver's origin is dropped silently.
    let own = crate::core::feature_policy::document_origin(scope);
    let ok = match target.as_str() {
        "*" => true,
        "/" => own == receiver,
        t => url::Url::parse(t).ok().map(|u| u.origin().ascii_serialization()).as_deref() == Some(receiver.as_str()),
    };
    if !ok {
        return;
    }
    let bytes = crate::modules::clone_state::wire_serialize(scope, args.get(0), &[]);
    crate::core::html::popup::post_to_opener(&bytes, &own);
}

/// At the remote frame's document install, before its scripts: start receiving the parent's messages. A popup whose
/// opener runs in another process starts receiving the opener's.
pub fn install_frame_embedding(scope: &mut v8::PinScope) {
    if parent_proxy(scope).is_some() {
        arm_parent_recv(scope);
        return;
    }
    if opener_proxy(scope).is_some() {
        arm_opener_recv(scope);
    }
}

fn arm_opener_recv(scope: &mut v8::PinScope) {
    let Some(op) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_popup_opener_recv") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = op.call(scope, undef, &[]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    let step = build_fn(scope, cb_opener_message, "", 1);
    // each message a task, not a promise job (`then_task`)
    crate::then_task(scope, p, step, None);
}

/// One message from the opener: `{data, origin}`, or `null` once it is gone.
fn cb_opener_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.get(0).is_null() {
        return;
    }
    let Ok(rec) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let origin = get_prop(scope, rec, "origin").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let bytes = match get_prop(scope, rec, "data") { Some(v) => bytes_of_in(scope, v), None => Vec::new() };
    let global = scope.get_current_context().global(scope);
    let data = crate::modules::clone_state::wire_deserialize(scope, &bytes, &[]);
    let source = opener_proxy(scope);
    dispatch_message(scope, global, data, &origin, source.map(Into::into));
    arm_opener_recv(scope);
}

fn arm_parent_recv(scope: &mut v8::PinScope) {
    let Some(op) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_oopif_parent_recv") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = op.call(scope, undef, &[]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    let step = build_fn(scope, cb_parent_message, "", 1);
    // each message a task, not a promise job (`then_task`)
    crate::then_task(scope, p, step, None);
}

/// One message from the parent: `{data: Uint8Array, origin}`, or `null` when the parent is gone.
fn cb_parent_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(rec) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    if args.get(0).is_null() {
        return;
    }
    let origin = get_prop(scope, rec, "origin").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let bytes = match get_prop(scope, rec, "data") { Some(v) => bytes_of_in(scope, v), None => Vec::new() };
    let global = scope.get_current_context().global(scope);
    let data = crate::modules::clone_state::wire_deserialize(scope, &bytes, &[]);
    let source = parent_proxy(scope);
    dispatch_message(scope, global, data, &origin, source.map(Into::into));
    arm_parent_recv(scope);
}

// ── shared ──────────────────────────────────────────────────────────────────────────────────────────

/// The bytes of a `Uint8Array`, or of the plain number array a serde op answers for a `Vec<u8>`.
fn bytes_of(v: v8::Local<v8::Value>) -> Vec<u8> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut buf = vec![0u8; view.byte_length()];
        view.copy_contents(&mut buf);
        return buf;
    }
    Vec::new()
}

/// [`bytes_of`] for a plain array, which needs a scope to read its elements.
fn bytes_of_in(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<u8> {
    let direct = bytes_of(v);
    if !direct.is_empty() {
        return direct;
    }
    let Ok(arr) = v8::Local::<v8::Array>::try_from(v) else { return Vec::new() };
    (0..arr.length()).filter_map(|i| arr.get_index(scope, i)).map(|x| x.uint32_value(scope).unwrap_or(0) as u8).collect()
}

/// A trusted `message` event at `target`, with `source` written straight into its slot (the page-facing
/// constructor refuses a cross-origin view).
pub(crate) fn dispatch_message<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Object>, data: v8::Local<'s, v8::Value>, origin: &str, source: Option<v8::Local<'s, v8::Value>>) {
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "data", data);
    let o = str_val(scope, origin);
    set_prop(scope, init, "origin", o);
    let ty = str_val(scope, "message");
    let Some(ev) = crate::binding::protos::new_global(scope, "MessageEvent", &[ty, init.into()]) else { return };
    if let Some(s) = source {
        crate::core::events::event::evt_slot_write(scope, ev, "source", s);
    }
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    crate::core::events::event_target::et_dispatch(scope, target.into(), ev);
}

// ── the parent side ─────────────────────────────────────────────────────────────────────────────────

/// `KORVUS_OOPIF=1` turns remote frames on (P0: opt-in).
pub(crate) fn enabled() -> bool {
    std::env::var("KORVUS_OOPIF").as_deref() == Ok("1")
}

/// The kernel's remote-frame host, when the runtime provides one.
pub(crate) fn host(scope: &mut v8::PinScope) -> Option<std::sync::Arc<dyn spec::RemoteFrameHost>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RemoteFrames>()).map(|h| h.0.clone())
}

/// Should a frame whose document commits at `url`, under a top-level document at `top`, run remote?
/// Chrome's site-per-process rule: a different SITE (scheme + registrable domain).
pub(crate) fn wants_remote(url: &str, top: &str) -> bool {
    let (Ok(u), Ok(t)) = (url::Url::parse(url), url::Url::parse(top)) else { return false };
    matches!(u.scheme(), "http" | "https") && !transport::same_site(&u, &t)
}

/// The parent's `postMessage` into remote frame `rid`.
pub(crate) fn post_to_child(scope: &mut v8::PinScope, rid: u64, message: v8::Local<v8::Value>, sender_origin: &str) {
    let Some(h) = host(scope) else { return };
    let bytes = crate::modules::clone_state::wire_serialize(scope, message, &[]);
    h.post(rid, bytes, sender_origin.to_string());
}

/// Start receiving remote frame `rid`'s events for browsing context `ctx`.
pub(crate) fn arm_child_recv(scope: &mut v8::PinScope, ctx: u32, rid: u64) {
    let Some(op) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_oopif_recv") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, rid as f64).into();
    let Some(p) = op.call(scope, undef, &[idv]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    let d = v8::Array::new(scope, 2);
    let c: v8::Local<v8::Value> = v8::Number::new(scope, ctx as f64).into();
    d.set_index(scope, 0, c);
    d.set_index(scope, 1, idv);
    let step = build_fn_data(scope, cb_child_event, "", 1, d.into());
    // each event a task, not a promise job (`then_task`)
    crate::then_task(scope, p, step, None);
}

/// One event from a remote frame: `{kind: "message" | "loaded", data?, origin?}`, or `null` once it is gone.
fn cb_child_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let ctx = d.get_index(scope, 0).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    let rid = d.get_index(scope, 1).and_then(|v| v.number_value(scope)).unwrap_or(0.0) as u64;
    if args.get(0).is_null() {
        return;
    }
    let Ok(rec) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let kind = get_prop(scope, rec, "kind").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    match kind.as_str() {
        "message" => {
            let origin = get_prop(scope, rec, "origin").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let bytes = match get_prop(scope, rec, "data") { Some(v) => bytes_of_in(scope, v), None => Vec::new() };
            crate::core::html::browsing_context::remote_message_to_parent(scope, ctx, &bytes, &origin);
        }
        "loaded" => crate::core::html::browsing_context::remote_frame_loaded(scope, ctx),
        _ => {}
    }
    arm_child_recv(scope, ctx, rid);
}

/// Start receiving popup `rid`'s events for its browsing context `ctx` (`core/html/popup.rs`).
pub(crate) fn arm_popup_recv(scope: &mut v8::PinScope, ctx: u32, rid: u64) {
    let Some(op) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_popup_recv") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, rid as f64).into();
    let Some(p) = op.call(scope, undef, &[idv]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    let d = v8::Array::new(scope, 2);
    let c: v8::Local<v8::Value> = v8::Number::new(scope, ctx as f64).into();
    d.set_index(scope, 0, c);
    d.set_index(scope, 1, idv);
    let step = build_fn_data(scope, cb_popup_event, "", 1, d.into());
    // each event a task, not a promise job (`then_task`)
    crate::then_task(scope, p, step, None);
}

/// One event from a popup open as a target of its own: `{kind: "message", data, origin}`, `{kind: "closed"}`, or
/// `null` once nothing more can come.
fn cb_popup_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let ctx = d.get_index(scope, 0).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    let rid = d.get_index(scope, 1).and_then(|v| v.number_value(scope)).unwrap_or(0.0) as u64;
    if args.get(0).is_null() {
        return;
    }
    let Ok(rec) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let kind = get_prop(scope, rec, "kind").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    match kind.as_str() {
        "message" => {
            let origin = get_prop(scope, rec, "origin").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let bytes = match get_prop(scope, rec, "data") { Some(v) => bytes_of_in(scope, v), None => Vec::new() };
            crate::core::html::browsing_context::remote_popup_message(scope, ctx, &bytes, &origin);
        }
        "closed" => {
            crate::core::html::browsing_context::remote_popup_closed(scope, ctx);
            return;
        }
        _ => {}
    }
    arm_popup_recv(scope, ctx, rid);
}

#[op2(async(lazy), fast)]
#[serde]
pub(crate) async fn op_oopif_recv(state: Rc<RefCell<OpState>>, id: f64) -> Option<serde_json::Value> {
    let host = state
        .borrow()
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<spec::RemoteFrames>())
        .map(|h| h.0.clone())?;
    let ev = host.recv(id as u64).await?;
    Some(match ev {
        spec::RemoteFrameEvent::Message { data, origin } => serde_json::json!({"kind": "message", "data": data, "origin": origin}),
        spec::RemoteFrameEvent::Loaded => serde_json::json!({"kind": "loaded"}),
    })
}

crate::webapi_ops![op_oopif_recv];
