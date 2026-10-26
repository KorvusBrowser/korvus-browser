//! `core/html/window_globals.rs` — the window's SELF-ALIASES and its three host singletons.
//!
//! WHAT THIS REPLACES: the top of `core/korvus-runtime/src/js/host.js` — `globalThis.navigator = new
//! JsNavigator()`, `screen`, `location`, and `window`/`self`/`top`/`parent`/`frames`. Those lines are
//! the ones W6.2's finding said could never move, because they name `deno_core` `objects =` classes
//! and those were "reachable only from ESM".
//!
//! # The finding is wrong about the classes, and this file is the proof
//!
//! `deno_core` realises every `objects` entry onto `Deno.core.ops`; `ext:core/ops` re-exports that
//! same object. So [`crate::binding::hidden::native_class`] reaches the IDENTICAL constructor the ESM
//! import binds, and a Rust `new_instance` on it produces the same cppgc-backed object
//! `new JsNavigator()` did. `install::live::fetch_values::ensure_class` has read `Headers`/`FormData`
//! this way since W5.1 L20.
//!
//! # The three interface OBJECTS that were still in `host.js`, and where they went
//!
//! `globalThis.Screen` / `TextEncoder` / `TextDecoder` (and the `.name` renames and the
//! `JsScreen.prototype -> EventTarget.prototype` splice) stayed behind when this file was written,
//! and the reason was SEQUENCING, not reachability — a different mechanism from the one W6.2 named:
//!
//! `installer::install_registry` SKIPS any interface whose name is already on the global
//! (`if global.get(name).is_some() { continue }`), and `installer::backfill_members` only runs on an
//! interface it FINDS there. `Screen` and `Location` are `backfill: true` registry rows and
//! `TextEncoder`/`TextDecoder` are plain shell rows, so the natively-realised class has to be on the
//! global BEFORE the registry install, or the registry mints a shell and the class never appears.
//!
//! THAT CEILING WAS ALREADY GONE WHEN IT WAS WRITTEN DOWN, by one commit in a sibling lane. The
//! install was "scheduled FROM JS" because `core/korvus-webapi/src/js/generated_shells.js` called
//! `op_install_registry()` during the ESM layer — and W5.2 deleted that whole directory. The
//! registry install is `installer::install_window_registry`, an ordinary Rust call from the middle of
//! `install::live::bootstrap::install_window_bootstrap`, and there is no `op_install_registry` call
//! site in any `.js` file in the tree. So "no Rust hook runs before the registry install" stopped
//! being true, and anything at the TOP of that function is ahead of it.
//!
//! The three are `bootstrap::install_window_host_classes` now, the FIRST statement of that function
//! — which is also their position in the original order, since `korvus_ext` is the first extension in
//! `engine.rs`'s list and `host.js` is its ESM entry point, so these statements ran before
//! `dom/00-bootstrap.js`'s.
//!
//! # The OTHER half of the experiment is why they are not in this file
//!
//! BOTH ARRANGEMENTS WERE RUN, not reasoned about (2026-09-12):
//!
//!  1. DELETE the three `host.js` assignments and put nothing back. `screen instanceof Screen` goes
//!     FALSE and `new TextEncoder().encoding` answers `""` instead of `"utf-8"` — the registry minted
//!     a shell for each name and the deno_core class is unreachable from the page.
//!  2. MOVE them into this function instead, after the registry install. The class is back
//!     (`instanceof` true, `encoding` "utf-8") and something else breaks:
//!     `Object.prototype.toString.call(screen)` reads `[object EventTarget]` where it must read
//!     `[object Screen]`. `Symbol.toStringTag` is one of the two module-body BACKFILLS the registry
//!     runs right after create, onto whatever `Screen.prototype` it finds; assigning the class later
//!     means the backfill ran on the shell's prototype and `JsScreen.prototype` — spliced onto
//!     `EventTarget.prototype` — inherits EventTarget's tag instead.
//!
//! That is failure mode 4 of a table cutover: an install ORDER change discarding a decorator.
//! Supplying the tag from here too would work and is exactly the wrong shape — a SECOND writer of a
//! prototype the installer already claims, which is the invariant (I2) this goal exists to remove.
//! The fix was upstream, and it is the position above.
//!
//! The lines below have no such dependency: `navigator`, `screen` and `location` are lowercase
//! INSTANCE globals that no registry row names, and `window`/`self`/`top`/`parent`/`frames` name no
//! class at all. Nothing in the ESM layer reads any of the eight (grepped across
//! `core/korvus-webapi/src/js/`).

use deno_core::v8;


/// The two host singletons that are cppgc objects, as `(global name, maker, setter)`. `location` was the
/// third; its object is minted by the runtime's `reshape_location` now, and only its
/// accessor is defined here, at the same position.
///
/// Each is a real cppgc wrapper of its OWN class (`binding::class::mint`; `new` on the
/// kernel's `deno_core` class until then) -- `Object.create(Cls.prototype)` would be an impostor the
/// native getters reject.
const SINGLETONS: [(&str, MintFn, WindowSetter); 2] = [
    ("navigator", mint_navigator, WindowSetter::None),
    ("screen", mint_screen, WindowSetter::Replaceable),
];

fn mint_navigator<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    Some(crate::binding::class::mint(scope, crate::core::navigator::navigator::JsNavigator).into())
}

fn mint_screen<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    Some(crate::binding::class::mint(scope, crate::core::html::screen::JsScreen).into())
}

/// `window` / `self` / `top` / `parent` / `frames` — every one of them the global itself, with the
/// extended attributes Chrome gives each (`[LegacyUnforgeable]` makes `window` and `top`
/// non-configurable; `self`, `parent` and `frames` are `[Replaceable]`).
///
/// In a real top-level browsing context all five alias the same `Window`. `self` is load-bearing far
/// beyond fidelity: bundlers (webpack, UMD) read it at module top, and without it every bundle throws
/// `self is not defined` before the page starts.
const SELF_ALIASES: [(&str, WindowGetter, WindowSetter, bool); 5] = [
    ("window", WindowGetter::Receiver, WindowSetter::None, false),
    ("self", WindowGetter::Receiver, WindowSetter::Replaceable, true),
    ("top", WindowGetter::Global, WindowSetter::None, false),
    ("parent", WindowGetter::Global, WindowSetter::Replaceable, true),
    ("frames", WindowGetter::Receiver, WindowSetter::Replaceable, true),
];

// ── the window's own singleton ACCESSORS ───────────────────────────────────────────
//
// `Object.getOwnPropertyDescriptor(window, "navigator")` is one of the standard automation checks, and
// this engine answered an own DATA property for every singleton the deleted JS had assigned with
// `globalThis.x = …`: `document window self top parent frames location navigator screen performance
// history localStorage sessionStorage customElements cookieStore`. Every one is an own ACCESSOR on
// all four stock browsers of the family, with one shape (measured over a loopback page on Chrome
// 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93 and Opera 135.0.5973.133 --
// `tests/fixtures/p11a/desc.*.txt`, identical for every name the four share):
//
// ```text
// document window top          get="get X"/0, no setter          e=true c=false  [LegacyUnforgeable]
// location                     get, set="set location"/1         e=true c=false  [PutForwards=href]
// self parent frames screen    get, set="set X"/1                e=true c=true   [Replaceable]
//   performance
// navigator history            get only                          e=true c=true
//   customElements localStorage sessionStorage cookieStore
// ```
//
// and a foreign receiver throws `Illegal invocation` from every getter and setter
// (`tests/fixtures/p11a/recv.chrome-150.txt`) -- including `Object.create(Window.prototype)`, which
// has the right prototype and is still not a window. `get.call(undefined)` answers the global's.
//
// The SHAPE is the family's, not a persona's, so no bundle is read here. WHICH names exist is still
// the bundle's, wherever a writer asks it.

/// What a window singleton accessor's getter answers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowGetter {
    /// The object kept in the global's private slot for this name.
    Slot,
    /// The receiver itself -- `window`, `self` and `frames` are "this's relevant WindowProxy".
    Receiver,
    /// The realm's global -- `top` and `parent` of a top-level browsing context.
    Global,
}

/// What a window singleton accessor's setter does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowSetter {
    /// No setter: assignment is ignored in sloppy code and a TypeError in strict code.
    None,
    /// `[Replaceable]`: the setter replaces the accessor with an own `{w, e, c}` data property holding
    /// the value (`self = 1` then `self === 1`).
    Replaceable,
    /// `[PutForwards=href]`: `window.location = url` is `location.href = url`.
    PutForwardsHref,
}

/// `korvus.window.<name>`: the private a window singleton lives under on the realm's global, made once
/// per isolate and kept, by the bare name. Every `window.location`, `navigator`, `performance`, `history`
/// read comes through here ([`cb_window_get`]), and building it each time -- a `format!`, a fresh V8
/// string and `Private::for_api`'s registry lookup, which hashes that string -- is what made those four
/// reads cost MORE than in the pre-migration binary (`bench/percall_instr.py --p4`).
/// In an isolate slot, not `OpState`, for the reason `binding::protos::private_key` gives.
fn slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Private>> {
    let keys = match scope.get_slot::<WindowSlotKeys>() {
        Some(k) => k.clone(),
        None => {
            let k = WindowSlotKeys::default();
            scope.set_slot(k.clone());
            k
        }
    };
    if let Some(g) = keys.0.borrow().get(name) {
        return Some(v8::Local::new(scope, g));
    }
    let n = v8::String::new(scope, &format!("korvus.window.{name}"))?;
    let key = v8::Private::for_api(scope, Some(n));
    keys.0.borrow_mut().insert(name.to_string(), v8::Global::new(scope, key));
    Some(key)
}

/// The window getters V8's inline caches answer from their slot, as Blink's `[CachedAccessor]`s (FunctionTemplate::
/// NewWithCache, `shim/cached_accessor.cc`). An IC cannot keep a handler for an API getter on the global, whose
/// properties are a dictionary, so every unqualified `document` went through the runtime (~180-300 ns; Chrome ~5).
/// A getter here answers its slot whenever the global has it ([`window_singleton`]), and an IC that cached the slot
/// keeps reading it after the accessor is replaced -- which these two, `[LegacyUnforgeable]`, never are. Replaced
/// after warm reads, Chrome 152's configurable window getters answer the new value, all but `self` and `frames`
/// (measured over all 60, scratchpad cachedacc.html, 2026-10-23); those two and `window` answer their receiver, and
/// caching them made an isolated world's `window` the page's getter (`korvus-cdp` isolated_worlds), so they are not.
const CACHED_ACCESSORS: [&str; 2] = ["document", "location"];

/// [`slot`]'s cache: a window singleton's name -> its private.
#[derive(Default, Clone)]
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct WindowSlotKeys(std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, v8::Global<v8::Private>>>>);

/// THE receiver test for a member of a realm's global, without throwing: `this` is the realm's global
/// (which is what V8 hands a native function called with `undefined`/`null`), or a nested/auxiliary
/// browsing context's window.
///
/// One test, because a frame's window is not the realm's global in this engine (it is an object the
/// frame's scripts reach through `with`, `core/html/browsing_context.rs`), so every body that compared
/// `this` with the global by hand rejected a frame's own call. Measured on Chrome
/// 150.0.7871.46 over loopback, from inside an `<iframe srcdoc>` and an `<iframe src>` alike:
///
/// ```text
/// fetch(u) / atob("YQ==") / createImageBitmap(img)   korvus  Illegal invocation   chrome  answers
/// ```
///
/// A realm with no browsing contexts (a worker) has only its global, so the same test is right there.
/// Bodies that throw use [`window_receiver`]; a promise-returning body (`fetch`, `createImageBitmap`)
/// rejects with its own sentence instead.
pub(crate) fn is_global_receiver(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    let global = scope.get_current_context().global(scope);
    // The PAGE's window too: a frame realm's own `atob` can be called on its parent
    // (`frame.contentWindow.atob.call(window, s)`), and Chrome 150 answers it (measured, headless).
    let page = crate::core::html::browsing_context::main_global(scope);
    this.strict_equals(global.into())
        || this.strict_equals(page.into())
        || v8::Local::<v8::Object>::try_from(this)
            .ok()
            .map(|o| crate::core::html::browsing_context::ctx_of_window(scope, o).is_some())
            .unwrap_or(false)
}

/// Is `this` a window a `Window` member may run on: the realm's global, or a nested/auxiliary
/// browsing context's window (`core/html/browsing_context.rs` marks every one it mints). Anything
/// else -- a plain object, `Window.prototype`, `Object.create(Window.prototype)` -- throws Chrome's
/// `Illegal invocation`. `undefined`/`null` never reach here: V8 hands a native function called
/// with either the global proxy.
pub(crate) fn window_receiver(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    let ok = is_global_receiver(scope, this);
    if !ok {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// The object this realm keeps for window singleton `name` (the value its getter answers).
pub(crate) fn window_slot_get<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let global = scope.get_current_context().global(scope);
    let k = slot(scope, name)?;
    global.get_private(scope, k).filter(|v| !v.is_undefined())
}

/// Replace what window singleton `name`'s getter answers -- `document` on every navigation. The
/// accessor itself is defined once, by [`define_window_accessor`].
pub(crate) fn window_slot_set(scope: &mut v8::PinScope, name: &str, value: v8::Local<v8::Value>) {
    let global = scope.get_current_context().global(scope);
    if let Some(k) = slot(scope, name) {
        global.set_private(scope, k, value);
    }
}

fn cb_window_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if !window_receiver(scope, this) {
        return;
    }
    // The template's data, `<kind>:<name>`, read into a stack buffer: this runs on every read of a window
    // singleton, and a heap `String` per read showed in the profile.
    let Ok(data) = v8::Local::<v8::String>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let packed = data.to_rust_cow_lossy(scope, &mut buf);
    let (kind, name) = packed.split_once(':').unwrap_or(("s", &packed));
    match kind {
        "r" => rv.set(this),
        "g" => {
            // A FRAME's `top` and `parent` are its ancestors' windows, not itself (in a
            // frame realm this getter is the frame's own).
            if let Some(ctx) = v8::Local::<v8::Object>::try_from(this).ok().and_then(|o| crate::core::html::browsing_context::ctx_of_window(scope, o)) {
                if let Some(w) = crate::core::html::browsing_context::frame_top_or_parent(scope, ctx, name == "parent") {
                    rv.set(w.into());
                    return;
                }
            }
            // This engine runs a remote frame's document -- its `parent` and `top` are the
            // embedder's window, across the thread boundary.
            if let Some(p) = crate::core::html::remote_frame::parent_proxy(scope) {
                rv.set(p.into());
                return;
            }
            let g = scope.get_current_context().global(scope);
            rv.set(g.into());
        }
        _ => {
            if let Some(v) = window_singleton(scope, name) {
                rv.set(v);
            }
        }
    }
}

pub(crate) fn cb_window_replace(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if !window_receiver(scope, this) {
        return;
    }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Ok(target) = v8::Local::<v8::Object>::try_from(this) else { return };
    let Some(k) = v8::String::new(scope, &name) else { return };
    let mut d = v8::PropertyDescriptor::new_from_value_writable(args.get(0), true);
    d.set_enumerable(true);
    d.set_configurable(true);
    target.define_property(scope, k.into(), &d);
}

fn cb_window_put_forwards_href(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if !window_receiver(scope, this) {
        return;
    }
    let Some(loc) = window_slot_get(scope, "location").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(k) = v8::String::new(scope, "href") else { return };
    // `Set(location, "href", V)` -- the Location `href` setter, with its own conversion and errors.
    loc.set(scope, k.into(), args.get(0));
}

/// Define window singleton `name` as the own accessor Chrome has, at the key's CURRENT position (a
/// define on an existing key keeps its place in the own-key order, and a new key appends where a
/// `set` would have).
pub(crate) fn define_window_accessor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    name: &str,
    getter: WindowGetter,
    setter: WindowSetter,
    configurable: bool,
) {
    let tag = match getter {
        WindowGetter::Slot => "s",
        WindowGetter::Receiver => "r",
        WindowGetter::Global => "g",
    };
    // Each from its template: a `[LegacyUnforgeable]` accessor ends non-configurable, so
    // the page's cannot be switched to the template's afterwards as the rest of its global is.
    let data = format!("{tag}:{name}");
    // One of Blink's cached accessors ([`CACHED_ACCESSORS`]): an IC may read its slot in place of calling it.
    let cache = if CACHED_ACCESSORS.contains(&name) && getter == WindowGetter::Slot { slot(scope, name) } else { None };
    let get = crate::install::templates::standalone_fn_cached(scope, v8::MapFnTo::<v8::FunctionCallback>::map_fn_to(cb_window_get), &format!("get {name}"), 0, Some(&data), true, false, cache);
    let set: v8::Local<v8::Value> = match setter {
        WindowSetter::None => v8::undefined(scope).into(),
        WindowSetter::Replaceable => crate::binding::protos::template_fn(scope, cb_window_replace, &format!("set {name}"), 1, Some(name)).into(),
        WindowSetter::PutForwardsHref => crate::binding::protos::template_fn(scope, cb_window_put_forwards_href, &format!("set {name}"), 1, None).into(),
    };
    let Some(k) = v8::String::new(scope, name) else { return };
    let mut pd = v8::PropertyDescriptor::new_from_get_set(get.into(), set);
    pd.set_enumerable(true);
    // A `[LegacyUnforgeable]` accessor ends NON-configurable, but a non-configurable global cannot be
    // moved to its capture position, so it is defined configurable and the registry walk's seat makes
    // it non-configurable where it places it (`installer::seat_globals_in_capture_order`).
    // No page script runs in between.
    pd.set_configurable(true);
    global.define_property(scope, k.into(), &pd);
    if !configurable {
        crate::install::installer::lock_at_seat(scope, global, name);
    }
}

/// Define window global `name` as an own accessor whose GETTER IS SUPPLIED — the two names in the
/// family whose value is not an object in a slot: `offscreenBuffering`, whose getter
/// answers the constant `true`, and `fence`, whose getter answers `null` outside a fenced frame.
///
/// `enumerable` is a parameter here and hard-coded `true` in [`define_window_accessor`] because
/// `offscreenBuffering` is the ONE window accessor in either browser measured with `enumerable:
/// false` (Brave 151 and Chrome 150, 2026-09-19). The property is always defined configurable and is
/// never locked: neither name is `[LegacyUnforgeable]` on the device.
pub(crate) fn define_window_custom_accessor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    name: &str,
    get: v8::Local<'s, v8::Function>,
    setter: WindowSetter,
    enumerable: bool,
) {
    let set: v8::Local<v8::Value> = match setter {
        WindowSetter::None => v8::undefined(scope).into(),
        WindowSetter::Replaceable => {
            let n = crate::binding::protos::str_val(scope, name);
            crate::binding::protos::build_fn_data(scope, cb_window_replace, &format!("set {name}"), 1, n).into()
        }
        WindowSetter::PutForwardsHref => {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            crate::binding::protos::build_fn_data(scope, cb_window_put_forwards_href, &format!("set {name}"), 1, u).into()
        }
    };
    let Some(k) = v8::String::new(scope, name) else { return };
    let mut pd = v8::PropertyDescriptor::new_from_get_set(get.into(), set);
    pd.set_enumerable(enumerable);
    pd.set_configurable(true);
    global.define_property(scope, k.into(), &pd);
}

/// Put `value` in window singleton `name`'s slot and define its getter-backed accessor -- the whole
/// install of one slot-backed singleton. `mint` is how a realm built from templates makes the same
/// object on its first read ([`window_singleton`]).
pub(crate) fn define_window_singleton<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    name: &str,
    value: v8::Local<'s, v8::Value>,
    setter: WindowSetter,
    configurable: bool,
    mint: Mint,
) {
    window_slot_set(scope, name, value);
    if let Ok(mut m) = mints().lock() {
        m.entry(name.to_string()).or_insert((mint, None)).0 = mint;
    }
    define_window_accessor(scope, global, name, WindowGetter::Slot, setter, configurable);
}

// ── a window singleton in a realm built from templates ─────────────────────────────

/// How a window singleton is MADE in a realm built from templates. Such a realm runs no install, so it
/// makes each singleton on the first read of its accessor and keeps it in the same slot -- the way
/// Blink's `LocalDOMWindow::navigator()` makes its `Navigator` -- and every install that defines one
/// says here how its object is made.
#[derive(Clone, Copy)]
pub(crate) enum Mint {
    /// An ordinary object, on the prototype the page's own singleton ended its install on.
    Plain,
    /// An instance of this interface, branded as the shell members expect ([`mint_shell`]).
    Shell(&'static str),
    /// The object another singleton holds (`clientInformation` IS `navigator`).
    Alias(&'static str),
    /// Made by this function, in the calling realm.
    With(MintFn),
}

/// A window singleton's maker ([`Mint::With`]), run in the realm that reads it.
pub(crate) type MintFn = for<'s, 'i> fn(&mut v8::PinScope<'s, 'i>) -> Option<v8::Local<'s, v8::Value>>;

/// name -> (how it is made, the interface its prototype is at the end of the page's install).
fn mints() -> &'static std::sync::Mutex<std::collections::HashMap<String, (Mint, Option<String>)>> {
    static M: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (Mint, Option<String>)>>> =
        std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

/// Say how a realm built from templates makes the object in window slot `name` (for a slot whose accessor
/// is not [`define_window_singleton`]'s).
pub(crate) fn register_mint(name: &str, mint: Mint) {
    if let Ok(mut m) = mints().lock() {
        m.entry(name.to_string()).or_insert((mint, None)).0 = mint;
    }
}

/// Whether window singleton `name` has a maker a realm built from templates can run.
pub(crate) fn has_mint(name: &str) -> bool {
    mints().lock().is_ok_and(|m| m.contains_key(name))
}

/// An instance of interface `iface` of the calling realm, branded as its shell members expect.
pub(crate) fn mint_shell<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    Some(o)
}

/// Record, once the page's install is over, the interface each singleton's prototype ended on: an
/// install re-points some after minting them (`navigator` is a `JsNavigator` until the `Navigator`
/// table is built), or replaces them (`caches` holds a placeholder until `CacheStorage` is), and a realm built from templates
/// makes the finished object.
pub(crate) fn record_singleton_protos(scope: &mut v8::PinScope) {
    let names: Vec<String> = match mints().lock() {
        Ok(m) => m.keys().cloned().collect(),
        Err(_) => return,
    };
    for name in names {
        let Some(o) = window_slot_get(scope, &name).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { continue };
        let iface = o
            .get_prototype(scope)
            .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())
            .and_then(|p| crate::binding::protos::iface_name_of(scope, p))
            .map(|n| n.to_rust_string_lossy(scope));
        if let Ok(mut m) = mints().lock() {
            if let Some(e) = m.get_mut(&name) {
                e.1 = iface;
            }
        }
    }
}

/// Window singleton `name` of the calling realm: the object in its slot, or -- in a realm built from
/// templates, on the first read -- the one [`Mint`] makes now, on the realm's own prototype.
pub(crate) fn window_singleton<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    if let Some(v) = window_slot_get(scope, name) {
        return Some(v);
    }
    if !crate::install::templates::is_lazy_realm(scope) && !mints_on_read(scope) {
        return None;
    }
    let (mint, iface) = mints().lock().ok()?.get(name).cloned()?;
    let v = match mint {
        // from the interface's template when the page's install says which
        Mint::Plain => match iface.as_deref().and_then(|i| crate::binding::protos::shell_proto(scope, i)) {
            Some(p) => crate::binding::protos::mint_on(scope, p.into()).into(),
            None => v8::Object::new(scope).into(),
        },
        Mint::Shell(i) => mint_shell(scope, i)?.into(),
        Mint::Alias(other) => window_singleton(scope, other)?,
        Mint::With(f) => f(scope)?,
    };
    if let (Some(iface), Ok(o), false) = (iface, v8::Local::<v8::Object>::try_from(v), matches!(mint, Mint::Alias(_))) {
        if let Some(p) = crate::binding::protos::shell_proto(scope, &iface) {
            // the prototype the page's own singleton ended on, and that interface's brand
            crate::binding::protos::adopt_as(scope, o, p.into());
        }
    }
    window_slot_set(scope, name, v);
    Some(v)
}

/// The page-less realm's `document` placeholder, and the `document` accessor itself.
///
/// The placeholder is NOT dead, which was checked by deleting it: `native_binding::host_surface`
/// builds an `Engine` and runs the loop WITHOUT loading a page, and `typeof document` went from
/// `"object"` to `"undefined"`. A realm with no navigation has no real `Document` —
/// `Engine::install_document` creates that and puts it in the slot over this one — so between context
/// creation and the first navigation this is the only `document` there is, and a CDP client
/// evaluating in that window sees it.
// ── The context snapshot ─────────────────────────────────────────────────────────────────────

const MINT_ON_READ: &str = "korvus.mintSingletonsOnRead";

fn mint_on_read_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, MINT_ON_READ).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// Whether this realm's global mints a window singleton on its first read ([`unseat_minted`]).
fn mints_on_read(scope: &mut v8::PinScope) -> bool {
    let global = scope.get_current_context().global(scope);
    let k = mint_on_read_key(scope);
    global.get_private(scope, k).is_some_and(|v| v.is_true())
}

/// The snapshot creator's step: every singleton minted from Rust state ([`Mint::With`]: a cppgc object, per-run
/// values, a persona's answers), and every alias of one, leaves its slot, and the realm mints each on its first read -- in the process that
/// reads it, after a load -- as a realm built from templates does. The mark that says so is a private on the
/// global, so it travels in the snapshot. Returns how many left.
#[cfg(not(feature = "analysis"))]
pub(crate) fn unseat_minted(scope: &mut v8::PinScope) -> usize {
    let names: Vec<String> = match mints().lock() {
        // an ALIAS too: its slot holds its target's object (`clientInformation` is the cppgc navigator), and it
        // re-resolves to the target's fresh one on its first read
        Ok(m) => m.iter().filter(|(_, (mint, _))| matches!(mint, Mint::With(_) | Mint::Alias(_))).map(|(n, _)| n.clone()).collect(),
        Err(_) => return 0,
    };
    // deleted, not set to `undefined`: a cached getter's IC answers a slot the global HAS ([`CACHED_ACCESSORS`])
    let global = scope.get_current_context().global(scope);
    for n in &names {
        if let Some(k) = slot(scope, n) {
            global.delete_private(scope, k);
        }
    }
    let k = mint_on_read_key(scope);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    global.set_private(scope, k, t);
    names.len()
}

/// A [`Mint`] as a snapshot writes it: a minting function by its offset from the binary's anchor.
#[cfg(not(feature = "analysis"))]
pub(crate) enum MintRow {
    Plain,
    Shell(String),
    Alias(String),
    With(i64),
}

/// The process's mint registry (what [`window_singleton`] mints from), for a snapshot: a process made from it has
/// none of its own, since only an install fills it.
#[cfg(not(feature = "analysis"))]
pub(crate) fn mints_snapshot() -> Result<Vec<(String, MintRow, Option<String>)>, String> {
    let m = mints().lock().map_err(|_| "the mint registry is poisoned".to_string())?;
    let mut out = Vec::with_capacity(m.len());
    for (name, (mint, iface)) in m.iter() {
        let row = match mint {
            Mint::Plain => MintRow::Plain,
            Mint::Shell(s) => MintRow::Shell(s.to_string()),
            Mint::Alias(s) => MintRow::Alias(s.to_string()),
            Mint::With(f) => {
                let p = *f as usize;
                if !crate::install::templates::in_image(p) {
                    return Err(format!("{name}: a minting function outside the binary"));
                }
                MintRow::With(p as i64 - crate::install::templates::anchor() as i64)
            }
        };
        out.push((name.clone(), row, iface.clone()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// [`mints_snapshot`]'s reader, first half: the rows as mints, checked; nothing is written ([`mints_install`]).
#[cfg(not(feature = "analysis"))]
pub(crate) fn mints_decode(rows: Vec<(String, MintRow, Option<String>)>) -> Result<Vec<(String, Mint, Option<String>)>, String> {
    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }
    let mut out = Vec::with_capacity(rows.len());
    for (name, row, iface) in rows {
        let mint = match row {
            MintRow::Plain => Mint::Plain,
            MintRow::Shell(s) => Mint::Shell(leak(s)),
            MintRow::Alias(s) => Mint::Alias(leak(s)),
            MintRow::With(off) => {
                let p = (crate::install::templates::anchor() as i64 + off) as usize;
                if !crate::install::templates::in_image(p) {
                    return Err(format!("{name}: a minting function outside the binary"));
                }
                // SAFETY: `p` is the address `mints_snapshot` wrote as a `MintFn`'s, rebased on THIS process's anchor --
                // the same binary (the snapshot's key carries its build id), so the same function
                Mint::With(unsafe { std::mem::transmute::<usize, MintFn>(p) })
            }
        };
        out.push((name, mint, iface));
    }
    Ok(out)
}

/// [`mints_decode`]'s rows into the process's registry.
#[cfg(not(feature = "analysis"))]
pub(crate) fn mints_install(rows: Vec<(String, Mint, Option<String>)>) {
    if let Ok(mut m) = mints().lock() {
        for (name, mint, iface) in rows {
            m.insert(name, (mint, iface));
        }
    }
}

fn install_document_placeholder<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    if window_slot_get(scope, "document").is_none() {
        let doc = v8::Object::new(scope);
        let ready = crate::binding::protos::str_val(scope, "complete");
        crate::binding::protos::set_prop(scope, doc, "readyState", ready);
        window_slot_set(scope, "document", doc.into());
    }
    define_window_accessor(scope, global, "document", WindowGetter::Slot, WindowSetter::None, false);
}

/// The browser's own PROPRIETARY NAMESPACE OBJECTS on the window — `opr` and `g_opr` on Opera, and
/// nothing at all on the other three devices.
///
/// THE ENGINE DOES NOT KNOW THE WORD. Which namespaces exist, what they are called, what their
/// members are called and what each member answers all come from
/// [`spec::Persona::window_namespaces`]; this function knows only how to define an own data property
/// holding an object with native methods on it. That is the same split `navigator.brave` has had
/// since L7e and the reason `scripts/freeze.sh`'s `persona_literals` can hold at 0 with `opr` in its
/// needle set — the string is in `plugins/korvus-persona/src/hidden.rs`, where P2 allows it.
///
/// POSITION. It runs beside `window.chrome`'s install rather than here in the pre-bootstrap block,
/// because real Opera's `Object.getOwnPropertyNames(window)` reads
/// `… Float16Array, chrome, g_opr, WebAssembly, opr, crashReport …` — `g_opr` immediately after
/// `chrome`. Where the two finally sit is no longer this position's business: the registry walk's
/// last step (`installer::seat_globals_in_capture_order`) seats every global at the
/// worn capture's position, and `global_order.rs` ratchets the result at 0 under every bundle.
///
/// The shape is measured, not assumed — see `hidden.rs`'s table for the readings. `writable` and
/// `enumerable` are true on both; `configurable` differs between them and the persona carries it.
pub(crate) fn install_window_namespaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for ns in persona_window_namespaces(scope) {
        let o = v8::Object::new(scope);
        for m in &ns.members {
            let f = crate::core::navigator::devices::build_answer(scope, m);
            // A plain own `{writable, enumerable, configurable}` data property, which is what
            // `g_opr.scrap` carries on the real browser — the same descriptor `chrome`'s own keys have.
            crate::binding::protos::set_prop(scope, o, &m.name, f.into());
        }
        let Some(k) = v8::String::new(scope, &ns.global) else { continue };
        let value: v8::Local<v8::Value> = o.into();
        let mut d = v8::PropertyDescriptor::new_from_value_writable(value, true);
        d.set_enumerable(true);
        // Configurable until the registry walk seats it at its capture position, which applies the
        // persona's `configurable: false` there -- a non-configurable global cannot move.
        d.set_configurable(true);
        global.define_property(scope, k.into(), &d);
        if !ns.configurable {
            crate::install::installer::lock_at_seat(scope, global, &ns.global);
        }
    }
}

/// The persona this realm wears, asked for its window namespaces. The same I9 seam every other
/// persona-driven value reads through: the runtime PROVIDES the identity, the Web API READS it, and
/// there is no `KORVUS_BUNDLE` anywhere on the path.
fn persona_window_namespaces(scope: &mut v8::PinScope) -> Vec<spec::WindowNamespace> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::binding::protos::persona_handle(&st)
        .map(|p| p.0.window_namespaces())
        .unwrap_or_default()
}

/// Install the eight globals. Called from the runtime's pre-bootstrap block.
pub(crate) fn install_window_globals<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for (name, make, setter) in SINGLETONS {
        let Some(instance) = make(scope) else { continue };
        define_window_singleton(scope, global, name, instance, setter, true, Mint::With(make));
    }
    // `[LegacyUnforgeable]` and `[PutForwards=href]`; the object fills the slot later.
    define_window_accessor(scope, global, "location", WindowGetter::Slot, WindowSetter::PutForwardsHref, false);
    for (name, getter, setter, configurable) in SELF_ALIASES {
        define_window_accessor(scope, global, name, getter, setter, configurable);
    }
    install_document_placeholder(scope, global);
}
