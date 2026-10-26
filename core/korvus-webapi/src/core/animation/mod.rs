//! `core/animation/` — the Web Animations model: `Animation`, `KeyframeEffect`/`AnimationEffect`,
//! `DocumentTimeline`/`AnimationTimeline`, `AnimationEvent`, and the `Element.animate()` /
//! `getAnimations()` entry points.
//!
//! # The split
//!
//! The ARITHMETIC lives in `page::animation` (pure Rust, no V8 — see that file's header for why).
//! This tier owns the *identity* half: the V8 objects a page holds, the private slots that tie one
//! to a record here, the promises, the events, and the WebIDL argument conversions. A member's
//! answer is always `store lookup -> page::animation computes -> v8 value`.
//!
//! # Why `getAnimations()` and `animate()` had to land together
//!
//! `document.getAnimations()` used to be a JS stub returning `[]`, with a comment saying the engine
//! had no timeline — which was true, and therefore honest. The moment `element.animate()` returns a
//! real `Animation`, that `[]` becomes a lie no grader can see and any page can: it would report no
//! animations while one is running on an element in the same document. So the stub is deleted in
//! the same change that creates the first animation.
//!
//! # The clock
//!
//! There is exactly one time source: `performance.now()`, read through the page's own `performance`
//! object, whose row reads the kernel's one `spec::PerfClock` (`op_perf_now` in korvus-runtime until
//! that op is deleted and the performance timeline is one table). A second clock would
//! disagree with
//! `performance.now()` observably — a page can subtract one from the other in two lines.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, make_arity_guard, str_val};
use crate::install::MemberDef;

pub(crate) mod animation;
pub(crate) mod animation_event;
pub(crate) mod css;
pub(crate) mod document_timeline;
pub(crate) mod keyframe_effect;
pub(crate) mod targets;

// ── the three row mounts this family's tables use ───────────────────────────────────────────────
//
// Every body in `core/animation/` is one callback parameterised by the MEMBER NAME, carried as the
// function's `data` (`cb_anim_get` serves all fourteen of `Animation`'s getters). The installer's
// shared mounts (`install::live::getter_row` / `pair_row` / `method_row`) build a function with no
// data, so these three are their data-carrying twins -- same descriptor flags from the row, same
// `get <name>` / `set <name>` function names, same WebIDL arity guard. They replace the `acc()` and
// `meth()` helpers of the deleted `dom/09j-animation.js` one for one.

/// A readonly attribute row: `get <name>`, length 0, the member name as data.
pub(crate) fn row_get(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let d = str_val(scope, m.name);
    let g = build_fn_data(scope, gcb, &format!("get {}", m.name), 0, d);
    crate::install::kits::define_accessor(scope, proto, m, g, None);
}

/// A read/write attribute row: `get <name>` (length 0) and `set <name>` (length 1), each with the
/// member name as data.
pub(crate) fn row_get_set(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
    scb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let gd = str_val(scope, m.name);
    let g = build_fn_data(scope, gcb, &format!("get {}", m.name), 0, gd);
    let sd = str_val(scope, m.name);
    let s = build_fn_data(scope, scb, &format!("set {}", m.name), 1, sd);
    crate::install::kits::define_accessor(scope, proto, m, g, Some(s));
}

/// A method row: the row's WebIDL name and length, the member name as data, wrapped in the WebIDL
/// arity guard when the row declares required arguments (a table-owned member never sees the
/// registry's separate arity pass, so the guard is applied here).
pub(crate) fn row_method(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let d = str_val(scope, m.name);
    let f = build_fn_data(scope, cb, m.name, m.length as i32, d);
    let iface = m.label.split("::").next().unwrap_or(m.name);
    let v: v8::Local<v8::Value> = if m.required > 0 {
        make_arity_guard(scope, iface, m.name, m.required as i32, m.length as i32, f.into(), m.ty).into()
    } else {
        f.into()
    };
    crate::install::kits::define_value(scope, proto, m, v, true);
}

// ── the store ───────────────────────────────────────────────────────────────────────────────────

/// One keyframe after the spec's "process a keyframes argument" has run over it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Keyframe {
    /// The author-specified offset, `None` when it was left to be computed.
    pub offset: Option<f64>,
    pub computed_offset: f64,
    /// The keyframe-level easing, as authored (`getKeyframes()` echoes it back).
    pub easing: String,
    pub composite: String,
    /// `(IDL property name, CSS property name, value)` — the IDL name is what `getKeyframes()`
    /// reports (`backgroundColor`), the CSS name is what `commitStyles()` writes
    /// (`background-color`).
    pub props: Vec<(String, String, String)>,
}

/// A `KeyframeEffect` (or the `AnimationEffect` base it is the only concrete subclass of).
pub(crate) struct EffectRecord {
    pub target: Option<u32>,
    /// The exact element wrapper the page handed us, so `effect.target === el` holds rather than
    /// merely being an element with the same node id.
    pub target_obj: Option<v8::Global<v8::Object>>,
    pub pseudo: Option<String>,
    pub composite: String,
    pub timing: page::animation::Timing,
    pub keyframes: Vec<Keyframe>,
    /// The animation this effect is attached to, if any.
    pub animation: Option<u32>,
}

/// An `Animation`.
pub(crate) struct AnimRecord {
    pub playback: page::animation::Playback,
    pub id: String,
    pub effect: Option<u32>,
    /// False only when the page explicitly assigned `animation.timeline = null`.
    pub has_timeline: bool,
    /// The animation's own JS object, so `getAnimations()` can hand back the very object the page
    /// already holds and the event/promise paths can reach it.
    pub obj: Option<v8::Global<v8::Object>>,
    pub finished: Option<v8::Global<v8::PromiseResolver>>,
    pub finished_settled: bool,
    pub ready: Option<v8::Global<v8::PromiseResolver>>,
    /// Set by `cancel()`; a cancelled animation is idle and out of every `getAnimations()` list.
    pub cancelled: bool,
    /// Script, a `@keyframes` rule, or a transition -- which decides the interface the instance
    /// wears, its place in composite order, and whether `animationName` answers at all.
    pub origin: css::Origin,
    /// What a CSS animation or transition's events last reported, for the next frame: see
    /// [`css::send_events`]. Unused by a script animation.
    pub css_events: css::EventState,
    /// The last value of `animation-play-state` this record was reconciled against.
    ///
    /// The property is a CHANGE, not a state: toggling it to `paused` pauses the animation, and a
    /// tick that merely re-reads an unchanged `running` must not undo a script `pause()` on the same
    /// animation. Remembering what was declared last time is what tells those two apart.
    pub css_paused: bool,
}

/// Which interface a registered prototype belongs to.
pub(crate) const PROTO_ANIMATION: u32 = 0;
pub(crate) const PROTO_KEYFRAME_EFFECT: u32 = 1;
pub(crate) const PROTO_DOCUMENT_TIMELINE: u32 = 2;
pub(crate) const PROTO_PLAYBACK_EVENT: u32 = 3;
/// `CSSAnimation.prototype` / `CSSTransition.prototype`. Both interfaces stay REGISTRY rows (their
/// position in the capture's own-global order is fingerprint surface), so these two slots are filled
/// by the generator's per-member kit rather than by a table's install -- see `css::install_css_animation`.
pub(crate) const PROTO_CSS_ANIMATION: u32 = 4;
pub(crate) const PROTO_CSS_TRANSITION: u32 = 5;
pub(crate) const PROTO_COUNT: usize = 6;

/// Everything the animation model holds in `OpState`.
///
/// Prototypes are REGISTERED from the JS install module rather than looked up through
/// `globalThis.Animation` at call time: a page that overwrites `window.Animation` must not be able
/// to change what `element.animate()` returns.
#[derive(Default)]
pub(crate) struct AnimationStore {
    pub anims: Tracked<std::collections::HashMap<u32, AnimRecord>>,
    pub effects: Tracked<std::collections::HashMap<u32, EffectRecord>>,
    /// The JS object minted for each effect handle, so `animation.effect` hands back the same
    /// `KeyframeEffect` every time rather than a fresh wrapper per read.
    pub effect_objs: std::cell::RefCell<std::collections::HashMap<u32, v8::Global<v8::Object>>>,
    /// Creation order — the composite order `getAnimations()` returns animations in.
    pub order: Tracked<Vec<u32>>,
    pub next: std::cell::Cell<u32>,
    /// The moment this rendering opportunity is sampled at, or `None` until something asks. See
    /// [`timeline_now`].
    pub frame_time: std::cell::Cell<Option<f64>>,
    /// The CSS animation and transition events a style change queued between two frames -- the cancels --
    /// for the next frame to send first ([`css::send_events`]).
    pub css_queue: std::cell::RefCell<Vec<css::QueuedEvent>>,
    /// The CSS events' clock is armed ([`css::wake_after`]).
    pub css_clock: std::cell::Cell<bool>,
    /// When the host last stopped running the page's loop, and the gap the loop running now came in after --
    /// `(stopped, resumed)`, in the timeline's milliseconds -- for a pending CSS animation's start (`css::send_events`).
    pub loop_stopped: std::cell::Cell<Option<f64>>,
    pub loop_gap: std::cell::Cell<Option<(f64, f64)>>,
    /// What the last CSS tick worked its values out from ([`css::tick`]).
    pub tick_memo: std::cell::RefCell<Option<css::TickMemo>>,
    /// The page holds a non-empty set of script-held properties ([`css::transition_blockers`]).
    pub blockers_on: std::cell::Cell<bool>,
    /// The store's [`AnimationStore::writes`] when [`css::transition_blockers`] last looked.
    pub blockers_writes: std::cell::Cell<u64>,
}

impl AnimationStore {
    pub fn alloc(&self) -> u32 {
        let n = self.next.get() + 1;
        self.next.set(n);
        n
    }

    /// How many times the records, the effects or the order were borrowed to be written: an animation whose
    /// state a script or a tick changed moves it.
    pub fn writes(&self) -> u64 {
        self.anims.writes.get().wrapping_add(self.effects.writes.get()).wrapping_add(self.order.writes.get())
    }
}

/// One of the store's tables, counting its writes ([`AnimationStore::writes`]). A `RefCell` whose `borrow_mut` is
/// counted: every place that changes an animation borrows its table to write, so none can change one unseen.
pub(crate) struct Tracked<T> {
    cell: std::cell::RefCell<T>,
    writes: std::cell::Cell<u64>,
}

impl<T: Default> Default for Tracked<T> {
    fn default() -> Self {
        Self { cell: std::cell::RefCell::default(), writes: std::cell::Cell::new(0) }
    }
}

impl<T> Tracked<T> {
    pub fn borrow(&self) -> std::cell::Ref<'_, T> {
        self.cell.borrow()
    }

    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, T> {
        self.writes.set(self.writes.get().wrapping_add(1));
        self.cell.borrow_mut()
    }
}

pub(crate) fn store_rc(scope: &mut v8::PinScope) -> std::rc::Rc<std::cell::RefCell<deno_core::OpState>> {
    deno_core::JsRuntime::op_state_from(scope)
}

/// Run `f` with the store. Every caller clones out what it needs before touching V8 again: a V8 call
/// can re-enter an op, and a live `RefCell` borrow across one is a panic waiting for a page to find.
pub(crate) fn with_store<R>(scope: &mut v8::PinScope, f: impl FnOnce(&AnimationStore) -> R) -> R {
    let rc = store_rc(scope);
    // Create it on demand rather than requiring it. `webapi::lib.rs` puts one in at extension
    // setup, but the registry install runs in harnesses that build a bare `OpState` -- and since
    // `CSSAnimation.prototype` is now registered from a registry row's install (the kit), the first
    // touch of the store moved EARLIER than that `put`. Panicking there would make the store's
    // lifetime a property of install order, which is exactly the kind of thing that breaks when a
    // lane moves one line.
    if rc.borrow().try_borrow::<AnimationStore>().is_none() {
        rc.borrow_mut().put(AnimationStore::default());
    }
    let st = rc.borrow();
    let store = st.borrow::<AnimationStore>();
    f(store)
}

// ── private slots ───────────────────────────────────────────────────────────────────────────────

const ANIM_SLOT: &str = "korvus_animation";
const EFFECT_SLOT: &str = "korvus_animation_effect";
const ORIGIN_SLOT: &str = "korvus_timeline_origin";

fn slot_read(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str) -> Option<f64> {
    let pname = v8::String::new(scope, name)?;
    let key = v8::Private::for_api(scope, Some(pname));
    obj.get_private(scope, key).and_then(|v| if v.is_number() { v.number_value(scope) } else { None })
}
fn slot_write(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, n: f64) {
    if let Some(pname) = v8::String::new(scope, name) {
        let key = v8::Private::for_api(scope, Some(pname));
        let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
        obj.set_private(scope, key, v);
    }
}

/// The animation handle carried by an `Animation` object, or `None` for anything else — which is
/// what makes every member a genuine brand check rather than duck typing.
pub(crate) fn anim_handle(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    slot_read(scope, obj, ANIM_SLOT).map(|n| n as u32)
}
pub(crate) fn set_anim_handle(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, h: u32) {
    slot_write(scope, obj, ANIM_SLOT, h as f64);
}
pub(crate) fn effect_handle(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    slot_read(scope, obj, EFFECT_SLOT).map(|n| n as u32)
}
pub(crate) fn set_effect_handle(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, h: u32) {
    slot_write(scope, obj, EFFECT_SLOT, h as f64);
}
pub(crate) fn timeline_origin(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Option<f64> {
    slot_read(scope, obj, ORIGIN_SLOT)
}
pub(crate) fn set_timeline_origin(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, n: f64) {
    slot_write(scope, obj, ORIGIN_SLOT, n);
}

// ── the clock ───────────────────────────────────────────────────────────────────────────────────

/// The document timeline's current time: the moment THIS RENDERING OPPORTUNITY was sampled at.
///
/// The clock read is `performance.now()`, through the page's own `performance` object. This is
/// deliberately not a second `Instant`. `performance.now()` reads the kernel's one `spec::PerfClock`
/// (a monotonic clock quantized to 100 µs, Chrome's Spectre clamp) -- it was `op_perf_now` in
/// korvus-runtime until that op was deleted; an independent clock here would drift from it,
/// and `document.timeline.currentTime - performance.now()` is a two-line probe.
///
/// # The sample is taken ONCE PER RENDERING OPPORTUNITY, not once per observation
///
/// The timeline is not the wall clock. HTML samples it in "update the rendering", so every
/// observation within one task sees the same moment however long the task runs, and it moves only
/// when a frame does. Measured on stock Chrome 150.0.7871.46, inside a live rendering loop with 300
/// running `width` animations, five reads separated by 3 ms of spin each:
///
/// ```text
///   document.timeline.currentTime   193, 193, 193, 193, 193        constant across the task
///   animation.currentTime           133.4 x5                       constant
///   getComputedStyle(.a).width      "15.1875px" x5                 the SAMPLE does not move
///   performance.now()               209.3 212.3 215.3 218.3 221.3  the wall clock does
///   the timeline across one rAF     193 -> 243                     it moves when a frame does
/// ```
///
/// Reading the clock here instead made every observation its own frame, and the cost of that was
/// not the clock read. `getComputedStyle` runs the "update animations" step ([`css::tick`]); a fresh
/// moment resamples every running animation, which differs from the last sample, which drops the
/// whole style memo, bumps the layout fingerprint and -- on a document with `@container` rules --
/// re-lays the WHOLE document out. Measured on the `bandcamp.com` recording (2,260 nodes, 16 running
/// `width`/`padding`/`height` animations): `getComputedStyle(el).display` cost 20 ms EVERY call and
/// `document.body.innerText` never returned inside the 30 s probe, where Chrome answers both in
/// 0 ms after the first.
///
/// [`advance_frame_time`] is what a rendering opportunity calls; the sample is taken lazily, so a
/// document that never asks about a timeline never reads the clock at all.
pub(crate) fn timeline_now(scope: &mut v8::PinScope) -> Option<f64> {
    if let Some(held) = with_store(scope, |s| s.frame_time.get()) {
        return Some(held);
    }
    // the realm's clock itself, as `performance.now()` reads it -- never the page's `performance` or `now`
    // (`element.animate()` called both)
    let n = crate::core::timing::performance::now_ms(scope);
    let n = n.is_finite().then_some(n)?;
    with_store(scope, |s| s.frame_time.set(Some(n)));
    Some(n)
}

/// A rendering opportunity has arrived: the next observation takes a FRESH sample.
///
/// Three callers, all of them a frame or the host offering one: `Engine::rendering_opportunity`
/// (which `pump` opens between slices of the event loop), `Engine::run` (which offers the TIMELINE's
/// opportunity and not the observers', so it arms no frame task), and the animation-frame runner,
/// which is what a frame IS in an engine with no vsync. It does not read the clock --
/// [`timeline_now`] does that lazily, so advancing costs nothing on a document with no animation
/// and nothing that reads a timeline.
pub fn advance_frame_time(scope: &mut v8::PinScope) {
    with_store(scope, |s| s.frame_time.set(None));
}

// ── object construction ─────────────────────────────────────────────────────────────────────────

/// The interface each `PROTO_*` index names. An instance an engine path mints (`new_on_proto`) takes
/// the CURRENT realm's prototype of that interface, looked up by name -- it used to be read from a
/// runtime-wide registry the installers filled with one realm's prototype objects.
const PROTO_NAMES: [&str; PROTO_COUNT] =
    ["Animation", "KeyframeEffect", "DocumentTimeline", "AnimationPlaybackEvent", "CSSAnimation", "CSSTransition"];

pub(crate) fn registered_proto<'s>(scope: &mut v8::PinScope<'s, '_>, which: u32) -> Option<v8::Local<'s, v8::Object>> {
    crate::binding::protos::shell_proto(scope, PROTO_NAMES.get(which as usize)?)
}

pub(crate) fn new_on_proto<'s>(scope: &mut v8::PinScope<'s, '_>, which: u32) -> Option<v8::Local<'s, v8::Object>> {
    let proto = registered_proto(scope, which)?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    Some(obj)
}

/// The realm slot a realm's `DocumentTimeline` is kept under.
const TIMELINE_SLOT: &str = "korvus_document_timeline";

/// Create-or-return the document's `DocumentTimeline`. Every animation created without an explicit
/// timeline shares this one object, so `a.timeline === b.timeline === document.timeline`.
///
/// ONE PER REALM ([`crate::binding::protos::realm_slot`]): a frame has its own realm, so it
/// has its own timeline on its own `DocumentTimeline.prototype` -- Chrome 150.0.7871.46 answers
/// `frame.contentDocument.timeline instanceof frame.contentWindow.DocumentTimeline` true and `===
/// document.timeline` false (headless, 2026-09-28). It was one per runtime, and a frame's
/// `document.timeline` was the page's once the page had made it.
pub(crate) fn document_timeline<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    use crate::binding::protos::{realm_slot, set_realm_slot};
    if let Some(t) = realm_slot(scope, TIMELINE_SLOT).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(t);
    }
    let obj = new_on_proto(scope, PROTO_DOCUMENT_TIMELINE)?;
    // The document timeline's origin is the time origin itself, so its current time IS
    // `performance.now()` — the same zero both clocks are measured from.
    set_timeline_origin(scope, obj, 0.0);
    set_realm_slot(scope, TIMELINE_SLOT, obj.into());
    Some(obj)
}

// ── shared WebIDL conversions ───────────────────────────────────────────────────────────────────

/// WebIDL `double`: NaN and ±Infinity are a TypeError for a *restricted* double, which every timing
/// member is. Returns `None` when the value is not finite (the caller throws).
pub(crate) fn to_finite(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<f64> {
    let n = v.number_value(scope)?;
    n.is_finite().then_some(n)
}

/// WebIDL `unrestricted double` — NaN and Infinity are legal (`iterations: Infinity`).
pub(crate) fn to_unrestricted(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> f64 {
    v.number_value(scope).unwrap_or(f64::NAN)
}

/// A camelCase IDL property name as its CSS property name: `backgroundColor` -> `background-color`.
/// `cssFloat` and `cssOffset` are the two names CSS-OM renames to dodge reserved words, so they map
/// back rather than hyphenating into `css-float`.
pub(crate) fn css_property_name(idl: &str) -> String {
    match idl {
        "cssFloat" => return "float".to_string(),
        "cssOffset" => return "offset".to_string(),
        _ => {}
    }
    if idl.starts_with("--") {
        return idl.to_string();
    }
    let mut out = String::with_capacity(idl.len() + 4);
    for (i, c) in idl.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Set a string-valued own data property.
pub(crate) fn set_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: &str) {
    if let Some(k) = v8::String::new(scope, key) {
        let s = str_val(scope, v);
        o.create_data_property(scope, k.into(), s);
    }
}
pub(crate) fn set_num(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: f64) {
    if let Some(k) = v8::String::new(scope, key) {
        let n: v8::Local<v8::Value> = v8::Number::new(scope, v).into();
        o.create_data_property(scope, k.into(), n);
    }
}
pub(crate) fn set_num_or_null(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: Option<f64>) {
    match v {
        Some(n) => set_num(scope, o, key, n),
        None => {
            if let Some(k) = v8::String::new(scope, key) {
                let n: v8::Local<v8::Value> = v8::null(scope).into();
                o.create_data_property(scope, k.into(), n);
            }
        }
    }
}

/// The effect end time an animation's playback model is bounded by: its effect's end time, or 0
/// when it has no effect (`new Animation()` — which is why `finish()` on one does not throw).
pub(crate) fn effect_end(store: &AnimationStore, anim: u32) -> f64 {
    let effect = store.anims.borrow().get(&anim).and_then(|a| a.effect);
    match effect {
        Some(e) => store.effects.borrow().get(&e).map(|e| e.timing.end_time()).unwrap_or(0.0),
        None => 0.0,
    }
}
