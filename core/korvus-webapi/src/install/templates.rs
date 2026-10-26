//! The TemplateBuilder, in capture mode.
//!
//! An interface's template is recorded while the FIRST realm of an isolate installs it the ordinary way:
//! the central function builders (`binding::protos::build_fn` / `build_fn_data` / `build_interface_ctor`,
//! `make_arity_guard`, `install::receiver::wrap`) record how each function was made -- callback, data,
//! length, name, and the guard it wraps -- and once `installer::create` has finished the interface, its
//! prototype and interface object are read back IN ORDER and translated: every function into a
//! `FunctionTemplate`, every value into a template property. A later realm of the same isolate (an F19 frame)
//! instantiates the cached template instead of running the installer for that interface.
//!
//! A shape the translator does not understand -- a function nobody recorded, an object value, data that
//! belongs to one realm -- makes it REFUSE: the interface keeps the ordinary path in every realm, and the
//! refusal is counted (`KORVUS_TPL_REPORT`) so the migration can see what is left.
//!
//! The guards keep today's semantics exactly: a guarded member's template is FUSED -- one native callback
//! that runs the guard chain's checks (the SAME check functions the install's wrappers run:
//! `receiver::table_receiver_ok`, `webidl::brand_guard_check`, ...) and then the body on the same call
//! -- so the surface lock cannot tell the paths apart. A chain the fuser cannot take REFUSES:
//! there is no trampoline (a template calling another through JS), deleted after it was
//! measured unused in every realm. Replacing the receiver check with a V8 Signature changes what
//! `Object.create(I.prototype)` answers and would land as a B-row.
//!
//! ONE PATH: every realm takes every interface from its template -- the first realm too, which
//! instantiates the template it just recorded and leaves what it built while recording to the collector
//! ([`wanted`]). The run-time switch back to the ordinary install (`KORVUS_TPL_OLD`/`KORVUS_TPL_MAIN`,
//! the dual-path harness of P2-P5) is gone. `KORVUS_TPL_REPORT=1` prints what was recorded, refused and
//! instantiated.
//!
//! THE BLUEPRINT: the translation's output is plain data -- callbacks, primitives, lengths,
//! names, the shape -- kept for the whole process, and every isolate builds its templates from it
//! ([`build`]); only the first isolate to meet a key records it. A later isolate's first realm that
//! switches an interface instantiates it without running its install at all. The key names the surface
//! by content; a blueprint whose install read the persona is that persona's alone.

use deno_core::v8;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The guard a recorded function applies before its inner function.
pub(crate) enum Guard {
    None,
    Arity { need: usize, iface: String, member: String, ty: crate::install::TypeTag },
    /// `install::receiver::wrap`'s check against `proto` (the translator refuses one aimed anywhere
    /// but the prototype of the interface it is translating)
    Receiver { ord: i64, proto: v8::Global<v8::Value> },
    /// `install::webidl::brand_guard`: the brand, then the WebIDL arity
    Brand { iface: String, member: String, need: usize, is_void: bool },
    /// `install::webidl::brand_accessor`
    BrandAccessor { iface: String },
    /// `install::webidl::branded_promise_method_row`: the brand, refused by rejecting
    BrandPromise { iface: String, member: String },
    /// `install::live::ranges::wrap_brand`
    RangeBrand { idx: u32 },
    /// `core::html::plugins::wrap_recv`
    PluginRecv { iface: String },
    /// `core::html::window::guarded`: the receiver is a Window
    WindowRecv,
}

/// How one function was made.
pub(crate) struct Recipe {
    /// the callback (`None` for a guard: its template fuses the guard's checks over the inner body)
    pub cb: Option<v8::FunctionCallback>,
    pub data: Option<v8::Global<v8::Value>>,
    pub length: i32,
    pub name: String,
    pub ctor: bool,
    pub guard: Guard,
    pub inner: Option<v8::Global<v8::Function>>,
}

/// What a builder DECLARES a function is, when its data belongs to one realm and the template needs
/// another form of it (the owning module translates: `install::stub::stub_parts` / `stub_template_of`).
#[derive(Clone, Copy)]
pub(crate) enum Form {
    /// a receiver-checked stub (`install::stub::build_stub`)
    Stub { setter: bool },
    /// a body whose data is the prototype it brand-checks against (`install::kits::proto_data_fn`,
    /// read back by `install::kits::branded`)
    ProtoData,
}

/// An interface, as a template's data names it: its template, whose instance in the realm a call runs in
/// is that realm's interface object, and the `prototype` key to read off it (made once per isolate: a
/// `v8::String::new` per call hashes and interns the name every time).
#[derive(Clone)]
pub(crate) struct IfaceRef {
    /// the interface's template
    ft: Option<v8::Global<v8::FunctionTemplate>>,
    proto_key: v8::Global<v8::String>,
}

impl IfaceRef {
    fn of(ft: v8::Global<v8::FunctionTemplate>, proto_key: v8::Global<v8::String>) -> IfaceRef {
        IfaceRef { ft: Some(ft), proto_key }
    }

    /// The interface's prototype in the realm the caller runs in: the realm's instance of the template
    /// (V8 keeps one per context) and its `prototype`, which is non-writable and non-configurable, so a
    /// page cannot move it.
    pub(crate) fn proto<'s>(&self, scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
        let ft = self.ft.as_ref()?;
        let f = v8::Local::new(scope, ft).get_function(scope)?;
        let k = v8::Local::new(scope, &self.proto_key);
        f.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    }
}

/// The prototype a [`Form::ProtoData`] function's template-form data names, in the caller's realm.
/// `data` must be such a function's data (an `External` over an [`IfaceRef`] this module made).
pub(crate) fn realm_proto_of<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    // its record's ID (`build_ft`, for the context snapshot)
    if let Some(FnSlot::Proto(p)) = fn_slot(scope, data) {
        // SAFETY: an `IfaceRef` of this isolate's table, owned by its cache
        return unsafe { &*p }.proto(scope);
    }
    let ext = v8::Local::<v8::External>::try_from(data).ok()?;
    // SAFETY: only `build_ft` makes External data for a `Form::ProtoData` function, pointing at an
    // `IfaceRef` owned by the isolate's template cache, which outlives every function made from its templates.
    let r = unsafe { &*(ext.value() as *const IfaceRef) };
    r.proto(scope)
}

struct Capture {
    iface: String,
    /// the realm-wide recording under every interface's ([`begin_global_capture`]): what the install builds
    /// outside any interface, for the global's own template
    catch_all: bool,
    recipes: HashMap<i32, Vec<(v8::Global<v8::Function>, Rc<Recipe>)>>,
    forms: HashMap<i32, Vec<(v8::Global<v8::Function>, Form)>>,
    /// the install read the persona ([`note_persona_read`])
    persona_read: bool,
    /// the process-wide bookkeeping the install's mounts recorded ([`note_mark`])
    marks: Vec<Mark>,
}

thread_local! {
    /// A stack: a create run inside another interface's member install records into its own.
    static CAPTURE: RefCell<Vec<Capture>> = const { RefCell::new(Vec::new()) };
}

/// A snapshot diagnostic: a report line under `KORVUS_TPL_REPORT`, and under `KORVUS_SNAPSHOT_REPORT` too --
/// the switch a RESTORED run can carry (the template reporter observes the install: a reporting run installs).
pub(crate) fn report_snap(line: &str) {
    static SNAP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if reporting() {
        report(line);
    } else if *SNAP.get_or_init(|| std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some()) {
        eprintln!("korvus: snapshot: {line}");
    }
}

/// Whether `KORVUS_TPL_REPORT` is set.
pub(crate) fn reporting() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("KORVUS_TPL_REPORT").is_some())
}

/// Under `KORVUS_TPL_REPORT`: [`report_surface`] again once the page has LOADED (`page-loaded`) and in every
/// built frame and popup realm (`frame-loaded`). A document and what hangs off it are made with the
/// document -- after the install's walk (`page`, `frame`) -- so that walk could not see them: the page's
/// own `document.location` pair was built per document outside any template until a measurement found it.
/// The runtime calls this right after the page's `load` (`navigation.rs`), and in a worker realm
/// once its top-level script has run (`worker-loaded`, `worker.rs`).
pub fn report_loaded_surfaces(scope: &mut v8::PinScope) {
    if !reporting() {
        return;
    }
    if crate::install::surface::realm(scope) != Some(spec::SurfaceRealm::Window) {
        report_surface(scope, "worker-loaded");
        return;
    }
    report_surface(scope, "page-loaded");
    for realm in crate::core::html::browsing_context::frame_realms(scope) {
        let realm = v8::Local::new(scope, &realm);
        let scope = &mut v8::ContextScope::new(scope, realm);
        report_surface(scope, "frame-loaded");
    }
}

/// One line on stderr under `KORVUS_TPL_REPORT`: what was recorded, refused, instantiated, and how long an
/// interface took to build or instantiate (the P3 CHECKPOINT's instantiation-versus-build numbers).
pub(crate) fn report(line: &str) {
    if reporting() {
        // ONE write: `eprintln!` writes its pieces separately, and a test binary's stdout landed between
        // them (96 spliced lines in one `--nocapture` run), which miscounts a log read back by kind.
        // Microseconds since the first report line: the gaps between lines are where a realm spends its
        // install outside the template steps.
        use std::io::Write;
        static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let us = T0.get_or_init(std::time::Instant::now).elapsed().as_micros();
        let _ = std::io::stderr().write_all(format!("[korvus-tpl] @{us} {line}\n").as_bytes());
    }
}

/// Whether this realm takes the template path: every interface of every realm with a surface
/// -- a window's first realm and the frames made from its templates, and each worker's, an
/// isolate of its own whose first realm records -- or builds from what an earlier worker of the process
/// recorded -- and instantiates.
///
/// Why the first realm too: each member is one native call (`FusedRec`, `3b0f174e`), and a `korvus` process
/// with its blueprint file builds the whole surface from it for less CPU than the ordinary install
/// (process task-clock 90.0 ms against 91.0, `f371ff8e`). A process with no file -- the first run of a
/// binary, or a library user such as a test binary -- records and translates first (about 68 ms more for
/// its first isolate); its later isolates build from the process's blueprints.
pub(crate) fn wanted(scope: &mut v8::PinScope) -> bool {
    crate::install::surface::realm(scope).is_some()
}

/// Under `KORVUS_TPL_REPORT`: what one run of a post-install pass changed in this realm, as
/// `PASS <name> r<realm> <changes>`.
pub(crate) fn report_pass(scope: &mut v8::PinScope, name: &str, changes: usize) {
    if reporting() {
        let ord = crate::install::installer::realm_ordinal(scope);
        report(&format!("PASS {name} r{ord} {changes}"));
    }
}

/// Mark the running capture: its install read the persona (`binding::protos::persona_handle`), so what it
/// built may be the persona's rather than the surface's.
pub fn note_persona_read() {
    CAPTURE.with(|c| {
        if let Some(cap) = c.borrow_mut().last_mut() {
            cap.persona_read = true;
        }
    });
}

/// One piece of the process-wide install bookkeeping a member mount records (`behavior_rt::record_layer`,
/// `behavior_rt::record_receiver`, `installer::record_install_flags`). A capture keeps them with the
/// blueprint so that a process which builds from a blueprint read from DISK -- and so runs none of the
/// installs -- still has them (`tools/status`, the receiver sweep and the audit read them).
#[derive(Clone)]
pub(crate) enum Mark {
    Layer { iface: String, member: String, layer: &'static str },
    Receiver { iface: String, member: String, policy: &'static str },
    Flags { realm: &'static str, iface: String, member: String, holder: crate::install::installer::Holder, en: bool, cfg: bool },
}

thread_local! {
    /// Set while [`replay_marks`] runs: a replay is not a mount of whatever capture is open around it.
    static REPLAYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Keep one piece of bookkeeping with the running capture (called by the recorders while [`capturing`],
/// whether or not this realm keeps bookkeeping: a blueprint must not be recorded without it).
pub(crate) fn note_mark(m: Mark) {
    if REPLAYING.with(|r| r.get()) {
        return;
    }
    CAPTURE.with(|c| {
        if let Some(cap) = c.borrow_mut().last_mut() {
            cap.marks.push(m);
        }
    });
}

/// Record a blueprint's bookkeeping as its install would have (the recorders drop it where this realm
/// keeps none).
fn replay_marks(marks: &[Mark]) {
    REPLAYING.with(|r| r.set(true));
    for m in marks {
        match m {
            Mark::Layer { iface, member, layer } => crate::behavior_rt::record_layer(iface, member, layer),
            Mark::Receiver { iface, member, policy } => crate::behavior_rt::record_receiver(iface, member, policy),
            Mark::Flags { realm, iface, member, holder, en, cfg } => {
                crate::install::installer::record_install_flags(realm, iface, member, *holder, *en, *cfg)
            }
        }
    }
    REPLAYING.with(|r| r.set(false));
}

/// Run `f` as no part of the captures open around it: engine work an install's own calls merely set off --
/// V8 fires the long-animation-frame monitor's call callbacks around every API call an install makes -- reads
/// and builds nothing for that install. A persona read there (the monitor's refresh grid) marked the capture,
/// and its blueprint was kept per persona ([`persona_key`]): one more of each per isolate, the runtime suite's
/// memory climbing ~50 MB a test to 35 GB (2026-10-09). A capture `f` opens itself records as ever.
pub(crate) fn outside_capture<R>(f: impl FnOnce() -> R) -> R {
    let Some(open) = CAPTURE.with(|c| c.try_borrow_mut().ok().map(|mut c| std::mem::take(&mut *c))) else { return f() };
    let r = f();
    CAPTURE.with(|c| *c.borrow_mut() = open);
    r
}

/// Whether a capture is running (the central builders record only then).
pub(crate) fn capturing() -> bool {
    CAPTURE.with(|c| !c.borrow().is_empty())
}

/// A function a central builder made OUTSIDE every capture, in a window realm: no template makes it
/// (the measure). Under `KORVUS_TPL_REPORT`, one `UNTEMPLATED r<realm> <builder> <name>` line
/// each, so what a realm still builds the old way is a list rather than a guess. Report-only.
#[track_caller]
pub(crate) fn note_untemplated(scope: &mut v8::PinScope, builder: &str, name: &str, f: v8::Local<v8::Function>) {
    if !reporting() || in_interface_capture() {
        return;
    }
    let at = std::panic::Location::caller();
    let label = format!("{builder} {}:{} [{name}]", at.file(), at.line());
    let h = f.get_identity_hash().get();
    let g = v8::Global::new(scope, f);
    with_cache(scope, |c| c.untemplated.entry(h).or_default().push((g, label.clone())));
    if crate::install::surface::realm(scope) != Some(spec::SurfaceRealm::Window) {
        return;
    }
    let ord = crate::install::installer::realm_ordinal(scope);
    report(&format!("UNTEMPLATED r{ord} {label}"));
}

/// [`note_untemplated`] for a function a refused capture recorded ([`finish_inner`]): no capture check.
fn note_untemplated_as(scope: &mut v8::PinScope, label: &str, name: &str, f: v8::Local<v8::Function>) {
    let label = format!("{label} [{name}]");
    let h = f.get_identity_hash().get();
    let g = v8::Global::new(scope, f);
    with_cache(scope, |c| c.untemplated.entry(h).or_default().push((g, label)));
}

fn untemplated_label(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> Option<String> {
    let h = f.get_identity_hash().get();
    let found: Vec<(v8::Global<v8::Function>, String)> = with_cache(scope, |c| c.untemplated.get(&h).cloned().unwrap_or_default());
    found.into_iter().find(|(g, _)| v8::Local::new(scope, g) == f).map(|(_, l)| l)
}

/// GOAL D1's MEASURE, ON THE SURFACE (widened in P7): every function a page of this realm can
/// reach -- the global's own members under a string OR a symbol key, each interface object's statics and its
/// prototype's members, a namespace's members (its own namespaces' too), a plain holder's members, and the
/// own members of every window singleton already in its slot -- that no template made. Three ways to be one:
///   - a central builder made it outside a template ([`note_untemplated`]);
///   - it is JavaScript (it has a script) -- no template makes one;
///   - it is native but no template of this builder made it (no [`TPL_MARK`]), and it is not V8's own: V8's
///     are the functions at a path a bare context has (`Array.prototype.map`), and those same functions
///     wherever else they hang (`NodeList.prototype.values` IS `Array.prototype.values`).
///
/// Until P7 only the first kind was counted, so a function made any other way -- `Function::builder`
/// directly, a script -- could not show up however reachable it was (a worker's `Symbol.for('korvus.bridge')`
/// holder, found by hand, and the global's symbol keys were not walked at all). A function built the
/// ordinary way and then replaced by its template's (the first realm's switch) is garbage and does not count.
/// Under `KORVUS_TPL_REPORT`, one `SURFACE untemplated <path> (<why>)` line each and a
/// `SURFACE-SUMMARY <label> untemplated N` line (`tools/templates/untemplated.py` ratchets the counts).
/// Walks without calling a getter.
pub(crate) fn report_surface(scope: &mut v8::PinScope, label: &str) {
    if !reporting() {
        return;
    }
    let global = scope.get_current_context().global(scope);
    let fns = collect_surface(scope, global, true);
    // V8's own: what a context with nothing installed has, walked the same way
    let bare: std::collections::HashSet<String> = {
        let ctx = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, ctx);
        let g = ctx.global(scope);
        collect_surface(scope, g, false).into_iter().map(|(p, _)| p).collect()
    };
    let mut v8_own: HashMap<i32, Vec<v8::Local<v8::Function>>> = HashMap::new();
    for (p, f) in &fns {
        if bare.contains(p) {
            v8_own.entry(f.get_identity_hash().get()).or_default().push(*f);
        }
    }
    let mut hits: Vec<(String, String)> = Vec::new();
    let mut seen: HashMap<i32, Vec<v8::Local<v8::Function>>> = HashMap::new();
    for (path, f) in &fns {
        let h = f.get_identity_hash().get();
        let once = seen.entry(h).or_default();
        if once.contains(f) {
            continue;
        }
        once.push(*f);
        let why = if let Some(l) = untemplated_label(scope, *f) {
            Some(l)
        } else if f.script_id() != 0 {
            let origin = f.get_script_origin(scope).resource_name().map(|n| n.to_rust_string_lossy(scope)).unwrap_or_default();
            Some(format!("JavaScript, script {} {origin:?}", f.script_id()))
        } else if !has_template_mark(scope, *f) && !bare.contains(path) && !v8_own.get(&h).is_some_and(|v| v.contains(f)) {
            Some("native, made by no template".to_string())
        } else {
            None
        };
        if let Some(why) = why {
            hits.push((path.clone(), why));
        }
    }
    for (path, l) in &hits {
        report(&format!("SURFACE untemplated {path} ({l})"));
    }
    report(&format!("SURFACE-SUMMARY {label} untemplated {}", hits.len()));
}

/// Under `KORVUS_TPL_REPORT` every function a template of this builder makes carries this private
/// ([`mark_template`]), so [`report_surface`] can tell it from a native function made any other way. Nothing
/// else reads it, and nothing sets it when not reporting.
const TPL_MARK: &str = "korvus.tpl";

fn mark_template(scope: &mut v8::PinScope, t: &v8::Template) {
    if reporting() {
        let k = private_key(scope, TPL_MARK);
        let yes = v8::Boolean::new(scope, true);
        set_private_on(t, k, yes.into());
    }
}

fn has_template_mark(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> bool {
    let k = private_key(scope, TPL_MARK);
    let o: v8::Local<v8::Object> = f.into();
    o.get_private(scope, k).is_some_and(|v| v.is_true())
}

/// Every function [`report_surface`] walks to from `global`, with its path, in walk order (a function at two
/// paths is listed at both). `singletons`: the realm's window singletons already minted, too.
fn collect_surface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, singletons: bool) -> Vec<(String, v8::Local<'s, v8::Function>)> {
    let mut out = Vec::new();
    // V8's own keys too: the install adds members to an intrinsic namespace (`WebAssembly.compileStreaming`),
    // and V8's functions were never noted
    let any_lazy = has_lazy_globals(scope);
    for k in own_keys(scope, global) {
        let Ok(key) = v8::Local::<v8::Name>::try_from(k) else { continue };
        let name = key_label(scope, k);
        // a page interface put lazily and not made yet: reading it would make it -- the report then
        // counts what the install made, not what it asked
        if any_lazy && lazy_global(scope, global, &name).is_some() {
            continue;
        }
        surface_member(scope, global, key, &name, &mut out, 0);
        if !singletons || !k.is_string() {
            continue;
        }
        // a window singleton already minted: its own members (a Location's unforgeable ones, a document's `location`)
        if let Some(o) = crate::core::html::window_globals::window_slot_get(scope, &name).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            for mk in own_keys(scope, o) {
                let Ok(mkey) = v8::Local::<v8::Name>::try_from(mk) else { continue };
                let mn = format!("{name}.{}", key_label(scope, mk));
                surface_member(scope, o, mkey, &mn, &mut out, 2);
            }
        }
    }
    out
}

/// One member of `holder` for [`report_surface`]: its function(s), and -- depth permitting -- an interface
/// object's statics and prototype members, or a namespace's members.
fn desc_field<'s>(scope: &mut v8::PinScope<'s, '_>, d: v8::Local<'s, v8::Object>, n: &str) -> Option<v8::Local<'s, v8::Value>> {
    let nk = v8::String::new(scope, n)?;
    d.get(scope, nk.into()).filter(|v| !v.is_undefined())
}

fn surface_member<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
    path: &str,
    out: &mut Vec<(String, v8::Local<'s, v8::Function>)>,
    depth: u32,
) {
    let Some(d) = holder.get_own_property_descriptor(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let mut nested: Option<(v8::Local<v8::Object>, bool)> = None;
    match desc_field(scope, d, "value") {
        Some(v) => {
            if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
                out.push((path.to_string(), f));
                nested = Some((f.into(), true));
            } else if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
                if !v.is_array() {
                    nested = Some((o, false));
                }
            }
        }
        None => {
            for part in ["get", "set"] {
                if let Some(f) = desc_field(scope, d, part).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
                    out.push((format!("{part} {path}"), f));
                }
            }
        }
    }
    // the global again (`globalThis`, a frame's `self`): walked once, as the global
    let global = scope.get_current_context().global(scope);
    let nested = nested.filter(|(o, _)| *o != global);
    if depth >= 2 {
        // a namespace's own namespace (`chrome.app`, `chrome.app.InstallState`): its members too, a few levels
        // down -- the recording takes every level ([`bp_global_entry`]); a holder with no prototype as well
        if let Some((o, false)) = nested {
            let op = v8::Object::new(scope).get_prototype(scope);
            let plain = o.get_prototype(scope).is_none_or(|p| Some(p) == op || p.is_null());
            if depth < 8 && plain {
                for mk in own_keys(scope, o) {
                    let Ok(mkey) = v8::Local::<v8::Name>::try_from(mk) else { continue };
                    let n = key_label(scope, mk);
                    surface_member(scope, o, mkey, &format!("{path}.{n}"), out, depth + 1);
                }
            }
        }
        return;
    }
    let Some((o, is_fn)) = nested else { return };
    let mut holders = vec![(o, path.to_string())];
    if is_fn {
        let pk = v8::String::new(scope, "prototype").unwrap();
        let has_proto = o.get_own_property_descriptor(scope, pk.into()).is_some_and(|v| v.is_object());
        if !has_proto && depth == 0 {
            // a plain function (not an interface object): nothing of its own to walk
            holders.clear();
        } else if has_proto {
            if let Some(p) = o.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
                holders.push((p, format!("{path}.prototype")));
            }
        }
    }
    for (h, hp) in holders {
        for mk in own_keys(scope, h) {
            let Ok(mkey) = v8::Local::<v8::Name>::try_from(mk) else { continue };
            let n = key_label(scope, mk);
            if n == "prototype" || n == "constructor" {
                continue;
            }
            surface_member(scope, h, mkey, &format!("{hp}.{n}"), out, 2);
        }
    }
}

/// A FUNCTION NO INTERFACE'S TEMPLATE MAKES, MADE FROM A TEMPLATE: a `Location`'s
/// unforgeable member, a `[LegacyUnforgeable]` window accessor, a legacy factory -- what a realm's install
/// defines on an object of its own, where no interface template attaches it. ONE `FunctionTemplate` per
/// isolate for each callback, name, length, data, brand and constructor behaviour; the function is its
/// instance in the calling realm, of which V8 keeps one per context -- so a realm asking twice gets the
/// same function, and a caller that needs two distinct ones in one realm cannot use this. `data` is the
/// same in every realm (a string, or nothing: data that belongs to one realm has no template); `engine` is
/// the `korvus.engineFn` brand `binding::protos::build_fn` stamps (`build_kernel_fn`'s functions carry
/// none); `ctor` is `ConstructorBehavior::Allow` (the function gets a `prototype` of its own, which a
/// legacy factory redefines). Recorded like a central builder's function while a capture runs, so the
/// realm-wide recording translates it as it translated the function the builder made.
pub(crate) fn standalone_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: v8::FunctionCallback,
    name: &str,
    length: i32,
    data: Option<&str>,
    engine: bool,
    ctor: bool,
) -> v8::Local<'s, v8::Function> {
    standalone_fn_cached(scope, cb, name, length, data, engine, ctor, None)
}

extern "C" {
    fn korvus_function_template_with_cache(
        isolate: *mut std::ffi::c_void,
        callback: v8::FunctionCallback,
        cache: *const v8::Private,
        data: *const v8::Value,
        length: i32,
    ) -> *const v8::FunctionTemplate;
}

/// A non-constructor template on `cb` whose IC loads may read private `cache` off the holder in place of calling it
/// (`FunctionTemplate::NewWithCache`, through `shim/cached_accessor.cc`).
#[cfg(not(feature = "analysis"))]
fn cached_getter_template<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: v8::FunctionCallback,
    cache: v8::Local<'s, v8::Private>,
    data: Option<v8::Local<'s, v8::Value>>,
    length: i32,
) -> v8::Local<'s, v8::FunctionTemplate> {
    let isolate: &mut v8::Isolate = scope;
    // SAFETY: `UnsafeRawIsolatePtr` is `repr(transparent)` over the C++ `v8::Isolate*` (as `incumbent_realm`); the
    // handles are this scope's, and the answer is a `Local` in it
    unsafe {
        let raw: *mut std::ffi::c_void = std::mem::transmute(isolate.as_raw_isolate_ptr());
        let p = korvus_function_template_with_cache(
            raw,
            crate::binding::refs::c(cb),
            &*cache as *const v8::Private,
            data.map_or(std::ptr::null(), |d| &*d as *const v8::Value),
            length,
        );
        std::mem::transmute::<*const v8::FunctionTemplate, v8::Local<'s, v8::FunctionTemplate>>(p)
    }
}

/// [`standalone_fn`] for a getter whose value lives in private `cache` on its holder (Blink's `[CachedAccessor]`,
/// `shim/cached_accessor.cc`): an inline cache that finds the getter on the receiver and `cache` as a data property of
/// the holder loads `cache` in place of calling it. So the getter must answer exactly that private's value whenever the
/// holder has it, and a holder without it is answered by the getter as before. Only a template made for one key ever
/// carries the cache (the template cache is keyed without it). The analysis build traces every call, so it caches
/// nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn standalone_fn_cached<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: v8::FunctionCallback,
    name: &str,
    length: i32,
    data: Option<&str>,
    engine: bool,
    ctor: bool,
    cache: Option<v8::Local<'s, v8::Private>>,
) -> v8::Local<'s, v8::Function> {
    let t = standalone_template(scope, cb, name, length, data, engine, ctor, cache);
    let f = t.get_function(scope).unwrap();
    if capturing() {
        let data = data.map(|d| {
            let v: v8::Local<v8::Value> = v8::String::new(scope, d).unwrap().into();
            v8::Global::new(scope, v)
        });
        record(scope, f, Recipe { cb: Some(cb), data, length, name: name.to_string(), ctor, guard: Guard::None, inner: None });
    }
    note_known(scope, f, "standalone");
    f
}

/// The isolate's template behind [`standalone_fn_cached`] -- made the first time it is asked for -- for a template
/// that hands it to V8 itself: an accessor an instance template carries (`event::declare_is_trusted`), whose function
/// in each realm is then the one [`standalone_fn`] answers for the same callback, name and length there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn standalone_template<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: v8::FunctionCallback,
    name: &str,
    length: i32,
    data: Option<&str>,
    engine: bool,
    ctor: bool,
    cache: Option<v8::Local<'s, v8::Private>>,
) -> v8::Local<'s, v8::FunctionTemplate> {
    let key: StandaloneKey = (cb as usize, name.to_string(), length, data.map(str::to_string), engine, ctor);
    let have = with_cache(scope, |c| c.standalone.get(&key).cloned());
    let t = match have {
        Some(t) => v8::Local::new(scope, &t),
        None => {
            let behavior = if ctor { v8::ConstructorBehavior::Allow } else { v8::ConstructorBehavior::Throw };
            #[cfg(not(feature = "analysis"))]
            let cached = cache.filter(|_| !ctor).map(|cache| {
                let d: Option<v8::Local<v8::Value>> = data.map(|d| v8::String::new(scope, d).unwrap().into());
                cached_getter_template(scope, cb, cache, d, length)
            });
            #[cfg(feature = "analysis")]
            let _ = cache;
            // the release statements exactly as they were (A3: a block around them moved release codegen)
            #[cfg(not(feature = "analysis"))]
            let mut b = v8::FunctionTemplate::builder_raw(crate::binding::refs::c(cb)).length(length).constructor_behavior(behavior);
            #[cfg(not(feature = "analysis"))]
            if let Some(d) = data {
                let d = v8::String::new(scope, d).unwrap();
                b = b.data(d.into());
            }
            #[cfg(not(feature = "analysis"))]
            let t = match cached {
                Some(t) => t,
                None => b.build(scope),
            };
            #[cfg(feature = "analysis")]
            let t = {
                let d = data.map(|d| -> v8::Local<v8::Value> { v8::String::new(scope, d).unwrap().into() });
                let (cb2, d2) = analysis_untemplated(scope, cb, d, name);
                v8::FunctionTemplate::builder_raw(crate::binding::refs::c(cb2)).data(d2).length(length).constructor_behavior(behavior).build(scope)
            };
            let n = v8::String::new(scope, name).unwrap();
            t.set_class_name(n);
            if engine {
                let names = names(scope);
                let k = v8::Local::new(scope, &names.brands[0]);
                let yes = v8::Boolean::new(scope, true);
                set_private_on(&t, k, yes.into());
            }
            mark_template(scope, &t);
            let g = v8::Global::new(scope, t);
            with_cache(scope, |c| c.standalone.insert(key, g));
            t
        }
    };
    t
}

/// Record that `g` is a guard over `inner` (called by a guard builder while [`capturing`], after the
/// recording builder made `g` over the guard's own callback and data).
pub(crate) fn record_guard(scope: &mut v8::PinScope, g: v8::Local<v8::Function>, name: &str, length: i32, guard: Guard, inner: v8::Local<v8::Function>) {
    let inner = Some(v8::Global::new(scope, inner));
    record(scope, g, Recipe { cb: None, data: None, length, name: name.to_string(), ctor: false, guard, inner });
}

/// Record how `f` was made (called by the central builders while [`capturing`]).
pub(crate) fn record(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, r: Recipe) {
    let h = f.get_identity_hash().get();
    let g = v8::Global::new(scope, f);
    CAPTURE.with(|c| {
        if let Some(cap) = c.borrow_mut().last_mut() {
            cap.recipes.entry(h).or_default().push((g, Rc::new(r)));
        }
    });
}

/// Drop `f`'s recording: the install REPLACED it before the capture ended, so neither object holds it and no
/// template replays it. Left recorded it is an orphan, and [`translate`] refuses the whole interface -- what the
/// registry's arity pass did to three, rebuilding a receiver guard round an arity guard (`installer::guard_member`):
/// `StylePropertyMap`, `StorageEvent`, `CSSNumericValue` went the ordinary way in every
/// later realm, 24 page-visible functions outside a template.
pub(crate) fn forget(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) {
    let h = f.get_identity_hash().get();
    CAPTURE.with(|c| {
        let mut c = c.borrow_mut();
        let Some(cap) = c.last_mut() else { return };
        if let Some(list) = cap.recipes.get_mut(&h) {
            list.retain(|(g, _)| v8::Local::new(scope, g) != f);
            if list.is_empty() {
                cap.recipes.remove(&h);
            }
        }
        if let Some(list) = cap.forms.get_mut(&h) {
            list.retain(|(g, _)| v8::Local::new(scope, g) != f);
            if list.is_empty() {
                cap.forms.remove(&h);
            }
        }
    });
}

/// Declare what `f` is (called by a builder while [`capturing`], after the recording builder made it).
pub(crate) fn declare(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, form: Form) {
    let h = f.get_identity_hash().get();
    let g = v8::Global::new(scope, f);
    CAPTURE.with(|c| {
        if let Some(cap) = c.borrow_mut().last_mut() {
            cap.forms.entry(h).or_default().push((g, form));
        }
    });
}

fn form_of(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> Option<Form> {
    let h = f.get_identity_hash().get();
    let found: Vec<(v8::Global<v8::Function>, Form)> =
        CAPTURE.with(|c| c.borrow().last().and_then(|cap| cap.forms.get(&h).cloned()).unwrap_or_default());
    found.into_iter().rev().find(|(g, _)| v8::Local::new(scope, g) == f).map(|(_, x)| x)
}

/// How `f` was made. A function recorded twice keeps the LATER recipe: `make_arity_guard` and
/// `receiver::wrap` build their wrapper with `build_fn_data` (recorded as a plain function over the
/// wrapper's own callback and data) and then record what the wrapper IS.
fn recipe_of(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> Option<Rc<Recipe>> {
    let h = f.get_identity_hash().get();
    let found: Vec<(v8::Global<v8::Function>, Rc<Recipe>)> =
        CAPTURE.with(|c| c.borrow().last().and_then(|cap| cap.recipes.get(&h).cloned()).unwrap_or_default());
    found.into_iter().rev().find(|(g, _)| v8::Local::new(scope, g) == f).map(|(_, r)| r)
}

/// Every function the running capture recorded.
fn recorded(scope: &mut v8::PinScope) -> Vec<(v8::Global<v8::Function>, String)> {
    let _ = scope;
    CAPTURE.with(|c| {
        c.borrow()
            .last()
            .map(|cap| cap.recipes.values().flatten().map(|(g, r)| (g.clone(), r.name.clone())).collect())
            .unwrap_or_default()
    })
}

// ── the cache: one template per interface per isolate (in the runtime's OpState) ────────────────────

/// Property attributes as flags (`v8::PropertyAttribute` is not `Copy`).
#[derive(Clone, Copy)]
struct Attrs {
    read_only: bool,
    dont_enum: bool,
    dont_delete: bool,
}

impl Attrs {
    fn v8(self) -> v8::PropertyAttribute {
        let mut a = v8::PropertyAttribute::NONE;
        if self.read_only {
            a = a | v8::PropertyAttribute::READ_ONLY;
        }
        if self.dont_enum {
            a = a | v8::PropertyAttribute::DONT_ENUM;
        }
        if self.dont_delete {
            a = a | v8::PropertyAttribute::DONT_DELETE;
        }
        a
    }
}

/// One of `Array.prototype`'s iteration functions, which list interfaces carry as their own members
/// (WebIDL's value iterators) -- the realm's own function object, as V8's intrinsic data property gives it.
#[derive(Clone, Copy)]
enum Intr {
    Entries,
    ForEach,
    Keys,
    Values,
}

impl Intr {
    const ALL: [(Intr, &'static str); 4] = [(Intr::Entries, "entries"), (Intr::ForEach, "forEach"), (Intr::Keys, "keys"), (Intr::Values, "values")];
    fn v8(self) -> v8::Intrinsic {
        match self {
            Intr::Entries => v8::Intrinsic::ArrayProtoEntries,
            Intr::ForEach => v8::Intrinsic::ArrayProtoForEach,
            Intr::Keys => v8::Intrinsic::ArrayProtoKeys,
            Intr::Values => v8::Intrinsic::ArrayProtoValues,
        }
    }
    fn name(self) -> &'static str {
        Intr::ALL.iter().find(|(i, _)| *i as u8 == self as u8).map(|(_, n)| *n).unwrap_or("values")
    }
    /// Which of them `f` is (`fns`: this realm's, in [`Intr::ALL`] order).
    fn of(scope: &mut v8::PinScope, fns: &[Option<v8::Global<v8::Function>>], f: v8::Local<v8::Function>) -> Option<Intr> {
        for ((i, _), g) in Intr::ALL.iter().zip(fns) {
            if g.as_ref().is_some_and(|g| v8::Local::new(scope, g) == f) {
                return Some(*i);
            }
        }
        None
    }
}

/// `Array.prototype[name]` in the realm the caller runs in.
fn array_proto_fn<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Function>> {
    let ctx = scope.get_current_context();
    let global = ctx.global(scope);
    let a = v8::String::new(scope, "Array")?;
    let arr = global.get(scope, a.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let p = v8::String::new(scope, "prototype")?;
    let proto = arr.get(scope, p.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let k = v8::String::new(scope, name)?;
    proto.get(scope, k.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
}

enum Entry {
    Value(v8::Global<v8::Value>, Attrs),
    Func(v8::Global<v8::FunctionTemplate>, Attrs),
    Intrinsic(Intr, Attrs),
    Accessor(Option<v8::Global<v8::FunctionTemplate>>, Option<v8::Global<v8::FunctionTemplate>>, Attrs),
    Namespace(Vec<(v8::Global<v8::Name>, Entry)>, Attrs),
    Unscopables(Vec<String>, Attrs),
}

/// The template a property goes on.
enum On<'a, 's> {
    Proto(&'a v8::Local<'s, v8::ObjectTemplate>),
    Iface(&'a v8::Local<'s, v8::FunctionTemplate>),
}

pub(crate) struct Tpl {
    ctor: v8::Global<v8::FunctionTemplate>,
    /// string-keyed members attached per realm, each from its own template: on the prototype, those
    /// after `constructor` (V8 adds it after a prototype template's own properties); on either object,
    /// those from the first function a key would misname onwards (see `misnamed_by_key`)
    late: Late,
    /// what the installer did besides building the two objects, replayed in each realm
    pub(crate) replay: crate::install::installer::Replay,
    /// false for an interface object with no `prototype` (`NodeFilter`)
    constructible: bool,
    /// a `[Global]` interface's members on the realm's global object ([`attach_global`])
    global: Vec<(v8::Global<v8::Name>, Entry)>,
    /// the interface this is the template of (the report's label)
    iface: String,
    /// the interface its prototype chains to, if any (what a realm made from templates splices)
    parent: Option<String>,
    /// [`Tpl::global`]'s entries by key ([`attach_one`])
    global_index: HashMap<String, usize>,
}

/// FxHash, rustc's own: one rotate-xor-multiply per word. The template maps below are keyed by the
/// engine's own strings -- template keys, interface names -- never by anything a page chose, so SipHash's
/// resistance to chosen-key flooding buys nothing there, and hashing those long keys with it was ~6 % of a
/// minimal page's whole run (`Sip13Rounds::write` 4.7 % + `hash_one<&String>` 1.4 %, measured 2026-10-12).
/// Deterministic, so nothing written to disk depends on it either way.
#[derive(Default, Clone, Copy)]
pub(crate) struct FxHasher(u64);

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().expect("an 8-byte chunk")));
        }
        for &b in chunks.remainder() {
            self.add(u64::from(b));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// A map keyed by the engine's own strings or ids ([`FxHasher`]).
pub(crate) type FxMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;

#[derive(Default)]
struct Cache {
    tpls: FxMap<String, Rc<Tpl>>,
    /// A snapshot's templates not decoded yet -- each one's record in `snap_bytes`, by key -- and each
    /// exposed name's key; decoded on first lookup ([`cached`], [`by_name_tpl`])
    #[cfg(not(feature = "analysis"))]
    pending: FxMap<String, std::ops::Range<usize>>,
    #[cfg(not(feature = "analysis"))]
    pending_names: FxMap<String, String>,
    #[cfg(not(feature = "analysis"))]
    snap_bytes: Option<Rc<[u8]>>,
    /// the interface's exposed name -> its template (the last one recorded under that name)
    by_name: FxMap<String, Rc<Tpl>>,
    /// the records template data points at (fused members, stub forms), alive as long as the
    /// templates that point at them -- the isolate's
    keep: Vec<Box<dyn std::any::Any>>,
    names: Option<Rc<Names>>,
    counts: TemplateCounts,
    /// what this isolate's templates made, by identity, under `KORVUS_TPL_REPORT` ([`note_known`])
    known: HashMap<i32, Vec<(v8::Global<v8::Function>, String)>>,
    /// under `KORVUS_TPL_REPORT`: every function a central builder made outside a template, with where
    /// ([`note_untemplated`]); [`report_surface`] counts those a page can still reach
    untemplated: HashMap<i32, Vec<(v8::Global<v8::Function>, String)>>,
    /// each interface the page realm instantiated, as its template made it: its two holders (the prototype,
    /// the interface object) and their own descriptors in order ([`record_deltas`]); emptied at `persist`
    snaps: Vec<(String, [(v8::Global<v8::Object>, Vec<(v8::Global<v8::Value>, v8::Global<v8::Object>)>); 2])>,
    /// the page realm's install is over (`persist` ran): no more snapshots
    persisted: bool,
    /// what the page's install changed on an interface after instantiating it, built for this isolate
    /// from the process's record on first use ([`delta_entries`]; applied by [`realm_interface`])
    deltas: HashMap<String, Option<Rc<Vec<(u8, Vec<(v8::Global<v8::Name>, DeltaE)>)>>>>,
    /// the page realm's surface identity ([`realm_identity`]), for the records realms of this isolate read
    rid: Option<String>,
    /// whether this isolate's page realm records what its install changes ([`recording_deltas`])
    delta_mode: Option<bool>,
    /// an interface's template key in this isolate's page realm ([`note_key`]): where a realm built from
    /// templates finds the blueprint of one the page built the ordinary way
    keys_by_name: FxMap<String, String>,
    /// [`realm_identity`]'s answer for each surface this isolate's realms install against, by the surface's
    /// pinned address: it is asked once per interface and never changes for a surface.
    identities: FxMap<usize, String>,
    /// the templates of the functions no interface's template makes ([`standalone_fn`]), by what makes one
    standalone: HashMap<StandaloneKey, v8::Global<v8::FunctionTemplate>>,
    /// the templates of the objects no interface's template makes ([`object_template`]), by name
    objects: HashMap<&'static str, v8::Global<v8::ObjectTemplate>>,
}

/// The template of an object no interface's template makes but a page can see -- the named properties
/// object in `Window`'s chain (`WindowProperties`) -- made once per isolate, as every interface's is, and
/// given what its instances carry by `configure`. Blink's bindings cache the same
/// object's template per isolate.
pub(crate) fn object_template<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &'static str,
    configure: fn(v8::Local<v8::ObjectTemplate>),
) -> v8::Local<'s, v8::ObjectTemplate> {
    if let Some(g) = with_cache(scope, |c| c.objects.get(name).cloned()) {
        return v8::Local::new(scope, &g);
    }
    let t = v8::ObjectTemplate::new(scope);
    configure(t);
    let g = v8::Global::new(scope, t);
    with_cache(scope, |c| c.objects.insert(name, g));
    t
}

/// What makes a [`standalone_fn`]'s template: its callback, name, length, data, whether it carries the
/// engine brand, and whether it is a constructor.
type StandaloneKey = (usize, String, i32, Option<String>, bool, bool);

/// One interface's recorded changes ([`record_deltas`]), kept by the process as the blueprint
/// `#delta:<iface>|<surface>` -- and so in the blueprint file: recorded ONCE per surface, as its templates are.
#[derive(Clone)]
struct DeltaSet {
    holders: Vec<(u8, Vec<(KeyBp, DeltaBp)>)>,
}

/// One own property of a holder the page's install changed after instantiating the interface, in the
/// holder's final order ([`record_deltas`]).
#[derive(Clone)]
enum DeltaBp {
    /// the template's own descriptor (at this position)
    Keep,
    /// a descriptor the install put there: data or accessor, its three parts (value, get, set) and flags
    Changed { data: bool, parts: [PartBp; 3], attrs: Attrs },
}

/// A slot of an interface its template made in the page realm: the interface, the holder (0 its
/// prototype, 1 its interface object), the key, and the part (0 value, 1 get, 2 set).
#[derive(Clone)]
struct SlotBp {
    iface: String,
    holder: u8,
    key: KeyBp,
    part: u8,
}

/// Where one part of a changed descriptor comes from, in a realm built from templates.
#[derive(Clone)]
enum PartBp {
    /// nothing (`undefined`)
    Absent,
    /// the template's own part at this key
    Same,
    Prim(PrimBp),
    /// a function the realm-wide recording made
    Fn(usize),
    /// the function that slot holds (a borrow: `SVGAElement.download` IS `HTMLAnchorElement`'s)
    Ref(SlotBp),
    /// this interface's own interface object (a prototype's `constructor`)
    Iface,
}

/// [`DeltaBp`] built for this isolate.
enum DeltaE {
    Keep,
    Changed { data: bool, parts: [PartE; 3], attrs: Attrs },
}

/// [`PartBp`] built for this isolate (a slot's key as a name).
enum PartE {
    Absent,
    Same,
    Prim(v8::Global<v8::Value>),
    Fn(v8::Global<v8::FunctionTemplate>),
    Ref(String, u8, v8::Global<v8::Name>, u8),
    Iface,
}

/// What one isolate's template cache did (`Engine::template_counts`; Rust only, a page cannot read it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TemplateCounts {
    /// keys this isolate recorded (it ran the install and translated it into a blueprint)
    pub recorded: usize,
    /// keys this isolate built from a blueprint ANOTHER isolate recorded, without running the install
    pub from_blueprint: usize,
    /// interface objects and prototypes its realms took from templates
    pub instantiated: usize,
    /// page interfaces put LAZILY: a template this isolate has, or a blueprint it would have built from,
    /// left to the interface's first read -- which builds it then, or never
    pub lazy: usize,
}

/// This isolate's [`TemplateCounts`].
pub fn template_counts(st: &deno_core::OpState) -> TemplateCounts {
    st.try_borrow::<Cache>().map(|c| c.counts).unwrap_or_default()
}

/// Every member row `install::stub::stub_data` made an External over while a capture ran: the one kind of
/// External a recorded function's data may be. An isolate's own template records (an `IfaceRef`,
/// `StubTpl`, `FusedRec`) are never in it, so a blueprint never points at one.
fn member_exts() -> &'static std::sync::Mutex<std::collections::HashSet<usize>> {
    static M: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<usize>>> = std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

/// Note a member row's External (called by `install::stub::stub_data` while [`capturing`]).
pub(crate) fn note_member_external(m: &'static crate::install::MemberDef) {
    if let Ok(mut s) = member_exts().lock() {
        s.insert(m as *const crate::install::MemberDef as usize);
    }
}

/// The member row at `addr`, when `stub_data` made an External over it.
fn member_of(addr: usize) -> Option<&'static crate::install::MemberDef> {
    let known = member_exts().lock().ok()?.contains(&addr);
    // SAFETY: `addr` was taken from a `&'static MemberDef` by `note_member_external`.
    known.then(|| unsafe { &*(addr as *const crate::install::MemberDef) })
}

fn with_cache<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut Cache) -> R) -> R {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let mut st = st.borrow_mut();
    if !st.has::<Cache>() {
        st.put(Cache::default());
    }
    f(st.borrow_mut::<Cache>())
}

/// The cached template for `key`, if one was recorded.
pub(crate) fn cached(scope: &mut v8::PinScope, key: &str) -> Option<Rc<Tpl>> {
    if let Some(t) = with_cache(scope, |c| c.tpls.get(key).cloned()) {
        return Some(t);
    }
    // A template a snapshot restored, decoded on its first lookup
    #[cfg(not(feature = "analysis"))]
    if let Some(t) = decode_pending(scope, key) {
        return Some(t);
    }
    None
}

/// The template a snapshot carries under `key`, decoded now from its record with the store the restore
/// handed over ([`keep_snapshot_store`]); `None` when no record is pending under `key`.
#[cfg(not(feature = "analysis"))]
fn decode_pending(scope: &mut v8::PinScope, key: &str) -> Option<Rc<Tpl>> {
    let (range, bytes) = with_cache(scope, |c| Some((c.pending.remove(key)?, c.snap_bytes.clone()?)))?;
    let mut store = scope.remove_slot::<SnapStore>()?;
    let t = disk::snap_tpl(scope, &bytes[range], &mut store.0);
    scope.set_slot(store);
    let t = t?;
    with_cache(scope, |c| c.tpls.insert(key.to_string(), t.clone()));
    report_snap(&format!("SNAP-TPL decoded {key}"));
    Some(t)
}

/// The template [`Cache::by_name`] holds for `name` -- a pending snapshot template decoded first, never
/// one built again beside it (the realm's objects came from the snapshot's).
fn by_name_tpl(scope: &mut v8::PinScope, name: &str) -> Option<Rc<Tpl>> {
    if let Some(t) = with_cache(scope, |c| c.by_name.get(name).cloned()) {
        return Some(t);
    }
    #[cfg(not(feature = "analysis"))]
    if let Some(key) = with_cache(scope, |c| c.pending_names.remove(name)) {
        let t = cached(scope, &key)?;
        with_cache(scope, |c| c.by_name.insert(name.to_string(), t.clone()));
        return Some(t);
    }
    None
}

/// The snapshot's data store, kept past the restore for the templates decoded on first lookup.
#[cfg(not(feature = "analysis"))]
struct SnapStore(deno_core::snapshot::SnapshotLoadDataStore);

/// Keep the restore's data store: the runtime hands it over once its last section has read from it.
#[cfg(not(feature = "analysis"))]
pub fn keep_snapshot_store(scope: &mut v8::PinScope, store: deno_core::snapshot::SnapshotLoadDataStore) {
    scope.set_slot(SnapStore(store));
}

/// Whether `key` was refused in this process (so no later realm or isolate records it again).
pub(crate) fn refused(scope: &mut v8::PinScope, key: &str) -> bool {
    let _ = scope;
    process_refused().lock().map(|m| m.contains_key(key)).unwrap_or(false)
}

/// Start recording for `iface` (the caller runs the ordinary install, then calls [`finish`]).
pub(crate) fn begin(iface: &str) {
    CAPTURE.with(|c| c.borrow_mut().push(Capture { iface: iface.to_string(), catch_all: false, recipes: HashMap::new(), forms: HashMap::new(), persona_read: false, marks: Vec::new() }));
}

/// Start recording everything a realm's install builds OUTSIDE its interfaces' own recordings:
/// the global's own functions and accessors, translated by [`finish_global_capture`] when the
/// install is over. Every interface capture runs on top of it.
pub(crate) fn begin_global_capture() {
    CAPTURE.with(|c| c.borrow_mut().push(Capture { iface: GLOBAL_TPL.to_string(), catch_all: true, recipes: HashMap::new(), forms: HashMap::new(), persona_read: false, marks: Vec::new() }));
}

/// The name the global's own template is kept under ([`tpl_by_name`]).
pub(crate) const GLOBAL_TPL: &str = "#global";

/// Whether an INTERFACE's recording is the one running (not only the realm-wide one).
fn in_interface_capture() -> bool {
    CAPTURE.with(|c| c.borrow().last().is_some_and(|cap| !cap.catch_all))
}

/// Translate what the ordinary install just built into a blueprint, keep it for the whole process, build
/// this isolate's templates from it, cache them under `key`, and stop recording.
pub(crate) fn finish(
    scope: &mut v8::PinScope,
    key: &str,
    ctor: v8::Local<v8::Function>,
    proto: v8::Local<v8::Object>,
    parent: Option<&str>,
    replay: crate::install::installer::Replay,
    global: Option<(v8::Local<v8::Object>, &[&str])>,
) {
    finish_inner(scope, key, ctor, proto, parent, replay, global, true);
}

/// Translate what the realm-wide recording saw into the GLOBAL's own template: the global's
/// `keys` (in order) whose functions and accessors the install built outside every interface -- the window
/// getters, `fetch`, `atob`, ... -- kept under [`GLOBAL_TPL`] for a realm built from templates
/// ([`attach_one`]). A no-op unless [`begin_global_capture`]'s recording is the running one.
pub(crate) fn finish_global_capture(scope: &mut v8::PinScope, keys: &[String]) {
    if !CAPTURE.with(|c| c.borrow().last().is_some_and(|cap| cap.catch_all)) {
        return;
    }
    let key = format!("{GLOBAL_TPL}|{}", realm_identity(scope));
    // A template is an interface object's; the global's has one nobody sees.
    let dummy = crate::binding::protos::build_fn(scope, crate::binding::protos::cb_noop, GLOBAL_TPL, 0);
    let proto = v8::Object::new(scope);
    let global = scope.get_current_context().global(scope);
    let names: Vec<&str> = keys.iter().map(String::as_str).collect();
    // The realm-wide recording holds every function the install built outside an interface, most of them
    // not the global's: no orphan check.
    finish_inner(scope, &key, dummy, proto, None, crate::install::installer::Replay::default(), Some((global, &names)), false);
}

#[allow(clippy::too_many_arguments)]
fn finish_inner(
    scope: &mut v8::PinScope,
    key: &str,
    ctor: v8::Local<v8::Function>,
    proto: v8::Local<v8::Object>,
    parent: Option<&str>,
    mut replay: crate::install::installer::Replay,
    global: Option<(v8::Local<v8::Object>, &[&str])>,
    check_orphans: bool,
) {
    let (iface, persona_read) = CAPTURE.with(|c| c.borrow().last().map(|c| (c.iface.clone(), c.persona_read))).unwrap_or_default();
    if persona_read {
        report(&format!("{key}: the install read the persona"));
    }
    let names = names(scope);
    let array_fns = Intr::ALL.iter().map(|(_, n)| array_proto_fn(scope, n).map(|f| v8::Global::new(scope, f))).collect();
    let mut tr = Tr { proto: v8::Global::new(scope, proto), iface_fn: Some(v8::Global::new(scope, ctor)), memo: HashMap::new(), fns: Vec::new(), names, array_fns };
    let result = translate(scope, &mut tr, ctor, proto, global, check_orphans);
    let refused_label = format!("refused {iface}");
    let mut recipes = HashMap::new();
    if let Some(cap) = CAPTURE.with(|c| c.borrow_mut().pop()) {
        replay.marks = cap.marks;
        recipes = cap.recipes;
    }
    let bp = result.and_then(|(ctor_id, [on_proto, on_ctor, late, late_statics, on_global])| {
        // The realm-wide recording leaves out a key it cannot translate, after that key's functions may
        // have taken their places in the table: nothing refers to them, and nothing builds them.
        let fns: Vec<Option<FnBp>> = if check_orphans {
            tr.fns
        } else {
            use deno_core::v8::MapFnTo;
            let noop: v8::FunctionCallback = crate::binding::protos::cb_noop.map_fn_to();
            tr.fns.into_iter().map(|f| Some(f.unwrap_or(FnBp { kind: FnKind::Plain { cb: noop, data: None }, length: 0, class_name: String::new(), ctor: false, brands: Vec::new() }))).collect()
        };
        let fns = fns.into_iter().collect::<Option<Vec<_>>>().ok_or("a function left half-translated")?;
        Ok(Blueprint { iface, fns, ctor: ctor_id, proto: on_proto, statics: on_ctor, late_proto: late, late_statics, global: on_global, parent: parent.map(str::to_string), replay, from_disk: false, deltas: None })
    });
    match bp {
        Ok(bp) => {
            let bp = std::sync::Arc::new(bp);
            let slot = if persona_read { persona_key(scope, key) } else { key.to_string() };
            if let Ok(mut m) = process_blueprints().lock() {
                m.entry(slot).or_insert_with(|| bp.clone());
            }
            if !persona_read {
                DIRTY.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            report(&format!("{key}: recorded"));
            if reporting() {
                let fused = bp.fns.iter().filter(|f| matches!(f.kind, FnKind::Fused { .. })).count();
                report(&format!("{key}: fused {fused}"));
            }
            with_cache(scope, |c| c.counts.recorded += 1);
            install_from(scope, key, &bp);
        }
        Err(why) => {
            report(&format!("{key}: refused -- {why}"));
            // What the recording saw stays as the ordinary install built it, in this realm: D1 counts it
            // ([`note_untemplated`] skipped it while the capture ran, expecting a template).
            if reporting() {
                for (g, r) in recipes.into_values().flatten() {
                    let f = v8::Local::new(scope, &g);
                    note_untemplated_as(scope, &refused_label, &r.name, f);
                }
            }
            if let Ok(mut m) = process_refused().lock() {
                m.insert(key.to_string(), why);
            }
        }
    }
}

/// Every blueprint this process has recorded, by template key.
fn process_blueprints() -> &'static std::sync::Mutex<FxMap<String, std::sync::Arc<Blueprint>>> {
    static B: std::sync::OnceLock<std::sync::Mutex<FxMap<String, std::sync::Arc<Blueprint>>>> = std::sync::OnceLock::new();
    B.get_or_init(Default::default)
}

/// A blueprint file's record nothing has asked for yet: the file's bytes (checksummed when it was read) and the record's
/// range in them.
type LazyBp = (std::sync::Arc<[u8]>, std::ops::Range<usize>);

/// The blueprints a file holds and nothing has asked for -- decoded on first ask ([`process_blueprint`]),
/// not all at load: a page makes most interfaces lazily, a restored one almost none.
fn lazy_blueprints() -> &'static std::sync::Mutex<FxMap<String, LazyBp>> {
    static B: std::sync::OnceLock<std::sync::Mutex<FxMap<String, LazyBp>>> = std::sync::OnceLock::new();
    B.get_or_init(Default::default)
}

/// The blueprint under `key`: recorded or decoded already, or decoded now from its file's record (a record this process
/// recorded itself wins, as at load).
fn process_blueprint(key: &str) -> Option<std::sync::Arc<Blueprint>> {
    if let Some(b) = process_blueprints().lock().ok()?.get(key).cloned() {
        return Some(b);
    }
    let (bytes, range) = lazy_blueprints().lock().ok()?.remove(key)?;
    let Some(bp) = disk::decode_record(&bytes[range]) else {
        report(&format!("disk: blueprint {key} does not decode"));
        return None;
    };
    report_snap(&format!("disk: blueprint {key} decoded"));
    let mut m = process_blueprints().lock().ok()?;
    Some(m.entry(key.to_string()).or_insert_with(|| std::sync::Arc::new(bp)).clone())
}

/// Every key this process refused, with why (a later isolate does not record it again).
fn process_refused() -> &'static std::sync::Mutex<FxMap<String, String>> {
    static R: std::sync::OnceLock<std::sync::Mutex<FxMap<String, String>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// This process recorded a blueprint no file has yet ([`persist`]).
static DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keys whose disk blueprint's bookkeeping this process has replayed.
fn marks_replayed() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static M: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

/// Use blueprint files (the `korvus` binary calls this at start-up): a fresh process builds its templates
/// from what an earlier run of the SAME binary recorded, instead of recording them again. Off in a library
/// user, so a test run never depends on a file an earlier run left; `KORVUS_TPL_CACHE=off` keeps it off.
/// The context snapshot (slice 3): the file a snapshot of this binary's page realm lives in -- beside the
/// blueprint files, under the build id, keyed by the environment as a blueprint file is ([`disk`]'s
/// `env_fingerprint`: `KORVUS_*`, `TZ`, `LANG`, `LC_*`) and by `persona`, the selected persona's own stamp (what it
/// reads at run time). `None` when the disk cache is off or this binary has no build id.
/// The context snapshot: the running binary's build id, the blueprint files' word-wise checksum, the cache's trust
/// check and its private writers -- what the snapshot file is gated and written with (`webapi::snapshot`).
#[cfg(not(feature = "analysis"))]
pub(crate) mod snapshot_disk {
    pub(crate) fn build_id() -> Option<&'static [u8]> {
        super::disk::build_id()
    }
    pub(crate) fn checksum(b: &[u8]) -> u64 {
        super::disk::checksum(b)
    }
    pub(crate) fn trusted(path: &std::path::Path) -> bool {
        super::disk::trusted(path)
    }
    pub(crate) fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
        super::disk::private_dir(dir)
    }
    pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        super::disk::write_private(path, bytes)
    }
}

#[cfg(not(feature = "analysis"))]
pub fn snapshot_file(persona: u64) -> Option<std::path::PathBuf> {
    if !disk::ON.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    disk::snapshot_file(persona)
}

pub fn enable_disk_cache() {
    if !std::env::var("KORVUS_TPL_CACHE").is_ok_and(|v| v == "off") {
        disk::ON.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Write what this process recorded for the realm's surface, when it recorded anything new (called
/// once the top-level window realm has installed; `korvus-runtime`'s `install_window_realm`).
pub fn persist(scope: &mut v8::PinScope) {
    // Before the realm-wide recording stops: what the install changed is translated from it.
    record_deltas(scope);
    if CAPTURE.with(|c| c.borrow().last().is_some_and(|cap| cap.catch_all)) {
        let keys = global_record_keys(scope);
        finish_global_capture(scope, &keys);
    }
    let chains = record_final_chains(scope);
    crate::core::html::window_globals::record_singleton_protos(scope);
    let page = crate::install::surface::realm(scope) == Some(spec::SurfaceRealm::Window);
    if page {
        switch_page_global(scope);
        report_surface(scope, "page");
    } else {
        report_surface(scope, "worker");
    }
    report_global(scope);
    if !disk::ON.load(std::sync::atomic::Ordering::Relaxed) || !DIRTY.swap(false, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(a) = crate::install::surface::active(scope) else { return };
    // The page's final chains with its blueprints: a later process makes an interface the page
    // put lazily from its template, and chains it as this install did -- not to the template's parent, which is
    // not how every install splices ([`ChainTo`]). A worker's surface keeps none.
    disk::write(surface_fingerprint(&a), if page { &chains } else { &[] });
}

/// The blueprint this process recorded for `key` in this realm: the surface's, else this persona's.
pub(crate) fn blueprint(scope: &mut v8::PinScope, key: &str) -> Option<std::sync::Arc<Blueprint>> {
    if let Some(b) = process_blueprint(key) {
        return Some(b);
    }
    let pk = persona_key(scope, key);
    process_blueprint(&pk)
}

/// This isolate's templates for `key`, built from the process's blueprint. `None` when there is no
/// blueprint, or when this isolate cannot build from it (the realm then builds the ordinary way, without
/// recording: the blueprint exists).
pub(crate) fn from_blueprint(scope: &mut v8::PinScope, key: &str) -> Option<Rc<Tpl>> {
    let bp = blueprint(scope, key)?;
    if !install_from(scope, key, &bp) {
        return None;
    }
    if bp.from_disk && crate::behavior_rt::bookkeeping_on() && marks_replayed().lock().is_ok_and(|mut m| m.insert(key.to_string())) {
        replay_marks(&bp.replay.marks);
    }
    report(&format!("{key}: built from the blueprint"));
    with_cache(scope, |c| c.counts.from_blueprint += 1);
    cached(scope, key)
}

/// What a realm installs against, for a template key: its SURFACE, named by content
/// ([`surface_fingerprint`]). The persona is not in it: an install that read the persona marked its
/// capture ([`note_persona_read`]), and that blueprint is kept under [`persona_key`] instead -- measured
/// at P4: one of 871 keys (`Performance`, whose `memory` member exists only for a persona with a heap
/// window). Every other blueprint is shared by any isolate installing against the same surface.
pub(crate) fn realm_identity(scope: &mut v8::PinScope) -> String {
    match crate::install::surface::active(scope) {
        Some(a) => {
            let id = pinned(&a);
            if let Some(known) = with_cache(scope, |c| c.identities.get(&id).cloned()) {
                return known;
            }
            let fp = surface_fingerprint(&a);
            disk::ensure_loaded(fp);
            let identity = format!("s{fp:016x}");
            with_cache(scope, |c| c.identities.insert(id, identity.clone()));
            identity
        }
        None => "s-".to_string(),
    }
}

/// The surface a realm installs against, named by what the builders read from it -- the capture id, the
/// globals, and every interface's member and static order and `constructor`'s seat -- memoised per
/// surface allocation ([`pinned`] keeps an address from meaning another surface later).
fn surface_fingerprint(s: &std::sync::Arc<spec::RealmSurface>) -> u64 {
    use std::hash::{Hash, Hasher};
    type Fps = std::sync::Mutex<HashMap<usize, u64>>;
    static FPS: std::sync::OnceLock<Fps> = std::sync::OnceLock::new();
    let id = pinned(s);
    if let Some(v) = FPS.get_or_init(Default::default).lock().ok().and_then(|m| m.get(&id).copied()) {
        return v;
    }
    // the engine's own hasher: SipHash over every interface's member lists was ~3.4 % of a warm start
    let mut h = FxHasher(0);
    s.capture_id.hash(&mut h);
    s.globals.hash(&mut h);
    let mut names: Vec<&String> = s.interfaces.keys().collect();
    names.sort();
    for n in names {
        let m = &s.interfaces[n];
        n.hash(&mut h);
        m.members.hash(&mut h);
        m.statics.hash(&mut h);
        m.ctor_index.hash(&mut h);
    }
    let v = h.finish();
    if let Ok(mut m) = FPS.get_or_init(Default::default).lock() {
        m.insert(id, v);
    }
    v
}

/// `key` for the persona this realm wears (named by its pinned address): where a blueprint whose capture
/// read the persona is kept.
fn persona_key(scope: &mut v8::PinScope, key: &str) -> String {
    let persona = {
        let st = deno_core::JsRuntime::op_state_from(scope);
        let st = st.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::PersonaHandle>()).map(|h| h.0.clone())
    };
    format!("{key}|p{:?}", persona.map(|p| pinned(&p)))
}

/// A process-unique id for an `Arc`'s allocation: its address, which the `Weak` kept here stops the
/// allocator from giving to anything else while the process lives (a `Weak` keeps the allocation, not
/// the value).
fn pinned<T: ?Sized + Send + Sync + 'static>(a: &std::sync::Arc<T>) -> usize {
    type Pins = std::sync::Mutex<FxMap<usize, Box<dyn std::any::Any + Send>>>;
    static PINS: std::sync::OnceLock<Pins> = std::sync::OnceLock::new();
    let id = std::sync::Arc::as_ptr(a) as *const () as usize;
    let mut m = PINS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    m.entry(id).or_insert_with(|| Box::new(std::sync::Arc::downgrade(a)));
    id
}

/// Build this isolate's templates for `key` from `bp` and cache them (the parent's template is this
/// isolate's, by name). `false` when a template could not be made (reported).
pub(crate) fn install_from(scope: &mut v8::PinScope, key: &str, bp: &Blueprint) -> bool {
    let parent_ft = bp.parent.as_deref().and_then(|p| by_name_tpl(scope, p).map(|t| t.ctor.clone()));
    if let Some(p) = bp.parent.as_deref().filter(|_| parent_ft.is_none()) {
        report(&format!("{key}: no parent template ({p})"));
    }
    let mut keep = Vec::new();
    let t0 = reporting().then(std::time::Instant::now);
    let built = build(scope, key, bp, parent_ft, &mut keep);
    if let Some(t0) = t0 {
        let ord = crate::install::installer::realm_ordinal(scope);
        report(&format!("TIME {} r{ord} tpl-built {}us", bp.iface, t0.elapsed().as_micros()));
    }
    match built {
        Ok(t) => {
            let t = Rc::new(t);
            let iface = bp.iface.clone();
            with_cache(scope, |c| {
                c.keep.extend(keep);
                c.tpls.insert(key.to_string(), t.clone());
                c.by_name.insert(iface, t);
            });
            true
        }
        Err(why) => {
            report(&format!("{key}: not built -- {why}"));
            with_cache(scope, |c| c.keep.extend(keep));
            false
        }
    }
}

fn attrs_of(scope: &mut v8::PinScope, names: &Names, d: v8::Local<v8::Object>, is_data: bool) -> Attrs {
    let flag = |scope: &mut v8::PinScope, k: &v8::Global<v8::String>| {
        let k = v8::Local::new(scope, k);
        d.get(scope, k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false)
    };
    Attrs {
        read_only: is_data && !flag(scope, &names.writable),
        dont_enum: !flag(scope, &names.enumerable),
        dont_delete: !flag(scope, &names.configurable),
    }
}

fn primitive(v: v8::Local<v8::Value>) -> bool {
    v.is_null_or_undefined() || v.is_boolean() || v.is_number() || v.is_string() || v.is_symbol() || v.is_big_int()
}

/// A short description of a value for a refusal report: its type, and an array's element types.
fn describe(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> String {
    if v.is_external() {
        return "External".into();
    }
    if let Ok(a) = v8::Local::<v8::Array>::try_from(v) {
        let parts: Vec<String> = (0..a.length().min(8))
            .map(|i| match a.get_index(scope, i) {
                Some(x) if x.is_external() => "ext".to_string(),
                Some(x) if x.is_function() => "fn".to_string(),
                Some(x) if x.is_array() => "arr".to_string(),
                Some(x) if x.is_object() => "obj".to_string(),
                Some(x) if x.is_string() => "str".to_string(),
                Some(x) if x.is_number() => "num".to_string(),
                Some(x) if x.is_boolean() => "bool".to_string(),
                Some(x) if x.is_null_or_undefined() => "nil".to_string(),
                _ => "?".to_string(),
            })
            .collect();
        return format!("Array[{}]", parts.join(","));
    }
    if v.is_function() {
        return "Function".into();
    }
    "Object".into()
}

/// One translation: the interface, its prototype, and one blueprint function per function object -- a
/// function that sits in two slots (`values` and `@@iterator`) is ONE object in every realm, as it was here.
struct Tr {
    proto: v8::Global<v8::Object>,
    /// the interface object being translated: its own properties are its statics (translated as such),
    /// not a body's stray properties, even when it is not constructible (`NodeFilter`)
    iface_fn: Option<v8::Global<v8::Function>>,
    /// function identity -> its index in `fns`
    memo: HashMap<i32, Vec<(v8::Global<v8::Function>, usize)>>,
    /// the blueprint's function table (a slot is `None` while its function is being translated)
    fns: Vec<Option<FnBp>>,
    names: Rc<Names>,
    /// this realm's `Array.prototype` iteration functions, in [`Intr::ALL`] order
    array_fns: Vec<Option<v8::Global<v8::Function>>>,
}

/// The names the translator reads on every member, made once per isolate and kept in the cache.
struct Names {
    value: v8::Global<v8::String>,
    get: v8::Global<v8::String>,
    set: v8::Global<v8::String>,
    writable: v8::Global<v8::String>,
    enumerable: v8::Global<v8::String>,
    configurable: v8::Global<v8::String>,
    length: v8::Global<v8::String>,
    prototype: v8::Global<v8::String>,
    /// FN_BRANDS' keys, in order
    brands: Vec<v8::Global<v8::Private>>,
}

fn names(scope: &mut v8::PinScope) -> Rc<Names> {
    if let Some(n) = with_cache(scope, |c| c.names.clone()) {
        return n;
    }
    let s = |scope: &mut v8::PinScope, t: &str| {
        let v = v8::String::new_from_utf8(scope, t.as_bytes(), v8::NewStringType::Internalized).unwrap();
        v8::Global::new(scope, v)
    };
    let p = |scope: &mut v8::PinScope, t: &str| {
        let k = private_key(scope, t);
        v8::Global::new(scope, k)
    };
    let n = Rc::new(Names {
        value: s(scope, "value"),
        get: s(scope, "get"),
        set: s(scope, "set"),
        writable: s(scope, "writable"),
        enumerable: s(scope, "enumerable"),
        configurable: s(scope, "configurable"),
        length: s(scope, "length"),
        prototype: s(scope, "prototype"),
        brands: FN_BRANDS.iter().map(|b| p(scope, b)).collect(),
    });
    with_cache(scope, |c| c.names = Some(n.clone()));
    n
}

/// The V8 privates a function carries that some later pass READS, with primitive values the template
/// carries as they are. (A receiver guard's inner function, policy and prototype were carried too, for the
/// re-aim `receiver::rebind` made; it and they are deleted, as nothing read them any more.)
/// `korvus.installedBy` is install bookkeeping, which a frame realm's install does not keep
/// (`behavior_rt::without_bookkeeping`).
const FN_BRANDS: &[&str] = &["korvus.engineFn", "korvus.arityGuarded", "korvus.receiverGuarded", "korvus.installedBy"];

fn private_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// `Template::SetPrivate`, which rusty_v8 does not wrap: V8's own is `Set(Local<Name>::Cast(name), ..)`
/// -- a private IS a symbol inside V8, and `Template::Set` takes any name.
pub(crate) fn set_private_on(t: &v8::Template, key: v8::Local<v8::Private>, value: v8::Local<v8::Data>) {
    // SAFETY: both are one V8 handle; `v8::Private` is a `v8::Name` (an `i::Symbol`) in V8 itself,
    // which is exactly the cast `v8::Template::SetPrivate` makes.
    let name: v8::Local<v8::Name> = unsafe { std::mem::transmute::<v8::Local<v8::Private>, v8::Local<v8::Name>>(key) };
    t.set_with_attr(name, value, v8::PropertyAttribute::NONE);
}

// ── the blueprint: what one capture recorded, as plain data ──────────────────────────────────
//
// A capture's translation produces a BLUEPRINT -- no V8 handle in it, only callbacks, primitives, lengths,
// names and the shape -- and every isolate of the process builds the interface's templates from it
// ([`build`]). The first isolate records it while its first realm installs the ordinary way; every later
// isolate builds from it without running the interface's install at all.

/// A value a template carries as it is: one that is the same in every realm (a primitive), or a member
/// row (`install::stub::stub_data`'s External over a `&'static MemberDef`, the one External a recorded
/// function may carry: [`note_member_external`] names them, and any other is refused).
#[derive(Clone)]
enum PrimBp {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Member(&'static crate::install::MemberDef),
}

/// A V8 string as Rust text, when it round-trips exactly (a lone surrogate would not).
fn exact_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let t = v.to_rust_string_lossy(scope);
    let back = v8::String::new(scope, &t)?;
    v.strict_equals(back.into()).then_some(t)
}

impl PrimBp {
    fn of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<PrimBp> {
        Some(if v.is_undefined() {
            PrimBp::Undefined
        } else if v.is_null() {
            PrimBp::Null
        } else if v.is_boolean() {
            PrimBp::Bool(v.is_true())
        } else if v.is_number() {
            PrimBp::Num(v.number_value(scope)?)
        } else if v.is_string() {
            PrimBp::Str(exact_string(scope, v)?)
        } else if let Ok(e) = v8::Local::<v8::External>::try_from(v) {
            PrimBp::Member(member_of(e.value() as usize)?)
        } else {
            return None;
        })
    }
    fn v8<'s>(&self, scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
        match self {
            PrimBp::Undefined => v8::undefined(scope).into(),
            PrimBp::Null => v8::null(scope).into(),
            PrimBp::Bool(b) => v8::Boolean::new(scope, *b).into(),
            PrimBp::Num(n) => v8::Number::new(scope, *n).into(),
            PrimBp::Str(t) => v8::String::new(scope, t).map(|s| s.into()).unwrap_or_else(|| v8::undefined(scope).into()),
            PrimBp::Member(m) => v8::External::new(scope, crate::binding::refs::ext(*m as *const crate::install::MemberDef as *mut std::ffi::c_void)).into(),
        }
    }
}

/// A property key: a string, or one of the well-known symbols (any other symbol is one realm's, refused).
#[derive(Clone)]
enum KeyBp {
    Str(String),
    Sym(u8),
}

/// The well-known symbols by index, as rusty_v8 exposes them.
fn well_known<'s>(scope: &mut v8::PinScope<'s, '_>, i: u8) -> Option<v8::Local<'s, v8::Symbol>> {
    Some(match i {
        0 => v8::Symbol::get_async_iterator(scope),
        1 => v8::Symbol::get_has_instance(scope),
        2 => v8::Symbol::get_is_concat_spreadable(scope),
        3 => v8::Symbol::get_iterator(scope),
        4 => v8::Symbol::get_match(scope),
        5 => v8::Symbol::get_replace(scope),
        6 => v8::Symbol::get_search(scope),
        7 => v8::Symbol::get_split(scope),
        8 => v8::Symbol::get_to_primitive(scope),
        9 => v8::Symbol::get_to_string_tag(scope),
        10 => v8::Symbol::get_unscopables(scope),
        _ => return None,
    })
}

impl KeyBp {
    fn of(scope: &mut v8::PinScope, k: v8::Local<v8::Value>) -> Option<KeyBp> {
        if k.is_string() {
            return exact_string(scope, k).map(KeyBp::Str);
        }
        (0u8..11).find(|i| well_known(scope, *i).is_some_and(|s| {
            let s: v8::Local<v8::Value> = s.into();
            s == k
        })).map(KeyBp::Sym)
    }
    fn v8<'s>(&self, scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Name>> {
        match self {
            KeyBp::Str(t) => v8::String::new_from_utf8(scope, t.as_bytes(), v8::NewStringType::Internalized).map(|s| s.into()),
            KeyBp::Sym(i) => well_known(scope, *i).map(|s| s.into()),
        }
    }
}

#[derive(Clone)]
enum EntryBp {
    Value(PrimBp, Attrs),
    Func(usize, Attrs),
    Accessor(Option<usize>, Option<usize>, Attrs),
    Intrinsic(Intr, Attrs),
    /// a NAMESPACE object on the global (`CSS`, `chrome`, `GPUBufferUsage`): a plain object of this realm
    /// with these own properties
    Namespace(Vec<(KeyBp, EntryBp)>, Attrs),
    /// an `@@unscopables` object: null-prototype, these names each `true`, made per realm -- a
    /// template cannot carry an object, so the entry is always in the late list
    Unscopables(Vec<String>, Attrs),
}

/// The check a recorded guard makes, as the translator reads it ([`fuse`] turns a chain of them into a
/// fused member's steps).
#[derive(Clone)]
enum GuardBp {
    Arity { need: usize, iface: String, member: String, ty: crate::install::TypeTag },
    Receiver { ord: i64 },
    Brand { iface: String, member: String, need: usize, is_void: bool },
    BrandAccessor { iface: String },
    BrandPromise { iface: String, member: String },
    RangeBrand { idx: u32 },
    PluginRecv { iface: String },
    WindowRecv,
}

/// A fused member's own check (see [`FusedCheck`]; the receiver's prototype is resolved at build).
#[derive(Clone)]
enum CheckBp {
    /// the refusal's form, and the member's name (a promise member's rejection names it, as Chrome's does)
    Receiver(i64, String),
    Brand { iface: String, member: String, need: usize },
    BrandAccessor(String),
    BrandPromise { iface: String, member: String },
    RangeBrand(u32),
    PluginRecv(String),
    WindowRecv,
}

/// One check of a fused member, in the order its guards made them (the outermost first).
#[derive(Clone)]
enum StepBp {
    Check(CheckBp),
    /// `make_arity_guard`'s count: required, interface, member, the row's type
    Arity(usize, String, String, crate::install::TypeTag),
}

/// A fused body's data. The two forms that name the interface become one of the isolate's records at
/// build, as their unfused templates' data does.
enum BodyDataBp {
    None,
    Prim(PrimBp),
    /// a receiver-checked stub: its template form (`install::stub::stub_template_of`), whose callback
    /// replaces the recorded one
    Stub { setter: bool, member: &'static crate::install::MemberDef, table: bool },
    /// the interface's prototype (`Form::ProtoData`), as an `IfaceRef`
    ProtoData,
}

enum FnKind {
    /// a plain function: its callback and its data
    Plain { cb: v8::FunctionCallback, data: Option<PrimBp> },
    /// `install::stub::build_stub` over a receiver-checked row
    Stub { setter: bool, member: &'static crate::install::MemberDef, table: bool },
    /// `install::kits::proto_data_fn`: the interface's prototype as the body's data
    ProtoData { cb: v8::FunctionCallback },
    /// a fused member: its guards' checks in the order they ran, then the body -- one native call
    Fused { steps: Vec<StepBp>, body: v8::FunctionCallback, data: BodyDataBp, is_void: bool },
}

struct FnBp {
    kind: FnKind,
    length: i32,
    class_name: String,
    ctor: bool,
    /// FN_BRANDS index -> value
    brands: Vec<(usize, PrimBp)>,
}

/// What one capture recorded about one interface. `Send + Sync`: it outlives the isolate that recorded it.
pub(crate) struct Blueprint {
    iface: String,
    fns: Vec<FnBp>,
    ctor: usize,
    proto: Vec<(KeyBp, EntryBp)>,
    statics: Vec<(KeyBp, EntryBp)>,
    late_proto: Vec<(KeyBp, EntryBp)>,
    late_statics: Vec<(KeyBp, EntryBp)>,
    /// a `[Global]` interface's own members, which live on the realm's GLOBAL OBJECT (the Window
    /// global): attached per realm, in this order, after the interface object is published
    global: Vec<(KeyBp, EntryBp)>,
    parent: Option<String>,
    replay: crate::install::installer::Replay,
    /// read from a file (`disk`): its install ran in another process, so its bookkeeping is replayed here
    from_disk: bool,
    /// what the page's install changed on the interface after instantiating it ([`record_deltas`])
    deltas: Option<DeltaSet>,
}

/// A recorded guard as the blueprint keeps it (`None` for a plain function). A receiver guard is kept
/// only when it checks against the prototype of the interface being translated.
fn guard_bp(scope: &mut v8::PinScope, tr: &Tr, r: &Recipe) -> Result<Option<GuardBp>, String> {
    Ok(Some(match &r.guard {
        Guard::None => return Ok(None),
        Guard::Arity { need, iface, member, ty } => GuardBp::Arity { need: *need, iface: iface.clone(), member: member.clone(), ty: *ty },
        Guard::Receiver { ord, proto } => {
            let aimed = v8::Local::new(scope, proto);
            let own: v8::Local<v8::Value> = v8::Local::new(scope, &tr.proto).into();
            if aimed != own {
                return Err(format!("`{}` is receiver-checked against another interface's prototype", r.name));
            }
            GuardBp::Receiver { ord: *ord }
        }
        Guard::Brand { iface, member, need, is_void } => GuardBp::Brand { iface: iface.clone(), member: member.clone(), need: *need, is_void: *is_void },
        Guard::BrandAccessor { iface } => GuardBp::BrandAccessor { iface: iface.clone() },
        Guard::BrandPromise { iface, member } => GuardBp::BrandPromise { iface: iface.clone(), member: member.clone() },
        Guard::RangeBrand { idx } => GuardBp::RangeBrand { idx: *idx },
        Guard::PluginRecv { iface } => GuardBp::PluginRecv { iface: iface.clone() },
        Guard::WindowRecv => GuardBp::WindowRecv,
    }))
}

/// The blueprint function for a recorded function (recursively, through a guard's inner function).
fn bp_fn(scope: &mut v8::PinScope, tr: &mut Tr, f: v8::Local<v8::Function>) -> Result<usize, String> {
    let h = f.get_identity_hash().get();
    if let Some(list) = tr.memo.get(&h) {
        for (g, id) in list {
            if v8::Local::new(scope, g) == f {
                return Ok(*id);
            }
        }
    }
    let Some(r) = recipe_of(scope, f) else {
        let n = f.get_name(scope).to_rust_string_lossy(scope);
        return Err(format!("function `{n}` was not made by a recording builder"));
    };
    // reserve the slot first: a recursion back to this function finds it
    let id = tr.fns.len();
    tr.fns.push(None);
    tr.memo.entry(h).or_default().push((v8::Global::new(scope, f), id));
    let kind = match &r.guard {
        Guard::None => {
            let Some(cb) = r.cb else { return Err(format!("`{}` was recorded without its callback", r.name)) };
            let data = r.data.as_ref().map(|d| v8::Local::new(scope, d));
            match data {
                None => FnKind::Plain { cb, data: None },
                Some(d) if primitive(d) || d.is_external() => match PrimBp::of(scope, d) {
                    Some(p) => FnKind::Plain { cb, data: Some(p) },
                    None => return Err(format!("`{}` carries a symbol, a BigInt, an inexact string or an External that is not a member row as its data", r.name)),
                },
                // Data that belongs to one realm: only a builder's declared form can translate it.
                Some(d) => match form_of(scope, f) {
                    Some(Form::Stub { setter }) => {
                        let own: v8::Local<v8::Value> = v8::Local::new(scope, &tr.proto).into();
                        let (member, table) = crate::install::stub::stub_parts(scope, d, own)?;
                        FnKind::Stub { setter, member, table }
                    }
                    Some(Form::ProtoData) => {
                        let own: v8::Local<v8::Value> = v8::Local::new(scope, &tr.proto).into();
                        if d != own {
                            return Err(format!("`{}` carries another interface's prototype as its data", r.name));
                        }
                        FnKind::ProtoData { cb }
                    }
                    None => {
                        let what = describe(scope, d);
                        return Err(format!("`{}` carries an object as its data: {what}", r.name));
                    }
                },
            }
        }
        _ => {
            let inner = r.inner.as_ref().map(|g| v8::Local::new(scope, g)).ok_or_else(|| format!("`{}`: a guard with no inner function", r.name))?;
            // the inner function translates too (a chain over one that cannot refuses here)
            bp_fn(scope, tr, inner)?;
            let gbp = guard_bp(scope, tr, &r)?.ok_or("a guard recipe with no guard")?;
            // FUSED: the whole guard chain's checks and the body as ONE native call; the body's
            // data rides in the record (`binding::protos::body_data`). A chain over a body `fuse` cannot take
            // refuses the interface: no trampoline exists to keep it (0 were built in any
            // realm of any bundle once every interface fused, `3b0f174e`).
            fuse(scope, tr, &r, &gbp).ok_or_else(|| format!("`{}`: a guard chain the fuser cannot take", r.name))?
        }
    };
    // The name is the function's OWN (a body may rename what it built). V8 gives a template's function
    // this name only where no property key names it -- an accessor, a symbol-keyed member, the interface
    // object; a string-keyed data property names its function after the KEY, which `translate` handles.
    let fname: v8::Local<v8::Value> = f.get_name(scope).into();
    let Some(class_name) = exact_string(scope, fname) else { return Err(format!("`{}` has an inexact name", r.name)) };
    let fo: v8::Local<v8::Object> = f.into();
    let is_iface = tr.iface_fn.as_ref().is_some_and(|g| v8::Local::new(scope, g) == f);
    if !r.ctor && !is_iface {
        // anything but `length` and `name` is a property a body put on the function, which the template
        // would not have
        let keys = own_keys(scope, fo);
        let names: Vec<String> = keys.iter().map(|k| k.to_rust_string_lossy(scope)).collect();
        if names.iter().any(|k| k != "length" && k != "name") {
            return Err(format!("`{}` carries its own properties {names:?}", r.name));
        }
        let lk = v8::Local::new(scope, &tr.names.length);
        let len = fo.get(scope, lk.into()).and_then(|v| v.int32_value(scope));
        if len != Some(r.length) {
            return Err(format!("`{}` has length {len:?}, recorded {}", r.name, r.length));
        }
    }
    // the brands (see FN_BRANDS)
    let names = tr.names.clone();
    let mut brands = Vec::new();
    for (i, (name, k)) in FN_BRANDS.iter().zip(&names.brands).enumerate() {
        let k = v8::Local::new(scope, k);
        let Some(v) = fo.get_private(scope, k).filter(|v| !v.is_undefined()) else { continue };
        match PrimBp::of(scope, v).filter(|_| primitive(v)) {
            Some(p) => brands.push((i, p)),
            None => return Err(format!("`{}` carries an object under {name}", r.name)),
        }
    }
    tr.fns[id] = Some(FnBp { kind, length: r.length, class_name, ctor: r.ctor, brands });
    Ok(id)
}

fn bp_entry(scope: &mut v8::PinScope, tr: &mut Tr, d: v8::Local<v8::Object>) -> Result<EntryBp, String> {
    let names = tr.names.clone();
    let k_value = v8::Local::new(scope, &names.value);
    let k_get = v8::Local::new(scope, &names.get);
    let k_set = v8::Local::new(scope, &names.set);
    let has_value = d.has_own_property(scope, k_value.into()).unwrap_or(false);
    if has_value {
        let a = attrs_of(scope, &names, d, true);
        let v = d.get(scope, k_value.into()).unwrap();
        if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
            if let Some(i) = Intr::of(scope, &tr.array_fns, f) {
                return Ok(EntryBp::Intrinsic(i, a));
            }
            return Ok(EntryBp::Func(bp_fn(scope, tr, f)?, a));
        }
        if !primitive(v) {
            // An `@@unscopables` object: null-prototype, every own key a string whose value is `true`.
            if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
                if o.get_prototype(scope).is_some_and(|p| p.is_null()) {
                    let keys = o.get_own_property_names(scope, v8::GetPropertyNamesArgs::default());
                    if let Some(keys) = keys {
                        let mut names = Vec::new();
                        let mut all_true = true;
                        for i in 0..keys.length() {
                            let Some(k) = keys.get_index(scope, i) else { all_true = false; break };
                            let val = o.get(scope, k);
                            if !val.is_some_and(|x| x.is_true()) {
                                all_true = false;
                                break;
                            }
                            names.push(k.to_rust_string_lossy(scope));
                        }
                        if all_true {
                            return Ok(EntryBp::Unscopables(names, a));
                        }
                    }
                }
            }
            return Err("an object value".into());
        }
        let p = PrimBp::of(scope, v).ok_or("a symbol or BigInt value")?;
        return Ok(EntryBp::Value(p, a));
    }
    let a = attrs_of(scope, &names, d, false);
    let fof = |scope: &mut v8::PinScope, tr: &mut Tr, k: v8::Local<v8::String>| -> Result<Option<usize>, String> {
        let v = d.get(scope, k.into()).unwrap();
        match v8::Local::<v8::Function>::try_from(v) {
            Ok(f) => Ok(Some(bp_fn(scope, tr, f)?)),
            Err(_) => Ok(None),
        }
    };
    let g = fof(scope, tr, k_get)?;
    let s = fof(scope, tr, k_set)?;
    Ok(EntryBp::Accessor(g, s, a))
}

/// [`bp_entry`] for a key of the GLOBAL: a plain object there is a namespace, recorded with its own
/// properties (recursively), which a realm built from templates makes as its own object.
fn bp_global_entry(scope: &mut v8::PinScope, tr: &mut Tr, d: v8::Local<v8::Object>) -> Result<EntryBp, String> {
    let names = tr.names.clone();
    let k_value = v8::Local::new(scope, &names.value);
    let v = d.get(scope, k_value.into());
    let plain = v.filter(|v| v.is_object() && !v.is_function() && !v.is_array()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()).filter(|o| {
        let op = v8::Object::new(scope).get_prototype(scope);
        o.get_prototype(scope) == op
    });
    let Some(o) = plain else { return bp_entry(scope, tr, d) };
    let a = attrs_of(scope, &names, d, true);
    let mut rows = Vec::new();
    for k in own_keys(scope, o) {
        let Ok(name) = v8::Local::<v8::Name>::try_from(k) else { return Err("an index key on a namespace".into()) };
        let key = KeyBp::of(scope, k).ok_or("a namespace key that is neither a string nor a well-known symbol")?;
        let Some(cd) = o.get_own_property_descriptor(scope, name).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) else { continue };
        let e = bp_global_entry(scope, tr, cd).map_err(|w| format!("{}: {w}", k.to_rust_string_lossy(scope)))?;
        rows.push((key, e));
    }
    Ok(EntryBp::Namespace(rows, a))
}

fn own_keys<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    let args = v8::GetPropertyNamesArgs {
        mode: v8::KeyCollectionMode::OwnOnly,
        property_filter: v8::PropertyFilter::ALL_PROPERTIES,
        index_filter: v8::IndexFilter::IncludeIndices,
        key_conversion: v8::KeyConversionMode::KeepNumbers,
    };
    let Some(a) = o.get_own_property_names(scope, args) else { return Vec::new() };
    (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect()
}

fn put_on(scope: &mut v8::PinScope, on: On, key: v8::Local<v8::Name>, e: &Entry) {
    match e {
        Entry::Value(v, a) => {
            let v = v8::Local::new(scope, v);
            match on {
                On::Proto(t) => t.set_with_attr(key, v.into(), a.v8()),
                On::Iface(t) => t.set_with_attr(key, v.into(), a.v8()),
            }
        }
        Entry::Func(f, a) => {
            let f = v8::Local::new(scope, f);
            match on {
                On::Proto(t) => t.set_with_attr(key, f.into(), a.v8()),
                On::Iface(t) => t.set_with_attr(key, f.into(), a.v8()),
            }
        }
        Entry::Intrinsic(i, a) => match on {
            On::Proto(t) => t.set_intrinsic_data_property(key, i.v8(), a.v8()),
            On::Iface(t) => t.set_intrinsic_data_property(key, i.v8(), a.v8()),
        },
        Entry::Accessor(g, s, a) => {
            let g = g.as_ref().map(|g| v8::Local::new(scope, g));
            let s = s.as_ref().map(|s| v8::Local::new(scope, s));
            match on {
                On::Proto(t) => t.set_accessor_property(key, g, s, a.v8()),
                On::Iface(t) => t.set_accessor_property(key, g, s, a.v8()),
            }
        }
        // only the global's own rows hold one, and those are attached per realm ([`attach`])
        Entry::Namespace(..) => {}
        // always late: made per realm by [`attach`]
        Entry::Unscopables(..) => {}
    }
}

/// What a realm attaches after instantiating, in order.
struct Late {
    proto: Vec<(v8::Global<v8::Name>, Entry)>,
    statics: Vec<(v8::Global<v8::Name>, Entry)>,
}

/// The keys, entries and function table of an interface, read back off what its ordinary install built.
#[allow(clippy::type_complexity)]
fn translate(
    scope: &mut v8::PinScope,
    tr: &mut Tr,
    ctor: v8::Local<v8::Function>,
    proto: v8::Local<v8::Object>,
    global: Option<(v8::Local<v8::Object>, &[&str])>,
    check_orphans: bool,
) -> Result<(usize, [Vec<(KeyBp, EntryBp)>; 5]), String> {
    // A template carries own properties, not `[[Prototype]]`s: every realm sets those itself (the parent
    // splice, `wire_chain`). A member install that re-parented either object would be lost.
    let fresh_proto = v8::Object::new(scope).get_prototype(scope);
    if proto.get_prototype(scope) != fresh_proto {
        return Err("the install re-parented the prototype".into());
    }
    let fproto = tr.array_fns[1].as_ref().map(|f| v8::Local::new(scope, f)).and_then(|f| f.get_prototype(scope));
    if ctor.get_prototype(scope) != fproto {
        return Err("the install re-parented the interface object".into());
    }
    let ctor_id = bp_fn(scope, tr, ctor)?;
    let (mut on_proto, mut late) = (Vec::new(), Vec::new());
    // String keys from here on are attached per realm: after `constructor` (V8 adds it after a prototype
    // template's own properties), and after a function a key would misname.
    let mut late_from_here = false;
    for k in own_keys(scope, proto) {
        let is_string = k.is_string();
        if is_string && k.to_rust_string_lossy(scope) == "constructor" {
            late_from_here = true;
            continue;
        }
        let Ok(name) = v8::Local::<v8::Name>::try_from(k) else { return Err("an index key on the prototype".into()) };
        let key = KeyBp::of(scope, k).ok_or("a key that is neither a string nor a well-known symbol")?;
        let d = proto.get_own_property_descriptor(scope, name).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok());
        let Some(d) = d else { return Err("a key with no descriptor".into()) };
        let e = bp_entry(scope, tr, d).map_err(|w| format!("{}: {w}", k.to_rust_string_lossy(scope)))?;
        // An object a template cannot carry is made per realm, after instantiation.
        if matches!(e, EntryBp::Unscopables(..)) {
            late.push((key, e));
            continue;
        }
        if is_string && misnamed_by_key(scope, &tr.names, d, k) {
            late_from_here = true;
        }
        if late_from_here && is_string {
            late.push((key, e));
        } else {
            on_proto.push((key, e));
        }
    }
    let co: v8::Local<v8::Object> = ctor.into();
    let (mut on_ctor, mut late_statics) = (Vec::new(), Vec::new());
    let mut statics_late_from_here = false;
    for k in own_keys(scope, co) {
        let s = k.to_rust_string_lossy(scope);
        if k.is_string() && (s == "length" || s == "name" || s == "prototype") {
            continue; // V8 makes these for a template's function, with the same attributes
        }
        let Ok(name) = v8::Local::<v8::Name>::try_from(k) else { return Err("an index key on the interface object".into()) };
        let key = KeyBp::of(scope, k).ok_or("a static key that is neither a string nor a well-known symbol")?;
        let d = co.get_own_property_descriptor(scope, name).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok());
        let Some(d) = d else { return Err("a static with no descriptor".into()) };
        let e = bp_entry(scope, tr, d).map_err(|w| format!("static {s}: {w}"))?;
        if k.is_string() && misnamed_by_key(scope, &tr.names, d, k) {
            statics_late_from_here = true;
        }
        if statics_late_from_here && k.is_string() {
            late_statics.push((key, e));
        } else {
            on_ctor.push((key, e));
        }
    }
    // A `[Global]` interface's own members: the keys its install put on the GLOBAL OBJECT, in that order.
    let mut on_global = Vec::new();
    if let Some((g, keys)) = global {
        for name in keys {
            let k = v8::String::new(scope, name).ok_or("an unmakeable global key")?;
            let d = g.get_own_property_descriptor(scope, k.into()).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok());
            let Some(d) = d else { return Err(format!("global {name}: not on the global")) };
            match bp_global_entry(scope, tr, d) {
                Ok(e) => on_global.push((KeyBp::Str(name.to_string()), e)),
                // The realm-wide recording: a key it cannot translate is left out (a realm built from
                // templates takes it from the page), not the whole global.
                Err(w) if !check_orphans => report(&format!("GLOBAL-SKIP {name}: {w}")),
                Err(w) => return Err(format!("global {name}: {w}")),
            }
        }
    }
    // AN ORPHAN IS A SIDE EFFECT THE TEMPLATE CANNOT REPLAY: a function the install built that is on
    // neither object -- put on the global, on another interface, or kept by a body for later. A realm
    // instantiating the template would not have it, so the interface keeps the ordinary path.
    for (g, name) in if check_orphans { recorded(scope) } else { Vec::new() } {
        let f = v8::Local::new(scope, &g);
        let h = f.get_identity_hash().get();
        let used = tr.memo.get(&h).is_some_and(|l| l.iter().any(|(m, _)| v8::Local::new(scope, m) == f));
        if !used {
            return Err(format!("an orphan function `{name}` (built by the install, on neither object)"));
        }
    }
    Ok((ctor_id, [on_proto, on_ctor, late, late_statics, on_global]))
}

// ── building an isolate's templates from a blueprint ─────────────────────────────────────────────────

/// One build: the blueprint, its templates so far, the interface (once its object's template exists),
/// and the records its templates' data point at.
struct Bd<'b> {
    /// the template's key (what a snapshot's [`Origin`] names)
    key: &'b str,
    bp: &'b Blueprint,
    fts: Vec<Option<v8::Global<v8::FunctionTemplate>>>,
    iref: Option<IfaceRef>,
    /// the interface's brand id, once its templates carry it (a fused receiver check reads it)
    id: Option<u32>,
    keep: Vec<Box<dyn std::any::Any>>,
    names: Rc<Names>,
}

/// The template of blueprint function `id` in this isolate.
/// A member's recorded label: `Iface.member`, the global's own members under `Window` (the interface
/// whose members they are; [`GLOBAL_TPL`] is only the name the template is kept under).
#[cfg(feature = "analysis")]
fn analysis_label(iface: &str, member: &str) -> &'static str {
    spec::analysis::intern(if iface == GLOBAL_TPL { "Window" } else { iface }, member)
}

/// A function built OUTSIDE every interface template (`binding::protos::build_fn` and kin, a standalone
/// template) as a fused record with no checks, so the recorder's one hook sees it too (korvus-analysis
/// C1): the callback and data to build it with instead of `cb` and `data`. Its label is found on its
/// first call ([`crate::analysis_hook::resolve_label`]). The record lives as long as the process: the
/// function may outlive any cache it could be kept in, and this is an analysis build.
#[cfg(feature = "analysis")]
pub(crate) fn analysis_untemplated<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: v8::FunctionCallback,
    data: Option<v8::Local<v8::Value>>,
    name: &str,
) -> (v8::FunctionCallback, v8::Local<'s, v8::Value>) {
    let data = data.map(|d| v8::Global::new(scope, d));
    let rec = Box::leak(Box::new(FusedRec {
        magic: FUSED_MAGIC,
        data,
        steps: Vec::new(),
        body: cb,
        is_void: false,
        claim: None,
        label: std::cell::Cell::new(""),
        name: spec::analysis::intern_str(name),
        sited: std::cell::Cell::new(0),
    }));
    let d = fn_id(scope, FnSlot::Fused(rec as *const FusedRec), None).expect("more function records than IDs");
    (cb_fused, d)
}

/// A member as a fused record with no checks: its body and the data it was built with.
#[cfg(feature = "analysis")]
fn analysis_fused<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    bd: &mut Bd,
    body: v8::FunctionCallback,
    data: Option<v8::Global<v8::Value>>,
    label: &'static str,
    length: i32,
    behavior: v8::ConstructorBehavior,
) -> v8::Local<'s, v8::FunctionTemplate> {
    let rec = Box::new(FusedRec { magic: FUSED_MAGIC, data, steps: Vec::new(), body, is_void: false, claim: None, label: std::cell::Cell::new(label), name: "", sited: std::cell::Cell::new(0) });
    let ptr = &*rec as *const FusedRec;
    bd.keep.push(rec);
    let d = fn_id(scope, FnSlot::Fused(ptr), None).expect("more function records than IDs");
    v8::FunctionTemplate::builder_raw(crate::binding::refs::c(cb_fused as v8::FunctionCallback)).data(d).length(length).constructor_behavior(behavior).build(scope)
}

fn build_ft<'s>(scope: &mut v8::PinScope<'s, '_>, bd: &mut Bd, id: usize) -> Result<v8::Local<'s, v8::FunctionTemplate>, String> {
    if let Some(Some(t)) = bd.fts.get(id) {
        return Ok(v8::Local::new(scope, t));
    }
    let bp: &Blueprint = bd.bp;
    let fb = &bp.fns[id];
    // In a run that will snapshot its realm, what rebuilds this function's records after a load
    let org = note_fn(scope, bd, fb);
    let behavior = if fb.ctor { v8::ConstructorBehavior::Allow } else { v8::ConstructorBehavior::Throw };
    let length = fb.length;
    // A member a loaded plugin claims is a fused record on `cb_claimed`, whatever its kind; nothing else changes.
    #[cfg(not(feature = "analysis"))]
    let claim = claim_of(&bp.iface, &fb.class_name);
    #[cfg(feature = "analysis")]
    let claim: Option<&'static std::ffi::CStr> = None;
    #[cfg(not(feature = "analysis"))]
    if let (Some(label), false) = (claim, matches!(fb.kind, FnKind::Fused { .. })) {
        let t = claimed_ft(scope, bd, &fb.kind, label, length, behavior, org)?;
        return finish_ft(scope, bd, id, fb, t);
    }
    let t = match &fb.kind {
        #[cfg(not(feature = "analysis"))]
        FnKind::Plain { cb, data } => {
            let mut b = v8::FunctionTemplate::builder_raw(crate::binding::refs::c(*cb)).length(length).constructor_behavior(behavior);
            if let Some(PrimBp::Num(n)) = data {
                if in_fn_id_range(*n) {
                    return Err("a plain function's numeric data in the function-record ID range".into());
                }
            }
            if let Some(d) = data {
                let d = d.v8(scope);
                b = b.data(d);
            }
            b.build(scope)
        }
        #[cfg(not(feature = "analysis"))]
        FnKind::Stub { setter, member, table } => {
            let iref = bd.iref.clone().ok_or("a stub before its interface object")?;
            let (cb, rec) = crate::install::stub::stub_template_of(*setter, *member, *table, iref);
            let ptr = &*rec as *const crate::install::stub::StubTpl;
            bd.keep.push(rec);
            let d = fn_id(scope, FnSlot::Stub(ptr), org).ok_or("more function records than IDs")?;
            v8::FunctionTemplate::builder_raw(crate::binding::refs::c(cb)).data(d).length(length).constructor_behavior(behavior).build(scope)
        }
        #[cfg(not(feature = "analysis"))]
        FnKind::ProtoData { cb } => {
            let iref = bd.iref.clone().ok_or("prototype data before its interface object")?;
            let rec = Box::new(iref);
            let ptr = &*rec as *const IfaceRef;
            bd.keep.push(rec);
            let d = fn_id(scope, FnSlot::Proto(ptr), org).ok_or("more function records than IDs")?;
            v8::FunctionTemplate::builder_raw(crate::binding::refs::c(*cb)).data(d).length(length).constructor_behavior(behavior).build(scope)
        }
        // korvus-analysis: in an analysis build every native member is ONE fused record, so the
        // recorder's single hook (`cb_fused`) sees it -- a plain function, a stub, a prototype-data body.
        // Their bodies read their data through `body_data` (the codemod rule), which answers the record's
        // data, so a body cannot tell. A release build compiles none of this.
        #[cfg(feature = "analysis")]
        FnKind::Plain { cb, data } => {
            let d = data.as_ref().map(|d| {
                let v = d.v8(scope);
                v8::Global::new(scope, v)
            });
            analysis_fused(scope, bd, *cb, d, analysis_label(&bp.iface, &fb.class_name), length, behavior)
        }
        #[cfg(feature = "analysis")]
        FnKind::Stub { setter, member, table } => {
            let iref = bd.iref.clone().ok_or("a stub before its interface object")?;
            let (cb, rec) = crate::install::stub::stub_template_of(*setter, *member, *table, iref);
            let ptr = &*rec as *const crate::install::stub::StubTpl as *mut std::ffi::c_void;
            bd.keep.push(rec);
            let v: v8::Local<v8::Value> = v8::External::new(scope, crate::binding::refs::ext(ptr)).into();
            let d = Some(v8::Global::new(scope, v));
            analysis_fused(scope, bd, cb, d, analysis_label(&bp.iface, &fb.class_name), length, behavior)
        }
        #[cfg(feature = "analysis")]
        FnKind::ProtoData { cb } => {
            let iref = bd.iref.clone().ok_or("prototype data before its interface object")?;
            let rec = Box::new(iref);
            let ptr = &*rec as *const IfaceRef as *mut std::ffi::c_void;
            bd.keep.push(rec);
            let v: v8::Local<v8::Value> = v8::External::new(scope, crate::binding::refs::ext(ptr)).into();
            let d = Some(v8::Global::new(scope, v));
            analysis_fused(scope, bd, *cb, d, analysis_label(&bp.iface, &fb.class_name), length, behavior)
        }
        FnKind::Fused { steps, body, data, is_void } => {
            let fsteps = fused_steps(steps, bd.iref.as_ref(), bd.id)?;
            let mut body = *body;
            let mut body_ix = u32::MAX;
            let data: Option<v8::Global<v8::Value>> = match data {
                BodyDataBp::None => None,
                BodyDataBp::Prim(p) => {
                    let v = p.v8(scope);
                    Some(v8::Global::new(scope, v))
                }
                BodyDataBp::Stub { setter, member, table } => {
                    let iref = bd.iref.clone().ok_or("a fused stub before its interface object")?;
                    let (cb, rec) = crate::install::stub::stub_template_of(*setter, *member, *table, iref);
                    body = cb;
                    let ptr = &*rec as *const crate::install::stub::StubTpl;
                    bd.keep.push(rec);
                    let v = fn_id(scope, FnSlot::Stub(ptr), org.map(|o| Origin { part: 1, ..o })).ok_or("more function records than IDs")?;
                    body_ix = id_index(v);
                    Some(v8::Global::new(scope, v))
                }
                BodyDataBp::ProtoData => {
                    let rec = Box::new(bd.iref.clone().ok_or("fused prototype data before its interface object")?);
                    let ptr = &*rec as *const IfaceRef;
                    bd.keep.push(rec);
                    let v = fn_id(scope, FnSlot::Proto(ptr), org.map(|o| Origin { part: 1, ..o })).ok_or("more function records than IDs")?;
                    body_ix = id_index(v);
                    Some(v8::Global::new(scope, v))
                }
            };
            let rec = Box::new(FusedRec {
                magic: FUSED_MAGIC,
                data,
                steps: fsteps,
                body,
                is_void: *is_void,
                claim,
                #[cfg(feature = "analysis")]
                label: std::cell::Cell::new(analysis_label(&bp.iface, &fb.class_name)),
                #[cfg(feature = "analysis")]
                name: "",
                #[cfg(feature = "analysis")]
                sited: std::cell::Cell::new(0),
            });
            let ptr = &*rec as *const FusedRec;
            bd.keep.push(rec);
            let d = fn_id(scope, FnSlot::Fused(ptr), org.map(|o| Origin { body: body_ix, ..o })).ok_or("more function records than IDs")?;
            let tramp = if claim.is_some() { crate::binding::refs::c(cb_claimed as v8::FunctionCallback) } else { crate::binding::refs::c(cb_fused as v8::FunctionCallback) };
            v8::FunctionTemplate::builder_raw(tramp).data(d).length(length).constructor_behavior(behavior).build(scope)
        }
    };
    finish_ft(scope, bd, id, fb, t)
}

/// The rest of [`build_ft`]: the class name, the brands, the mark, and the per-isolate cache.
fn finish_ft<'s>(scope: &mut v8::PinScope<'s, '_>, bd: &mut Bd, id: usize, fb: &FnBp, t: v8::Local<'s, v8::FunctionTemplate>) -> Result<v8::Local<'s, v8::FunctionTemplate>, String> {
    let n = v8::String::new(scope, &fb.class_name).ok_or("a class name V8 would not make")?;
    t.set_class_name(n);
    let names = bd.names.clone();
    for (i, v) in &fb.brands {
        let k = v8::Local::new(scope, &names.brands[*i]);
        let v = v.v8(scope);
        set_private_on(&t, k, v.into());
    }
    mark_template(scope, &t);
    bd.fts[id] = Some(v8::Global::new(scope, t));
    Ok(t)
}

fn build_entry(scope: &mut v8::PinScope, bd: &mut Bd, e: &EntryBp) -> Result<Entry, String> {
    Ok(match e {
        EntryBp::Value(p, a) => {
            let v = p.v8(scope);
            Entry::Value(v8::Global::new(scope, v), *a)
        }
        EntryBp::Func(id, a) => {
            let t = build_ft(scope, bd, *id)?;
            Entry::Func(v8::Global::new(scope, t), *a)
        }
        EntryBp::Intrinsic(i, a) => Entry::Intrinsic(*i, *a),
        EntryBp::Accessor(g, s, a) => {
            let g = match g {
                Some(id) => {
                    let t = build_ft(scope, bd, *id)?;
                    Some(v8::Global::new(scope, t))
                }
                None => None,
            };
            let s = match s {
                Some(id) => {
                    let t = build_ft(scope, bd, *id)?;
                    Some(v8::Global::new(scope, t))
                }
                None => None,
            };
            Entry::Accessor(g, s, *a)
        }
        EntryBp::Namespace(rows, a) => {
            let mut out = Vec::new();
            for (k, e) in rows {
                let key = k.v8(scope).ok_or("an unmakeable key")?;
                let e = build_entry(scope, bd, e)?;
                out.push((v8::Global::new(scope, key), e));
            }
            Entry::Namespace(out, *a)
        }
        EntryBp::Unscopables(names, a) => Entry::Unscopables(names.clone(), *a),
    })
}

/// This isolate's templates for an interface, from its blueprint (and its parent's template, if this
/// isolate has one): the records their data points at go into `keep`.
fn build(scope: &mut v8::PinScope, key: &str, bp: &Blueprint, parent_ft: Option<v8::Global<v8::FunctionTemplate>>, keep: &mut Vec<Box<dyn std::any::Any>>) -> Result<Tpl, String> {
    let names = names(scope);
    let mut bd = Bd { key, bp, fts: vec![None; bp.fns.len()], iref: None, id: None, keep: Vec::new(), names };
    let ct = build_ft(scope, &mut bd, bp.ctor)?;
    if let Some(p) = parent_ft {
        let p = v8::Local::new(scope, &p);
        ct.inherit(p);
    }
    // a non-constructible interface object (`NodeFilter`) has no `prototype` to make read-only
    let constructible = bp.fns[bp.ctor].ctor;
    if constructible {
        ct.read_only_prototype();
    }
    bd.iref = Some(IfaceRef::of(v8::Global::new(scope, ct), bd.names.prototype.clone()));
    let pt = ct.prototype_template(scope);
    // The prototype's interface name -- the seal's stamp -- from the template, so every realm's prototype
    // carries it from the moment it is made: the objects an install mints before its seal
    // ran are minted from their interface's template as well (`protos::mint_on`).
    if constructible && !bp.iface.starts_with('#') {
        if let (Some(k), Some(n)) = (crate::binding::protos::iface_name_private(scope), v8::String::new(scope, &bp.iface)) {
            set_private_on(&pt, k, n.into());
        }
        // The BRAND (`binding::brand`): the interface's id on every instance this template makes,
        // and on its prototype, in every realm -- "which interface made this object", asked by the checks.
        let id = crate::binding::brand::iface_id(&bp.iface, bp.parent.as_deref());
        if reporting() {
            report(&format!("BRAND-ID {} parent={:?}", bp.iface, bp.parent));
        }
        let idv = v8::Integer::new_from_unsigned(scope, id);
        let bk = crate::binding::brand::brand_key(scope);
        let it = ct.instance_template(scope);
        set_private_on(&it, bk, idv.into());
        // and the field a check caches it in (`binding::brand`); a list's template three more, for its hot state
        // (`collections::LIST_FIELDS`)
        it.set_internal_field_count(if crate::core::dom::collections::list_shape(&bp.iface).is_some() { crate::core::dom::collections::LIST_FIELDS } else { 1 });
        // and the interceptors an interface with indexed or named properties answers them from (D1)
        crate::install::instance_shapes::configure(scope, &bp.iface, it);
        // and the private slots the engine keeps on its instances, so a mint's writes add no property
        crate::install::instance_shapes::declare_engine_slots(scope, &bp.iface, it);
        let pk = crate::binding::brand::proto_id_key(scope);
        set_private_on(&pt, pk, idv.into());
        bd.id = Some(id);
    }
    for (k, e) in &bp.proto {
        let key = k.v8(scope).ok_or("an unmakeable key")?;
        let e = build_entry(scope, &mut bd, e)?;
        put_on(scope, On::Proto(&pt), key, &e);
    }
    for (k, e) in &bp.statics {
        let key = k.v8(scope).ok_or("an unmakeable key")?;
        let e = build_entry(scope, &mut bd, e)?;
        put_on(scope, On::Iface(&ct), key, &e);
    }
    let mut late = Late { proto: Vec::new(), statics: Vec::new() };
    for (list, out) in [(&bp.late_proto, &mut late.proto), (&bp.late_statics, &mut late.statics)] {
        for (k, e) in list {
            let key = k.v8(scope).ok_or("an unmakeable key")?;
            let e = build_entry(scope, &mut bd, e)?;
            out.push((v8::Global::new(scope, key), e));
        }
    }
    let mut global = Vec::new();
    for (k, e) in &bp.global {
        let key = k.v8(scope).ok_or("an unmakeable key")?;
        let e = build_entry(scope, &mut bd, e)?;
        global.push((v8::Global::new(scope, key), e));
    }
    keep.append(&mut bd.keep);
    let global_index = bp.global.iter().enumerate().filter_map(|(i, (k, _))| match k {
        KeyBp::Str(s) => Some((s.clone(), i)),
        KeyBp::Sym(_) => None,
    }).collect();
    Ok(Tpl { ctor: v8::Global::new(scope, ct), late, replay: bp.replay.clone(), constructible, global, iface: bp.iface.clone(), parent: bp.parent.clone(), global_index })
}

/// Whether a template would give the function in this data property the wrong name: V8 names a
/// template's function after the string key it is instantiated under, before its class name.
fn misnamed_by_key(scope: &mut v8::PinScope, names: &Names, d: v8::Local<v8::Object>, key: v8::Local<v8::Value>) -> bool {
    let vk = v8::Local::new(scope, &names.value);
    let Some(v) = d.get(scope, vk.into()) else { return false };
    let Ok(f) = v8::Local::<v8::Function>::try_from(v) else { return false };
    let n = f.get_name(scope).to_rust_string_lossy(scope);
    n != key.to_rust_string_lossy(scope)
}

/// A realm's interface object and prototype, from the cached template.
pub(crate) fn instantiate<'s>(scope: &mut v8::PinScope<'s, '_>, t: &Tpl) -> Option<(v8::Local<'s, v8::Function>, v8::Local<'s, v8::Object>)> {
    with_cache(scope, |c| c.counts.instantiated += 1);
    let ct = v8::Local::new(scope, &t.ctor);
    let ctor = ct.get_function(scope)?;
    let k = v8::String::new(scope, "prototype")?;
    let proto = match ctor.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(p) => p,
        // an interface object with no `prototype` (`NodeFilter`): the caller gets an object nothing holds
        None if !t.constructible => v8::Object::new(scope),
        None => return None,
    };
    let co: v8::Local<v8::Object> = ctor.into();
    let lists = [(proto, &t.late.proto), (co, &t.late.statics)];
    for (target, list) in lists {
        attach(scope, target, list)?;
    }
    note_known(scope, ctor, &format!("iface {}", t.iface));
    snapshot_interface(scope, &t.iface, ctor, proto);
    Some((ctor, proto))
}

/// The PAGE realm's interface `iface` as its template has it -- just made from the template -- until the
/// install is over: what the install changes after this is what a realm built from templates, which runs
/// no install, must be given ([`record_deltas`]). Only a page realm whose bootstrap decided to record: a
/// worker's install is not measured.
fn snapshot_interface(scope: &mut v8::PinScope, iface: &str, ctor: v8::Local<v8::Function>, proto: v8::Local<v8::Object>) {
    if !with_cache(scope, |c| !c.persisted && c.delta_mode == Some(true)) || is_lazy_realm(scope) {
        return;
    }
    let co: v8::Local<v8::Object> = ctor.into();
    let snap = [(proto, snap_rows(scope, proto)), (co, snap_rows(scope, co))];
    let snap = snap.map(|(o, rows)| (v8::Global::new(scope, o), rows));
    with_cache(scope, |c| c.snaps.push((iface.to_string(), snap)));
}

/// A key as a report names it: a string as itself, a symbol as `@@<description>`.
fn key_label(scope: &mut v8::PinScope, k: v8::Local<v8::Value>) -> String {
    match v8::Local::<v8::Symbol>::try_from(k) {
        Ok(sym) => format!("@@{}", sym.description(scope).to_rust_string_lossy(scope)),
        Err(_) => k.to_rust_string_lossy(scope),
    }
}

fn same_descriptor(scope: &mut v8::PinScope, a: v8::Local<v8::Object>, b: v8::Local<v8::Object>) -> bool {
    let names = names(scope);
    for g in [&names.value, &names.get, &names.set, &names.writable, &names.enumerable, &names.configurable] {
        let k = v8::Local::new(scope, g);
        let (x, y) = (a.get(scope, k.into()), b.get(scope, k.into()));
        let same = match (x, y) {
            (Some(x), Some(y)) => x.strict_equals(y),
            (None, None) => true,
            _ => false,
        };
        if !same {
            return false;
        }
    }
    true
}


/// Whether this isolate's page realm runs the realm-wide recording: its surface has no global template or
/// no record of what its install changes yet, in the process or in the blueprint file.
pub(crate) fn records_global(scope: &mut v8::PinScope) -> bool {
    let rid = realm_identity(scope);
    recording_deltas(scope) || blueprint(scope, &format!("{GLOBAL_TPL}|{rid}")).is_none()
}

/// Whether this isolate's page realm records what its install changes after instantiating each interface:
/// only while the process (or the blueprint file) has no record for its surface -- the changes are the
/// install's, the same for every page of the surface, and the snapshots and the comparison cost a page
/// ~25 ms (measured 2026-09-28), which only the first page of a surface pays.
fn recording_deltas(scope: &mut v8::PinScope) -> bool {
    if let Some(m) = with_cache(scope, |c| c.delta_mode) {
        return m;
    }
    let rid = realm_identity(scope);
    let m = blueprint(scope, &format!("#deltas|{rid}")).is_none();
    with_cache(scope, |c| {
        c.delta_mode = Some(m);
        c.rid = Some(rid);
    });
    m
}

fn snap_rows(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Vec<(v8::Global<v8::Value>, v8::Global<v8::Object>)> {
    let mut out = Vec::new();
    for k in own_keys(scope, o) {
        let Ok(name) = v8::Local::<v8::Name>::try_from(k) else { continue };
        let Some(d) = o.get_own_property_descriptor(scope, name).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) else { continue };
        out.push((v8::Global::new(scope, k), v8::Global::new(scope, d)));
    }
    out
}

/// WHAT THE PAGE'S INSTALL CHANGED on each interface after instantiating its template: a
/// pass re-seats `constructor`, borrows another interface's member (SVG's `HTMLOrSVGElement` rows,
/// `OffscreenCanvasRenderingContext2D`'s canvas rows) and re-aims its receiver check at the borrower, adds
/// `@@iterator`, wraps a member in an arity guard, takes an unpublished shell's `constructor` away. A realm
/// built from templates runs no install, so it is given the same: for each holder that changed, its
/// complete final own-key list, each key either the template's own descriptor or the install's, part by
/// part -- the template's own, another template-made slot (by identity), that slot re-aimed, the own part
/// arity-guarded, or a function the realm-wide recording (still running) made. NOT a re-run of the passes:
/// a pass ran against the surface as it was at that moment of the page's install, which a finished realm
/// is not (`SVGStyleElement.disabled` is borrowed from nothing in the page, because its donor did not have
/// it yet). Built for this isolate now, applied by [`realm_interface`]. Measured on a blank page
/// 2026-09-28: 37 holders.
fn record_deltas(scope: &mut v8::PinScope) {
    let (snaps, recording) = with_cache(scope, |c| {
        c.persisted = true;
        (std::mem::take(&mut c.snaps), c.delta_mode == Some(true))
    });
    if !recording {
        return;
    }
    let rid = realm_identity(scope);
    let names = names(scope);
    let array_fns: Vec<Option<v8::Global<v8::Function>>> = Intr::ALL.iter().map(|(_, n)| array_proto_fn(scope, n).map(|f| v8::Global::new(scope, f))).collect();
    // the page's own instantiation of each interface (a later one is a re-create the page does not hold)
    let mut seen = std::collections::HashSet::new();
    let snaps: Vec<_> = snaps.into_iter().filter(|(i, _)| seen.insert(i.clone())).collect();
    // every snapshot row by its key's identity (keys are internalized names), for the slot searches
    let mut by_key: HashMap<i32, Vec<(usize, usize, usize)>> = HashMap::new();
    for (si, (_, holders)) in snaps.iter().enumerate() {
        for (hi, (_, rows)) in holders.iter().enumerate() {
            for (ri, (k, _)) in rows.iter().enumerate() {
                let k = v8::Local::new(scope, k);
                if let Ok(n) = v8::Local::<v8::Name>::try_from(k) {
                    by_key.entry(n.get_identity_hash().get()).or_default().push((si, hi, ri));
                }
            }
        }
    }
    let parts_k: [v8::Local<v8::String>; 3] = [v8::Local::new(scope, &names.value), v8::Local::new(scope, &names.get), v8::Local::new(scope, &names.set)];
    // The template-made slot (any interface, same key) holding `f` itself.
    let find_slot = |scope: &mut v8::PinScope, key: v8::Local<v8::Name>, f: v8::Local<v8::Value>| -> Option<SlotBp> {
        let cands = by_key.get(&key.get_identity_hash().get())?;
        for &(si, hi, ri) in cands {
            let (k, d) = &snaps[si].1[hi].1[ri];
            if v8::Local::new(scope, k) != Into::<v8::Local<v8::Value>>::into(key) {
                continue;
            }
            let d = v8::Local::new(scope, d);
            for (pi, pk) in parts_k.iter().enumerate() {
                let Some(v) = d.get(scope, (*pk).into()) else { continue };
                if v == f {
                    let kb = KeyBp::of(scope, key.into())?;
                    return Some(SlotBp { iface: snaps[si].0.clone(), holder: hi as u8, key: kb, part: pi as u8 });
                }
            }
        }
        None
    };
    // (label, snapshot holders, the final objects)
    let work: Vec<(String, &[(v8::Global<v8::Object>, Vec<(v8::Global<v8::Value>, v8::Global<v8::Object>)>); 2], [v8::Global<v8::Object>; 2])> =
        snaps.iter().map(|(i, h)| (i.clone(), h, [h[0].0.clone(), h[1].0.clone()])).collect();
    for (iface, holders, finals) in work {
        let iface = &iface;
        let proto = v8::Local::new(scope, &finals[0]);
        let ctor = v8::Local::<v8::Function>::try_from(v8::Local::new(scope, &finals[1])).ok();
        let mut tr = Tr { proto: v8::Global::new(scope, proto), iface_fn: ctor.map(|c| v8::Global::new(scope, c)), memo: HashMap::new(), fns: Vec::new(), names: names.clone(), array_fns: array_fns.clone() };
        let mut out: Vec<(u8, Vec<(KeyBp, DeltaBp)>)> = Vec::new();
        let mut why = None;
        'holders: for (hi, (_, rows)) in holders.iter().enumerate().filter(|_| why.is_none()) {
            let o = v8::Local::new(scope, &finals[hi]);
            let now = snap_rows(scope, o);
            let mut list = Vec::new();
            let mut changed = now.len() != rows.len();
            for (i, (k, d)) in now.iter().enumerate() {
                let k = v8::Local::new(scope, k);
                let d = v8::Local::new(scope, d);
                let (Some(key), Ok(kn)) = (KeyBp::of(scope, k), v8::Local::<v8::Name>::try_from(k)) else {
                    why = Some("a key a template cannot carry".to_string());
                    break 'holders;
                };
                let before = rows.iter().position(|(bk, _)| v8::Local::new(scope, bk) == k);
                changed |= before != Some(i);
                let old = before.map(|b| v8::Local::new(scope, &rows[b].1));
                if let Some(od) = old {
                    if same_descriptor(scope, od, d) {
                        list.push((key, DeltaBp::Keep));
                        continue;
                    }
                }
                changed = true;
                let data = d.has_own_property(scope, parts_k[0].into()).unwrap_or(false);
                let mut parts = [PartBp::Absent, PartBp::Absent, PartBp::Absent];
                for pi in 0..3 {
                    if data != (pi == 0) {
                        continue;
                    }
                    let Some(v) = d.get(scope, parts_k[pi].into()).filter(|v| !v.is_undefined()) else { continue };
                    let own = old.and_then(|od| od.get(scope, parts_k[pi].into()));
                    let is_iface = ctor.is_some_and(|c| Into::<v8::Local<v8::Value>>::into(c) == v);
                    parts[pi] = if own.is_some_and(|o| o == v) {
                        PartBp::Same
                    } else if is_iface {
                        PartBp::Iface
                    } else if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
                        // the same key of any interface (a borrow), else another key of this holder (`@@iterator`
                        // IS `entries`)
                        let mut slot = find_slot(scope, kn, v);
                        if slot.is_none() {
                            'rows: for (bk, bd) in rows.iter() {
                                let bd = v8::Local::new(scope, bd);
                                for pi in 0..3u8 {
                                    if bd.get(scope, parts_k[pi as usize].into()).is_some_and(|x| x == v) {
                                        let bk = v8::Local::new(scope, bk);
                                        slot = KeyBp::of(scope, bk).map(|kb| SlotBp { iface: iface.clone(), holder: hi as u8, key: kb, part: pi });
                                        break 'rows;
                                    }
                                }
                            }
                        }
                        // Anything else is a function of its own, from a template like every other (the delta's
                        // own wrappers -- an arity guard, a ToNumber setter, a re-aimed receiver guard over the
                        // template's part, each built per realm at apply time -- were never needed once the
                        // passes that made them were gone, and were deleted).
                        if let Some(slot) = slot {
                            PartBp::Ref(slot)
                        } else {
                            match bp_fn(scope, &mut tr, f) {
                                Ok(id) => PartBp::Fn(id),
                                Err(w) => {
                                    why = Some(format!("{}: {w}", k.to_rust_string_lossy(scope)));
                                    break 'holders;
                                }
                            }
                        }
                    } else {
                        match PrimBp::of(scope, v).filter(|_| primitive(v)) {
                            Some(p) => PartBp::Prim(p),
                            None => {
                                why = Some(format!("{}: an object value", k.to_rust_string_lossy(scope)));
                                break 'holders;
                            }
                        }
                    };
                }
                if reporting() {
                    let why = if old.is_none() { "added" } else { "changed" };
                    report(&format!("DELTA-DETAIL {iface} {} {} {why}", ["proto", "iface"][hi], key_label(scope, k)));
                }
                list.push((key, DeltaBp::Changed { data, parts, attrs: attrs_of(scope, &names, d, data) }));
            }
            if changed && reporting() {
                for (bk, _) in rows.iter() {
                    let bk = v8::Local::new(scope, bk);
                    if !now.iter().any(|(k, _)| v8::Local::new(scope, k) == bk) {
                        report(&format!("DELTA-DETAIL {iface} {} {} removed", ["proto", "iface"][hi], key_label(scope, bk)));
                    }
                }
            }
            if changed {
                out.push((hi as u8, list));
            }
        }
        if let Some(w) = why {
            report(&format!("DELTA-REFUSED {iface}: {w}"));
            continue;
        }
        if out.is_empty() {
            continue;
        }
        let Some(fns) = tr.fns.into_iter().collect::<Option<Vec<_>>>() else {
            report(&format!("DELTA-REFUSED {iface}: a function left half-translated"));
            continue;
        };
        report(&format!("DELTA {iface}: {} holder(s)", out.len()));
        let bp = Blueprint { iface: iface.clone(), fns, ctor: 0, proto: Vec::new(), statics: Vec::new(), late_proto: Vec::new(), late_statics: Vec::new(), global: Vec::new(), parent: None, replay: Default::default(), from_disk: false, deltas: Some(DeltaSet { holders: out }) };
        if let Ok(mut m) = process_blueprints().lock() {
            m.insert(format!("#delta:{iface}|{rid}"), std::sync::Arc::new(bp));
        }
    }
    // this surface's record is complete: no later page of it records again
    let done = Blueprint { iface: "#deltas".into(), fns: Vec::new(), ctor: 0, proto: Vec::new(), statics: Vec::new(), late_proto: Vec::new(), late_statics: Vec::new(), global: Vec::new(), parent: None, replay: Default::default(), from_disk: false, deltas: None };
    if let Ok(mut m) = process_blueprints().lock() {
        m.insert(format!("#deltas|{rid}"), std::sync::Arc::new(done));
    }
    DIRTY.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// What the page's install changed on `iface`, built for this isolate from the process's record the first
/// time a realm here makes it ([`record_deltas`]).
fn delta_entries(scope: &mut v8::PinScope, iface: &str) -> Option<Rc<Vec<(u8, Vec<(v8::Global<v8::Name>, DeltaE)>)>>> {
    if let Some(d) = with_cache(scope, |c| c.deltas.get(iface).cloned()) {
        return d;
    }
    let built = (|| {
        let rid = with_cache(scope, |c| c.rid.clone())?;
        let bp = blueprint(scope, &format!("#delta:{iface}|{rid}"))?;
        let set = bp.deltas.as_ref()?;
        let names = names(scope);
        let tpl = tpl_by_name(scope, iface)?;
        let iref = IfaceRef::of(tpl.ctor.clone(), names.prototype.clone());
        // The key of the template whose interface the delta's functions check against, for
        // their origins (empty: none is kept, and a snapshot refuses)
        let iref_key = match crate::binding::refs::recording() {
            true => with_cache(scope, |c| c.tpls.iter().find(|(_, t)| Rc::ptr_eq(t, &tpl)).map(|(k, _)| k.clone())).unwrap_or_default(),
            false => String::new(),
        };
        // (a delta's checks read the interface's prototype: `id` stays unset)
        let mut bd = Bd { key: &iref_key, bp: &bp, fts: vec![None; bp.fns.len()], iref: Some(iref), id: None, keep: Vec::new(), names: names.clone() };
        let mut built = Vec::new();
        for (hi, list) in &set.holders {
            let mut rows = Vec::new();
            for (k, d) in list {
                let key = k.v8(scope)?;
                let e = match d {
                    DeltaBp::Keep => DeltaE::Keep,
                    DeltaBp::Changed { data, parts, attrs } => {
                        let mut pe = [PartE::Absent, PartE::Absent, PartE::Absent];
                        for (pi, p) in parts.iter().enumerate() {
                            pe[pi] = match p {
                                PartBp::Absent => PartE::Absent,
                                PartBp::Same => PartE::Same,
                                PartBp::Prim(p) => {
                                    let v = p.v8(scope);
                                    PartE::Prim(v8::Global::new(scope, v))
                                }
                                PartBp::Fn(id) => {
                                    let t = build_ft(scope, &mut bd, *id).ok()?;
                                    PartE::Fn(v8::Global::new(scope, t))
                                }
                                PartBp::Ref(sl) => {
                                    let sk = sl.key.v8(scope)?;
                                    let sk = v8::Global::new(scope, sk);
                                    PartE::Ref(sl.iface.clone(), sl.holder, sk, sl.part)
                                }
                                PartBp::Iface => PartE::Iface,
                            };
                        }
                        DeltaE::Changed { data: *data, parts: pe, attrs: *attrs }
                    }
                };
                rows.push((v8::Global::new(scope, key), e));
            }
            built.push((*hi, rows));
        }
        let keep = std::mem::take(&mut bd.keep);
        with_cache(scope, |c| c.keep.extend(keep));
        Some(Rc::new(built))
    })();
    with_cache(scope, |c| c.deltas.insert(iface.to_string(), built.clone()));
    built
}

/// Give a realm's interface what the page's install changed on it ([`record_deltas`]): each changed holder's
/// own keys re-defined in the page's final order, as the page has them.
fn apply_deltas<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, ctor: v8::Local<'s, v8::Function>, proto: v8::Local<'s, v8::Object>) {
    let Some(list) = delta_entries(scope, iface) else { return };
    let names = names(scope);
    let parts_k: [v8::Local<'s, v8::String>; 3] = [v8::Local::new(scope, &names.value), v8::Local::new(scope, &names.get), v8::Local::new(scope, &names.set)];
    for (hi, rows) in list.iter() {
        let holder: v8::Local<'s, v8::Object> = if *hi == 0 { proto } else { ctor.into() };
        // the template's own descriptors, before anything moves
        let source: v8::Local<'s, v8::Object> = holder;
        let mut saved: Vec<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>)> = Vec::new();
        for k in own_keys(scope, source) {
            let Ok(n) = v8::Local::<v8::Name>::try_from(k) else { continue };
            let Some(d) = source.get_own_property_descriptor(scope, n).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) else { continue };
            saved.push((k, d));
        }
        // every part resolved before the holder is emptied (a slot of this holder reads the saved list)
        let mut resolved: Vec<Option<(bool, [Option<v8::Local<'s, v8::Value>>; 3], Attrs)>> = Vec::with_capacity(rows.len());
        for (k, e) in rows.iter() {
            let key: v8::Local<'s, v8::Value> = v8::Local::new(scope, k).into();
            resolved.push(match e {
                DeltaE::Keep => None,
                DeltaE::Changed { data, parts, attrs } => {
                    let mut vals: [Option<v8::Local<'s, v8::Value>>; 3] = [None, None, None];
                    for (pi, p) in parts.iter().enumerate() {
                        vals[pi] = match p {
                            PartE::Absent => None,
                            PartE::Same => saved_part(scope, &saved, key, parts_k[pi]),
                            PartE::Prim(v) => Some(v8::Local::new(scope, v)),
                            PartE::Fn(t) => v8::Local::new(scope, t).get_function(scope).map(|f| f.into()),
                            PartE::Ref(i, h, sk, sp) => slot_value(scope, iface, *hi, &saved, &parts_k, i, *h, sk, *sp),
                            PartE::Iface => Some(ctor.into()),
                        };
                    }
                    Some((*data, vals, *attrs))
                }
            });
        }
        // The holder's keys in the page's final order, moving as little as possible: a key the list does not
        // have goes; the longest prefix already in that order is re-defined where it is (a define on an
        // existing key keeps its place, and a non-configurable one cannot move); from the first key out of
        // place on, each is taken out and put back in order.
        let targets: Vec<v8::Local<v8::Value>> = rows.iter().map(|(k, _)| v8::Local::new(scope, k).into()).collect();
        let current = own_keys(scope, holder);
        for k in &current {
            if !targets.contains(k) {
                if let Ok(n) = v8::Local::<v8::Name>::try_from(*k) {
                    holder.delete(scope, n.into());
                }
            }
        }
        let current: Vec<v8::Local<v8::Value>> = current.into_iter().filter(|k| targets.contains(k)).collect();
        let in_place = targets.iter().zip(&current).take_while(|(a, b)| a == b).count();
        for k in &targets[in_place..] {
            if let Ok(n) = v8::Local::<v8::Name>::try_from(*k) {
                holder.delete(scope, n.into());
            }
        }
        for (i, (k, _)) in rows.iter().enumerate() {
            let key = v8::Local::new(scope, k);
            match &resolved[i] {
                None => {
                    let kv: v8::Local<v8::Value> = key.into();
                    if let Some((_, d)) = saved.iter().find(|(sk, _)| *sk == kv) {
                        let pd = descriptor_from(scope, &names, *d);
                        holder.define_property(scope, key, &pd);
                    }
                }
                Some((data, vals, a)) => {
                    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
                    let mut pd = if *data {
                        v8::PropertyDescriptor::new_from_value_writable(vals[0].unwrap_or(u), !a.read_only)
                    } else {
                        v8::PropertyDescriptor::new_from_get_set(vals[1].unwrap_or(u), vals[2].unwrap_or(u))
                    };
                    pd.set_enumerable(!a.dont_enum);
                    pd.set_configurable(!a.dont_delete);
                    holder.define_property(scope, key, &pd);
                }
            }
        }
    }
}

/// Part `pk` (`value`/`get`/`set`) of the saved descriptor under `key`.
fn saved_part<'s>(scope: &mut v8::PinScope<'s, '_>, saved: &[(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>)], key: v8::Local<'s, v8::Value>, pk: v8::Local<'s, v8::String>) -> Option<v8::Local<'s, v8::Value>> {
    let (_, d) = saved.iter().find(|(k, _)| *k == key)?;
    d.get(scope, pk.into()).filter(|v| !v.is_undefined())
}

/// The function a slot holds in THIS realm: the holder being changed as its template made it (`saved`), or
/// another interface's, made now if it is not yet.
#[allow(clippy::too_many_arguments)]
fn slot_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    hi: u8,
    saved: &[(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>)],
    parts_k: &[v8::Local<'s, v8::String>; 3],
    i: &str,
    h: u8,
    k: &v8::Global<v8::Name>,
    p: u8,
) -> Option<v8::Local<'s, v8::Value>> {
    let k = v8::Local::new(scope, k);
    if i == iface && h == hi {
        return saved_part(scope, saved, k.into(), parts_k[p as usize]);
    }
    let (c, pr) = realm_interface(scope, i)?;
    let o: v8::Local<v8::Object> = if h == 0 { pr } else { c.into() };
    let d = o.get_own_property_descriptor(scope, k).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok())?;
    d.get(scope, parts_k[p as usize].into()).filter(|v| !v.is_undefined())
}

/// A descriptor object's [`v8::PropertyDescriptor`].
fn descriptor_from(scope: &mut v8::PinScope, names: &Names, d: v8::Local<v8::Object>) -> v8::PropertyDescriptor {
    let flag = |scope: &mut v8::PinScope, k: &v8::Global<v8::String>| {
        let k = v8::Local::new(scope, k);
        d.get(scope, k.into()).is_some_and(|v| v.is_true())
    };
    let vk = v8::Local::new(scope, &names.value);
    let mut pd = if d.has_own_property(scope, vk.into()).unwrap_or(false) {
        let v = d.get(scope, vk.into()).unwrap_or_else(|| v8::undefined(scope).into());
        let w = flag(scope, &names.writable);
        v8::PropertyDescriptor::new_from_value_writable(v, w)
    } else {
        let gk = v8::Local::new(scope, &names.get);
        let sk = v8::Local::new(scope, &names.set);
        let g = d.get(scope, gk.into()).unwrap_or_else(|| v8::undefined(scope).into());
        let s = d.get(scope, sk.into()).unwrap_or_else(|| v8::undefined(scope).into());
        v8::PropertyDescriptor::new_from_get_set(g, s)
    };
    let e = flag(scope, &names.enumerable);
    let c = flag(scope, &names.configurable);
    pd.set_enumerable(e);
    pd.set_configurable(c);
    pd
}

/// Under `KORVUS_TPL_REPORT` only: note that this isolate's templates made `f` (an interface object, an
/// attached member), by identity -- [`report_global`]'s key. Kept in the isolate's cache: a handle must not
/// outlive its isolate (a successor document is another isolate of the same thread).
fn note_known(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, label: &str) {
    if !reporting() {
        return;
    }
    let h = f.get_identity_hash().get();
    let g = v8::Global::new(scope, f);
    with_cache(scope, |c| c.known.entry(h).or_default().push((g, label.to_string())));
}

fn known_label(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> Option<String> {
    let h = f.get_identity_hash().get();
    let found: Vec<(v8::Global<v8::Function>, String)> = with_cache(scope, |c| c.known.get(&h).cloned().unwrap_or_default());
    found.into_iter().find(|(g, _)| v8::Local::new(scope, g) == f).map(|(_, l)| l)
}

/// The keys of this realm's global the global's own template records ([`finish_global_capture`]): its own
/// properties, in order, that are neither an intrinsic (a fresh context has it), an interface object (a
/// realm built from templates makes those lazily), one of `Window`'s own members (`Window`'s template has
/// them), an object, nor a singleton whose value lives in a slot a new realm does not have yet.
fn global_record_keys(scope: &mut v8::PinScope) -> Vec<String> {
    let intrinsics = fresh_context_keys(scope);
    let window: std::collections::HashSet<String> = tpl_by_name(scope, "Window").map(|t| t.global_index.keys().cloned().collect()).unwrap_or_default();
    let global = scope.get_current_context().global(scope);
    let names = names(scope);
    let mut out = Vec::new();
    let any_lazy = has_lazy_globals(scope);
    for k in own_keys(scope, global) {
        if !k.is_string() {
            continue;
        }
        let name = k.to_rust_string_lossy(scope);
        if intrinsics.contains(&name) || window.contains(&name) {
            continue;
        }
        // a page interface put lazily: an interface object, which this walk leaves out below -- and
        // reading its descriptor would make it. The global's capture runs on every page install, a hit too.
        if any_lazy && lazy_global(scope, global, &name).is_some() {
            continue;
        }
        let Ok(key) = v8::Local::<v8::Name>::try_from(k) else { continue };
        let Some(d) = global.get_own_property_descriptor(scope, key).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) else { continue };
        let vk = v8::Local::new(scope, &names.value);
        match d.get(scope, vk.into()).filter(|v| !v.is_undefined()) {
            Some(v) => match v8::Local::<v8::Function>::try_from(v) {
                Ok(f) => {
                    if interface_name_of_value(scope, f).is_some() || owns_foreign_prototype(scope, f) {
                        continue;
                    }
                }
                // a namespace (`CSS`, `chrome`): recorded with its members ([`bp_global_entry`])
                Err(_) if v.is_object() && !v.is_array() => {}
                Err(_) => continue,
            },
            None => {
                // A slot-backed singleton's accessor reads the calling realm's slot, so a realm built from
                // templates can take it only when it can make the object itself ([`window_globals::Mint`]);
                // `document` and `location` are every frame's own, filled by the frame.
                if name != "document"
                    && name != "location"
                    && crate::core::html::window_globals::window_slot_get(scope, &name).is_some()
                    && !crate::core::html::window_globals::has_mint(&name)
                {
                    continue;
                }
            }
        }
        out.push(name);
    }
    out
}

/// The own keys of a fresh context's global: what V8 gives every realm.
pub(crate) fn fresh_context_keys(scope: &mut v8::PinScope) -> std::collections::HashSet<String> {
    // the same for every context of this binary: made once (a throwaway context costs a frame ~0.4 ms)
    static KEYS: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    if let Some(k) = KEYS.get() {
        return k.clone();
    }
    let fresh = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, fresh);
    let g = fresh.global(scope);
    let keys: std::collections::HashSet<String> = own_keys(scope, g).into_iter().map(|k| k.to_rust_string_lossy(scope)).collect();
    let _ = KEYS.set(keys.clone());
    keys
}

/// `f`'s interface name when `f` IS an interface object: its `prototype` carries an interface stamp this
/// isolate has a template for, and that prototype's `constructor` is `f` itself (an alias such as
/// `webkitURL` answers `URL`).
pub(crate) fn interface_name_of_value(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> Option<String> {
    // BY ITS OWN NAME, which an interface object always carries (`webkitURL` holds a function named `URL`); a
    // prototype's name stamp can be a legacy factory's (`Option` shares `HTMLOptionElement`'s prototype).
    let n = f.get_name(scope).to_rust_string_lossy(scope);
    tpl_by_name(scope, &n)?;
    let pk = v8::String::new(scope, "prototype")?;
    let fo: v8::Local<v8::Object> = f.into();
    // An interface object with no `prototype` (`NodeFilter`).
    if !fo.has_own_property(scope, pk.into()).unwrap_or(false) {
        return Some(n);
    }
    let p = fo.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let ck = v8::String::new(scope, "constructor")?;
    let c = p.get(scope, ck.into())?;
    let fv: v8::Local<v8::Value> = f.into();
    (c == fv).then_some(n)
}

/// Whether `f`'s own `prototype` is another interface's (a legacy factory: `Image`, `Audio`, `Option`), which
/// a template cannot carry.
fn owns_foreign_prototype(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) -> bool {
    let Some(pk) = v8::String::new(scope, "prototype") else { return false };
    let fo: v8::Local<v8::Object> = f.into();
    fo.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()).is_some_and(|p| crate::binding::protos::iface_name_of(scope, p).is_some())
}

/// Under `KORVUS_TPL_REPORT`: every own property of this realm's global, as a global template would
/// have to give it -- an interface object a template made, a member a template attached, a
/// V8 intrinsic, an object, or something no template knows -- one `GLOBAL <class> <key>` line each for
/// the last kinds, and a `GLOBAL-SUMMARY` line.
pub(crate) fn report_global(scope: &mut v8::PinScope) {
    if !reporting() {
        return;
    }
    let intrinsics: std::collections::HashSet<String> = {
        let fresh = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, fresh);
        let g = fresh.global(scope);
        own_keys(scope, g).into_iter().map(|k| k.to_rust_string_lossy(scope)).collect()
    };
    let global = scope.get_current_context().global(scope);
    let (mut iface, mut entry, mut intrinsic, mut object, mut unknown, mut prim) = (0, 0, 0, 0, 0, 0);
    let names = names(scope);
    let any_lazy = has_lazy_globals(scope);
    for k in own_keys(scope, global) {
        let name = k.to_rust_string_lossy(scope);
        let Ok(key) = v8::Local::<v8::Name>::try_from(k) else { continue };
        // a page interface put lazily: the report does not make it
        if any_lazy && lazy_global(scope, global, &name).is_some() {
            continue;
        }
        let Some(d) = global.get_own_property_descriptor(scope, key).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) else { continue };
        let is_intrinsic = intrinsics.contains(&name);
        let vk = v8::Local::new(scope, &names.value);
        let value = d.get(scope, vk.into()).filter(|v| !v.is_undefined());
        let mut fns = Vec::new();
        match value {
            Some(v) => {
                if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
                    fns.push(f);
                } else if v.is_object() {
                    if is_intrinsic {
                        intrinsic += 1;
                    } else {
                        object += 1;
                        report(&format!("GLOBAL object {name}"));
                    }
                    continue;
                } else {
                    prim += 1;
                    continue;
                }
            }
            None => {
                for part in ["get", "set"] {
                    let pk = v8::String::new(scope, part).unwrap();
                    if let Some(f) = d.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
                        fns.push(f);
                    }
                }
            }
        }
        let labels: Vec<Option<String>> = fns.iter().map(|f| known_label(scope, *f)).collect();
        if labels.iter().any(|l| l.as_deref().is_some_and(|l| l.starts_with("iface "))) {
            iface += 1;
        } else if !labels.is_empty() && labels.iter().all(|l| l.is_some()) {
            entry += 1;
        } else if is_intrinsic {
            intrinsic += 1;
        } else {
            unknown += 1;
            report(&format!("GLOBAL untemplated {name}"));
        }
    }
    report(&format!("GLOBAL-SUMMARY interfaces {iface}, template members {entry}, intrinsics {intrinsic}, objects {object}, primitives {prim}, untemplated {unknown}"));
}

/// Mark this realm as built from templates: its resolvers make an interface from its
/// template on first ask ([`realm_interface`]) instead of reading a map its install sealed.
pub(crate) fn mark_lazy_realm(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    let key = private_key(scope, "korvus_lazy_realm");
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    global.set_private(scope, key, t);
}

/// Whether this realm was built from templates ([`mark_lazy_realm`]).
pub(crate) fn is_lazy_realm(scope: &mut v8::PinScope) -> bool {
    let global = scope.get_current_context().global(scope);
    let key = private_key(scope, "korvus_lazy_realm");
    global.get_private(scope, key).is_some_and(|v| v.is_true())
}

/// Where an interface's prototype and interface object chain to, as the page's install finally left them:
/// recorded when the install is over ([`record_final_chains`]), because a template carries no
/// `[[Prototype]]` and the installs splice the two chains differently -- some only the prototype's, some to a
/// JavaScript intrinsic's (`DOMException.prototype` to `Error.prototype`).
#[derive(Clone)]
pub(crate) enum ChainTo {
    /// the default: `Object.prototype` for a prototype, `Function.prototype` for an interface object
    Default,
    /// that interface's (prototype, or interface object), made in the same realm
    Iface(String),
    /// that JavaScript intrinsic's (`prototype`, or itself), of the same realm
    Intrinsic(String),
}

/// iface -> (prototype's chain, interface object's chain), per process (the surface decides it).
fn final_chains() -> &'static std::sync::Mutex<HashMap<String, (ChainTo, ChainTo)>> {
    static C: std::sync::OnceLock<std::sync::Mutex<HashMap<String, (ChainTo, ChainTo)>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Record, for every interface this isolate has a template for, what its prototype and interface object
/// chain to in THIS realm's finished install (see [`ChainTo`]).
pub(crate) fn record_final_chains(scope: &mut v8::PinScope) -> Vec<(String, ChainTo, ChainTo)> {
    #[cfg(not(feature = "analysis"))]
    debug_assert!(with_cache(scope, |c| c.pending_names.is_empty()), "record_final_chains in a realm with snapshot templates pending");
    let names: Vec<String> = with_cache(scope, |c| c.by_name.keys().cloned().collect());
    let global = scope.get_current_context().global(scope);
    let Some(pk) = v8::String::new(scope, "prototype") else { return Vec::new() };
    let intrinsics: Vec<String> = fresh_context_keys(scope).into_iter().collect();
    let object_proto = v8::Object::new(scope).get_prototype(scope);
    let function_proto = v8::Function::builder(crate::binding::refs::f(crate::binding::protos::cb_noop)).build(scope).and_then(|f| f.get_prototype(scope));
    let mut out = HashMap::new();
    for n in names {
        // a lazy page interface not made yet keeps its template's chain: asking for it here would make it
        if lazy_global(scope, global, &n).is_some() {
            continue;
        }
        let Some(c) = crate::binding::protos::iface_object(scope, &n).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { continue };
        let co: v8::Local<v8::Object> = c.into();
        let classify = |scope: &mut v8::PinScope, v: Option<v8::Local<v8::Value>>, default: Option<v8::Local<v8::Value>>, of_proto: bool| -> ChainTo {
            let Some(v) = v else { return ChainTo::Default };
            if default.is_some_and(|d| d == v) || v.is_null() {
                return ChainTo::Default;
            }
            if of_proto {
                if let Some(o) = v8::Local::<v8::Object>::try_from(v).ok() {
                    if let Some(sn) = crate::binding::protos::iface_name_of(scope, o).map(|s| s.to_rust_string_lossy(scope)) {
                        if let Some(sn) = with_cache(scope, |c| c.by_name.contains_key(&sn).then(|| sn.clone())) {
                            return ChainTo::Iface(sn);
                        }
                    }
                }
            } else if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
                if let Some(sn) = interface_name_of_value(scope, f) {
                    return ChainTo::Iface(sn);
                }

            }
            for i in &intrinsics {
                let Some(ik) = v8::String::new(scope, i) else { continue };
                let Some(iv) = global.get(scope, ik.into()) else { continue };
                let candidate = if of_proto { v8::Local::<v8::Object>::try_from(iv).ok().and_then(|o| o.get(scope, pk.into())) } else { Some(iv) };
                if candidate.is_some_and(|c| c == v) {
                    return ChainTo::Intrinsic(i.clone());
                }
            }
            ChainTo::Default
        };
        let proto = co.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
        let pc = classify(scope, proto.and_then(|p| p.get_prototype(scope)), object_proto, true);
        let cc = classify(scope, c.get_prototype(scope), function_proto, false);
        out.insert(n, (pc, cc));
    }
    let mine: Vec<(String, ChainTo, ChainTo)> = out.iter().map(|(n, (p, c))| (n.clone(), p.clone(), c.clone())).collect();
    let worker = chains_of_worker(scope);
    if let Ok(mut m) = final_chains().lock() {
        m.extend(out.into_iter().map(|(n, v)| (chain_key(worker, &n), v)));
    }
    mine
}

/// Whether this realm's final chains are a WORKER's: kept apart from a window's, because the same interface chains
/// differently there -- a parent a worker does not expose is a phantom's (`installer::install_worker_registry`),
/// and a worker's record written over the page's gave a page interface made later the phantom's chain
/// (`CSSSkewX`'s interface object on `Function.prototype`).
fn chains_of_worker(scope: &mut v8::PinScope) -> bool {
    crate::install::surface::realm(scope).is_some_and(|r| r != spec::SurfaceRealm::Window)
}

/// [`final_chains`]' key for interface `name` in a window realm (its name) or a worker's.
fn chain_key(worker: bool, name: &str) -> String {
    if worker { format!("worker|{name}") } else { name.to_string() }
}

/// This isolate's template for the interface exposed as `name`, if one was built.
/// This isolate's template for interface `name` -- its interface object's -- if it has one.
pub(crate) fn iface_template<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::FunctionTemplate>> {
    let t = tpl_by_name(scope, name)?;
    Some(v8::Local::new(scope, &t.ctor))
}

/// `Window`'s instance template in this isolate, once the isolate has made `Window`'s template: what a frame
/// realm's global is made from, as Blink makes every frame's -- a `Window` from birth, with
/// its brand. V8 makes the global object from the global template it is given (and the global proxy from a
/// template of its own), so every realm created with this one has the same shape.
pub(crate) fn window_global_template<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::ObjectTemplate>> {
    let t = tpl_by_name(scope, "Window")?;
    let ct = v8::Local::new(scope, &t.ctor);
    Some(ct.instance_template(scope))
}

pub(crate) fn tpl_by_name(scope: &mut v8::PinScope, name: &str) -> Option<Rc<Tpl>> {
    if let Some(t) = by_name_tpl(scope, name) {
        return Some(t);
    }
    // One the page did not make from its template (this isolate could not build the process's blueprint, and
    // the page built it the ordinary way), and the global's own when the page did not record it: from the
    // process's blueprint.
    let key = if name == GLOBAL_TPL {
        let rid = with_cache(scope, |c| c.rid.clone())?;
        format!("{GLOBAL_TPL}|{rid}")
    } else {
        with_cache(scope, |c| c.keys_by_name.get(name).cloned())?
    };
    let bp = blueprint(scope, &key)?;
    // its parent's template first, which it inherits from
    if let Some(p) = bp.parent.as_deref() {
        tpl_by_name(scope, p);
    }
    install_from(scope, &key, &bp);
    with_cache(scope, |c| c.by_name.get(name).cloned())
}

/// Remember `iface`'s template key in this isolate (its first realm's), for [`tpl_by_name`].
pub(crate) fn note_key(scope: &mut v8::PinScope, iface: &str, key: &str) {
    with_cache(scope, |c| {
        if !c.keys_by_name.contains_key(iface) {
            c.keys_by_name.insert(iface.to_string(), key.to_string());
        }
    });
}

/// THIS REALM'S interface `name`, made from its template ONCE: the template's interface object
/// and prototype, chained to the parent's -- made the same way -- and kept on the realm's global, so every
/// later ask (a lazy global, a resolver) gets the same pair. `None` when no template has that name.
pub(crate) fn realm_interface<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<(v8::Local<'s, v8::Function>, v8::Local<'s, v8::Object>)> {
    let global = scope.get_current_context().global(scope);
    // THE PAGE REALM: most of its interfaces were made by its install, eagerly, and are not in the map
    // below. One of those is the answer -- for an interface asked for by name and for the PARENT of one the page
    // put lazily ([`put_lazy_global`]) -- so a lazy `AudioContext` chains to the page's own `EventTarget`, never
    // to a second one made from the template.
    let page_lazy = if is_lazy_realm(scope) { false } else { lazy_global(scope, global, name).is_some() };
    // made by engine work an install merely set off: no part of a capture still open around it (MAIN 10-13)
    if page_lazy && capturing() {
        return outside_capture(|| realm_interface(scope, name));
    }
    let key = private_key(scope, "korvus_realm_ifaces");
    let made = global.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let k = v8::String::new(scope, name)?;
    let pk = v8::String::new(scope, "prototype")?;
    // Made already -- FIRST, before the page's eager answer below: a page interface put lazily that the ENGINE made
    // (`shell_proto`, the seal's pristine members) is no longer lazy in the record, but its global is still the lazy
    // property until something reads it. That read lands here, and the eager answer -- the global's own descriptor
    // -- would read the lazy property again: a stack overflow (`anim_live` on a warm page).
    if let Some(c) = made.and_then(|m| m.get(scope, k.into())).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        let p = c.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()).unwrap_or_else(|| v8::Object::new(scope));
        return Some((c, p));
    }
    if !is_lazy_realm(scope) && !page_lazy {
        if let Some(c) = crate::binding::protos::page_eager_interface(scope, global, name) {
            let p = c.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
            return Some((c, p));
        }
    }
    let map = match made {
        Some(m) => m,
        None => {
            let m = v8::Object::new(scope);
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            m.set_prototype(scope, null);
            global.set_private(scope, key, m.into());
            m
        }
    };
    // This surface's blueprint file, and the final chains in it, are read by the first template key a realm makes
    // (`realm_identity`). A page realm restored from a snapshot made none: its first interface made
    // from a template is the first that needs them. A cached no-op everywhere else.
    realm_identity(scope);
    let t = tpl_by_name(scope, name);
    let (ctor, proto) = instantiate(scope, t.as_ref()?)?;
    // In the map before the parent is made: a parent that asks for its child (none today) gets it.
    map.set(scope, k.into(), ctor.into());
    // The chains the page's install finally made (`record_final_chains`); the template's parent for both when
    // nothing was recorded.
    let ck = chain_key(chains_of_worker(scope), name);
    let recorded = final_chains().lock().ok().and_then(|m| m.get(&ck).cloned());
    let (pchain, cchain) = recorded.unwrap_or_else(|| match t.as_ref().and_then(|t| t.parent.clone()) {
        Some(p) => (ChainTo::Iface(p.clone()), ChainTo::Iface(p)),
        None => (ChainTo::Default, ChainTo::Default),
    });
    for (target, chain, of_proto) in [(proto, pchain, true), (ctor.into(), cchain, false)] {
        let to: Option<v8::Local<v8::Value>> = match chain {
            ChainTo::Default => None,
            ChainTo::Iface(p) => realm_interface(scope, &p).map(|(pc, pp)| if of_proto { pp.into() } else { pc.into() }),
            ChainTo::Intrinsic(i) => {
                let ik = v8::String::new(scope, &i)?;
                let iv = global.get(scope, ik.into());
                if of_proto { iv.and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()).and_then(|o| o.get(scope, pk.into())) } else { iv }
            }
        };
        if let Some(to) = to {
            target.set_prototype(scope, to);
        }
    }
    // The interface's name on its prototype, as the page's seal stamps it: a receiver check that asks a
    // prototype chain for an interface by name, and the cross-realm checks, read it.
    let nv = v8::String::new(scope, name)?;
    crate::binding::protos::stamp_iface_name(scope, proto.into(), nv.into());
    apply_deltas(scope, name, ctor, proto);
    // the members the engine runs, sealed now -- before any page can reach this prototype
    crate::binding::protos::seal_members(scope, name, proto);
    if page_lazy {
        // made: the page's engine lookups find it where they find every other interface of the page
        forget_lazy_global(scope, global, name);
        crate::binding::protos::stamp_sealed(scope, name, ctor, proto);
        if reporting() {
            report(&format!("LAZY-PAGE made {name}"));
        }
    }
    Some((ctor, proto))
}

// ── Interface objects the PAGE realm puts lazily ────────────────────────────────────────

/// The page realm's record of the global names it put LAZILY ([`put_lazy_global`]): name -> (interface, enumerable,
/// writable), for `global`, the realm they belong to (a later page realm in the isolate starts its own). In an
/// isolate slot, not on the global: the engine asks it on its hot paths ([`shell_proto`], the seal, every
/// pristine member), and a Rust map answers "not lazy" without V8's private registry. A frame realm keeps none --
/// every interface of a frame is lazy, which [`is_lazy_realm`] already says.
///
/// [`shell_proto`]: crate::binding::protos::shell_proto
#[derive(Default, Clone)]
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct LazyPage(Rc<RefCell<LazyPageNames>>);

#[derive(Default)]
struct LazyPageNames {
    global: Option<v8::Global<v8::Object>>,
    names: HashMap<String, (String, bool, bool)>,
    /// The element interfaces `install::dom_chain::fill_dom_protos` left out of the tag maps because they were
    /// lazy: a wrapper of such a tag makes its interface then ([`lazy_tag_iface`]).
    tag_ifaces: std::collections::HashSet<&'static str>,
}

fn lazy_page(scope: &mut v8::PinScope) -> LazyPage {
    if let Some(l) = scope.get_slot::<LazyPage>() {
        return l.clone();
    }
    let l = LazyPage::default();
    scope.set_slot(l.clone());
    l
}

/// Whether this isolate's page realm has a lazy interface object not made yet: the hot paths' first test.
/// The context snapshot (slice 3): the page's lazy names -- each name's interface and flags -- and the tag
/// interfaces `fill_dom_protos` left lazy, for a snapshot of the page realm (the slot is the isolate's: a process
/// made from the snapshot has none).
#[cfg(not(feature = "analysis"))]
pub(crate) fn lazy_page_snapshot(scope: &mut v8::PinScope) -> (Vec<(String, String, bool, bool)>, Vec<String>) {
    let Some(l) = scope.get_slot::<LazyPage>().cloned() else { return (Vec::new(), Vec::new()) };
    let l = l.0.borrow();
    let mut names: Vec<(String, String, bool, bool)> = l.names.iter().map(|(k, (i, e, w))| (k.clone(), i.clone(), *e, *w)).collect();
    names.sort();
    let mut tags: Vec<String> = l.tag_ifaces.iter().map(|s| s.to_string()).collect();
    tags.sort();
    (names, tags)
}

/// Whether the page realm restored from the snapshot can make every interface it put lazily (`ifaces`):
/// each one's template came with the snapshot, or its blueprint is in this process, from the surface's file -- read
/// and verified now ([`disk::blueprints_loaded`]). `Err` says what is missing, before the restore writes any static:
/// the runtime then installs the ordinary way, that install writes the file again ([`persist`]), and the next run
/// restores. Without it, a restored realm whose file was missing, damaged or not trusted read `undefined` for such an
/// interface, for good (`lazy50.html`: 38 of 50).
#[cfg(not(feature = "analysis"))]
pub(crate) fn restored_lazy_sources<'a>(scope: &mut v8::PinScope, ifaces: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let fp = with_cache(scope, |c| c.rid.clone())
        .and_then(|rid| rid.strip_prefix('s').and_then(|h| u64::from_str_radix(h, 16).ok()))
        .ok_or("the snapshot names no surface")?;
    if !disk::blueprints_loaded(fp) {
        return Err("the surface's blueprint file is missing, damaged or not trusted".to_string());
    }
    let mut keys = Vec::new();
    for iface in ifaces {
        let (snap, key) = with_cache(scope, |c| (c.by_name.contains_key(iface) || c.pending_names.contains_key(iface), c.keys_by_name.get(iface).cloned()));
        match (snap, key) {
            (true, _) => {}
            (false, Some(key)) => keys.push((iface, key)),
            (false, None) => return Err(format!("no template key for the lazy interface {iface}")),
        }
    }
    let missing: Vec<_> = {
        let (Ok(done), Ok(lazy)) = (process_blueprints().lock(), lazy_blueprints().lock()) else {
            return Err("the process's blueprints are poisoned".to_string());
        };
        keys.into_iter().filter(|(_, k)| !done.contains_key(k) && !lazy.contains_key(k)).collect()
    };
    for (iface, key) in missing {
        // the persona's own blueprint ([`blueprint`]'s second ask)
        let pk = persona_key(scope, &key);
        if !process_blueprints().lock().is_ok_and(|m| m.contains_key(&pk)) && !lazy_blueprints().lock().is_ok_and(|m| m.contains_key(&pk)) {
            return Err(format!("no blueprint for the lazy interface {iface}"));
        }
    }
    Ok(())
}

/// [`lazy_page_snapshot`]'s reader, in the realm deserialised from the snapshot (its global is the lazy names').
#[cfg(not(feature = "analysis"))]
pub(crate) fn lazy_page_restore(scope: &mut v8::PinScope, names: Vec<(String, String, bool, bool)>, tags: Vec<String>) {
    if names.is_empty() && tags.is_empty() {
        return;
    }
    let global = scope.get_current_context().global(scope);
    let g = v8::Global::new(scope, global);
    let lp = lazy_page(scope);
    let mut m = lp.0.borrow_mut();
    m.global = Some(g);
    for (k, i, e, w) in names {
        m.names.insert(k, (i, e, w));
    }
    for t in tags {
        m.tag_ifaces.insert(Box::leak(t.into_boxed_str()));
    }
}

pub(crate) fn has_lazy_globals(scope: &mut v8::PinScope) -> bool {
    scope.get_slot::<LazyPage>().is_some_and(|l| !l.0.borrow().names.is_empty())
}

/// Put `key` on the page `global` as a LAZY interface object: the property is there, in its place, with
/// its flags, and the interface is made from its template the first time anything reads it -- the page, or the
/// engine through [`realm_interface`] -- as a frame realm's are (`browsing_context::install_lazy_frame`).
/// Reading the property's descriptor makes it, which is Chrome's own lazy data property (`SetLazyDataProperty`).
pub(crate) fn put_lazy_global(scope: &mut v8::PinScope, global: v8::Local<v8::Object>, key: &str, iface: &str, enumerable: bool, writable: bool) -> bool {
    let (Some(k), Some(data)) = (v8::String::new(scope, key), v8::String::new(scope, iface)) else {
        return false;
    };
    let mut attr = v8::PropertyAttribute::NONE;
    if !enumerable {
        attr = attr | v8::PropertyAttribute::DONT_ENUM;
    }
    if !writable {
        attr = attr | v8::PropertyAttribute::READ_ONLY;
    }
    let put = global.set_lazy_data_property_with_data(
        scope,
        k.into(),
        crate::binding::refs::g(crate::core::html::browsing_context::cb_lazy_interface),
        data.into(),
        attr,
        v8::SideEffectType::HasSideEffect,
        v8::SideEffectType::HasSideEffect,
    );
    if put != Some(true) {
        return false;
    }
    let lp = lazy_page(scope);
    let same = lp.0.borrow().global.as_ref().is_some_and(|g| v8::Local::new(scope, g) == global);
    let mut m = lp.0.borrow_mut();
    if !same {
        m.global = Some(v8::Global::new(scope, global));
        m.names.clear();
        m.tag_ifaces.clear();
    }
    m.names.insert(key.to_string(), (iface.to_string(), enumerable, writable));
    true
}

/// `key` on the page `global` is a lazy interface object not made yet: its interface's name, enumerable,
/// writable. `None` for every other name, and always in a frame realm.
pub(crate) fn lazy_global(scope: &mut v8::PinScope, global: v8::Local<v8::Object>, key: &str) -> Option<(String, bool, bool)> {
    let lp = scope.get_slot::<LazyPage>()?.clone();
    let m = lp.0.borrow();
    let hit = m.names.get(key)?.clone();
    let g = m.global.as_ref()?;
    (v8::Local::new(scope, g) == global).then_some(hit)
}

/// [`lazy_global`] in the current context's realm: whether `key` is a page interface put lazily and not made yet.
pub(crate) fn lazy_here(scope: &mut v8::PinScope, key: &str) -> bool {
    if !scope.get_slot::<LazyPage>().is_some_and(|l| l.0.borrow().names.contains_key(key)) {
        return false;
    }
    let global = scope.get_current_context().global(scope);
    lazy_global(scope, global, key).is_some()
}

/// One more page interface put lazily ([`TemplateCounts::lazy`]).
pub(crate) fn count_lazy(scope: &mut v8::PinScope) {
    with_cache(scope, |c| c.counts.lazy += 1);
}

/// Record that `iface`'s tags were left out of the page's tag maps because it was lazy ([`LazyPageNames`]).
pub(crate) fn note_lazy_tag_iface(scope: &mut v8::PinScope, iface: &'static str) {
    lazy_page(scope).0.borrow_mut().tag_ifaces.insert(iface);
}

/// Whether `iface`'s tags were left out of the page's tag maps because it was lazy: a wrapper of one makes it.
pub(crate) fn lazy_tag_iface(scope: &mut v8::PinScope, iface: &str) -> bool {
    scope.get_slot::<LazyPage>().is_some_and(|l| l.0.borrow().tag_ifaces.contains(iface))
}

/// `key` was made -- its getter ran, or the engine asked for its interface: no longer lazy.
pub(crate) fn forget_lazy_global(scope: &mut v8::PinScope, global: v8::Local<v8::Object>, key: &str) {
    if lazy_global(scope, global, key).is_some() {
        if let Some(l) = scope.get_slot::<LazyPage>() {
            l.0.borrow_mut().names.remove(key);
        }
    }
}

/// The LEDGER ROWS of `key`'s template -- built in this isolate, or in the process's blueprint -- without building
/// it, or `None` when this realm has no such template (a lazy page interface is built and instantiated on
/// its first read, through [`tpl_by_name`]). The rows are what its create would have recorded (`installer::tpl_step`
/// replays them on an instantiate), so the lazy put records them too: the install ledger and the built surface
/// agree (`installer_parity`). The blueprint's bookkeeping is replayed now, as [`from_blueprint`] would have done.
pub(crate) fn template_rows(scope: &mut v8::PinScope, key: &str) -> Option<Vec<crate::install::installer::LedgerEntry>> {
    if let Some(t) = cached(scope, key) {
        return Some(t.replay.rows.clone());
    }
    let bp = blueprint(scope, key)?;
    if bp.from_disk && crate::behavior_rt::bookkeeping_on() && marks_replayed().lock().is_ok_and(|mut m| m.insert(key.to_string())) {
        replay_marks(&bp.replay.marks);
    }
    Some(bp.replay.rows.clone())
}

/// Whether a template has a global member under `key`.
pub(crate) fn has_global_entry(t: &Tpl, key: &str) -> bool {
    t.global_index.contains_key(key)
}

/// THE FIRST REALM'S OWN GLOBAL MEMBERS FROM THEIR TEMPLATES: what its install built the
/// ordinary way -- the window getters, `atob`, `structuredClone`, `console`'s methods, ... -- and recorded into
/// `Window`'s template and the global's own (`#global`), re-attached from them IN PLACE, as every frame's are,
/// so no realm keeps a function its template builds. A key keeps its place and its own flags; one the install
/// locked (`window`, `document`, `location`, `top`: `[LegacyUnforgeable]`) cannot be redefined and keeps what
/// it has -- functions its install made from templates already (`binding::protos::template_fn`); a namespace
/// keeps its object, locked or not (`chrome`), and takes its members from the template.
pub(crate) fn switch_page_global(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    let any_lazy = has_lazy_globals(scope);
    for name in ["Window", GLOBAL_TPL] {
        let Some(t) = tpl_by_name(scope, name) else { continue };
        let mut keys: Vec<(usize, String)> = t.global_index.iter().map(|(k, i)| (*i, k.clone())).collect();
        keys.sort();
        for (i, key) in keys {
            // A page interface put lazily and not made yet: reading its descriptor here would make it.
            // There is nothing to switch: its first read makes it from its own template (`realm_interface`), the
            // function a switch would attach, under the flags it was put with.
            if any_lazy && lazy_global(scope, global, &key).is_some() {
                continue;
            }
            switch_one(scope, global, &t.global[i..i + 1], &key);
        }
    }
}

/// One recorded global entry over the member of the same key on `target`, in place (see [`switch_page_global`]).
fn switch_one(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, row: &[(v8::Global<v8::Name>, Entry)], key: &str) {
    let Some(k) = v8::String::new(scope, key) else { return };
    let Some(d) = target.get_own_property_descriptor(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let flag = |scope: &mut v8::PinScope, n: &str| -> Option<bool> {
        let nk = v8::String::new(scope, n)?;
        d.get(scope, nk.into()).filter(|v| !v.is_undefined()).map(|v| v.boolean_value(scope))
    };
    // A namespace keeps its object -- under a key the install locked too (`chrome`) -- and switches its members.
    if let Entry::Namespace(rows, _) = &row[0].1 {
        let Some(vk) = v8::String::new(scope, "value") else { return };
        let Some(obj) = d.get(scope, vk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
        for r in rows {
            let rk = v8::Local::new(scope, &r.0).to_rust_string_lossy(scope);
            switch_one(scope, obj, std::slice::from_ref(r), &rk);
        }
        return;
    }
    if flag(scope, "configurable") != Some(true) {
        return;
    }
    let enumerable = flag(scope, "enumerable").unwrap_or(false);
    let writable = flag(scope, "writable");
    if attach(scope, target, row).is_none() {
        return;
    }
    // The key's own flags, not the template's: what the install left (a later pass may have changed them).
    let mut pd = match writable {
        Some(w) => {
            let v = target.get(scope, k.into()).unwrap_or_else(|| v8::undefined(scope).into());
            v8::PropertyDescriptor::new_from_value_writable(v, w)
        }
        None => v8::PropertyDescriptor::new(),
    };
    pd.set_enumerable(enumerable);
    pd.set_configurable(true);
    target.define_property(scope, k.into(), &pd);
}

/// ONE of a template's global members -- the one under `key`, if it has one -- onto `target` (a realm built
/// from templates defines its global's keys one at a time, in the page's order).
pub(crate) fn attach_one(scope: &mut v8::PinScope, t: &Tpl, key: &str, target: v8::Local<v8::Object>) -> bool {
    let Some(i) = t.global_index.get(key).copied() else { return false };
    attach(scope, target, &t.global[i..i + 1]).is_some()
}

/// A `[Global]` interface's members onto this realm's global object, each from its own template, in the
/// recorded order (the interface object is published first, as the ordinary install does).
pub(crate) fn attach_global(scope: &mut v8::PinScope, t: &Tpl, global: v8::Local<v8::Object>) -> Option<()> {
    attach(scope, global, &t.global)
}

fn attach(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, list: &[(v8::Global<v8::Name>, Entry)]) -> Option<()> {
    for (key, e) in list {
        let key = v8::Local::new(scope, key);
        match e {
            Entry::Value(v, a) => {
                let v = v8::Local::new(scope, v);
                proto.define_own_property(scope, key, v, a.v8());
            }
            Entry::Func(f, a) => {
                let f = v8::Local::new(scope, f).get_function(scope)?;
                note_known(scope, f, "entry");
                proto.define_own_property(scope, key, f.into(), a.v8());
            }
            Entry::Intrinsic(i, a) => {
                let f = array_proto_fn(scope, i.name())?;
                proto.define_own_property(scope, key, f.into(), a.v8());
            }
            Entry::Accessor(g, s, a) => {
                let g = match g {
                    Some(g) => {
                        let f = v8::Local::new(scope, g).get_function(scope)?;
                        note_known(scope, f, "entry");
                        f.into()
                    }
                    None => v8::undefined(scope).into(),
                };
                let s = match s {
                    Some(s) => {
                        let f = v8::Local::new(scope, s).get_function(scope)?;
                        note_known(scope, f, "entry");
                        f.into()
                    }
                    None => v8::undefined(scope).into(),
                };
                let mut pd = v8::PropertyDescriptor::new_from_get_set(g, s);
                pd.set_enumerable(!a.dont_enum);
                pd.set_configurable(!a.dont_delete);
                proto.define_property(scope, key, &pd);
            }
            Entry::Namespace(rows, a) => {
                let o = v8::Object::new(scope);
                attach(scope, o, rows)?;
                proto.define_own_property(scope, key, o.into(), a.v8());
            }
            Entry::Unscopables(names, a) => {
                // One call: the null-prototype object with its names, each `true`.
                let null: v8::Local<v8::Value> = v8::null(scope).into();
                let mut ks: Vec<v8::Local<v8::Name>> = Vec::with_capacity(names.len());
                for n in names {
                    ks.push(v8::String::new(scope, n)?.into());
                }
                let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
                let vs = vec![t; ks.len()];
                let o = v8::Object::with_prototype_and_properties(scope, null, &ks, &vs);
                proto.define_own_property(scope, key, o.into(), a.v8());
            }
        }
    }
    Some(())
}

// ── the fused member: its guards and body as one native call ──────────────────────────

/// The first word of every [`FusedRec`]: how `binding::protos::body_data` knows the data it holds is one.
/// A non-canonical address (the high bits are set), so no pointer or `&str` a record of another kind starts
/// with can be this value; every `v8::External` this crate makes points at a Rust struct of at least a word.
const FUSED_MAGIC: u64 = 0xF05E_D0DA_7A5E_ED01;

/// What one fused member checks before its body runs, and the body.
#[repr(C)]
pub(crate) struct FusedRec {
    magic: u64,
    /// the body's own data, for `binding::protos::body_data` (None: the body was built without data)
    data: Option<v8::Global<v8::Value>>,
    /// the guards' checks, in the order the guards made them (the outermost first)
    steps: Vec<Step>,
    /// the body's own callback, called on the SAME FunctionCallbackInfo
    body: v8::FunctionCallback,
    /// a guard never passed its body's value on (a WebIDL `undefined` return)
    is_void: bool,
    /// The member's label when a loaded plugin claims it (goal 1020 step 2): its trampoline is [`cb_claimed`], which
    /// hands the call to the plugins once the body has answered. `None` for every member of a process no plugin
    /// claims anything in, whose functions are built exactly as before.
    claim: Option<&'static std::ffi::CStr>,
    /// `Interface.member` for the call recorder (korvus-analysis); empty until the first call for a
    /// function built outside every template, whose interface only its holder knows ([`analysis_untemplated`])
    #[cfg(feature = "analysis")]
    label: std::cell::Cell<&'static str>,
    /// such a function's own name (`get brands`, `toJSON`), from which that first call finds the label
    #[cfg(feature = "analysis")]
    name: &'static str,
    /// calls of this member that recorded a call site (the recorder samples the first few)
    #[cfg(feature = "analysis")]
    sited: std::cell::Cell<u32>,
}

// ── A template-built function's record named by an ID ───────────────────────────────

/// What a template-built function's data names (the context snapshot, slice 2b): its fused record, its
/// stub's, or its prototype-data interface. The function's data is the record's ID -- a Smi -- never the record's
/// address: an address is this process's, and a snapshot of the realm cannot carry it, while an ID names the same
/// record in any process that builds (or, after a snapshot load, rebuilds) the table in the same order.
#[derive(Clone, Copy)]
pub(crate) enum FnSlot {
    Fused(*const FusedRec),
    Stub(*const crate::install::stub::StubTpl),
    Proto(*const IfaceRef),
    /// a record a snapshot's realm names, not rebuilt yet: its [`Origin`] says how ([`resolve`], slice 2a). No reader
    /// sees one: [`fn_slot`] and [`cb_fused`] rebuild it first.
    #[cfg_attr(feature = "analysis", allow(dead_code))]
    Pending,
}

/// Where a function record came from: its template's key, its blueprint function (at
/// `at` in [`FnTable::fn_bytes`]), which of that function's records it is, and what `build_ft` gave it besides --
/// whether it had the interface's [`IfaceRef`], the interface's brand id, and a fused record's body record. Kept by a
/// run that will snapshot its realm, and restored with the snapshot, so a later process rebuilds the record a
/// deserialised function's ID names.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "analysis", allow(dead_code))]
pub(crate) struct Origin {
    key: u32,
    at: u32,
    /// 0: the function's own record (its stub, its prototype data, its fused record); 1: a fused function's body's
    part: u8,
    iref: bool,
    brand: Option<u32>,
    /// a fused record's body record's ID (`u32::MAX`: its body has none)
    body: u32,
}

/// A record's kind in the low bits of its word in [`FnTable::recs`] (records are boxed, so 8-aligned): a fused record
/// is UNTAGGED -- its word is its address -- so the read [`cb_fused`] makes on every call is one load and one test.
const TAG_STUB: usize = 1;
const TAG_PROTO: usize = 2;
const PENDING_WORD: usize = 3;

#[inline(always)]
fn encode(slot: FnSlot) -> usize {
    match slot {
        FnSlot::Fused(p) => p as usize,
        FnSlot::Stub(p) => p as usize | TAG_STUB,
        FnSlot::Proto(p) => p as usize | TAG_PROTO,
        FnSlot::Pending => PENDING_WORD,
    }
}

#[inline(always)]
fn decode(w: usize) -> FnSlot {
    match w & 3 {
        0 => FnSlot::Fused(w as *const FusedRec),
        TAG_STUB => FnSlot::Stub((w & !3) as *const crate::install::stub::StubTpl),
        TAG_PROTO => FnSlot::Proto((w & !3) as *const IfaceRef),
        _ => FnSlot::Pending,
    }
}

/// The isolate's function records by ID ([`fn_id`]): owned by an isolate slot, dropped with the isolate; its address
/// also sits in isolate data slot [`FN_TABLE_SLOT`], so a call reads it without a lookup (MAIN, 10-13: per isolate,
/// no lock, no hash, no OpState). The records themselves live in the isolate's [`Cache::keep`], as before.
#[derive(Default)]
pub(crate) struct FnTable {
    /// each record as one word ([`encode`]): its address with its kind in the two low bits
    recs: Vec<usize>,
    /// each record's [`Origin`], kept while recording for a snapshot ([`crate::binding::refs::recording`]) and
    /// restored with one: `None` for a record no origin was kept for (a snapshot of its realm refuses)
    origins: Vec<Option<Origin>>,
    /// the template keys origins name, by index
    keys: Vec<String>,
    key_ix: FxMap<String, u32>,
    /// the blueprint functions origins name, as the blueprint file writes them (`disk::fn_bytes`)
    fn_bytes: Vec<u8>,
    /// why this isolate's records cannot go into a snapshot, if a function could not be kept
    refusal: Option<String>,
    /// the records [`resolve`] rebuilt (a build's live in the cache's `keep`)
    #[cfg_attr(feature = "analysis", allow(dead_code))]
    keep: Vec<Box<dyn std::any::Any>>,
}

struct FnTableOwner(Box<FnTable>);

impl Drop for FnTableOwner {
    /// The table goes with its isolate: no call on this thread may find it in [`LAST_TABLE`] afterwards (another
    /// isolate can be made at the same address).
    fn drop(&mut self) {
        let me: *const FnTable = &*self.0;
        let _ = LAST_TABLE.try_with(|c| {
            if c.get().1 == me {
                c.set((0, std::ptr::null()));
            }
        });
    }
}

thread_local! {
    /// The isolate the thread's last fused call ran in, and its table: [`cb_fused`] reads it without asking V8 while
    /// the isolate stays the same, as it nearly always does. Raw pointers, never a V8 handle; cleared when the table's
    /// isolate drops it ([`FnTableOwner`]).
    static LAST_TABLE: std::cell::Cell<(usize, *const FnTable)> = const { std::cell::Cell::new((0, std::ptr::null())) };
}

/// The table of the isolate a callback runs in ([`LAST_TABLE`], else the isolate's data slot).
#[inline(always)]
fn table_of(isolate: v8::UnsafeRawIsolatePtr) -> *const FnTable {
    // SAFETY: `UnsafeRawIsolatePtr` is `#[repr(transparent)]` over the isolate's pointer
    let key = unsafe { std::mem::transmute::<v8::UnsafeRawIsolatePtr, usize>(isolate) };
    LAST_TABLE.with(|c| {
        let (k, t) = c.get();
        if k == key {
            // the entry is the isolate's own table: a new isolate at a dropped one's address made a table first
            // ([`fn_table`] resets the entry), and the drop cleared it ([`FnTableOwner`]); the debug runs check
            debug_assert_eq!(
                t,
                // SAFETY: as below
                unsafe { v8::Isolate::ref_from_raw_isolate_ptr_unchecked(&isolate) }.get_data(FN_TABLE_SLOT) as *const FnTable,
                "a stale isolate -> table entry"
            );
            return t;
        }
        // SAFETY: V8 hands the isolate the callback runs in, never null; the table is that isolate's own and outlives
        // its functions
        let t = unsafe { v8::Isolate::ref_from_raw_isolate_ptr_unchecked(&isolate) }.get_data(FN_TABLE_SLOT) as *const FnTable;
        c.set((key, t));
        t
    })
}

/// How [`cb_fused`] reads an ID out of its data's handle without asking V8, measured once on a real ID
/// ([`measure_id_read`]): `0..DIRECT` = the shift of a Smi in the slot the handle points at, `DIRECT + shift` = the
/// handle IS the tagged value (direct handles), [`ID_BY_V8`] = ask V8 (until measured, or when an ID is no Smi here).
static ID_READ: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(ID_BY_V8);
const ID_BY_V8: u8 = u8::MAX;
const DIRECT: u8 = 64;

/// A handle's own word.
#[inline(always)]
fn handle_word(v: v8::Local<v8::Value>) -> i64 {
    // SAFETY: `Local` is `#[repr(C)]` over one pointer (rusty_v8 150 `handle.rs`)
    unsafe { std::mem::transmute_copy::<v8::Local<v8::Value>, i64>(&v) }
}

/// Measure [`ID_READ`] on an ID this V8 makes: a Smi's shift (32 without pointer compression, 1 with it), and
/// whether a handle holds the value or the address of a slot that does.
fn measure_id_read(scope: &mut v8::PinScope) {
    use std::sync::atomic::Ordering::Relaxed;
    if ID_READ.load(Relaxed) != ID_BY_V8 {
        return;
    }
    let want = FN_ID_BASE + 7;
    let v: v8::Local<v8::Value> = v8::Integer::new(scope, want as i32).into();
    let word = handle_word(v);
    for s in [32u8, 1] {
        if word & 1 == 0 && word >> s == want {
            ID_READ.store(DIRECT + s, Relaxed);
            return;
        }
    }
    // not the value itself (a user-space address never decodes to a negative ID): the address of its slot
    // SAFETY: a live handle's slot
    let tagged = unsafe { *(word as usize as *const i64) };
    for s in [32u8, 1] {
        if tagged & 1 == 0 && tagged >> s == want {
            ID_READ.store(s, Relaxed);
            return;
        }
    }
}

/// The table index a fused function's data names ([`fn_id`]).
/// [`ID_READ`]'s value for this build's layout (no pointer compression: a handle is the address of a slot holding a
/// Smi shifted by 32): the one the hot readers test for first.
const SLOT_SMI32: u8 = 32;

/// [`cb_fused`]'s entry, decided once by [`entry_cold`]: [`ENTRY_INLINE`] reads the isolate as word [`ISOLATE_WORD`] of
/// the frame block V8 lays the callback info over (V8 15 `FunctionCallbackInfo::GetIsolate`, `values_[kIsolateIndex]`)
/// and the ID off the slot its data handle names ([`SLOT_SMI32`]); [`ENTRY_V8`] asks V8 for both on every call. Both
/// reads are the V8 build's layout -- the same for every isolate and thread of the process -- as [`ID_READ`] is.
static ENTRY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(ENTRY_UNMEASURED);
const ENTRY_UNMEASURED: u8 = 0;
const ENTRY_INLINE: u8 = 1;
const ENTRY_V8: u8 = 2;
/// `kIsolateIndex` on this target (v8-function-callback.h: argc, the exit frame's sp / type / fp / pc, the isolate).
const ISOLATE_WORD: usize = 5;

/// [`cb_fused`]'s isolate and table index the slow way -- V8's own answers -- and, once the ID's layout is known
/// ([`ID_READ`]), the decision for every later call ([`ENTRY`]): inline when the isolate word and the ID read both hold,
/// V8's answers for good otherwise, said once on stderr when it is the isolate word (V8's layout moved).
#[cold]
#[inline(never)]
fn entry_cold(info: *const v8::FunctionCallbackInfo, data: v8::Local<v8::Value>) -> (v8::UnsafeRawIsolatePtr, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    // SAFETY: the info V8 handed `cb_fused`
    let parts = unsafe { &*info }.get_parts();
    let ix = fused_index_cold(data);
    let id_read = ID_READ.load(Relaxed);
    if ENTRY.load(Relaxed) == ENTRY_UNMEASURED && id_read != ID_BY_V8 {
        // SAFETY: the info's frame block has `kArgsLength` (> ISOLATE_WORD) words
        let word = unsafe { *(info as *const usize).add(ISOLATE_WORD) };
        // SAFETY: `UnsafeRawIsolatePtr` is `#[repr(transparent)]` over the isolate's pointer
        let isolate = unsafe { std::mem::transmute::<v8::UnsafeRawIsolatePtr, usize>(parts.isolate) };
        if word != isolate {
            ENTRY.store(ENTRY_V8, Relaxed);
            eprintln!("korvus: a fused call's isolate is not word {ISOLATE_WORD} of its callback info (V8's layout moved): every fused call asks V8");
        } else {
            ENTRY.store(if id_read == SLOT_SMI32 { ENTRY_INLINE } else { ENTRY_V8 }, Relaxed);
        }
    }
    (parts.isolate, ix)
}

/// Debug builds: [`cb_fused`]'s isolate and table index against V8's own answers, on every call -- `get_parts` for the
/// isolate, the ID as V8 reads the Smi (never the slot read [`ENTRY_INLINE`] makes).
#[cfg(debug_assertions)]
fn entry_check(info: *const v8::FunctionCallbackInfo, data: v8::Local<v8::Value>, isolate: v8::UnsafeRawIsolatePtr, ix: usize) {
    // SAFETY: the info V8 handed `cb_fused`
    let parts = unsafe { &*info }.get_parts();
    // SAFETY: `UnsafeRawIsolatePtr` is `#[repr(transparent)]` over the isolate's pointer
    let (got, want) = unsafe { (std::mem::transmute::<v8::UnsafeRawIsolatePtr, usize>(isolate), std::mem::transmute::<v8::UnsafeRawIsolatePtr, usize>(parts.isolate)) };
    assert_eq!(got, want, "cb_fused's isolate (entry state {}) is not V8's", ENTRY.load(std::sync::atomic::Ordering::Relaxed));
    // SAFETY: a `cb_fused` function's data is its record's ID ([`fn_id`])
    let id = unsafe { v8::Local::<v8::Integer>::cast_unchecked(data) }.value();
    assert_eq!(ix as i64, id - FN_ID_BASE, "cb_fused's table index (entry state {}) is not its ID's", ENTRY.load(std::sync::atomic::Ordering::Relaxed));
}

/// A fused function's table index ([`fn_id`]) for any layout, or before one is measured.
#[cold]
#[inline(never)]
fn fused_index_cold(data: v8::Local<v8::Value>) -> usize {
    let m = ID_READ.load(std::sync::atomic::Ordering::Relaxed);
    let v = if m < DIRECT {
        // SAFETY: measured: the handle is the address of the slot holding the ID's Smi
        (unsafe { *(handle_word(data) as usize as *const i64) }) >> m
    } else if m != ID_BY_V8 {
        handle_word(data) >> (m - DIRECT)
    } else {
        // SAFETY: the data is an ID ([`fn_id`])
        unsafe { v8::Local::<v8::Integer>::cast_unchecked(data) }.value()
    };
    (v - FN_ID_BASE) as usize
}

/// The isolate data slot that holds the [`FnTable`]'s address (deno_core's runtime state is slot 0).
const FN_TABLE_SLOT: u32 = 1;
/// IDs are the Smis `FN_ID_BASE .. FN_ID_BASE + FN_ID_SPAN`: a range no plain function's data falls in (`build_ft`
/// refuses one that would), so a reader tells an ID from any other data by its value.
const FN_ID_BASE: i64 = i32::MIN as i64;
const FN_ID_SPAN: i64 = 1 << 24;

fn fn_table(scope: &mut v8::PinScope) -> *mut FnTable {
    let p = scope.get_data(FN_TABLE_SLOT) as *mut FnTable;
    if !p.is_null() {
        return p;
    }
    let mut b = Box::<FnTable>::default();
    let raw: *mut FnTable = &mut *b;
    scope.set_slot(FnTableOwner(b));
    scope.set_data(FN_TABLE_SLOT, raw as *mut std::ffi::c_void);
    measure_id_read(scope);
    // a new isolate may have a dropped one's address: no entry of this thread may still name the old table
    let _ = LAST_TABLE.try_with(|c| c.set((0, std::ptr::null())));
    raw
}

/// Register `slot` in this isolate's table: the value a function's data carries for it. `origin` is kept while
/// recording for a snapshot (slice 2a).
fn fn_id<'s>(scope: &mut v8::PinScope<'s, '_>, slot: FnSlot, origin: Option<Origin>) -> Option<v8::Local<'s, v8::Value>> {
    let t = fn_table(scope);
    // SAFETY: the table lives as long as the isolate, and no reader keeps a reference into `recs` (each copies its
    // slot out), so this push cannot invalidate one
    let t = unsafe { &mut *t };
    if t.recs.len() as i64 >= FN_ID_SPAN {
        return None;
    }
    t.recs.push(encode(slot));
    if crate::binding::refs::recording() {
        t.origins.push(origin);
    }
    let id = t.recs.len() as i64 - 1;
    Some(v8::Integer::new(scope, (FN_ID_BASE + id) as i32).into())
}

/// The record a function's data names, when that data is an ID ([`fn_id`]); `None` for any other data.
pub(crate) fn fn_slot(scope: &mut v8::PinScope, d: v8::Local<v8::Value>) -> Option<FnSlot> {
    let ix = id_of(d)?;
    // SAFETY: the scope's own isolate
    let t = table_of(unsafe { scope.as_raw_isolate_ptr() });
    if t.is_null() {
        return None;
    }
    // SAFETY: the table outlives every function of the isolate
    let recs = unsafe { &(*t).recs };
    match decode(*recs.get(ix)?) {
        FnSlot::Pending => resolve(scope, ix),
        s => Some(s),
    }
}

/// The table index `d` names when it is a record ID ([`fn_id`]), `None` for any other value: read off its handle as
/// [`cb_fused`] reads one, without asking V8 (a heap object's tag bit is 1, a Smi's 0); asked of V8 until the
/// layout is measured.
#[inline(always)]
fn id_of(d: v8::Local<v8::Value>) -> Option<usize> {
    let m = ID_READ.load(std::sync::atomic::Ordering::Relaxed);
    if m == SLOT_SMI32 {
        // SAFETY: measured: a handle is the address of the slot holding its value
        let tagged = unsafe { *(handle_word(d) as usize as *const i64) };
        if tagged & 1 != 0 {
            return None;
        }
        let v = tagged >> 32;
        return (FN_ID_BASE..FN_ID_BASE + FN_ID_SPAN).contains(&v).then_some((v - FN_ID_BASE) as usize);
    }
    let v = if m == ID_BY_V8 {
        if !d.is_int32() {
            return None;
        }
        v8::Local::<v8::Integer>::try_from(d).ok()?.value()
    } else {
        let word = handle_word(d);
        let (tagged, shift) = if m < DIRECT {
            // SAFETY: measured: a handle is the address of the slot holding its value
            (unsafe { *(word as usize as *const i64) }, m)
        } else {
            (word, m - DIRECT)
        };
        if tagged & 1 != 0 {
            return None;
        }
        tagged >> shift
    };
    (FN_ID_BASE..FN_ID_BASE + FN_ID_SPAN).contains(&v).then_some((v - FN_ID_BASE) as usize)
}

/// In a run that will snapshot its realm, keep blueprint function `fb` of `bd`'s template
/// for the records `build_ft` is about to make of it; `None` (nothing kept) in every other run.
fn note_fn(scope: &mut v8::PinScope, bd: &Bd, fb: &FnBp) -> Option<Origin> {
    if !crate::binding::refs::recording() {
        return None;
    }
    let t = fn_table(scope);
    // SAFETY: the isolate's table; no reference into it is held across this call
    let t = unsafe { &mut *t };
    if bd.key.is_empty() {
        t.refusal.get_or_insert_with(|| format!("{}: a function whose interface's template is under no key", bd.bp.iface));
        return None;
    }
    let key = match t.key_ix.get(bd.key) {
        Some(k) => *k,
        None => {
            let k = t.keys.len() as u32;
            t.keys.push(bd.key.to_string());
            t.key_ix.insert(bd.key.to_string(), k);
            k
        }
    };
    let at = t.fn_bytes.len() as u32;
    if let Err(why) = disk::fn_bytes(&mut t.fn_bytes, fb) {
        t.refusal.get_or_insert_with(|| format!("{}: a function a snapshot cannot carry -- {why}", bd.key));
        return None;
    }
    Some(Origin { key, at, part: 0, iref: bd.iref.is_some(), brand: bd.id, body: u32::MAX })
}

/// The ID a function's data carries as a [`fn_id`] value: its index in the table.
fn id_index(v: v8::Local<v8::Value>) -> u32 {
    fused_rec_addr(v).map(|i| i as u32).unwrap_or(u32::MAX)
}

/// A fused member's checks, as its guards made them (`build_ft`, and [`resolve`] after a snapshot load).
fn fused_steps(steps: &[StepBp], iref: Option<&IfaceRef>, id: Option<u32>) -> Result<Vec<Step>, String> {
    let mut fsteps = Vec::with_capacity(steps.len());
    for st in steps {
        fsteps.push(match st.clone() {
            StepBp::Arity(need, iface, member, ty) => Step::Arity(need, iface, member, ty),
            StepBp::Check(CheckBp::Receiver(ord, member)) => {
                Step::Check(FusedCheck::Receiver(iref.cloned().ok_or("a fused receiver check before its interface object")?, ord, member, id, std::cell::Cell::new(u32::MAX)))
            }
            StepBp::Check(CheckBp::Brand { iface, member, need }) => Step::Check(FusedCheck::Brand { iface, member, need }),
            StepBp::Check(CheckBp::BrandAccessor(i)) => Step::Check(FusedCheck::BrandAccessor(i)),
            StepBp::Check(CheckBp::BrandPromise { iface, member }) => Step::Check(FusedCheck::BrandPromise { iface, member }),
            StepBp::Check(CheckBp::RangeBrand(i)) => Step::Check(FusedCheck::RangeBrand(i)),
            StepBp::Check(CheckBp::PluginRecv(i)) => Step::Check(FusedCheck::PluginRecv(i)),
            StepBp::Check(CheckBp::WindowRecv) => Step::Check(FusedCheck::WindowRecv),
        });
    }
    Ok(fsteps)
}

/// Rebuild record `ix`, which a snapshot's realm names ([`FnSlot::Pending`]), from its
/// [`Origin`] -- as `build_ft` made it: the interface's template (restored with the snapshot, so its receiver checks
/// find the deserialised interface) and the blueprint function. `None` if it cannot be.
#[cfg(not(feature = "analysis"))]
#[cold]
#[inline(never)]
fn resolve(scope: &mut v8::PinScope, ix: usize) -> Option<FnSlot> {
    let t = fn_table(scope);
    // SAFETY: the isolate's table; no reference into it is held across this call
    let (o, key, fb) = unsafe {
        let t = &*t;
        let o = (*t.origins.get(ix)?)?;
        (o, t.keys.get(o.key as usize)?.clone(), disk::fn_at(&t.fn_bytes, o.at)?)
    };
    let iref = match o.iref {
        true => {
            let tpl = cached(scope, &key)?;
            let names = names(scope);
            Some(IfaceRef::of(tpl.ctor.clone(), names.prototype.clone()))
        }
        false => None,
    };
    let id_value = |scope: &mut v8::PinScope, body: u32| -> Option<v8::Global<v8::Value>> {
        (body != u32::MAX).then(|| {
            let v: v8::Local<v8::Value> = v8::Integer::new(scope, (FN_ID_BASE + body as i64) as i32).into();
            v8::Global::new(scope, v)
        })
    };
    let (slot, rec): (FnSlot, Box<dyn std::any::Any>) = match (&fb.kind, o.part) {
        (FnKind::Stub { setter, member, table }, 0) | (FnKind::Fused { data: BodyDataBp::Stub { setter, member, table }, .. }, 1) => {
            let (_, rec) = crate::install::stub::stub_template_of(*setter, *member, *table, iref?);
            (FnSlot::Stub(&*rec as *const crate::install::stub::StubTpl), rec)
        }
        (FnKind::ProtoData { .. }, 0) | (FnKind::Fused { data: BodyDataBp::ProtoData, .. }, 1) => {
            let rec = Box::new(iref?);
            (FnSlot::Proto(&*rec as *const IfaceRef), rec)
        }
        (FnKind::Fused { steps, body, data, is_void }, 0) => {
            let mut body = *body;
            let data = match data {
                BodyDataBp::None => None,
                BodyDataBp::Prim(p) => {
                    let v = p.v8(scope);
                    Some(v8::Global::new(scope, v))
                }
                BodyDataBp::Stub { setter, member, table } => {
                    // the callback `build_ft` gave the body: the one `stub_template_of` picks
                    body = crate::install::stub::stub_template_of(*setter, *member, *table, iref.clone()?).0;
                    Some(id_value(scope, o.body)?)
                }
                BodyDataBp::ProtoData => Some(id_value(scope, o.body)?),
            };
            let steps = fused_steps(steps, iref.as_ref(), o.brand).ok()?;
            let rec = Box::new(FusedRec { magic: FUSED_MAGIC, data, steps, body, is_void: *is_void, claim: None });
            (FnSlot::Fused(&*rec as *const FusedRec), rec)
        }
        _ => return None,
    };
    // SAFETY: as above; the record lives in the table's `keep` as long as the table
    let t = unsafe { &mut *t };
    *t.recs.get_mut(ix)? = encode(slot);
    t.keep.push(rec);
    Some(slot)
}

/// An analysis build makes no snapshot: nothing is ever pending.
#[cfg(feature = "analysis")]
fn resolve(_scope: &mut v8::PinScope, _ix: usize) -> Option<FnSlot> {
    None
}

/// [`cb_fused`]'s record when its slot is still [`FnSlot::Pending`]: the first call of the function after a
/// snapshot load.
#[cold]
#[inline(never)]
fn resolve_fused<'a>(i: &v8::FunctionCallbackInfo, ix: usize) -> &'a FusedRec {
    // SAFETY: the info V8 gave `cb_fused`
    let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
    let scope = &mut scope.init();
    match resolve(scope, ix) {
        // SAFETY: a record of this isolate's table, which outlives its functions
        Some(FnSlot::Fused(p)) => unsafe { &*p },
        _ => panic!("the fused record of snapshot ID {ix} cannot be rebuilt from its origin"),
    }
}

/// This isolate's templates and function records, into a snapshot of its page realm. Every
/// template goes into `store` as V8 data, so a realm deserialised from the snapshot keeps using the templates its
/// objects came from (V8 keeps one instance of a template per context: a receiver check's `IfaceRef` finds the
/// deserialised interface). The rest is appended to `out`. An error says why the snapshot cannot carry them.
#[cfg(not(feature = "analysis"))]
pub fn snapshot_templates(scope: &mut v8::PinScope, store: &mut deno_core::snapshot::SnapshotStoreDataStore, out: &mut Vec<u8>) -> Result<(), String> {
    disk::snap_write(scope, store, out)
}

/// [`snapshot_templates`]' reader, in a runtime made from the snapshot, before anything builds a template: reads
/// `bytes` from `*at` on and moves `*at` past its section. Each function record stays [`FnSlot::Pending`] until its
/// first use.
#[cfg(not(feature = "analysis"))]
pub fn restore_templates(scope: &mut v8::PinScope, bytes: &[u8], at: &mut usize, store: &mut deno_core::snapshot::SnapshotLoadDataStore) -> Result<(), String> {
    // the ID read's layout, as an install measures it when it hands out its first ID ([`fn_id`]): a process made from
    // a snapshot hands out none until a template is built, and its fused calls read the IDs the snapshot carries
    measure_id_read(scope);
    disk::snap_read(scope, bytes, at, store)
}

/// Whether `n` could be read as a function record's ID: a plain function's numeric data never is.
#[cfg(not(feature = "analysis"))]
fn in_fn_id_range(n: f64) -> bool {
    n.fract() == 0.0 && (FN_ID_BASE as f64..(FN_ID_BASE + FN_ID_SPAN) as f64).contains(&n)
}

/// One check of a fused member.
enum Step {
    Check(FusedCheck),
    /// `make_arity_guard`'s count: required, interface, member, the row's type
    Arity(usize, String, String, crate::install::TypeTag),
}

/// A fused member's own check: each is the check its guard kind makes (the SAME function), before the
/// body instead of before a JS call to it.
enum FusedCheck {
    /// `install::receiver`: the receiver's brand against the interface; the refusal's form; the member; the
    /// interface's id (`binding::brand`), when its template carries one -- the check then needs no prototype
    ///
    /// The `Cell` is the last brand this check found to BE the interface (`u32::MAX`: none yet), so a loop
    /// over one kind of receiver asks `brand::is_a` once. Only a yes is kept: "is a" only ever
    /// gains ancestors, as installs record parents, so a yes never turns into a no.
    Receiver(IfaceRef, i64, String, Option<u32>, std::cell::Cell<u32>),
    /// `install::webidl::brand_guard`: the brand, then its own WebIDL count
    Brand { iface: String, member: String, need: usize },
    BrandAccessor(String),
    BrandPromise { iface: String, member: String },
    RangeBrand(u32),
    PluginRecv(String),
    WindowRecv,
}

/// The fused form of a guarded function: the checks of its whole guard chain, in the order the guards
/// ran them -- every guard's run function checks, then calls its inner function with the same receiver
/// and arguments and passes its value on (or drops it, for a void member), so one native call doing the
/// checks in turn and then the body is the same call -- over a body whose data is the same in every realm
/// or is one of the two forms naming the interface (`Form::Stub`, `Form::ProtoData`).
fn fuse(scope: &mut v8::PinScope, tr: &Tr, r: &Recipe, guard: &GuardBp) -> Option<FnKind> {
    let mut steps = Vec::new();
    let mut is_void = false;
    let mut g = guard.clone();
    let mut inner = r.inner.as_ref().map(|x| v8::Local::new(scope, x))?;
    let body_r = loop {
        steps.push(match &g {
            GuardBp::Arity { need, iface, member, ty } => {
                is_void |= ty.is_undefined_return();
                StepBp::Arity(*need, iface.clone(), member.clone(), *ty)
            }
            GuardBp::Receiver { ord } => StepBp::Check(CheckBp::Receiver(*ord, r.name.clone())),
            // the brand guard counts arguments itself
            GuardBp::Brand { iface, member, need, is_void: v } => {
                is_void |= *v;
                StepBp::Check(CheckBp::Brand { iface: iface.clone(), member: member.clone(), need: *need })
            }
            GuardBp::BrandAccessor { iface } => StepBp::Check(CheckBp::BrandAccessor(iface.clone())),
            GuardBp::BrandPromise { iface, member } => StepBp::Check(CheckBp::BrandPromise { iface: iface.clone(), member: member.clone() }),
            GuardBp::RangeBrand { idx } => StepBp::Check(CheckBp::RangeBrand(*idx)),
            GuardBp::PluginRecv { iface } => StepBp::Check(CheckBp::PluginRecv(iface.clone())),
            GuardBp::WindowRecv => StepBp::Check(CheckBp::WindowRecv),
        });
        let ir = recipe_of(scope, inner)?;
        match guard_bp(scope, tr, &ir).ok()? {
            None => break ir,
            Some(next) => {
                g = next;
                inner = ir.inner.as_ref().map(|x| v8::Local::new(scope, x))?;
            }
        }
    };
    if body_r.ctor {
        return None;
    }
    // The body's own data rides in the record (`binding::protos::body_data` answers it).
    let data = match &body_r.data {
        None => BodyDataBp::None,
        Some(gd) => {
            let d = v8::Local::new(scope, gd);
            if primitive(d) || d.is_external() {
                BodyDataBp::Prim(PrimBp::of(scope, d)?)
            } else {
                let own: v8::Local<v8::Value> = v8::Local::new(scope, &tr.proto).into();
                match form_of(scope, inner)? {
                    Form::Stub { setter } => {
                        let (member, table) = crate::install::stub::stub_parts(scope, d, own).ok()?;
                        BodyDataBp::Stub { setter, member, table }
                    }
                    Form::ProtoData if d == own => BodyDataBp::ProtoData,
                    Form::ProtoData => return None,
                }
            }
        }
    };
    let body = body_r.cb?;
    Some(FnKind::Fused { steps, body, data, is_void })
}
/// A key naming a template-built member function for as long as its isolate lives, when `d` is that function's data:
/// its record's ID ([`fn_id`]); `None` for any other data. A per-isolate memo keys by it
/// (`core::events::event::evt_keys`). Two V8 calls, as the External test it replaces made.
pub(crate) fn fused_rec_addr(d: v8::Local<v8::Value>) -> Option<usize> {
    id_of(d)
}

/// The body's own data when `d` is a fused member's data (see [`FusedRec`]): what the body was built with,
/// or `undefined` for a body built without any. `None` for any other data.
pub(crate) fn fused_body_data<'s>(scope: &mut v8::PinScope<'s, '_>, d: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    let FnSlot::Fused(p) = fn_slot(scope, d)? else { return None };
    // SAFETY: a fused record of this isolate's table, owned by its cache
    let rec = unsafe { &*p };
    Some(match &rec.data {
        Some(g) => v8::Local::new(scope, g),
        None => v8::undefined(scope).into(),
    })
}

/// The fused member's one native callback: its guards' checks in the order the guards made them (Chrome's
/// order, as the wrappers had it), then the body -- on this call, not through a JS call to another function.
unsafe extern "C" fn cb_fused(info: *const v8::FunctionCallbackInfo) {
    // SAFETY: V8 hands a valid info for the duration of the callback.
    let i = unsafe { &*info };
    let args = v8::FunctionCallbackArguments::from_function_callback_info(i);
    // the data as V8 reads it (`Data()`, the one shim call the External form made too). A `cb_fused` function is made
    // only with its record's ID as its data ([`fn_id`]), so nothing is asked of V8 to confirm it. The isolate and the
    // ID are read off their words as measured ([`ENTRY`]); `FunctionCallbackArguments::get_isolate` is never
    // used (rusty_v8 150.4 casts the C++ isolate to the Rust handle that holds a pointer to it: a SIGSEGV)
    let data = args.data();
    let (isolate, ix) = if ENTRY.load(std::sync::atomic::Ordering::Relaxed) == ENTRY_INLINE {
        // SAFETY: measured ([`entry_cold`]): word ISOLATE_WORD of the info's frame block is the isolate, and the data
        // handle is the address of the slot holding the ID's Smi
        unsafe {
            let isolate = std::mem::transmute::<usize, v8::UnsafeRawIsolatePtr>(*(info as *const usize).add(ISOLATE_WORD));
            (isolate, ((*(handle_word(data) as usize as *const i64) >> 32) - FN_ID_BASE) as usize)
        }
    } else {
        entry_cold(info, data)
    };
    #[cfg(debug_assertions)]
    entry_check(info, data, isolate, ix);
    let t = table_of(isolate);
    // SAFETY: the ID was handed out by `fn_id` for a `Fused` slot of this isolate's table
    // SAFETY: the table outlives its functions; nothing holds a reference into it across this read
    let recs: &Vec<usize> = unsafe { &(*t).recs };
    // SAFETY: the ID was handed out by `fn_id` for this isolate's table
    let w = unsafe { *recs.get_unchecked(ix) };
    let rec = if w & 3 == 0 {
        // SAFETY: an untagged word is a fused record's address ([`encode`]), owned by this isolate's table
        unsafe { &*(w as *const FusedRec) }
    } else {
        // pending (the first call after a snapshot load); any other kind cannot name a cb_fused function
        resolve_fused(i, ix)
    };
    #[cfg(feature = "analysis")]
    if spec::analysis::enabled() {
        // SAFETY: the info V8 gave this callback; the body runs on it exactly as below.
        let mut label = rec.label.get();
        if label.is_empty() {
            // SAFETY: the info V8 gave this callback.
            label = unsafe { crate::analysis_hook::resolve_label(info, rec.name) };
            // A NAMELESS function is an engine continuation (a promise reaction, a pump step): not a member
            // the page can name, never recorded (`?` marks it for good). A named one whose holder this
            // call's receiver does not reach (`get.call({})`, a brand test) is recorded as `?.name` and
            // looked up again next call; the first call that finds its holder fixes the label.
            if rec.name.is_empty() {
                label = "?";
                rec.label.set(label);
            } else if label == "?" || !label.starts_with('?') {
                // exactly "?": an engine stand-in's trap (resolve_label), never recorded, fixed for good
                rec.label.set(label);
            }
        }
        if label != "?" {
            // SAFETY: the info V8 gave this callback; the body runs on it exactly as below.
            unsafe { crate::analysis_hook::traced(info, spec::analysis::Kind::Call, label, &rec.sited, || fused_run(info, i, &args, rec)) };
            return;
        }
    }
    let pass = {
        let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
        let scope = &mut scope.init();
        let mut rv = v8::ReturnValue::from_function_callback_info(i);
        fused_checks(scope, &args, &mut rv, rec)
    };
    if pass {
        // SAFETY: the body is a V8 function callback, called with the info V8 gave this one.
        unsafe { (rec.body)(info) };
        if rec.is_void {
            v8::ReturnValue::from_function_callback_info(i).set_undefined();
        }
    }
}

/// A member's plugin label, when a loaded plugin claims it: `Interface.member` in the host ABI's form (`Navigator.get
/// platform`), the global's own members under `Window`. Interned once per process, as every realm builds the member's
/// template again. `None` -- at once, with no allocation -- in a process no plugin claims anything in.
#[cfg(not(feature = "analysis"))]
fn claim_of(iface: &str, member: &str) -> Option<&'static std::ffi::CStr> {
    if !plugin_host::claims_any() {
        return None;
    }
    let iface = if iface == GLOBAL_TPL { "Window" } else { iface };
    let label = format!("{iface}.{member}");
    if !plugin_host::claims(&label) {
        return None;
    }
    static LABELS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, &'static std::ffi::CStr>>> = std::sync::OnceLock::new();
    let mut labels = LABELS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let c = labels
        .entry(label)
        .or_insert_with_key(|l| Box::leak(std::ffi::CString::new(l.as_str()).unwrap_or_default().into_boxed_c_str()));
    Some(*c)
}

/// A claimed plain, stub or prototype-data member: a fused record with no checks (its body reads its data through
/// `body_data`, which answers the record's, as the analysis build's records do), on [`cb_claimed`].
#[cfg(not(feature = "analysis"))]
fn claimed_ft<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    bd: &mut Bd,
    kind: &FnKind,
    label: &'static std::ffi::CStr,
    length: i32,
    behavior: v8::ConstructorBehavior,
    org: Option<Origin>,
) -> Result<v8::Local<'s, v8::FunctionTemplate>, String> {
    let (body, data, body_ix): (v8::FunctionCallback, Option<v8::Global<v8::Value>>, u32) = match kind {
        FnKind::Plain { cb, data } => {
            let d = data.as_ref().map(|d| {
                let v = d.v8(scope);
                v8::Global::new(scope, v)
            });
            (*cb, d, u32::MAX)
        }
        FnKind::Stub { setter, member, table } => {
            let iref = bd.iref.clone().ok_or("a stub before its interface object")?;
            let (cb, rec) = crate::install::stub::stub_template_of(*setter, *member, *table, iref);
            let ptr = &*rec as *const crate::install::stub::StubTpl;
            bd.keep.push(rec);
            let v = fn_id(scope, FnSlot::Stub(ptr), org.map(|o| Origin { part: 1, ..o })).ok_or("more function records than IDs")?;
            (cb, Some(v8::Global::new(scope, v)), id_index(v))
        }
        FnKind::ProtoData { cb } => {
            let rec = Box::new(bd.iref.clone().ok_or("prototype data before its interface object")?);
            let ptr = &*rec as *const IfaceRef;
            bd.keep.push(rec);
            let v = fn_id(scope, FnSlot::Proto(ptr), org.map(|o| Origin { part: 1, ..o })).ok_or("more function records than IDs")?;
            (*cb, Some(v8::Global::new(scope, v)), id_index(v))
        }
        FnKind::Fused { .. } => return Err("a fused member is claimed in place".into()),
    };
    let rec = Box::new(FusedRec { magic: FUSED_MAGIC, data, steps: Vec::new(), body, is_void: false, claim: Some(label) });
    let ptr = &*rec as *const FusedRec;
    bd.keep.push(rec);
    let d = fn_id(scope, FnSlot::Fused(ptr), org.map(|o| Origin { body: body_ix, ..o })).ok_or("more function records than IDs")?;
    Ok(v8::FunctionTemplate::builder_raw(crate::binding::refs::c(cb_claimed)).data(d).length(length).constructor_behavior(behavior).build(scope))
}

/// F2 (goal 1021): a member built OUTSIDE every interface template (`binding::protos::build_fn` / `build_fn_data`,
/// the hand-mounted `Window` methods and the native accessors) that a loaded plugin CLAIMS. It is a fused record with
/// no checks whose trampoline is [`cb_claimed`], so its body runs and then the claiming plugins' `on_call` (and
/// `on_call_pre` before it) see the call, exactly as a template-built claimed member does. `name` is the member's own
/// function name (`confirm`, `get platform`), which is also the member half of the plugin label `iface.name`; `data`
/// is the body's data (as `build_fn_data` passes), read back through `binding::protos::body_data`.
///
/// `None` when no plugin claims `iface.name`, so the caller builds the function the ordinary way and an unclaimed
/// member pays nothing (invariant I7). The record is leaked (as [`analysis_untemplated`]'s is): a `build_fn` function
/// may outlive any cache it could be kept in. Only reached in a process a plugin claims something in, so the per-realm
/// leak is bounded by the claimed-member count.
#[cfg(not(feature = "analysis"))]
pub fn claim_untemplated<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: v8::FunctionCallback,
    data: Option<v8::Local<v8::Value>>,
    iface: &str,
    name: &str,
    length: i32,
) -> Option<v8::Local<'s, v8::Function>> {
    let label = claim_of(iface, name)?;
    let data = data.map(|d| v8::Global::new(scope, d));
    let rec = Box::leak(Box::new(FusedRec { magic: FUSED_MAGIC, data, steps: Vec::new(), body: cb, is_void: false, claim: Some(label) }));
    let d = fn_id(scope, FnSlot::Fused(rec as *const FusedRec), None)?;
    let f = v8::Function::builder_raw(crate::binding::refs::c(cb_claimed)).data(d).length(length).constructor_behavior(v8::ConstructorBehavior::Throw).build(scope)?;
    let n = v8::String::new(scope, name)?;
    f.set_name(n);
    crate::binding::protos::stamp_engine_fn(scope, f);
    Some(f)
}

/// A CLAIMED member's trampoline: [`cb_fused`]'s checks and body, then -- unless the body threw -- the loaded plugins
/// that claim the member, which may read the call and replace or perturb its answer (`plugin_host::on_call`). A copy
/// rather than a branch inside `cb_fused`, so an unclaimed member's call is byte-for-byte what it was (invariant I7).
unsafe extern "C" fn cb_claimed(info: *const v8::FunctionCallbackInfo) {
    // SAFETY: V8 hands a valid info for the duration of the callback.
    let i = unsafe { &*info };
    let args = v8::FunctionCallbackArguments::from_function_callback_info(i);
    let data = args.data();
    let (isolate, ix) = if ENTRY.load(std::sync::atomic::Ordering::Relaxed) == ENTRY_INLINE {
        // SAFETY: as `cb_fused`.
        unsafe {
            let isolate = std::mem::transmute::<usize, v8::UnsafeRawIsolatePtr>(*(info as *const usize).add(ISOLATE_WORD));
            (isolate, ((*(handle_word(data) as usize as *const i64) >> 32) - FN_ID_BASE) as usize)
        }
    } else {
        entry_cold(info, data)
    };
    let t = table_of(isolate);
    // SAFETY: as `cb_fused`: the ID names a `Fused` slot of this isolate's table, which outlives its functions.
    let recs: &Vec<usize> = unsafe { &(*t).recs };
    let w = unsafe { *recs.get_unchecked(ix) };
    let rec = if w & 3 == 0 {
        // SAFETY: an untagged word is a fused record's address.
        unsafe { &*(w as *const FusedRec) }
    } else {
        resolve_fused(i, ix)
    };
    let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
    let scope = &mut scope.init();
    let pass = {
        let mut rv = v8::ReturnValue::from_function_callback_info(i);
        fused_checks(scope, &args, &mut rv, rec)
    };
    if !pass {
        return;
    }
    // F1 (ABI 3 wave 1): every claiming plugin's `on_call_pre` runs BEFORE the body, may seed the answer with
    // `set_*`, and -- when any returns `KORVUS_SKIP` -- skips the body entirely (its `set_*` then stands). `on_call`
    // still runs at the exit below in both cases. A reborrow (`&mut *scope`) so the body and `on_call` keep the scope.
    let skip = if let Some(label) = rec.claim {
        let rv = v8::ReturnValue::from_function_callback_info(i);
        let mut call = crate::plugin_call::V8Call { scope: &mut *scope, args: &args, rv };
        plugin_host::on_call_pre(label, &mut call)
    } else {
        false
    };
    if !skip {
        let threw = {
            let tc = std::pin::pin!(v8::TryCatch::new(scope));
            let tc = &mut tc.init();
            // SAFETY: the body is a V8 function callback, called with the info V8 gave this one.
            unsafe { (rec.body)(info) };
            if tc.has_caught() {
                tc.rethrow();
                true
            } else {
                false
            }
        };
        if threw {
            return;
        }
        if rec.is_void {
            v8::ReturnValue::from_function_callback_info(i).set_undefined();
        }
    }
    if let Some(label) = rec.claim {
        let rv = v8::ReturnValue::from_function_callback_info(i);
        let mut call = crate::plugin_call::V8Call { scope, args: &args, rv };
        plugin_host::on_call(label, &mut call);
    }
}

/// [`cb_fused`]'s checks-then-body, for the call recorder to run inside its capture. A copy rather than
/// a shared function so a production build's `cb_fused` is byte-for-byte what it was (a hook on this
/// path moved the per-call bench through code layout alone, `touch-log`).
#[cfg(feature = "analysis")]
fn fused_run(info: *const v8::FunctionCallbackInfo, i: &v8::FunctionCallbackInfo, args: &v8::FunctionCallbackArguments, rec: &FusedRec) {
    let pass = {
        let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
        let scope = &mut scope.init();
        let mut rv = v8::ReturnValue::from_function_callback_info(i);
        fused_checks(scope, args, &mut rv, rec)
    };
    if pass {
        // SAFETY: the body is a V8 function callback, called with the info V8 gave `cb_fused`.
        unsafe { (rec.body)(info) };
        if rec.is_void {
            v8::ReturnValue::from_function_callback_info(i).set_undefined();
        }
    }
}

fn fused_checks(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, rec: &FusedRec) -> bool {
    for step in &rec.steps {
        let ok = match step {
            Step::Check(c) => fused_check(scope, args, rv, c),
            Step::Arity(need, iface, member, ty) => {
                let have = args.length() as usize;
                if have < *need {
                    let msg = crate::binding::protos::exec_arity_msg(iface, member, *need, have);
                    crate::binding::protos::raise_webidl_type_error(scope, rv, &msg, ty.rejects_rather_than_throws());
                    false
                } else {
                    true
                }
            }
        };
        if !ok {
            return false;
        }
    }
    true
}

/// One guard's own check (the SAME function its run function calls). `false`: refused, answered.
fn fused_check(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, c: &FusedCheck) -> bool {
    match c {
        FusedCheck::Receiver(iface, ord, member, id, known) => {
            let this: v8::Local<v8::Value> = args.this().into();
            // The interface's id, fixed when its template was built: the brand check needs no prototype,
            // and the realm's prototype is read only to word a refusal. (Without one, the prototype's --
            // `table_receiver_ok`, which reads the same id off it, or walks.)
            let ok = match id {
                Some(t) => match crate::binding::brand::brand_of(scope, this) {
                    Some(b) if b == known.get() => true,
                    Some(b) if crate::binding::brand::is_a(b, *t) => {
                        known.set(b);
                        true
                    }
                    _ => false,
                },
                None => {
                    let proto: v8::Local<v8::Value> = match iface.proto(scope) {
                        Some(p) => p.into(),
                        None => v8::undefined(scope).into(),
                    };
                    crate::binding::protos::table_receiver_ok(scope, this, proto)
                }
            };
            if ok {
                true
            } else {
                let proto: v8::Local<v8::Value> = match iface.proto(scope) {
                    Some(p) => p.into(),
                    None => v8::undefined(scope).into(),
                };
                match ord {
                    // a promise member rejects, in Chrome's words: "Failed to execute 'm' on 'I': ..." /
                    // "Failed to read the 'a' property from 'I': ..." (measured)
                    1 => {
                        let msg = crate::binding::protos::illegal_invocation_message(scope, proto, member);
                        rv.set(crate::binding::protos::rejected_type_error(scope, &msg))
                    }
                    2 => rv.set(v8::undefined(scope).into()),
                    _ => crate::binding::protos::throw_type_error(scope, "Illegal invocation"),
                }
                false
            }
        }
        FusedCheck::Brand { iface, member, need } => crate::install::webidl::brand_guard_check(scope, args, iface, |_| member.clone(), *need),
        FusedCheck::BrandAccessor(iface) => crate::install::webidl::brand_accessor_check(scope, args, iface),
        FusedCheck::BrandPromise { iface, member } => crate::install::webidl::brand_promise_check(scope, args, rv, iface, |_| member.clone()),
        FusedCheck::RangeBrand(idx) => crate::install::live::ranges::range_brand_check(scope, args, *idx),
        FusedCheck::PluginRecv(iface) => crate::core::html::plugins::recv_guard_check(scope, args, iface),
        FusedCheck::WindowRecv => crate::core::html::window::recv_guard_check(scope, args),
    }
}

// ── the blueprint on disk: a fresh process builds from what an earlier run recorded ──────────
//
// One file per (binary, surface, environment): `<cache>/<build id>/<surface fp>-<env fp>.bp`. A file is
// trusted only when its build id is this binary's and its checksum holds: callbacks and `MemberDef`
// statics are stored as offsets from a function of this binary, which only the same link keeps valid.
// Blueprints whose install read the persona are never written (they are that persona's alone).
/// The binary's anchor and image range, for the snapshot's external references (`binding::refs`).
pub(crate) use disk::{anchor, image_known, in_image};

mod disk {
    use super::*;

    pub(super) static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    // \x07: the `Unscopables` entry (tag 5) -- a file of an older format is refused, not misread.
    // \x08: the final-chains section, after the refusals -- and the word-wise [`checksum`].
    const MAGIC: &[u8; 8] = b"KVTPLBP\x09";
    /// build-id directories kept (a rebuild starts a new one; the oldest go)
    const KEEP_BUILDS: usize = 3;

/// Little-endian writer.
#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.0.push(v as u8);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn usize(&mut self, v: usize) {
        self.u32(v as u32);
    }
}

/// Little-endian reader; every read is bounds-checked (a short or corrupt file reads as `None`).
struct R<'a> {
    b: &'a [u8],
    i: usize,
    /// The bytes are a payload whose checksum has passed: its strings were written by this
    /// binary's own writer from `String`s, so they are UTF-8 without checking each one again.
    trusted: bool,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn bool(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_bits(self.u64()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let bytes = self.take(n)?.to_vec();
        if self.trusted {
            // SAFETY: `trusted` is set only on the reader over `payload` in `disk::read`, AFTER `checksum(payload)` matched
            // -- the checksum covers exactly these bytes, every string included -- and that payload was written by
            // `disk::write` from `String`s of this same binary (the build id is checked first): it is the UTF-8 it was
            // written as.
            return Some(unsafe { String::from_utf8_unchecked(bytes) });
        }
        String::from_utf8(bytes).ok()
    }
    fn usize(&mut self) -> Option<usize> {
        Some(self.u32()? as usize)
    }
    fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

/// FNV-1a over the payload: a torn or edited file is refused before any offset in it is trusted.
///
/// Over 8-byte WORDS in four independent lanes, then the tail bytes and the length: byte by byte it was one
/// multiply chain per byte, ~5 ms of every warm start on a 3.8 MB blueprint file -- 5.4-5.8 % of a page's cycles,
/// all of it in this loop (`perf annotate`, 2026-10-13). Still 64 bits over every byte, so a torn or damaged file is
/// refused as before; the format version ([`disk`]'s `MAGIC`) moved with it.
pub(super) fn checksum(b: &[u8]) -> u64 {
    const P: u64 = 0x0100_0000_01b3;
    let mut lanes: [u64; 4] = [0xcbf2_9ce4_8422_2325, 0x8422_2325_cbf2_9ce4, 0x9e37_79b9_7f4a_7c15, 0x6a09_e667_f3bc_c909];
    let mut blocks = b.chunks_exact(32);
    for block in &mut blocks {
        for (i, lane) in lanes.iter_mut().enumerate() {
            let w = u64::from_le_bytes(block[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
            *lane = (*lane ^ w).wrapping_mul(P);
        }
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for l in lanes {
        h = (h ^ l).wrapping_mul(P);
    }
    for x in blocks.remainder() {
        h = (h ^ *x as u64).wrapping_mul(P);
    }
    (h ^ b.len() as u64).wrapping_mul(P)
}

/// The running binary's GNU build id (`NT_GNU_BUILD_ID` in a `PT_NOTE` segment of /proc/self/exe):
/// what makes the offsets in a file valid -- the same id is the same link. `None`: no disk cache.
pub(super) fn build_id() -> Option<&'static [u8]> {
    static ID: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open("/proc/self/exe").ok()?;
        let mut eh = [0u8; 64];
        f.read_exact(&mut eh).ok()?;
        if &eh[0..4] != b"\x7fELF" || eh[4] != 2 || eh[5] != 1 {
            return None; // ELF64, little-endian only
        }
        let phoff = u64::from_le_bytes(eh[0x20..0x28].try_into().ok()?);
        let phentsize = u16::from_le_bytes(eh[0x36..0x38].try_into().ok()?) as u64;
        let phnum = u16::from_le_bytes(eh[0x38..0x3a].try_into().ok()?) as u64;
        for i in 0..phnum {
            let mut ph = [0u8; 56];
            f.seek(SeekFrom::Start(phoff + i * phentsize)).ok()?;
            f.read_exact(&mut ph).ok()?;
            if u32::from_le_bytes(ph[0..4].try_into().ok()?) != 4 {
                continue; // PT_NOTE
            }
            let off = u64::from_le_bytes(ph[8..16].try_into().ok()?);
            let size = u64::from_le_bytes(ph[32..40].try_into().ok()?).min(1 << 16) as usize;
            let mut notes = vec![0u8; size];
            f.seek(SeekFrom::Start(off)).ok()?;
            f.read_exact(&mut notes).ok()?;
            let mut j = 0usize;
            let al = |n: usize| (n + 3) & !3;
            while j + 12 <= notes.len() {
                let namesz = u32::from_le_bytes(notes[j..j + 4].try_into().ok()?) as usize;
                let descsz = u32::from_le_bytes(notes[j + 4..j + 8].try_into().ok()?) as usize;
                let ty = u32::from_le_bytes(notes[j + 8..j + 12].try_into().ok()?);
                let name = notes.get(j + 12..j + 12 + namesz)?;
                let d0 = j + 12 + al(namesz);
                let desc = notes.get(d0..d0 + descsz)?;
                if ty == 3 && name.starts_with(b"GNU") {
                    return Some(desc.to_vec());
                }
                j = d0 + al(descsz);
            }
        }
        None
    })
    .as_deref()
}

/// The running binary's mapped range (the lowest start to the highest end of the mappings of
/// /proc/self/exe): a callback or a static must lie inside it to be written as an offset.
fn image_range() -> Option<(usize, usize)> {
    static R: std::sync::OnceLock<Option<(usize, usize)>> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        let exe = std::fs::read_link("/proc/self/exe").ok()?;
        let exe = exe.to_str()?.to_string();
        let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        for line in maps.lines() {
            let mut parts = line.split_whitespace();
            let range = parts.next()?;
            let path = parts.nth(4).unwrap_or("");
            if path != exe {
                continue;
            }
            let (a, b) = range.split_once('-')?;
            lo = lo.min(usize::from_str_radix(a, 16).ok()?);
            hi = hi.max(usize::from_str_radix(b, 16).ok()?);
        }
        (lo < hi).then_some((lo, hi))
    })
}

/// The anchor every offset is taken from: a function of this binary.
pub(crate) fn anchor() -> usize {
    anchor as fn() -> usize as usize
}

/// Where blueprint files live: `KORVUS_TPL_CACHE_DIR`, else `$XDG_CACHE_HOME/korvus/templates`, else
/// `~/.cache/korvus/templates`; `KORVUS_TPL_CACHE=off` turns the disk off.
fn cache_dir() -> Option<std::path::PathBuf> {
    if std::env::var("KORVUS_TPL_CACHE").is_ok_and(|v| v == "off") {
        return None;
    }
    if let Some(d) = std::env::var_os("KORVUS_TPL_CACHE_DIR") {
        return Some(d.into());
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".cache")))?;
    Some(base.join("korvus").join("templates"))
}

/// The environment a blueprint file is valid for: every `KORVUS_*` variable, `TZ` and the locale
/// variables, sorted -- a different configuration gets its own file, and no list of which variables
/// matter has to be kept. Left out: the ones that only say where output goes or what to report
/// (`*_OUT`, `*_LOG`, `KORVUS_TRACE_*`, `KORVUS_TPL_REPORT`, the cache's own), which build nothing.
fn env_fingerprint() -> u64 {
    // `KORVUS_GPU*` decide what WebGL answers per call (the device's values, or none) and how the GPU service is
    // reached, never what the install records; `KORVUS_NAV_TRACE` only reports
    let output_only = |k: &str| {
        k.ends_with("_OUT") || k.ends_with("_LOG") || k.starts_with("KORVUS_TRACE_") || k == "KORVUS_TPL_REPORT" || k.starts_with("KORVUS_TPL_CACHE")
            || k.starts_with("KORVUS_GPU") || k == "KORVUS_NAV_TRACE"
            // `KORVUS_SNAPSHOT*` decide whether the page realm is deserialised (and whether that is reported),
            // never what the install records -- and a snapshot run must find the files a fresh one wrote
            || k.starts_with("KORVUS_SNAPSHOT")
    };
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| (k.starts_with("KORVUS_") && !output_only(k)) || k == "TZ" || k == "LANG" || k.starts_with("LC_"))
        .collect();
    vars.sort();
    let mut b = Vec::new();
    for (k, v) in vars {
        b.extend_from_slice(k.as_bytes());
        b.push(0);
        b.extend_from_slice(v.as_bytes());
        b.push(0);
    }
    checksum(&b)
}

// ── encoding (draft) ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn in_image(p: usize) -> bool {
    image_range().is_some_and(|(lo, hi)| p >= lo && p < hi)
}

/// Whether [`in_image`] can answer at all (the image's range is read from `/proc`).
pub(crate) fn image_known() -> bool {
    image_range().is_some()
}

fn put_cb(w: &mut W, cb: v8::FunctionCallback) -> Result<(), String> {
    let p = cb as usize;
    if !in_image(p) {
        return Err("a callback outside the binary".into());
    }
    w.i64(p as i64 - anchor() as i64);
    Ok(())
}

fn get_cb(r: &mut R) -> Option<v8::FunctionCallback> {
    let p = (anchor() as i64).checked_add(r.i64()?)? as usize;
    if !in_image(p) {
        return None;
    }
    // SAFETY: the file carries this binary's build id and a matching checksum, and the offset was taken
    // from a callback of this binary: the same link puts the same function there.
    Some(unsafe { std::mem::transmute::<usize, v8::FunctionCallback>(p) })
}

/// A member row: an interned one by its name (`install::stub::interned_member` makes it again), any
/// other by its offset in the binary (the `interface!` tables are statics).
fn put_member(w: &mut W, m: &'static crate::install::MemberDef) -> Result<(), String> {
    if crate::install::stub::is_interned(m) {
        w.u8(1);
        w.str(m.label);
        w.str(m.name);
        w.u32(m.length);
        w.bool(m.nullable);
        return Ok(());
    }
    let p = m as *const crate::install::MemberDef as usize;
    if !in_image(p) {
        return Err(format!("member `{}` is neither a static nor interned", m.label));
    }
    w.u8(0);
    w.i64(p as i64 - anchor() as i64);
    Ok(())
}

fn get_member(r: &mut R) -> Option<&'static crate::install::MemberDef> {
    match r.u8()? {
        1 => {
            let (label, name, length, nullable) = (r.str()?, r.str()?, r.u32()?, r.bool()?);
            let iface = label.strip_suffix(name.as_str())?.strip_suffix("::")?;
            Some(crate::install::stub::interned_member(iface, &name, length, nullable))
        }
        0 => {
            let p = (anchor() as i64).checked_add(r.i64()?)? as usize;
            if !in_image(p) || p % std::mem::align_of::<crate::install::MemberDef>() != 0 {
                return None;
            }
            // SAFETY: as `get_cb`: this binary's `MemberDef` static at the offset it was written from.
            Some(unsafe { &*(p as *const crate::install::MemberDef) })
        }
        _ => None,
    }
}

/// A `&'static str` made again from a file: one leak per distinct string, for the process.
fn intern(s: String) -> &'static str {
    static POOL: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>> = std::sync::OnceLock::new();
    let mut pool = POOL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = pool.get(s.as_str()) {
        return v;
    }
    let v: &'static str = Box::leak(s.into_boxed_str());
    pool.insert(v);
    v
}

fn put_prim(w: &mut W, p: &PrimBp) -> Result<(), String> {
    match p {
        PrimBp::Undefined => w.u8(0),
        PrimBp::Null => w.u8(1),
        PrimBp::Bool(b) => {
            w.u8(2);
            w.bool(*b)
        }
        PrimBp::Num(n) => {
            w.u8(3);
            w.f64(*n)
        }
        PrimBp::Str(s) => {
            w.u8(4);
            w.str(s)
        }
        PrimBp::Member(m) => {
            w.u8(5);
            put_member(w, m)?
        }
    }
    Ok(())
}

fn get_prim(r: &mut R) -> Option<PrimBp> {
    Some(match r.u8()? {
        0 => PrimBp::Undefined,
        1 => PrimBp::Null,
        2 => PrimBp::Bool(r.bool()?),
        3 => PrimBp::Num(r.f64()?),
        4 => PrimBp::Str(r.str()?),
        5 => PrimBp::Member(get_member(r)?),
        _ => return None,
    })
}

fn put_opt_u(w: &mut W, v: Option<usize>) {
    match v {
        None => w.u8(0),
        Some(x) => {
            w.u8(1);
            w.usize(x)
        }
    }
}

fn get_opt_u(r: &mut R) -> Option<Option<usize>> {
    Some(match r.u8()? {
        0 => None,
        1 => Some(r.usize()?),
        _ => return None,
    })
}

fn put_attrs(w: &mut W, a: Attrs) {
    w.u8(a.read_only as u8 | (a.dont_enum as u8) << 1 | (a.dont_delete as u8) << 2);
}

fn get_attrs(r: &mut R) -> Option<Attrs> {
    let b = r.u8()?;
    (b < 8).then_some(Attrs { read_only: b & 1 != 0, dont_enum: b & 2 != 0, dont_delete: b & 4 != 0 })
}

fn put_key(w: &mut W, k: &KeyBp) {
    match k {
        KeyBp::Str(s) => {
            w.u8(0);
            w.str(s)
        }
        KeyBp::Sym(i) => {
            w.u8(1);
            w.u8(*i)
        }
    }
}

fn get_key(r: &mut R) -> Option<KeyBp> {
    Some(match r.u8()? {
        0 => KeyBp::Str(r.str()?),
        1 => {
            let i = r.u8()?;
            if i >= 11 {
                return None;
            }
            KeyBp::Sym(i)
        }
        _ => return None,
    })
}

fn put_entry(w: &mut W, e: &EntryBp) -> Result<(), String> {
    match e {
        EntryBp::Value(p, a) => {
            w.u8(0);
            put_prim(w, p)?;
            put_attrs(w, *a)
        }
        EntryBp::Func(i, a) => {
            w.u8(1);
            w.usize(*i);
            put_attrs(w, *a)
        }
        EntryBp::Accessor(g, s, a) => {
            w.u8(2);
            put_opt_u(w, *g);
            put_opt_u(w, *s);
            put_attrs(w, *a)
        }
        EntryBp::Intrinsic(i, a) => {
            w.u8(3);
            w.u8(*i as u8);
            put_attrs(w, *a)
        }
        EntryBp::Namespace(rows, a) => {
            w.u8(4);
            put_attrs(w, *a);
            w.usize(rows.len());
            for (k, e) in rows {
                put_key(w, k);
                put_entry(w, e)?;
            }
        }
        EntryBp::Unscopables(names, a) => {
            w.u8(5);
            put_attrs(w, *a);
            w.usize(names.len());
            for n in names {
                put_key(w, &KeyBp::Str(n.clone()));
            }
        }
    }
    Ok(())
}

fn get_entry(r: &mut R, nfns: usize) -> Option<EntryBp> {
    let fi = |r: &mut R| r.usize().filter(|i| *i < nfns);
    Some(match r.u8()? {
        0 => EntryBp::Value(get_prim(r)?, get_attrs(r)?),
        1 => EntryBp::Func(fi(r)?, get_attrs(r)?),
        2 => {
            let g = get_opt_u(r)?;
            let s = get_opt_u(r)?;
            if g.is_some_and(|i| i >= nfns) || s.is_some_and(|i| i >= nfns) {
                return None;
            }
            EntryBp::Accessor(g, s, get_attrs(r)?)
        }
        3 => {
            let i = match r.u8()? {
                0 => Intr::Entries,
                1 => Intr::ForEach,
                2 => Intr::Keys,
                3 => Intr::Values,
                _ => return None,
            };
            EntryBp::Intrinsic(i, get_attrs(r)?)
        }
        4 => {
            let a = get_attrs(r)?;
            let n = r.usize()?;
            let mut rows = Vec::new();
            for _ in 0..n {
                let k = get_key(r)?;
                rows.push((k, get_entry(r, nfns)?));
            }
            EntryBp::Namespace(rows, a)
        }
        5 => {
            let a = get_attrs(r)?;
            let n = r.usize()?;
            let mut names = Vec::new();
            for _ in 0..n {
                match get_key(r)? {
                    KeyBp::Str(s) => names.push(s),
                    KeyBp::Sym(_) => return None,
                }
            }
            EntryBp::Unscopables(names, a)
        }
        _ => return None,
    })
}

fn put_check(w: &mut W, c: &CheckBp) {
    match c {
        CheckBp::Receiver(ord, member) => {
            w.u8(0);
            w.i64(*ord);
            w.str(member)
        }
        CheckBp::Brand { iface, member, need } => {
            w.u8(1);
            w.str(iface);
            w.str(member);
            w.usize(*need)
        }
        CheckBp::BrandAccessor(i) => {
            w.u8(2);
            w.str(i)
        }
        CheckBp::BrandPromise { iface, member } => {
            w.u8(3);
            w.str(iface);
            w.str(member)
        }
        CheckBp::RangeBrand(i) => {
            w.u8(4);
            w.u32(*i)
        }
        CheckBp::PluginRecv(i) => {
            w.u8(5);
            w.str(i)
        }
        CheckBp::WindowRecv => w.u8(6),
    }
}

fn get_check(r: &mut R) -> Option<CheckBp> {
    Some(match r.u8()? {
        0 => CheckBp::Receiver(r.i64()?, r.str()?),
        1 => CheckBp::Brand { iface: r.str()?, member: r.str()?, need: r.usize()? },
        2 => CheckBp::BrandAccessor(r.str()?),
        3 => CheckBp::BrandPromise { iface: r.str()?, member: r.str()? },
        4 => CheckBp::RangeBrand(r.u32()?),
        5 => CheckBp::PluginRecv(r.str()?),
        6 => CheckBp::WindowRecv,
        _ => return None,
    })
}

fn put_ty(w: &mut W, t: crate::install::TypeTag) {
    w.i32(t.ordinal());
}

fn get_ty(r: &mut R) -> Option<crate::install::TypeTag> {
    let n = r.i32()?;
    let t = crate::install::TypeTag::from_ordinal(n as i64);
    (t.ordinal() == n).then_some(t)
}

fn put_fn(w: &mut W, f: &FnBp) -> Result<(), String> {
    match &f.kind {
        FnKind::Plain { cb, data } => {
            w.u8(0);
            put_cb(w, *cb)?;
            match data {
                None => w.u8(0),
                Some(p) => {
                    w.u8(1);
                    put_prim(w, p)?
                }
            }
        }
        FnKind::Stub { setter, member, table } => {
            w.u8(1);
            w.bool(*setter);
            put_member(w, member)?;
            w.bool(*table)
        }
        FnKind::ProtoData { cb } => {
            w.u8(2);
            put_cb(w, *cb)?
        }
        FnKind::Fused { steps, body, data, is_void } => {
            w.u8(4);
            w.usize(steps.len());
            for s in steps {
                match s {
                    StepBp::Check(c) => {
                        w.u8(0);
                        put_check(w, c)
                    }
                    StepBp::Arity(need, iface, member, ty) => {
                        w.u8(1);
                        w.usize(*need);
                        w.str(iface);
                        w.str(member);
                        put_ty(w, *ty)
                    }
                }
            }
            put_cb(w, *body)?;
            match data {
                BodyDataBp::None => w.u8(0),
                BodyDataBp::Prim(p) => {
                    w.u8(1);
                    put_prim(w, p)?
                }
                BodyDataBp::Stub { setter, member, table } => {
                    w.u8(2);
                    w.bool(*setter);
                    put_member(w, member)?;
                    w.bool(*table)
                }
                BodyDataBp::ProtoData => w.u8(3),
            }
            w.bool(*is_void)
        }
    }
    w.i32(f.length);
    w.str(&f.class_name);
    w.bool(f.ctor);
    w.usize(f.brands.len());
    for (i, p) in &f.brands {
        w.usize(*i);
        put_prim(w, p)?;
    }
    Ok(())
}

fn get_fn(r: &mut R) -> Option<FnBp> {
    let kind = match r.u8()? {
        0 => {
            let cb = get_cb(r)?;
            let data = match r.u8()? {
                0 => None,
                1 => Some(get_prim(r)?),
                _ => return None,
            };
            FnKind::Plain { cb, data }
        }
        1 => FnKind::Stub { setter: r.bool()?, member: get_member(r)?, table: r.bool()? },
        2 => FnKind::ProtoData { cb: get_cb(r)? },
        4 => {
            let n = r.usize()?;
            let mut steps = Vec::with_capacity(n.min(16));
            for _ in 0..n {
                steps.push(match r.u8()? {
                    0 => StepBp::Check(get_check(r)?),
                    1 => StepBp::Arity(r.usize()?, r.str()?, r.str()?, get_ty(r)?),
                    _ => return None,
                });
            }
            let body = get_cb(r)?;
            let data = match r.u8()? {
                0 => BodyDataBp::None,
                1 => BodyDataBp::Prim(get_prim(r)?),
                2 => BodyDataBp::Stub { setter: r.bool()?, member: get_member(r)?, table: r.bool()? },
                3 => BodyDataBp::ProtoData,
                _ => return None,
            };
            FnKind::Fused { steps, body, data, is_void: r.bool()? }
        }
        _ => return None,
    };
    let length = r.i32()?;
    let class_name = r.str()?;
    let ctor = r.bool()?;
    let nb = r.usize()?;
    let mut brands = Vec::with_capacity(nb.min(FN_BRANDS.len()));
    for _ in 0..nb {
        let i = r.usize().filter(|i| *i < FN_BRANDS.len())?;
        brands.push((i, get_prim(r)?));
    }
    Some(FnBp { kind, length, class_name, ctor, brands })
}

fn put_list(w: &mut W, l: &[(KeyBp, EntryBp)]) -> Result<(), String> {
    w.usize(l.len());
    for (k, e) in l {
        put_key(w, k);
        put_entry(w, e)?;
    }
    Ok(())
}

fn get_list(r: &mut R, nfns: usize) -> Option<Vec<(KeyBp, EntryBp)>> {
    let n = r.usize()?;
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        out.push((get_key(r)?, get_entry(r, nfns)?));
    }
    Some(out)
}

fn put_blueprint(w: &mut W, bp: &Blueprint) -> Result<(), String> {
    w.str(&bp.iface);
    w.usize(bp.fns.len());
    for f in &bp.fns {
        put_fn(w, f)?;
    }
    w.usize(bp.ctor);
    for l in [&bp.proto, &bp.statics, &bp.late_proto, &bp.late_statics, &bp.global] {
        put_list(w, l)?;
    }
    match &bp.parent {
        None => w.u8(0),
        Some(p) => {
            w.u8(1);
            w.str(p)
        }
    }
    put_replay(w, &bp.replay);
    put_deltas(w, &bp.deltas)?;
    Ok(())
}

/// What a template's install did besides its two objects ([`Replay`](crate::install::installer::Replay)), as the
/// blueprint file and a snapshot's template section write it.
fn put_replay(w: &mut W, rp: &crate::install::installer::Replay) {
    w.usize(rp.rows.len());
    for e in &rp.rows {
        w.u32(e.realm);
        w.str(e.iface);
        w.str(e.member);
        w.u8(e.holder as u8);
        w.u8(e.pass as u8);
    }
    w.usize(rp.gaps.len());
    for g in &rp.gaps {
        w.str(g.interface);
        w.str(&g.member);
    }
    w.usize(rp.marks.len());
    for m in &rp.marks {
        match m {
            Mark::Layer { iface, member, layer } => {
                w.u8(0);
                w.str(iface);
                w.str(member);
                w.str(layer)
            }
            Mark::Receiver { iface, member, policy } => {
                w.u8(1);
                w.str(iface);
                w.str(member);
                w.str(policy)
            }
            Mark::Flags { realm, iface, member, holder, en, cfg } => {
                w.u8(2);
                w.str(realm);
                w.str(iface);
                w.str(member);
                w.u8(*holder as u8);
                w.bool(*en);
                w.bool(*cfg)
            }
        }
    }
}

fn put_slot(w: &mut W, sl: &SlotBp) {
    w.str(&sl.iface);
    w.u8(sl.holder);
    put_key(w, &sl.key);
    w.u8(sl.part);
}

fn get_slot(r: &mut R) -> Option<SlotBp> {
    Some(SlotBp { iface: r.str()?, holder: r.u8()?, key: get_key(r)?, part: r.u8()? })
}

fn put_part(w: &mut W, p: &PartBp) -> Result<(), String> {
    match p {
        PartBp::Absent => w.u8(0),
        PartBp::Same => w.u8(1),
        PartBp::Prim(v) => {
            w.u8(2);
            put_prim(w, v)?;
        }
        PartBp::Fn(i) => {
            w.u8(3);
            w.usize(*i);
        }
        PartBp::Ref(sl) => {
            w.u8(4);
            put_slot(w, sl);
        }
        PartBp::Iface => w.u8(8),
    }
    Ok(())
}

fn get_part(r: &mut R, nfns: usize) -> Option<PartBp> {
    Some(match r.u8()? {
        0 => PartBp::Absent,
        1 => PartBp::Same,
        2 => PartBp::Prim(get_prim(r)?),
        3 => PartBp::Fn(r.usize().filter(|i| *i < nfns)?),
        4 => PartBp::Ref(get_slot(r)?),
        8 => PartBp::Iface,
        _ => return None,
    })
}

fn put_deltas(w: &mut W, d: &Option<DeltaSet>) -> Result<(), String> {
    let Some(d) = d else {
        w.u8(0);
        return Ok(());
    };
    w.u8(1);
    w.usize(d.holders.len());
    for (hi, list) in &d.holders {
        w.u8(*hi);
        w.usize(list.len());
        for (k, e) in list {
            put_key(w, k);
            match e {
                DeltaBp::Keep => w.u8(0),
                DeltaBp::Changed { data, parts, attrs } => {
                    w.u8(1);
                    w.bool(*data);
                    for p in parts {
                        put_part(w, p)?;
                    }
                    put_attrs(w, *attrs);
                }
            }
        }
    }
    Ok(())
}

fn get_deltas(r: &mut R, nfns: usize) -> Option<Option<DeltaSet>> {
    if r.u8()? == 0 {
        return Some(None);
    }
    let nh = r.usize()?;
    let mut holders = Vec::with_capacity(nh.min(4));
    for _ in 0..nh {
        let hi = r.u8()?;
        let n = r.usize()?;
        let mut list = Vec::with_capacity(n.min(1 << 12));
        for _ in 0..n {
            let k = get_key(r)?;
            let e = match r.u8()? {
                0 => DeltaBp::Keep,
                1 => {
                    let data = r.bool()?;
                    let parts = [get_part(r, nfns)?, get_part(r, nfns)?, get_part(r, nfns)?];
                    DeltaBp::Changed { data, parts, attrs: get_attrs(r)? }
                }
                _ => return None,
            };
            list.push((k, e));
        }
        holders.push((hi, list));
    }
    Some(Some(DeltaSet { holders }))
}

fn get_blueprint(r: &mut R) -> Option<Blueprint> {
    let iface = r.str()?;
    let nfns = r.usize()?;
    // a function refers to another by index, possibly a later one: read the count first
    let mut fns = Vec::with_capacity(nfns.min(1 << 14));
    for _ in 0..nfns {
        fns.push(get_fn(r)?);
    }
    // a record of changes carries no function of its own (`#delta:*`, `#deltas`): no interface object either
    let ctor = r.usize().filter(|i| *i < nfns || (nfns == 0 && *i == 0))?;
    let proto = get_list(r, nfns)?;
    let statics = get_list(r, nfns)?;
    let late_proto = get_list(r, nfns)?;
    let late_statics = get_list(r, nfns)?;
    let global = get_list(r, nfns)?;
    let parent = match r.u8()? {
        0 => None,
        1 => Some(r.str()?),
        _ => return None,
    };
    let replay = get_replay(r)?;
    let deltas = get_deltas(r, nfns)?;
    Some(Blueprint { iface, fns, ctor, proto, statics, late_proto, late_statics, global, parent, replay, from_disk: true, deltas })
}

/// [`put_replay`]'s reader.
fn get_replay(r: &mut R) -> Option<crate::install::installer::Replay> {
    use crate::install::installer::{Gap, Holder, LedgerEntry, Pass, Replay};
    let n = r.usize()?;
    let mut rows = Vec::with_capacity(n.min(1 << 14));
    for _ in 0..n {
        let realm = r.u32()?;
        let (i, m) = (intern(r.str()?), intern(r.str()?));
        let holder = match r.u8()? {
            0 => Holder::Prototype,
            1 => Holder::InterfaceObject,
            _ => return None,
        };
        let pass = match r.u8()? {
            0 => Pass::Create,
            1 => Pass::Backfill,
            2 => Pass::Verify,
            _ => return None,
        };
        rows.push(LedgerEntry { realm, iface: i, member: m, holder, pass });
    }
    let n = r.usize()?;
    let mut gaps = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        gaps.push(Gap { interface: intern(r.str()?), member: r.str()? });
    }
    let n = r.usize()?;
    let mut marks = Vec::with_capacity(n.min(1 << 14));
    for _ in 0..n {
        marks.push(match r.u8()? {
            0 => Mark::Layer { iface: r.str()?, member: r.str()?, layer: intern(r.str()?) },
            1 => Mark::Receiver { iface: r.str()?, member: r.str()?, policy: intern(r.str()?) },
            2 => {
                let (realm, iface, member) = (intern(r.str()?), r.str()?, r.str()?);
                let holder = match r.u8()? {
                    0 => Holder::Prototype,
                    1 => Holder::InterfaceObject,
                    _ => return None,
                };
                Mark::Flags { realm, iface, member, holder, en: r.bool()?, cfg: r.bool()? }
            }
            _ => return None,
        });
    }
    Some(Replay { rows, gaps, marks })
}


    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// [`super::snapshot_file`].
    #[cfg(not(feature = "analysis"))]
    pub(super) fn snapshot_file(persona: u64) -> Option<std::path::PathBuf> {
        let dir = cache_dir()?.join(hex(build_id()?));
        Some(dir.join(format!("snap-{:016x}-{persona:016x}.bin", env_fingerprint())))
    }

    /// The effective uid of this process (`/proc/self` belongs to it).
    #[cfg(unix)]
    fn euid() -> Option<u32> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").ok().map(|m| m.uid())
    }

    /// Whether a cache file may be read as this binary's own (a blueprint file's offsets name code; a snapshot is
    /// code): the file and its directory belong to this user, and nobody else may write either (MAIN, 10-13).
    #[cfg(unix)]
    pub(super) fn trusted(path: &std::path::Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Some(me) = euid() else { return false };
        let ok = |p: &std::path::Path| std::fs::metadata(p).is_ok_and(|m| m.uid() == me && m.mode() & 0o022 == 0);
        ok(path) && path.parent().is_some_and(ok)
    }
    /// Windows: the cache lives under the user profile ([`cache_dir`]), whose NTFS ACLs already restrict it
    /// to this user and administrators -- the guarantee POSIX mode bits carry on Unix. A file that exists
    /// with an existing parent is trusted on that basis (there is no portable mode to inspect).
    #[cfg(not(unix))]
    pub(super) fn trusted(path: &std::path::Path) -> bool {
        std::fs::metadata(path).is_ok() && path.parent().is_some_and(|p| std::fs::metadata(p).is_ok())
    }

    /// `dir`, created if missing, made this user's alone ([`trusted`]).
    #[cfg(unix)]
    pub(super) fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    /// Windows: a directory created under the user profile inherits the user-private NTFS ACLs.
    #[cfg(not(unix))]
    pub(super) fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)
    }

    /// `bytes` into a new file `path` that only this user can read or write.
    #[cfg(unix)]
    pub(super) fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(bytes)
    }
    /// Windows: the file inherits the user-private NTFS ACLs of its parent (under the user profile).
    #[cfg(not(unix))]
    pub(super) fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
        f.write_all(bytes)
    }

    fn file_for(fp: u64) -> Option<std::path::PathBuf> {
        let dir = cache_dir()?.join(hex(build_id()?));
        Some(dir.join(format!("{fp:016x}-{:016x}.bp", env_fingerprint())))
    }

    fn loaded() -> &'static std::sync::Mutex<std::collections::HashSet<u64>> {
        static L: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u64>>> = std::sync::OnceLock::new();
        L.get_or_init(Default::default)
    }

    /// The surfaces whose file [`ensure_loaded`] read, verified and indexed in this process.
    fn ready() -> &'static std::sync::Mutex<std::collections::HashSet<u64>> {
        static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u64>>> = std::sync::OnceLock::new();
        R.get_or_init(Default::default)
    }

    /// Whether surface `fp`'s file is in this process: read now if nothing read it yet, and verified. `false`
    /// for a file that is missing, damaged, another build's, or not this user's alone ([`trusted`]).
    #[cfg(not(feature = "analysis"))]
    pub(super) fn blueprints_loaded(fp: u64) -> bool {
        ensure_loaded(fp);
        ready().lock().is_ok_and(|r| r.contains(&fp))
    }

    /// Read the file for surface `fp` into the process's blueprints, once per process.
    pub(super) fn ensure_loaded(fp: u64) {
        if !ON.load(std::sync::atomic::Ordering::Relaxed) || !loaded().lock().is_ok_and(|mut l| l.insert(fp)) {
            return;
        }
        let Some(path) = file_for(fp) else { return };
        let t0 = reporting().then(std::time::Instant::now);
        if !trusted(&path) {
            if path.exists() {
                report(&format!("disk: {} is not this user's alone; not read", path.display()));
            }
            return;
        }
        let Ok(bytes) = std::fs::read(&path) else { return };
        match read(&bytes, fp) {
            Some((bps, refused, chains)) => {
                let n = bps.len();
                let bytes: std::sync::Arc<[u8]> = bytes.into();
                // what the install that wrote the file chained each interface to; one this process recorded itself wins
                if let Ok(mut m) = final_chains().lock() {
                    for (k, p, c) in chains {
                        m.entry(k).or_insert((p, c));
                    }
                }
                // indexed, not decoded: each on its first ask ([`process_blueprint`])
                if let Ok(mut m) = lazy_blueprints().lock() {
                    for (k, range) in bps {
                        m.entry(k).or_insert_with(|| (bytes.clone(), range));
                    }
                }
                if let Ok(mut m) = process_refused().lock() {
                    for (k, why) in refused {
                        m.entry(k).or_insert(why);
                    }
                }
                if let Ok(mut r) = ready().lock() {
                    r.insert(fp);
                }
                let us = t0.map(|t| t.elapsed().as_micros()).unwrap_or(0);
                report(&format!("disk: loaded {n} blueprints ({} bytes, indexed: each decoded on its first ask) in {us}us from {}", bytes.len(), path.display()));
            }
            None => {
                report(&format!("disk: {} is not this binary's or is damaged; removed", path.display()));
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn read(b: &[u8], fp: u64) -> Option<(Vec<(String, std::ops::Range<usize>)>, Vec<(String, String)>, Vec<(String, ChainTo, ChainTo)>)> {
        let mut r = R { b, i: 0, trusted: false };
        if r.take(8)? != MAGIC {
            return None;
        }
        let n = r.u32()? as usize;
        if r.take(n)? != build_id()? {
            return None;
        }
        if r.u64()? != fp || r.u64()? != env_fingerprint() {
            return None;
        }
        let len = r.u64()? as usize;
        let sum = r.u64()?;
        let at = r.i;
        let payload = r.take(len)?;
        if !r.done() || checksum(payload) != sum {
            return None;
        }
        let mut p = R { b: payload, i: 0, trusted: true };
        let n = p.usize()?;
        let mut bps = Vec::with_capacity(n.min(1 << 12));
        for _ in 0..n {
            let k = p.str()?;
            let len = p.usize()?;
            let start = at + p.i;
            p.take(len)?;
            bps.push((k, start..start + len));
        }
        let n = p.usize()?;
        let mut refused = Vec::with_capacity(n.min(1 << 12));
        for _ in 0..n {
            refused.push((p.str()?, p.str()?));
        }
        let n = p.usize()?;
        let mut chains = Vec::with_capacity(n.min(1 << 12));
        for _ in 0..n {
            let k = p.str()?;
            let pc = get_chain(&mut p)?;
            let cc = get_chain(&mut p)?;
            chains.push((k, pc, cc));
        }
        p.done().then_some((bps, refused, chains))
    }

    /// One record of a file [`read`] indexed: the blueprint, or `None` if it does not decode.
    pub(super) fn decode_record(b: &[u8]) -> Option<Blueprint> {
        let mut r = R { b, i: 0, trusted: true };
        let bp = get_blueprint(&mut r)?;
        r.done().then_some(bp)
    }

    fn put_chain(w: &mut W, c: &ChainTo) {
        match c {
            ChainTo::Default => w.u8(0),
            ChainTo::Iface(n) => {
                w.u8(1);
                w.str(n);
            }
            ChainTo::Intrinsic(n) => {
                w.u8(2);
                w.str(n);
            }
        }
    }

    fn get_chain(r: &mut R) -> Option<ChainTo> {
        match r.u8()? {
            0 => Some(ChainTo::Default),
            1 => Some(ChainTo::Iface(r.str()?)),
            2 => Some(ChainTo::Intrinsic(r.str()?)),
            _ => None,
        }
    }

    /// Write every surface-keyed blueprint and refusal this process holds for surface `fp`.
    pub(super) fn write(fp: u64, chains: &[(String, ChainTo, ChainTo)]) {
        let Some(path) = file_for(fp) else { return };
        let t0 = reporting().then(std::time::Instant::now);
        let tail = format!("|s{fp:016x}");
        let mut bps: Vec<(String, std::sync::Arc<Blueprint>)> = match process_blueprints().lock() {
            Ok(m) => m.iter().filter(|(k, _)| k.ends_with(&tail)).map(|(k, v)| (k.clone(), v.clone())).collect(),
            Err(_) => return,
        };
        bps.sort_by(|a, b| a.0.cmp(&b.0));
        let mut refused: Vec<(String, String)> = match process_refused().lock() {
            Ok(m) => m.iter().filter(|(k, _)| k.ends_with(&tail)).map(|(k, v)| (k.clone(), v.clone())).collect(),
            Err(_) => return,
        };
        refused.sort();
        // the records nothing asked for, as they were read (a key recorded or decoded since is written from its blueprint)
        let mut raw: Vec<(String, std::sync::Arc<[u8]>, std::ops::Range<usize>)> = match lazy_blueprints().lock() {
            Ok(m) => m.iter().filter(|(k, _)| k.ends_with(&tail)).map(|(k, (b, r))| (k.clone(), b.clone(), r.clone())).collect(),
            Err(_) => return,
        };
        raw.retain(|(k, _, _)| bps.binary_search_by(|(b, _)| b.as_str().cmp(k)).is_err());
        raw.sort_by(|a, b| a.0.cmp(&b.0));
        let mut body = W::default();
        let mut n = 0usize;
        let mut entries = W::default();
        let mut raw = raw.into_iter().peekable();
        for (k, bp) in &bps {
            while let Some((rk, b, r)) = raw.next_if(|(rk, _, _)| rk < k) {
                entries.str(&rk);
                entries.usize(r.len());
                entries.0.extend_from_slice(&b[r]);
                n += 1;
            }
            let mut one = W::default();
            match put_blueprint(&mut one, bp) {
                Ok(()) => {
                    entries.str(k);
                    entries.usize(one.0.len());
                    entries.0.extend_from_slice(&one.0);
                    n += 1;
                }
                Err(why) => report(&format!("disk: {k} not written -- {why}")),
            }
        }
        for (rk, b, r) in raw {
            entries.str(&rk);
            entries.usize(r.len());
            entries.0.extend_from_slice(&b[r]);
            n += 1;
        }
        body.usize(n);
        body.0.extend_from_slice(&entries.0);
        body.usize(refused.len());
        for (k, why) in &refused {
            body.str(k);
            body.str(why);
        }
        let mut chains: Vec<&(String, ChainTo, ChainTo)> = chains.iter().collect();
        chains.sort_by(|a, b| a.0.cmp(&b.0));
        body.usize(chains.len());
        for (k, p, c) in chains {
            body.str(k);
            put_chain(&mut body, p);
            put_chain(&mut body, c);
        }
        let Some(id) = build_id() else { return };
        let mut file = W::default();
        file.0.extend_from_slice(MAGIC);
        file.u32(id.len() as u32);
        file.0.extend_from_slice(id);
        file.u64(fp);
        file.u64(env_fingerprint());
        file.u64(body.0.len() as u64);
        file.u64(checksum(&body.0));
        file.0.extend_from_slice(&body.0);
        let Some(dir) = path.parent() else { return };
        if private_dir(dir).is_err() {
            return;
        }
        // written whole, then renamed: a reader sees the old file or the new one, never half of one
        let tmp = dir.join(format!(".{}.{}.tmp", std::process::id(), fp));
        if write_private(&tmp, &file.0).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
            let us = t0.map(|t| t.elapsed().as_micros()).unwrap_or(0);
            report(&format!("disk: wrote {n} blueprints ({} bytes) in {us}us to {}", file.0.len(), path.display()));
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        prune(dir);
    }

    /// Keep the newest [`KEEP_BUILDS`] build directories beside `dir`.
    fn prune(dir: &std::path::Path) {
        let Some(root) = dir.parent() else { return };
        let Ok(rd) = std::fs::read_dir(root) else { return };
        let mut builds: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        builds.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, old) in builds.into_iter().skip(KEEP_BUILDS) {
            if old.as_path() != dir {
                let _ = std::fs::remove_dir_all(old);
            }
        }
    }

    // ── The template cache and the function table in a snapshot ─────────────────────────

    /// Blueprint function `fb`, appended to `out` as the blueprint file writes it ([`super::note_fn`]).
    pub(super) fn fn_bytes(out: &mut Vec<u8>, fb: &FnBp) -> Result<(), String> {
        let mut w = W(std::mem::take(out));
        let r = put_fn(&mut w, fb);
        *out = w.0;
        r
    }

    #[cfg(not(feature = "analysis"))]
    pub(super) use snap::{fn_at, snap_read, snap_tpl, snap_write};

    /// The snapshot's template section (an analysis build makes no snapshot: its records are heap Externals).
    #[cfg(not(feature = "analysis"))]
    mod snap {
        use super::*;

        /// A snapshot's template section's tag, and its format's version.
        const SNAP_MAGIC: &[u8; 8] = b"KVSNTPL\x02";

        type Store = deno_core::snapshot::SnapshotStoreDataStore;
        type Load = deno_core::snapshot::SnapshotLoadDataStore;
        /// The blueprint function [`fn_bytes`] wrote at `at` ([`super::resolve`]).
        pub(crate) fn fn_at(b: &[u8], at: u32) -> Option<FnBp> {
            get_fn(&mut R { b, i: at as usize, trusted: false })
        }

        fn put_opt_str(w: &mut W, s: Option<&str>) {
            match s {
                None => w.u8(0),
                Some(s) => {
                    w.u8(1);
                    w.str(s)
                }
            }
        }

        fn get_opt_str(r: &mut R) -> Option<Option<String>> {
            match r.u8()? {
                0 => Some(None),
                1 => Some(Some(r.str()?)),
                _ => None,
            }
        }

        /// A template's entry: its V8 data registered in the snapshot's store, by id.
        fn put_rt_entry(w: &mut W, s: &mut Store, e: &Entry) {
            match e {
                Entry::Value(g, a) => {
                    w.u8(0);
                    w.u32(s.register(g.clone()));
                    put_attrs(w, *a);
                }
                Entry::Func(t, a) => {
                    w.u8(1);
                    w.u32(s.register(t.clone()));
                    put_attrs(w, *a);
                }
                Entry::Intrinsic(i, a) => {
                    w.u8(2);
                    w.u8(*i as u8);
                    put_attrs(w, *a);
                }
                Entry::Accessor(g, st, a) => {
                    w.u8(3);
                    for t in [g, st] {
                        match t {
                            None => w.u8(0),
                            Some(t) => {
                                w.u8(1);
                                w.u32(s.register(t.clone()));
                            }
                        }
                    }
                    put_attrs(w, *a);
                }
                Entry::Namespace(rows, a) => {
                    w.u8(4);
                    put_rt_list(w, s, rows);
                    put_attrs(w, *a);
                }
                Entry::Unscopables(names, a) => {
                    w.u8(5);
                    w.usize(names.len());
                    for n in names {
                        w.str(n);
                    }
                    put_attrs(w, *a);
                }
            }
        }

        fn put_rt_list(w: &mut W, s: &mut Store, rows: &[(v8::Global<v8::Name>, Entry)]) {
            w.usize(rows.len());
            for (k, e) in rows {
                w.u32(s.register(k.clone()));
                put_rt_entry(w, s, e);
            }
        }

        fn get_rt_entry(scope: &mut v8::PinScope, r: &mut R, s: &mut Load) -> Option<Entry> {
            Some(match r.u8()? {
                0 => {
                    let g = s.get::<v8::Value>(scope, r.u32()?);
                    Entry::Value(g, get_attrs(r)?)
                }
                1 => {
                    let t = s.get::<v8::FunctionTemplate>(scope, r.u32()?);
                    Entry::Func(t, get_attrs(r)?)
                }
                2 => {
                    let i = Intr::ALL.get(r.u8()? as usize)?.0;
                    Entry::Intrinsic(i, get_attrs(r)?)
                }
                3 => {
                    let mut two = [None, None];
                    for t in &mut two {
                        *t = match r.u8()? {
                            0 => None,
                            1 => Some(s.get::<v8::FunctionTemplate>(scope, r.u32()?)),
                            _ => return None,
                        };
                    }
                    let [g, st] = two;
                    Entry::Accessor(g, st, get_attrs(r)?)
                }
                4 => {
                    let rows = get_rt_list(scope, r, s)?;
                    Entry::Namespace(rows, get_attrs(r)?)
                }
                5 => {
                    let n = r.usize()?;
                    let mut names = Vec::with_capacity(n.min(64));
                    for _ in 0..n {
                        names.push(r.str()?);
                    }
                    Entry::Unscopables(names, get_attrs(r)?)
                }
                _ => return None,
            })
        }

        fn get_rt_list(scope: &mut v8::PinScope, r: &mut R, s: &mut Load) -> Option<Vec<(v8::Global<v8::Name>, Entry)>> {
            let n = r.usize()?;
            let mut out = Vec::with_capacity(n.min(1 << 12));
            for _ in 0..n {
                let k = s.get::<v8::Name>(scope, r.u32()?);
                out.push((k, get_rt_entry(scope, r, s)?));
            }
            Some(out)
        }

        /// One template record of a snapshot's section, which [`snap_read`] indexed: its `Tpl`, made with `s`.
        pub(crate) fn snap_tpl(scope: &mut v8::PinScope, b: &[u8], s: &mut Load) -> Option<Rc<Tpl>> {
            let mut r = R { b, i: 0, trusted: false };
            let t = get_tpl_body(scope, &mut r, s)?;
            r.done().then_some(t)
        }

        fn get_tpl_body(scope: &mut v8::PinScope, r: &mut R, s: &mut Load) -> Option<Rc<Tpl>> {
            let ctor = s.get::<v8::FunctionTemplate>(scope, r.u32()?);
            let proto = get_rt_list(scope, r, s)?;
            let statics = get_rt_list(scope, r, s)?;
            let global = get_rt_list(scope, r, s)?;
            let replay = get_replay(r)?;
            let constructible = r.bool()?;
            let iface = r.str()?;
            let parent = get_opt_str(r)?;
            let n = r.usize()?;
            let mut global_index = HashMap::with_capacity(n.min(1 << 12));
            for _ in 0..n {
                let k = r.str()?;
                global_index.insert(k, r.usize()?);
            }
            Some(Rc::new(Tpl { ctor, late: Late { proto, statics }, replay, constructible, global, iface, parent, global_index }))
        }

        type CacheParts = (
            Vec<(String, Rc<Tpl>)>,
            Vec<(String, Rc<Tpl>)>,
            Vec<(StandaloneKey, v8::Global<v8::FunctionTemplate>)>,
            Vec<(&'static str, v8::Global<v8::ObjectTemplate>)>,
            Option<String>,
            Option<bool>,
            Vec<(String, String)>,
        );

        /// [`super::snapshot_templates`]: the cache's templates -- every interface's, the standalone functions', the
        /// named objects' -- with what a realm made from them reads besides, then the function table's origins.
        pub(crate) fn snap_write(scope: &mut v8::PinScope, s: &mut Store, out: &mut Vec<u8>) -> Result<(), String> {
            let (tpls, by_name, standalone, objects, rid, delta_mode, keys_by_name): CacheParts = with_cache(scope, |c| {
                if !c.snaps.is_empty() {
                    return Err("the page realm's interface records are still held (persist has not run)".to_string());
                }
                if !c.known.is_empty() || !c.untemplated.is_empty() {
                    return Err("a KORVUS_TPL_REPORT run's diagnostics are held".to_string());
                }
                let sorted = |m: &FxMap<String, Rc<Tpl>>| {
                    let mut v: Vec<(String, Rc<Tpl>)> = m.iter().map(|(k, t)| (k.clone(), t.clone())).collect();
                    v.sort_by(|a, b| a.0.cmp(&b.0));
                    v
                };
                let mut st: Vec<_> = c.standalone.iter().map(|(k, t)| (k.clone(), t.clone())).collect();
                st.sort_by(|a, b| a.0.cmp(&b.0));
                let mut ob: Vec<_> = c.objects.iter().map(|(k, t)| (*k, t.clone())).collect();
                ob.sort_by(|a, b| a.0.cmp(b.0));
                let mut kb: Vec<_> = c.keys_by_name.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
                kb.sort();
                Ok((sorted(&c.tpls), sorted(&c.by_name), st, ob, c.rid.clone(), c.delta_mode, kb))
            })?;
            let mut w = W::default();
            w.0.extend_from_slice(SNAP_MAGIC);
            let ix: HashMap<*const Tpl, u32> = tpls.iter().enumerate().map(|(i, (_, t))| (Rc::as_ptr(t), i as u32)).collect();
            w.usize(tpls.len());
            for (k, t) in &tpls {
                w.str(k);
                // the record length-prefixed: a restore indexes it and decodes it on first lookup
                let mut one = W::default();
                one.u32(s.register(t.ctor.clone()));
                put_rt_list(&mut one, s, &t.late.proto);
                put_rt_list(&mut one, s, &t.late.statics);
                put_rt_list(&mut one, s, &t.global);
                put_replay(&mut one, &t.replay);
                one.bool(t.constructible);
                one.str(&t.iface);
                put_opt_str(&mut one, t.parent.as_deref());
                let mut gi: Vec<(&String, &usize)> = t.global_index.iter().collect();
                gi.sort();
                one.usize(gi.len());
                for (k, i) in gi {
                    one.str(k);
                    one.usize(*i);
                }
                w.usize(one.0.len());
                w.0.extend_from_slice(&one.0);
            }
            w.usize(by_name.len());
            for (n, t) in &by_name {
                w.str(n);
                w.u32(*ix.get(&Rc::as_ptr(t)).ok_or_else(|| format!("{n}: a template by name that is under no key"))?);
            }
            w.usize(standalone.len());
            for ((cb, name, len, data, engine, ctor), t) in &standalone {
                if !in_image(*cb) {
                    return Err(format!("{name}: a standalone function whose callback is outside the binary"));
                }
                w.i64(*cb as i64 - anchor() as i64);
                w.str(name);
                w.i32(*len);
                put_opt_str(&mut w, data.as_deref());
                w.bool(*engine);
                w.bool(*ctor);
                w.u32(s.register(t.clone()));
            }
            w.usize(objects.len());
            for (n, t) in &objects {
                w.str(n);
                w.u32(s.register(t.clone()));
            }
            put_opt_str(&mut w, rid.as_deref());
            w.u8(match delta_mode {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            });
            w.usize(keys_by_name.len());
            for (a, b) in &keys_by_name {
                w.str(a);
                w.str(b);
            }
            let t = fn_table(scope);
            // SAFETY: the isolate's table; nothing holds a reference into it across this call
            let t = unsafe { &*t };
            if let Some(why) = &t.refusal {
                return Err(why.clone());
            }
            if t.origins.len() != t.recs.len() {
                return Err(format!("{} of {} function records were made before recording began", t.recs.len().saturating_sub(t.origins.len()), t.recs.len()));
            }
            w.usize(t.keys.len());
            for k in &t.keys {
                w.str(k);
            }
            w.usize(t.origins.len());
            for (i, o) in t.origins.iter().enumerate() {
                let o = o.ok_or_else(|| format!("function record {i} has no origin (a path that keeps none made it)"))?;
                w.u32(o.key);
                w.u32(o.at);
                w.u8(o.part);
                w.bool(o.iref);
                match o.brand {
                    None => w.u8(0),
                    Some(b) => {
                        w.u8(1);
                        w.u32(b);
                    }
                }
                w.u32(o.body);
            }
            w.usize(t.fn_bytes.len());
            w.0.extend_from_slice(&t.fn_bytes);
            out.extend_from_slice(&w.0);
            Ok(())
        }

        /// [`super::restore_templates`].
        pub(crate) fn snap_read(scope: &mut v8::PinScope, b: &[u8], at: &mut usize, s: &mut Load) -> Result<(), String> {
            let fresh = with_cache(scope, |c| c.tpls.is_empty() && c.standalone.is_empty() && c.objects.is_empty());
            let t = fn_table(scope);
            // SAFETY: the isolate's table; nothing holds a reference into it across this call
            if !fresh || !unsafe { &*t }.recs.is_empty() {
                return Err("templates were built before the snapshot's were restored".to_string());
            }
            let bad = || "the snapshot's template section does not decode".to_string();
            let mut r = R { b, i: *at, trusted: false };
            if r.take(8).ok_or_else(bad)? != SNAP_MAGIC {
                return Err(bad());
            }
            // Indexed, not decoded -- each template on its first lookup ([`cached`], [`by_name_tpl`])
            let n = r.usize().ok_or_else(bad)?;
            let base = r.i;
            let mut keys: Vec<String> = Vec::with_capacity(n.min(1 << 14));
            let mut pending: FxMap<String, std::ops::Range<usize>> = FxMap::default();
            for _ in 0..n {
                let key = r.str().ok_or_else(bad)?;
                let len = r.usize().ok_or_else(bad)?;
                let start = r.i - base;
                r.take(len).ok_or_else(bad)?;
                pending.insert(key.clone(), start..start + len);
                keys.push(key);
            }
            let snap_bytes: Rc<[u8]> = b[base..r.i].into();
            let n = r.usize().ok_or_else(bad)?;
            let mut pending_names: FxMap<String, String> = FxMap::default();
            for _ in 0..n {
                let name = r.str().ok_or_else(bad)?;
                let key = keys.get(r.u32().ok_or_else(bad)? as usize).ok_or_else(bad)?.clone();
                pending_names.insert(name, key);
            }
            let n = r.usize().ok_or_else(bad)?;
            let mut standalone = HashMap::with_capacity(n.min(1 << 12));
            for _ in 0..n {
                let cb = (anchor() as i64 + r.i64().ok_or_else(bad)?) as usize;
                let name = r.str().ok_or_else(bad)?;
                let len = r.i32().ok_or_else(bad)?;
                let data = get_opt_str(&mut r).ok_or_else(bad)?;
                let (engine, ctor) = (r.bool().ok_or_else(bad)?, r.bool().ok_or_else(bad)?);
                let t = s.get::<v8::FunctionTemplate>(scope, r.u32().ok_or_else(bad)?);
                standalone.insert((cb, name, len, data, engine, ctor), t);
            }
            let n = r.usize().ok_or_else(bad)?;
            let mut objects = HashMap::with_capacity(n.min(64));
            for _ in 0..n {
                let name = intern(r.str().ok_or_else(bad)?);
                objects.insert(name, s.get::<v8::ObjectTemplate>(scope, r.u32().ok_or_else(bad)?));
            }
            let rid = get_opt_str(&mut r).ok_or_else(bad)?;
            let delta_mode = match r.u8().ok_or_else(bad)? {
                0 => None,
                1 => Some(false),
                2 => Some(true),
                _ => return Err(bad()),
            };
            let n = r.usize().ok_or_else(bad)?;
            let mut keys_by_name = FxMap::default();
            for _ in 0..n {
                let k = r.str().ok_or_else(bad)?;
                keys_by_name.insert(k, r.str().ok_or_else(bad)?);
            }
            let n = r.usize().ok_or_else(bad)?;
            let mut keys = Vec::with_capacity(n.min(1 << 14));
            for _ in 0..n {
                keys.push(r.str().ok_or_else(bad)?);
            }
            let n = r.usize().ok_or_else(bad)?;
            let mut origins = Vec::with_capacity(n.min(1 << 20));
            for _ in 0..n {
                let (key, at, part, iref) = (r.u32().ok_or_else(bad)?, r.u32().ok_or_else(bad)?, r.u8().ok_or_else(bad)?, r.bool().ok_or_else(bad)?);
                let brand = match r.u8().ok_or_else(bad)? {
                    0 => None,
                    1 => Some(r.u32().ok_or_else(bad)?),
                    _ => return Err(bad()),
                };
                let body = r.u32().ok_or_else(bad)?;
                if key as usize >= keys.len() || part > 1 {
                    return Err(bad());
                }
                origins.push(Some(Origin { key, at, part, iref, brand, body }));
            }
            let nb = r.usize().ok_or_else(bad)?;
            let fn_bytes = r.take(nb).ok_or_else(bad)?.to_vec();
            if origins.iter().flatten().any(|o| o.at as usize >= fn_bytes.len()) {
                return Err(bad());
            }
            with_cache(scope, |c| {
                c.pending = pending;
                c.pending_names = pending_names;
                c.snap_bytes = Some(snap_bytes);
                c.standalone = standalone;
                c.objects = objects;
                c.rid = rid;
                c.delta_mode = delta_mode;
                c.keys_by_name = keys_by_name;
                c.persisted = true;
            });
            // SAFETY: as above
            let t = unsafe { &mut *t };
            t.recs = vec![encode(FnSlot::Pending); origins.len()];
            t.origins = origins;
            t.key_ix = keys.iter().enumerate().map(|(i, k)| (k.clone(), i as u32)).collect();
            t.keys = keys;
            t.fn_bytes = fn_bytes;
            *at = r.i;
            Ok(())
        }
    }
}
