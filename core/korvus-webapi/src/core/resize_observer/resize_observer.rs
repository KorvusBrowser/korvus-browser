//! `core/resize_observer/resize_observer.rs` — `ResizeObserver` and `ResizeObserverEntry`, one
//! `interface!` table each, and the resize steps of the rendering update.
//!
//! The three members used to reach JS twice: through `#[op2] impl` onto the cppgc class template's
//! own prototype, and again through `dom/08g-observers.js`'s `op_arity_guard` pass, with the
//! interface object replaced by an `op_nativize_ctor` MASK over a JS arrow that checked the callback
//! and forwarded to `new C(...)`. `ResizeObserverEntry` was a registry shell nothing decorated.
//!
//! Now: two tables, mounted by `install::live::install_window_observers`. `cb_ro_ctor` is that check as a
//! real Rust constructor.
//!
//! # The observer observes
//!
//! `observe` was an empty body. It registers the target with the box it asked for, and the frame in
//! `core/intersection_observer/rendering.rs` runs HTML's resize-observer loop: gather the observations
//! whose size changed and whose target is deeper than the last delivered depth, broadcast them (one
//! callback per observer, in the order the observers first observed), repeat, and report
//! "ResizeObserver loop completed with undelivered notifications." when a shallower change was left
//! behind. Measured on Chrome 150 (`tests/fixtures/p12c/chrome-150.ro.txt`, `.ro-loop.txt`): every
//! target gets an initial callback, a zero-sized one included; a move is not a resize; the three size
//! lists are frozen single-entry arrays; the loop error is an `ErrorEvent` with `error` null, the
//! document's URL, line and column 0, delivered after the deeper callbacks of the same frame.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{get_prop, throw_type_error, unwrap_id};
use crate::core::events::event::evt_slot_write;
use crate::core::intersection_observer::rendering::{self, BoxKind, Geometry, RoObservation, RoRegistration, R};
use crate::install::live::{method_row, set_table_proto};
use crate::install::MemberDef;

#[repr(C)]
#[derive(CppgcBase)]
pub struct ResizeObserver {
    /// This observer's key in the realm's registry, `0` until it first observes.
    id: std::cell::Cell<u64>,
}

/// The cppgc class template: the CONSTRUCTOR only. The three members are rows of [`RESIZEOBSERVER`]
/// and are installed by THE installer onto the table's prototype (invariant I1).
#[webapi(base, own)]
impl ResizeObserver {
    #[constructor]
    #[cppgc]
    fn constructor(_scope: &mut v8::PinScope, _cb: v8::Local<v8::Value>) -> Result<ResizeObserver, ::deno_error::JsErrorBox> {
        Ok(ResizeObserver { id: std::cell::Cell::new(0) })
    }
}

/// `new ResizeObserver(callback)` — the `op_nativize_ctor` mask's JS arrow, in Rust.
pub fn cb_ro_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("ResizeObserver"));
        return;
    }
    // WebIDL's argument count before the callback's type (Chrome 150: "1 argument required, but only 0 present.")
    if !crate::install::webidl::require_args(scope, &args, "ResizeObserver", 1) {
        return;
    }
    if !args.get(0).is_function() {
        throw_type_error(scope, "Failed to construct 'ResizeObserver': parameter 1 is not of type 'Function'.");
        return;
    }
    let obj = crate::binding::class::mint(scope, ResizeObserver { id: std::cell::Cell::new(0) });
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => set_table_proto(scope, obj, "ResizeObserver"),
    }
    // On the observer's private slot, so the collector sees the edge (see `cb_io_ctor`).
    let cb = args.get(0);
    evt_slot_write(scope, obj, "observer_callback", cb);
    rv.set(obj.into());
}

/// The receiver's `ResizeObserver`, or `None` after throwing WebIDL's "Illegal invocation".
macro_rules! ro_this {
    ($scope:ident, $args:ident, $member:literal) => {
        match deno_core::cppgc::try_unwrap_cppgc_object::<ResizeObserver>($scope, $args.this().into()) {
            Some(p) => p,
            None => {
                throw_type_error($scope, concat!("Failed to execute '", $member, "' on 'ResizeObserver': Illegal invocation"));
                return;
            }
        }
    };
}

fn element_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, member: &str) -> Option<u32> {
    let id = unwrap_id(scope, v).filter(|id| Geometry::is_element(scope, *id));
    if id.is_none() {
        throw_type_error(scope, &format!("Failed to execute '{member}' on 'ResizeObserver': parameter 1 is not of type 'Element'."));
    }
    id
}

/// `ResizeObserverOptions.box`. `None` after Chrome's enum TypeError.
fn box_option(scope: &mut v8::PinScope, opts: v8::Local<v8::Value>) -> Option<BoxKind> {
    if opts.is_null_or_undefined() {
        return Some(BoxKind::Content);
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(opts) else {
        throw_type_error(scope, "Failed to execute 'observe' on 'ResizeObserver': The provided value is not of type 'ResizeObserverOptions'.");
        return None;
    };
    let v = get_prop(scope, o, "box")?;
    if v.is_undefined() {
        return Some(BoxKind::Content);
    }
    let s = v.to_string(scope)?.to_rust_string_lossy(scope);
    match s.as_str() {
        "content-box" => Some(BoxKind::Content),
        "border-box" => Some(BoxKind::Border),
        "device-pixel-content-box" => Some(BoxKind::DevicePixelContent),
        _ => {
            throw_type_error(scope, &format!("Failed to execute 'observe' on 'ResizeObserver': Failed to read the 'box' property from 'ResizeObserverOptions': The provided value '{s}' is not a valid enum value of type ResizeObserverBoxOptions."));
            None
        }
    }
}

fn cb_ro_observe(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let ro = ro_this!(scope, args, "observe");
    let Some(node) = element_arg(scope, args.get(0), "observe") else { return };
    let Some(box_kind) = box_option(scope, args.get(1)) else { return };
    let ro = unsafe { ro.as_ref() };
    if ro.id.get() == 0 {
        ro.id.set(rendering::next_id(scope));
    }
    let id = ro.id.get();
    rendering::note_document(scope);
    let this = v8::Global::new(scope, args.this());
    rendering::with_state(scope, |s| {
        let reg = match s.ro.iter().position(|r| r.id == id) {
            Some(i) => &mut s.ro[i],
            None => {
                s.ro.push(RoRegistration { id, observer: this, observations: Vec::new() });
                s.ro.last_mut().unwrap()
            }
        };
        // "If target is already in observationTargets, remove it" -- then observe afresh, so a second
        // `observe` with another box starts from no reported size.
        reg.observations.retain(|o| o.node != node);
        reg.observations.push(RoObservation { node, box_kind, last: (-1.0, -1.0) });
    });
    rendering::request_frame(scope);
}

fn cb_ro_unobserve(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let ro = ro_this!(scope, args, "unobserve");
    let Some(node) = element_arg(scope, args.get(0), "unobserve") else { return };
    let id = unsafe { ro.as_ref() }.id.get();
    rendering::with_state(scope, |s| {
        if let Some(i) = s.ro.iter().position(|r| r.id == id) {
            s.ro[i].observations.retain(|o| o.node != node);
            if s.ro[i].observations.is_empty() {
                s.ro.remove(i);
            }
        }
    });
}

fn cb_ro_disconnect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let ro = ro_this!(scope, args, "disconnect");
    let id = unsafe { ro.as_ref() }.id.get();
    rendering::with_state(scope, |s| s.ro.retain(|r| r.id != id));
}

// ── the resize steps ─────────────────────────────────────────────────────────────────────────────

/// One element's boxes, as the resize observer reports them.
#[derive(Clone, Copy, Default)]
struct Sizes {
    content: (f64, f64),
    border: (f64, f64),
    device: (f64, f64),
    /// The content box's offset inside the border box -- `contentRect.x`/`y`.
    padding_origin: (f64, f64),
}

impl Sizes {
    fn of(&self, kind: BoxKind) -> (f64, f64) {
        match kind {
            BoxKind::Content => self.content,
            BoxKind::Border => self.border,
            BoxKind::DevicePixelContent => self.device,
        }
    }
}

/// The boxes of `node`. An element with no box -- `display: none`, detached, or a non-replaced inline,
/// whose size is its line boxes' and not its own -- reports all zeros, as Chrome does.
fn sizes(scope: &mut v8::PinScope, geo: &mut Geometry, node: u32, dpr: f64) -> Sizes {
    let Some(b) = geo.border_box(scope, node) else { return Sizes::default() };
    if geo.is_inline(scope, node) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let replaced = st
            .try_borrow::<crate::binding::host_dom::HostDom>()
            .and_then(|d| d.0.tag_name(node))
            .is_some_and(|t| ["img", "video", "canvas", "iframe", "embed", "object", "svg", "audio"].iter().any(|r| t.eq_ignore_ascii_case(r)));
        if !replaced {
            return Sizes::default();
        }
    }
    // The layout's own content and padding boxes. This used to rebuild them from the
    // resolved paddings and borders, and the scrollbar from the client size's shortfall; the box tree
    // had both all along and now says so. A scrollbar sits between the border and the padding and
    // belongs to neither box, so the padding box already stops short of it.
    let boxes = geo.layout(scope, |out, _| out.content_box(node).zip(out.padding_box(node))).flatten();
    let Some((content_box, padding_box)) = boxes else { return Sizes::default() };
    let content = (content_box.width as f64, content_box.height as f64);
    Sizes {
        content,
        border: (b.w, b.h),
        device: ((content.0 * dpr).round(), (content.1 * dpr).round()),
        padding_origin: ((content_box.x - padding_box.x) as f64, (content_box.y - padding_box.y) as f64),
    }
}

fn depth(scope: &mut v8::PinScope, node: u32) -> u32 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(dom) = st.try_borrow::<crate::binding::host_dom::HostDom>().map(|d| &d.0) else { return 0 };
    let mut d = 0;
    let mut cur = dom.parent(node);
    while let Some(p) = cur {
        d += 1;
        cur = dom.parent(p);
    }
    d
}

fn device_pixel_ratio(scope: &mut v8::PinScope) -> f64 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dpr = crate::binding::protos::persona_handle(&st)
        .map(|p| p.0.screen().device_pixel_ratio)
        .unwrap_or(1.0);
    if dpr > 0.0 { dpr } else { 1.0 }
}

/// A frozen one-entry `ResizeObserverSize` list.
fn size_list<'s>(scope: &mut v8::PinScope<'s, '_>, kit: &mut rendering::Kit<'s>, (inline, block): (f64, f64)) -> v8::Local<'s, v8::Value> {
    let arr = v8::Array::new(scope, 1);
    if let Some(proto) = kit.proto(scope, "ResizeObserverSize") {
        let o = crate::binding::protos::mint_on(scope, proto.into());
        let i: v8::Local<v8::Value> = v8::Number::new(scope, inline).into();
        evt_slot_write(scope, o, "inlineSize", i);
        let b: v8::Local<v8::Value> = v8::Number::new(scope, block).into();
        evt_slot_write(scope, o, "blockSize", b);
        crate::install::webidl::brand(scope, o, "ResizeObserverSize");
        arr.set_index(scope, 0, o.into());
    }
    let _ = arr.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    arr.into()
}

fn mint_entry<'s>(scope: &mut v8::PinScope<'s, '_>, kit: &mut rendering::Kit<'s>, node: u32, s: &Sizes) -> Option<v8::Local<'s, v8::Object>> {
    let proto = kit.proto(scope, "ResizeObserverEntry")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let target: v8::Local<v8::Value> = crate::binding::protos::wrap_cached(scope, &op_state, node).into();
    evt_slot_write(scope, obj, "target", target);
    let cr = kit.rect(scope, R { x: s.padding_origin.0, y: s.padding_origin.1, w: s.content.0, h: s.content.1 });
    evt_slot_write(scope, obj, "contentRect", cr);
    let c = size_list(scope, kit, s.content);
    evt_slot_write(scope, obj, "contentBoxSize", c);
    let b = size_list(scope, kit, s.border);
    evt_slot_write(scope, obj, "borderBoxSize", b);
    let d = size_list(scope, kit, s.device);
    evt_slot_write(scope, obj, "devicePixelContentBoxSize", d);
    crate::install::webidl::brand(scope, obj, "ResizeObserverEntry");
    Some(obj)
}

/// "Gather active resize observations at depth": `(observer id, node, sizes)` for every observation
/// whose box changed and whose target is deeper than `at`, and whether a shallower one was skipped.
fn gather(scope: &mut v8::PinScope, at: Option<u32>, dpr: f64) -> (Vec<(u64, u32, Sizes)>, bool) {
    let obs: Vec<(u64, u32, BoxKind, (f64, f64))> = rendering::with_state(scope, |s| {
        s.ro.iter().flat_map(|r| r.observations.iter().map(move |o| (r.id, o.node, o.box_kind, o.last))).collect()
    });
    let mut geo = Geometry::new(scope);
    let mut active = Vec::new();
    let mut skipped = false;
    for (id, node, kind, last) in obs {
        let s = sizes(scope, &mut geo, node, dpr);
        if s.of(kind) == last {
            continue;
        }
        if at.is_none_or(|d| depth(scope, node) > d) {
            active.push((id, node, s));
        } else {
            skipped = true;
        }
    }
    geo.finish(scope);
    (active, skipped)
}

/// "Broadcast active resize observations": one callback per observer, in registration order. Answers
/// the shallowest depth delivered.
fn broadcast(scope: &mut v8::PinScope, active: Vec<(u64, u32, Sizes)>) -> Option<u32> {
    let ids: Vec<u64> = rendering::with_state(scope, |s| s.ro.iter().map(|r| r.id).collect());
    let mut shallowest: Option<u32> = None;
    for id in ids {
        let mine: Vec<&(u64, u32, Sizes)> = active.iter().filter(|a| a.0 == id).collect();
        if mine.is_empty() {
            continue;
        }
        let Some(observer) = rendering::with_state(scope, |s| s.ro.iter().find(|r| r.id == id).map(|r| r.observer.clone())) else { continue };
        let arr = v8::Array::new(scope, mine.len() as i32);
        // The batch's entries, then its observations' last sizes in one look at the registry, then its depths:
        // no page script runs between them (the callbacks run between batches), so this is what minting,
        // recording and measuring each entry in turn did -- whose per-entry search of the observations was
        // quadratic in the batch. `gather` lists a registration's entries in its observations' order, so one
        // forward scan finds each; a search from the start covers a callback that reordered them.
        let mut kit = rendering::Kit::default();
        for (i, (_, node, sz)) in mine.iter().enumerate() {
            if let Some(e) = mint_entry(scope, &mut kit, *node, sz) {
                arr.set_index(scope, i as u32, e.into());
            }
        }
        rendering::with_state(scope, |s| {
            let Some(r) = s.ro.iter_mut().find(|r| r.id == id) else { return };
            let mut from = 0;
            for (_, node, sz) in &mine {
                let at = r.observations[from..].iter().position(|o| o.node == *node).map(|p| p + from).or_else(|| r.observations.iter().position(|o| o.node == *node));
                if let Some(p) = at {
                    let o = &mut r.observations[p];
                    o.last = sz.of(o.box_kind);
                    from = p + 1;
                }
            }
        });
        for (_, node, _) in &mine {
            let d = depth(scope, *node);
            shallowest = Some(shallowest.map_or(d, |s| s.min(d)));
        }
        let observer = v8::Local::new(scope, &observer);
        rendering::invoke_callback(scope, observer, arr.into());
    }
    shallowest
}

/// The resize-observer half of "update the rendering". Answers whether anything was delivered or
/// left undelivered -- either way the next frame has work.
pub(crate) fn run_resize_steps(scope: &mut v8::PinScope) -> bool {
    let dpr = device_pixel_ratio(scope);
    let (mut active, mut skipped) = gather(scope, None, dpr);
    let mut delivered = false;
    while !active.is_empty() {
        delivered = true;
        let depth = broadcast(scope, active);
        let (a, s) = gather(scope, depth, dpr);
        active = a;
        skipped = s;
    }
    if skipped {
        let site = crate::core::events::report::ErrorSite {
            message: "ResizeObserver loop completed with undelivered notifications.".to_string(),
            filename: String::new(),
            lineno: 0,
            colno: 0,
        };
        let null: v8::Local<v8::Value> = v8::null(scope).into();
        crate::core::events::report::report_exception_at(scope, null, Some(site));
    }
    delivered || skipped
}

/// The body every implemented row of [`RESIZEOBSERVER`] names.
pub fn install_resize_observer(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "disconnect" => method_row(scope, proto, member, cb_ro_disconnect),
        "observe" => method_row(scope, proto, member, cb_ro_observe),
        "unobserve" => method_row(scope, proto, member, cb_ro_unobserve),
        other => debug_assert!(false, "install_resize_observer: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `ResizeObserver` block of `install/generated_registry.rs`, in the capture's
// order. `[value, window]`: the capture records `newable: false` (`new ResizeObserver()` throws
// without a callback) and `worker.surface.json` does not list the interface.
webapi::interface! {
    "ResizeObserver" [value, window] {
        ctor   "ResizeObserver"  len 1               = stub,
        method "disconnect" : Object len 0           = install_resize_observer,
        method "observe"    : Object len 1 required 1 = install_resize_observer,
        method "unobserve"  : Object len 1 required 1 = install_resize_observer
    }
}

// Rows VERBATIM from the `ResizeObserverEntry` block of `install/generated_registry.rs`, in the
// capture's order. Every member reads the private slot `mint_entry` wrote (the `MutationRecord`
// mechanism); they were honest D2 `stub`s while no observation ever fired. `[interface, window]` is
// the registry's own model, and its interface object throws "Illegal constructor", which is what the
// bundle's oracle records for `new ResizeObserverEntry()`.
webapi::interface! {
    "ResizeObserverEntry" [interface, window] {
        ctor   "ResizeObserverEntry"                    len 0 = stub,
        attr   "target"                    : Object readonly  = slot_get,
        attr   "contentRect"               : Object readonly  = slot_get,
        attr   "contentBoxSize"            : Object readonly  = slot_get,
        attr   "borderBoxSize"             : Object readonly  = slot_get,
        attr   "devicePixelContentBoxSize" : Object readonly  = slot_get
    }
}

