//! `core/intersection_observer/intersection_observer.rs` — `IntersectionObserver` and
//! `IntersectionObserverEntry`, one `interface!` table each, and the intersection
//! steps of the rendering update.
//!
//! The ten members used to reach JS three ways at once: seven through `#[op2] impl` onto the cppgc
//! class template's own prototype, `root` through an `Object.defineProperty` in
//! `dom/08g-observers.js`, and `observe`/`unobserve`/`disconnect` re-defined a second time by that
//! file's `op_arity_guard` pass — with the interface object itself replaced by an
//! `op_nativize_ctor` MASK wrapping a JS arrow that checked the callback and forwarded to `new C(...)`.
//! `IntersectionObserverEntry` was a registry shell nothing decorated.
//!
//! Now: two tables, mounted by `install::live::install_window_observers`. `cb_io_ctor` is the mask's check
//! as a real Rust constructor, so `new IntersectionObserver()` still answers
//! "Failed to construct 'IntersectionObserver': parameter 1 is not of type 'Function'." and
//! `IntersectionObserver.toString()` reads `[native code]` by construction rather than by costume.
//!
//! # The observer observes
//!
//! `observe` was an empty body, so no callback ever ran. It registers the target now, and the frame in
//! [`super::rendering`] computes each observation and queues an entry whenever the threshold index or
//! `isIntersecting` changed -- the first computation always does. The dictionary is read the way
//! Chrome 150 reads it (`tests/fixtures/p12c/chrome-150.io-options.txt`): members in WebIDL's
//! alphabetical order, `threshold` sorted and range-checked, `rootMargin` and `scrollMargin` in pixels
//! or percent only, `root` an Element or a Document, `trackVisibility` refused without a `delay` of at
//! least 100.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use super::rendering::{self, Geometry, IoObservation, IoRegistration, R};
use crate::binding::protos::{get_prop, global_get, str_val, throw_type_error, unwrap_id};
use crate::core::events::event::evt_slot_write;
use crate::install::live::{getter_row, method_row, set_table_proto};
use crate::install::MemberDef;

/// One side of a margin: pixels, or a percentage of the root's size on that axis.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Len {
    Px(f64),
    Pct(f64),
}

impl Len {
    fn resolve(self, basis: f64) -> f64 {
        match self {
            Len::Px(v) => v,
            // Chrome snaps a percentage margin DOWN to a whole pixel: 50% of a 991px root is 495, not
            // 495.5 (measured -- `rootBounds.height` 1486 for `0px 0px 50% 0px`).
            Len::Pct(p) => (basis * p / 100.0).floor(),
        }
    }

    fn serialize(self) -> String {
        match self {
            Len::Px(v) => format!("{}px", fmt_num(v)),
            Len::Pct(v) => format!("{}%", fmt_num(v)),
        }
    }
}

fn fmt_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct IntersectionObserver {
    pub(crate) root: std::cell::RefCell<Option<v8::Global<v8::Object>>>, // pub(crate): core/dom/node.rs's `cb_io_root` reads it.
    /// top, right, bottom, left.
    root_margin: std::cell::RefCell<[Len; 4]>,
    scroll_margin: std::cell::RefCell<[Len; 4]>,
    thresholds: std::cell::RefCell<Vec<f64>>,
    delay: std::cell::Cell<f64>,
    track_visibility: std::cell::Cell<bool>,
    /// This observer's key in the realm's registry, `0` until it is minted.
    id: std::cell::Cell<u64>,
}

const ZERO_MARGIN: [Len; 4] = [Len::Px(0.0); 4];

/// The cppgc class template: the CONSTRUCTOR only. The ten members are rows of [`INTERSECTIONOBSERVER`]
/// below and are installed by THE installer onto the table's prototype; an `#[op2] impl` member here
/// would be a second definition site for the same slot (invariant I1).
#[webapi(base, own)]
impl IntersectionObserver {
    #[constructor]
    #[cppgc]
    fn constructor(scope: &mut v8::PinScope, _cb: v8::Local<v8::Value>, opts: v8::Local<v8::Value>) -> Result<IntersectionObserver, ::deno_error::JsErrorBox> {
        match read_options(scope, opts) {
            Some(o) => Ok(o),
            None => Err(::deno_error::JsErrorBox::type_error("Failed to construct 'IntersectionObserver'")),
        }
    }
}

/// A margin string: one to four `<number>px` / `<number>%` tokens, expanded like the CSS shorthand.
/// `Err` carries Chrome's sentence.
fn parse_margin(raw: &str, what: &str) -> Result<[Len; 4], String> {
    let tokens: Vec<&str> = raw.split_ascii_whitespace().collect();
    if tokens.is_empty() {
        return Ok(ZERO_MARGIN);
    }
    let mut vals = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if i == 4 {
            return Err(format!("Extra text found at the end of {what}."));
        }
        let (num, pct) = if let Some(n) = t.strip_suffix("px") {
            (n, false)
        } else if let Some(n) = t.strip_suffix('%') {
            (n, true)
        } else {
            return Err(format!("{what} must be specified in pixels or percent."));
        };
        let v: f64 = match num.parse() {
            Ok(v) if !num.is_empty() && f64::is_finite(v) && !num.contains(|c: char| c.is_ascii_alphabetic()) => v,
            _ => return Err(format!("{what} must be specified in pixels or percent.")),
        };
        vals.push(if pct { Len::Pct(v) } else { Len::Px(v) });
    }
    Ok(match vals.len() {
        1 => [vals[0]; 4],
        2 => [vals[0], vals[1], vals[0], vals[1]],
        3 => [vals[0], vals[1], vals[2], vals[1]],
        _ => [vals[0], vals[1], vals[2], vals[3]],
    })
}

fn serialize_margin(m: &[Len; 4]) -> String {
    m.iter().map(|l| l.serialize()).collect::<Vec<_>>().join(" ")
}

const CTOR: &str = "Failed to construct 'IntersectionObserver': ";

/// `unrestricted`-free WebIDL `double` from a dictionary member: `None` after throwing.
fn idl_double(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, member: &str) -> Option<f64> {
    let n = v.number_value(scope)?;
    if !n.is_finite() {
        throw_type_error(scope, &format!("{CTOR}Failed to read the '{member}' property from 'IntersectionObserverInit': The provided double value is non-finite."));
        return None;
    }
    Some(n)
}

/// Read `IntersectionObserverInit`. `None` means an exception is pending.
fn read_options(scope: &mut v8::PinScope, opts: v8::Local<v8::Value>) -> Option<IntersectionObserver> {
    let mut root = None;
    let mut root_margin = ZERO_MARGIN;
    let mut scroll_margin = ZERO_MARGIN;
    let mut thresholds = vec![0.0];
    let mut delay = 0.0;
    let mut track_visibility = false;
    if !opts.is_null_or_undefined() {
        let Ok(o) = v8::Local::<v8::Object>::try_from(opts) else {
            throw_type_error(scope, &format!("{CTOR}The provided value is not of type 'IntersectionObserverInit'."));
            return None;
        };
        // WebIDL reads a dictionary's members in lexicographic order, so an error in an earlier member
        // is the one a page sees.
        let v = get_prop(scope, o, "delay")?;
        if !v.is_undefined() {
            delay = idl_double(scope, v, "delay")?;
        }
        let v = get_prop(scope, o, "root")?;
        if !v.is_null_or_undefined() {
            let ok = v8::Local::<v8::Object>::try_from(v).ok().filter(|obj| {
                deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, (*obj).into()).is_some()
                    || unwrap_id(scope, (*obj).into()).is_some_and(|id| {
                    let op_state = deno_core::JsRuntime::op_state_from(scope);
                    let st = op_state.borrow();
                    st.try_borrow::<crate::binding::host_dom::HostDom>().is_some_and(|d| matches!(d.0.node_type(id), 1 | 9))
                })
            });
            match ok {
                Some(obj) => root = Some(v8::Global::new(scope, obj)),
                None => {
                    throw_type_error(scope, &format!("{CTOR}Failed to read the 'root' property from 'IntersectionObserverInit': The provided value is not of type '(Document or Element)'."));
                    return None;
                }
            }
        }
        let v = get_prop(scope, o, "rootMargin")?;
        if !v.is_undefined() {
            let s = v.to_string(scope)?.to_rust_string_lossy(scope);
            match parse_margin(&s, "rootMargin") {
                Ok(m) => root_margin = m,
                Err(msg) => {
                    crate::binding::protos::throw_dom(scope, &format!("{CTOR}{msg}"), "SyntaxError");
                    return None;
                }
            }
        }
        let v = get_prop(scope, o, "scrollMargin")?;
        if !v.is_undefined() {
            let s = v.to_string(scope)?.to_rust_string_lossy(scope);
            match parse_margin(&s, "scrollMargin") {
                Ok(m) => scroll_margin = m,
                Err(msg) => {
                    crate::binding::protos::throw_dom(scope, &format!("{CTOR}{msg}"), "SyntaxError");
                    return None;
                }
            }
        }
        let v = get_prop(scope, o, "threshold")?;
        if !v.is_undefined() {
            let is_seq = v8::Local::<v8::Object>::try_from(v).ok().is_some_and(|obj| {
                let sym = v8::Symbol::get_iterator(scope);
                obj.get(scope, sym.into()).is_some_and(|f| f.is_function())
            });
            let mut list = Vec::new();
            if is_seq {
                for item in sequence(scope, v)? {
                    list.push(idl_double(scope, item, "threshold")?);
                }
            } else {
                list.push(idl_double(scope, v, "threshold")?);
            }
            if list.is_empty() {
                list.push(0.0);
            }
            thresholds = list;
        }
        let v = get_prop(scope, o, "trackVisibility")?;
        if !v.is_undefined() {
            track_visibility = v.boolean_value(scope);
        }
    }
    // Blink keeps the thresholds as `float` (`ParseThresholds`: `base::ClampedNumeric<float>` of each double, THEN the
    // range check and the sort), so `0.1` reads back as 0.10000000149011612 and `1 + 1e-12` is 1 (C2).
    for t in thresholds.iter_mut() {
        *t = t.clamp(-(f32::MAX as f64), f32::MAX as f64) as f32 as f64;
    }
    if thresholds.iter().any(|t| !(0.0..=1.0).contains(t)) {
        crate::binding::protos::throw_range_error(scope, &format!("{CTOR}Threshold values must be numbers between 0 and 1"));
        return None;
    }
    thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if track_visibility && delay < 100.0 {
        crate::binding::protos::throw_dom(
            scope,
            &format!("{CTOR}To enable the 'trackVisibility' option, you must also use a 'delay' option with a value of at least 100. Visibility is more expensive to compute than the basic intersection; enabling this option may negatively affect your page's performance. Please make sure you *really* need visibility tracking before enabling the 'trackVisibility' option."),
            "NotSupportedError",
        );
        return None;
    }
    Some(IntersectionObserver {
        root: std::cell::RefCell::new(root),
        root_margin: std::cell::RefCell::new(root_margin),
        scroll_margin: std::cell::RefCell::new(scroll_margin),
        thresholds: std::cell::RefCell::new(thresholds),
        delay: std::cell::Cell::new(delay),
        track_visibility: std::cell::Cell::new(track_visibility),
        id: std::cell::Cell::new(0),
    })
}

/// A WebIDL `sequence<any>` from an iterable: an Array by index, anything else by its iterator.
/// `None` when the iteration threw.
fn sequence<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
        let mut out = Vec::new();
        for i in 0..arr.length() {
            out.push(arr.get_index(scope, i)?);
        }
        return Some(out);
    }
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    let sym = v8::Symbol::get_iterator(scope);
    let method = obj.get(scope, sym.into()).and_then(|f| v8::Local::<v8::Function>::try_from(f).ok())?;
    let iter = method.call(scope, v, &[]).and_then(|i| v8::Local::<v8::Object>::try_from(i).ok())?;
    let next = get_prop(scope, iter, "next").and_then(|f| v8::Local::<v8::Function>::try_from(f).ok())?;
    let mut out = Vec::new();
    loop {
        let r = next.call(scope, iter.into(), &[]).and_then(|r| v8::Local::<v8::Object>::try_from(r).ok())?;
        if get_prop(scope, r, "done")?.boolean_value(scope) {
            return Some(out);
        }
        out.push(get_prop(scope, r, "value")?);
    }
}

/// `new IntersectionObserver(callback, options)` — the `op_nativize_ctor` mask's JS arrow, in Rust.
pub fn cb_io_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("IntersectionObserver"));
        return;
    }
    // WebIDL's argument count before the callback's type (Chrome 150: "1 argument required, but only 0 present.")
    if !crate::install::webidl::require_args(scope, &args, "IntersectionObserver", 1) {
        return;
    }
    if !args.get(0).is_function() {
        throw_type_error(scope, "Failed to construct 'IntersectionObserver': parameter 1 is not of type 'Function'.");
        return;
    }
    let cb = args.get(0);
    let opts = args.get(1);
    let Some(built) = read_options(scope, opts) else { return };
    let obj = crate::binding::class::mint(scope, built);
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => set_table_proto(scope, obj, "IntersectionObserver"),
    }
    // The callback lives on the observer's own private slot, so the garbage collector sees the edge:
    // an observer nothing observes and nothing references is collectable with its callback.
    evt_slot_write(scope, obj, "observer_callback", cb);
    rv.set(obj.into());
}

/// The receiver's `IntersectionObserver`, or `None` after throwing WebIDL's "Illegal invocation" --
/// the check that keeps every row of this table from accepting a foreign receiver.
macro_rules! io_this {
    ($scope:ident, $args:ident, $member:literal) => {
        match deno_core::cppgc::try_unwrap_cppgc_object::<IntersectionObserver>($scope, $args.this().into()) {
            Some(p) => p,
            None => {
                throw_type_error($scope, concat!("Failed to execute '", $member, "' on 'IntersectionObserver': Illegal invocation"));
                return;
            }
        }
    };
}

/// An `Element` argument's node id, or `None` after Chrome's TypeError.
fn element_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, member: &str) -> Option<u32> {
    let id = unwrap_id(scope, v).filter(|id| Geometry::is_element(scope, *id));
    if id.is_none() {
        throw_type_error(scope, &format!("Failed to execute '{member}' on 'IntersectionObserver': parameter 1 is not of type 'Element'."));
    }
    id
}

fn observer_id(scope: &mut v8::PinScope, io: &IntersectionObserver) -> u64 {
    if io.id.get() == 0 {
        io.id.set(rendering::next_id(scope));
    }
    io.id.get()
}

fn cb_io_observe(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "observe");
    let Some(node) = element_arg(scope, args.get(0), "observe") else { return };
    let id = observer_id(scope, unsafe { io.as_ref() });
    rendering::note_document(scope);
    let this = v8::Global::new(scope, args.this());
    let added = rendering::with_state(scope, |s| {
        let reg = match s.io.iter().position(|r| r.id == id) {
            Some(i) => &mut s.io[i],
            None => {
                s.io.push(IoRegistration { id, observer: this, observations: Vec::new(), records: Vec::new(), delivery_queued: false });
                s.io.last_mut().unwrap()
            }
        };
        if reg.observations.iter().any(|o| o.node == node) {
            return false;
        }
        reg.observations.push(IoObservation { node, prev_index: -1, prev_intersecting: false });
        true
    });
    if added {
        rendering::request_frame(scope);
    }
}

fn cb_io_unobserve(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "unobserve");
    let Some(node) = element_arg(scope, args.get(0), "unobserve") else { return };
    let id = unsafe { io.as_ref() }.id.get();
    rendering::with_state(scope, |s| {
        if let Some(i) = s.io.iter().position(|r| r.id == id) {
            s.io[i].observations.retain(|o| o.node != node);
            if s.io[i].observations.is_empty() && s.io[i].records.is_empty() {
                s.io.remove(i);
            }
        }
    });
}

fn cb_io_disconnect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "disconnect");
    let id = unsafe { io.as_ref() }.id.get();
    rendering::with_state(scope, |s| s.io.retain(|r| r.id != id));
}

fn cb_io_take_records(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "takeRecords");
    let id = unsafe { io.as_ref() }.id.get();
    let records = rendering::with_state(scope, |s| match s.io.iter_mut().find(|r| r.id == id) {
        Some(r) => std::mem::take(&mut r.records),
        None => Vec::new(),
    });
    let arr = v8::Array::new(scope, records.len() as i32);
    for (i, g) in records.iter().enumerate() {
        let l = v8::Local::new(scope, g);
        arr.set_index(scope, i as u32, l.into());
    }
    rv.set(arr.into());
}
fn cb_io_root_margin(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "get rootMargin");
    let s = serialize_margin(&unsafe { io.as_ref() }.root_margin.borrow());
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_io_scroll_margin(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "get scrollMargin");
    let s = serialize_margin(&unsafe { io.as_ref() }.scroll_margin.borrow());
    let v = str_val(scope, &s);
    rv.set(v);
}
/// `readonly attribute FrozenArray<double> thresholds`: the getter returns its `Vector<float>`, so a new frozen array
/// per read (`install::frozen_array`; it was a new writable one, C2).
fn cb_io_thresholds(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "get thresholds");
    let t = unsafe { io.as_ref() }.thresholds.borrow().clone();
    let answer = crate::install::frozen_array::answer(scope, args.this(), "IntersectionObserver", "thresholds", |_| 0.0, |scope| {
        Some(t.iter().map(|n| v8::Number::new(scope, *n).into()).collect())
    });
    if let Some(a) = answer {
        rv.set(a);
    }
}
fn cb_io_delay(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "get delay");
    let n: v8::Local<v8::Value> = v8::Number::new(scope, unsafe { io.as_ref() }.delay.get()).into();
    rv.set(n);
}
fn cb_io_track_visibility(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let io = io_this!(scope, args, "get trackVisibility");
    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, unsafe { io.as_ref() }.track_visibility.get()).into();
    rv.set(b);
}

// ── the intersection steps ───────────────────────────────────────────────────────────────────────

/// What one observation computes to.
struct Answer {
    intersecting: bool,
    ratio: f64,
    target: R,
    intersection: R,
    root_bounds: R,
}

const NOTHING: Answer = Answer { intersecting: false, ratio: 0.0, target: R::ZERO, intersection: R::ZERO, root_bounds: R::ZERO };

/// "Compute the intersection" for one target of one observer.
///
/// Measured on Chrome 150 (`chrome-150.io-geom.txt`), and each is a branch below: a target with no box
/// (`display: none`, detached) and a target outside an explicit root's subtree answer all zeros,
/// `rootBounds` included, not `null`; a zero-area target that touches the root is intersecting with a
/// ratio of 1; `visibility: hidden` does not matter; an explicit root's bounds are its padding box less
/// its scrollbar; an ancestor with `overflow` other than `visible` clips.
fn compute(scope: &mut v8::PinScope, geo: &mut Geometry, root: Option<u32>, margin: &[Len; 4], node: u32) -> Answer {
    let root_box = match root {
        None => Some(geo.viewport(scope)),
        Some(r) => {
            if r == node || !Geometry::contains(scope, r, node) {
                return NOTHING;
            }
            geo.client_box(scope, r)
        }
    };
    let Some(mut root_rect) = root_box else { return NOTHING };
    let Some(target) = geo.border_box(scope, node) else { return NOTHING };
    let (t, r, b, l) = (
        margin[0].resolve(root_rect.h),
        margin[1].resolve(root_rect.w),
        margin[2].resolve(root_rect.h),
        margin[3].resolve(root_rect.w),
    );
    root_rect = R { x: root_rect.x - l, y: root_rect.y - t, w: root_rect.w + l + r, h: root_rect.h + t + b };
    // Clip by every ancestor between the target and the root that clips its content.
    let mut clip = Some(target);
    let mut cur = Geometry::parent(scope, node);
    while let (Some(a), Some(c)) = (cur, clip) {
        if Some(a) == root || !Geometry::is_element(scope, a) {
            break;
        }
        if geo.clips(scope, a) {
            // only the axes it clips: `overflow-x: clip` beside a `visible` `overflow-y` clips nothing vertically (the
            // one pair that leaves an axis visible); and an element with no box of its own -- `display: contents` --
            // clips nothing. Chrome 150: minea's ticker, under a `display: contents; overflow: clip` wrapper and its
            // own `overflow-x: clip` list, intersects once scrolled to; it never did here, so it never showed.
            let (x, y) = geo.clip_axes(scope, a);
            clip = match geo.client_box(scope, a) {
                Some(cb) => {
                    const OPEN: f64 = 1e9;
                    let cb = R {
                        x: if x { cb.x } else { -OPEN },
                        w: if x { cb.w } else { 2.0 * OPEN },
                        y: if y { cb.y } else { -OPEN },
                        h: if y { cb.h } else { 2.0 * OPEN },
                    };
                    c.intersect(cb)
                }
                None => Some(c),
            };
        }
        cur = Geometry::parent(scope, a);
    }
    let inter = clip.and_then(|c| c.intersect(root_rect));
    match inter {
        None => Answer { intersecting: false, ratio: 0.0, target, intersection: R::ZERO, root_bounds: root_rect },
        Some(i) => {
            let ratio = if target.area() > 0.0 { (i.area() / target.area()).min(1.0) } else { 1.0 };
            Answer { intersecting: true, ratio, target, intersection: i, root_bounds: root_rect }
        }
    }
}

/// A REAL `IntersectionObserverEntry`: the table's prototype, eight private slots, the brand.
fn mint_entry<'s>(scope: &mut v8::PinScope<'s, '_>, kit: &mut rendering::Kit<'s>, time: f64, node: u32, a: &Answer) -> Option<v8::Local<'s, v8::Object>> {
    let proto = kit.proto(scope, "IntersectionObserverEntry")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let t: v8::Local<v8::Value> = v8::Number::new(scope, time).into();
    evt_slot_write(scope, obj, "time", t);
    let rb = kit.rect(scope, a.root_bounds);
    evt_slot_write(scope, obj, "rootBounds", rb);
    let br = kit.rect(scope, a.target);
    evt_slot_write(scope, obj, "boundingClientRect", br);
    let ir = kit.rect(scope, a.intersection);
    evt_slot_write(scope, obj, "intersectionRect", ir);
    let is: v8::Local<v8::Value> = v8::Boolean::new(scope, a.intersecting).into();
    evt_slot_write(scope, obj, "isIntersecting", is);
    // `trackVisibility` is not computed: no occlusion is modelled, and Chrome answers `false` for every
    // observer that did not ask.
    let vis: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    evt_slot_write(scope, obj, "isVisible", vis);
    let ratio: v8::Local<v8::Value> = v8::Number::new(scope, a.ratio).into();
    evt_slot_write(scope, obj, "intersectionRatio", ratio);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let target: v8::Local<v8::Value> = crate::binding::protos::wrap_cached(scope, &op_state, node).into();
    evt_slot_write(scope, obj, "target", target);
    crate::install::webidl::brand(scope, obj, "IntersectionObserverEntry");
    Some(obj)
}

/// "Run the update intersection observations steps" for every observer in the realm. Answers whether
/// any entry was queued.
pub(crate) fn run_intersection_steps(scope: &mut v8::PinScope) -> bool {
    let time = crate::core::timing::performance::clock_now_ms(scope);
    let regs: Vec<(u64, v8::Global<v8::Object>, Vec<u32>)> = rendering::with_state(scope, |s| {
        s.io.iter().map(|r| (r.id, r.observer.clone(), r.observations.iter().map(|o| o.node).collect())).collect()
    });
    let mut geo = Geometry::new(scope);
    let mut kit = rendering::Kit::default();
    let mut queued_any = false;
    for (id, observer, nodes) in regs {
        let observer = v8::Local::new(scope, &observer);
        let Some(io) = deno_core::cppgc::try_unwrap_cppgc_object::<IntersectionObserver>(scope, observer.into()) else { continue };
        let io = unsafe { io.as_ref() };
        let root_obj = io.root.borrow().clone();
        // `Some(None)`: the viewport. `Some(Some(el))`: an element. `None`: a Document other than this
        // page's, which no target of this page is inside.
        let root: Option<Option<u32>> = match root_obj {
            None => Some(None),
            Some(g) => {
                let l = v8::Local::new(scope, &g);
                match unwrap_id(scope, l.into()) {
                    Some(r) if Geometry::is_element(scope, r) => Some(Some(r)),
                    // A Document root is the implicit root's box when it is the page's own document.
                    _ => global_get(scope, "document").is_some_and(|d| d.strict_equals(l.into())).then_some(None),
                }
            }
        };
        let margin = *io.root_margin.borrow();
        let thresholds = io.thresholds.borrow().clone();
        let mut answers = Vec::with_capacity(nodes.len());
        for node in nodes {
            let a = match root {
                Some(root) => compute(scope, &mut geo, root, &margin, node),
                None => NOTHING,
            };
            let index = if a.intersecting {
                thresholds.iter().position(|t| *t > a.ratio).unwrap_or(thresholds.len()) as i64
            } else {
                0
            };
            // `isIntersecting` is Blink's `IntersectionGeometry::IsIntersecting`: the ratio reached the first
            // threshold (the index is past 0), not "the rectangles touch". Chrome 150: under `threshold: 0.4` a
            // section 39.6 % in view reads `false` -- GitHub shows its "back to top" button then, and korvus,
            // reading `true`, never did. Under `threshold: 0` an edge-adjacent target still reads `true`.
            let a = Answer { intersecting: index > 0, ..a };
            answers.push((node, a, index));
        }
        // The observations' last answers, all in one look at the registry: `nodes` is the registration's
        // observations in order (no script runs in a run, and `observe` keeps one per node), so the i-th answer is
        // normally the i-th observation's -- a search finds it otherwise. A look and a search per target was
        // quadratic in the targets. Computing, recording and minting touch disjoint state and run no page script,
        // so doing each for every target in turn records and mints what doing all three per target did.
        let changed: Vec<bool> = rendering::with_state(scope, |s| {
            let Some(reg) = s.io.iter_mut().find(|r| r.id == id) else { return vec![false; answers.len()] };
            answers
                .iter()
                .enumerate()
                .map(|(i, (node, a, index))| {
                    let at = if reg.observations.get(i).is_some_and(|o| o.node == *node) {
                        Some(i)
                    } else {
                        reg.observations.iter().position(|o| o.node == *node)
                    };
                    let Some(obs) = at.map(|p| &mut reg.observations[p]) else { return false };
                    let changed = obs.prev_index != *index || obs.prev_intersecting != a.intersecting;
                    obs.prev_index = *index;
                    obs.prev_intersecting = a.intersecting;
                    changed
                })
                .collect()
        });
        let mut entries = Vec::new();
        for ((node, a, _), changed) in answers.iter().zip(changed) {
            if changed {
                if let Some(e) = mint_entry(scope, &mut kit, time, *node, a) {
                    entries.push(v8::Global::new(scope, e));
                }
            }
        }
        if entries.is_empty() {
            continue;
        }
        queued_any = true;
        let schedule = rendering::with_state(scope, |s| {
            let Some(reg) = s.io.iter_mut().find(|r| r.id == id) else { return false };
            reg.records.extend(entries);
            !std::mem::replace(&mut reg.delivery_queued, true)
        });
        if schedule {
            rendering::queue_task_with_id(scope, cb_deliver, id, 0.0);
        }
    }
    geo.finish(scope);
    let scrollers = std::mem::take(&mut geo.scrollers);
    rendering::with_state(scope, |s| s.scrollers = scrollers);
    queued_any
}

/// One observer's delivery task: hand it its queued entries, if `takeRecords` has not already.
fn cb_deliver(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u64) else { return };
    let taken = rendering::with_state(scope, |s| {
        let reg = s.io.iter_mut().find(|r| r.id == id)?;
        reg.delivery_queued = false;
        Some((reg.observer.clone(), std::mem::take(&mut reg.records)))
    });
    let Some((observer, records)) = taken else { return };
    if records.is_empty() {
        return;
    }
    let arr = v8::Array::new(scope, records.len() as i32);
    for (i, g) in records.iter().enumerate() {
        let l = v8::Local::new(scope, g);
        arr.set_index(scope, i as u32, l.into());
    }
    let observer = v8::Local::new(scope, &observer);
    rendering::invoke_callback(scope, observer, arr.into());
}

/// The body every implemented row of [`INTERSECTIONOBSERVER`] names.
pub fn install_intersection_observer(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // `cb_io_root` stays in `core/dom/node.rs` beside the node-identity helpers it uses; this row
        // NAMES it, which is what a one-table row is for.
        "root" => getter_row(scope, proto, member, crate::core::dom::node::cb_io_root),
        "rootMargin" => getter_row(scope, proto, member, cb_io_root_margin),
        "scrollMargin" => getter_row(scope, proto, member, cb_io_scroll_margin),
        "thresholds" => getter_row(scope, proto, member, cb_io_thresholds),
        "delay" => getter_row(scope, proto, member, cb_io_delay),
        "trackVisibility" => getter_row(scope, proto, member, cb_io_track_visibility),
        "disconnect" => method_row(scope, proto, member, cb_io_disconnect),
        "observe" => method_row(scope, proto, member, cb_io_observe),
        "takeRecords" => method_row(scope, proto, member, cb_io_take_records),
        "unobserve" => method_row(scope, proto, member, cb_io_unobserve),
        other => debug_assert!(false, "install_intersection_observer: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `IntersectionObserver` block of `install/generated_registry.rs`, in the
// capture's order: six readonly accessors (`root` is the nullable one), then the four operations.
// `[value, window]`: the capture records `newable: false` (`new IntersectionObserver()` throws without
// a callback) and `worker.surface.json` does not list the interface.
webapi::interface! {
    "IntersectionObserver" [value, window] {
        ctor   "IntersectionObserver"        len 1                  = stub,
        attr   "root"            : Object readonly nullable         = install_intersection_observer,
        attr   "rootMargin"      : Object readonly                  = install_intersection_observer,
        attr   "scrollMargin"    : Object readonly                  = install_intersection_observer,
        attr   "thresholds"      : Object readonly                  = install_intersection_observer,
        attr   "delay"           : Object readonly                  = install_intersection_observer,
        attr   "trackVisibility" : Object readonly                  = install_intersection_observer,
        method "disconnect"      : Object len 0                     = install_intersection_observer,
        method "observe"         : Object len 1 required 1          = install_intersection_observer,
        method "takeRecords"     : Object len 0                     = install_intersection_observer,
        method "unobserve"       : Object len 1 required 1          = install_intersection_observer
    }
}

// Rows VERBATIM from the `IntersectionObserverEntry` block of `install/generated_registry.rs`, in the
// capture's order. Every member reads the private slot [`mint_entry`] wrote -- the `MutationRecord`
// mechanism (`core/dom/mutation_observer.rs`). They were honest D2 `stub`s while no observation ever
// fired and no object ever wore this prototype.
//
// Ruling: the header is `[interface, window]` where the registry recorded `Model::Value`. A hand
// table's `Value` interface object is the installer's no-op NEWABLE placeholder, and the bundle's
// oracle records `new IntersectionObserverEntry()` and `new IntersectionObserverEntry({})` as
// TypeErrors; `interface` is the model whose interface object throws "Illegal constructor", which is
// the answer the shell gave and the answer Brave gives.
webapi::interface! {
    "IntersectionObserverEntry" [interface, window] {
        ctor   "IntersectionObserverEntry"          len 0     = stub,
        attr   "time"               : Object readonly         = slot_get,
        attr   "rootBounds"         : Object readonly nullable = slot_get,
        attr   "boundingClientRect" : Object readonly         = slot_get,
        attr   "intersectionRect"   : Object readonly         = slot_get,
        attr   "isIntersecting"     : Object readonly         = slot_get,
        attr   "isVisible"          : Object readonly         = slot_get,
        attr   "intersectionRatio"  : Object readonly         = slot_get,
        attr   "target"             : Object readonly         = slot_get
    }
}

