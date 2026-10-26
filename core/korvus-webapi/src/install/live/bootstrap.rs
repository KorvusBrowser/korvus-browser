//! THE WEBAPI EXTENSION'S ESM LAYER, IN RUST -- what `core/korvus-webapi/src/js/` did,
//! in its own order, in the slot it evaluated in.
//!
//! # Why this file exists at all
//!
//! `deno_core` evaluates an extension's ESM entry point and its import graph INSIDE `JsRuntime::new`.
//! The five files under `src/js/` were, between them, a list of Rust calls in a fixed order plus the
//! class-template assignments -- so the whole layer was a bootstrap SEQUENCE written in the one
//! language the goal says must not define Web API (invariants I2/I3). What kept it in JS was not the
//! work; it was the MOMENT: nothing could run between the last ESM body and the runtime's own
//! pre-bootstrap block.
//!
//! THE SECOND THING THAT KEPT IT IN JS WAS BELIEVED TO BE THE `objects =` CLASSES, and that turned
//! out not to hold here. W6.2 kept `worker-host.js` partly because "the deno_core `objects =`
//! classes are reachable only from ESM ... removing it needs a Rust route to a deno_core class
//! template, which does not exist". The route does exist, and it predates that finding:
//! `install::live::fetch_values::ensure_class` has read `Headers` and `FormData` off
//! `Deno.core.ops` -- the same object `ext:core/ops` re-exports -- in BOTH
//! realms. `native_class` was that route, and it is what let `dom/00-bootstrap.js` go. (A later change then
//! deleted the route with the last deno_core class: every class is an own class built from its table.)
//!
//! Nothing has to. The pre-bootstrap block in `korvus-runtime/src/engine.rs` is the very next thing
//! that runs after `JsRuntime::new` returns, and every `install::live` doc comment in the tree
//! already says so. Putting this function FIRST in that block is therefore the identical position in
//! the order, statement for statement -- which is what the surface snapshot diff proves.
//!
//! # The order, and what each step used to be
//!
//! | step | was |
//! |---|---|
//! | the `deno_core` class templates on the global | `js/dom/00-bootstrap.js`'s nine assignments |
//! | the internal node plumbing off the two cppgc prototypes | the same file's `__unexpose` pass |
//! | the DOM inheritance spine | `OPS.op_install_dom_spine()` |
//! | the catalogued-member backfill | `js/generated_reflect.js` |
//! | the registry create + finalize | `js/generated_shells.js`'s `op_install_registry()` |
//! | crypto/XHR, media, element refs | its three `op_install_*` calls |
//! | the `SharedArrayBuffer` gate | its `op_isolation_gate()` |
//! | the device oracle | its `op_wire_canvas_oracle(op_persona_oracle())` |
//!
//! Each step is wrapped in a try-catch, because each JS statement it replaces was: a throw inside one
//! must not stop the ones after it. That is the ONLY thing [`quietly`] is for -- it is not an error
//! policy, it is the `try { … } catch (e) {}` those lines carried, kept byte-for-byte in behaviour.

use deno_core::v8;


/// The interfaces this slot puts on the realm's global, IN THE ORDER THE GLOBAL RECEIVED THEM -- a
/// realm's `globalThis` own-key insertion order is fingerprint surface, so this list is
/// `dom/00-bootstrap.js`'s assignment order verbatim and not an alphabetisation of it. The geometry
/// names are OWN classes built here from their tables (`geometry::install_geometry_interface`);
/// `MutationObserver` is still a `deno_core` `objects =` class template.
///
/// `WebKitCSSMatrix` is not here because it is not a class of its own: it is the legacy alias for
/// `DOMMatrix` and is assigned from whatever `DOMMatrix` ended up being, which is what the JS did.
const NATIVE_CLASSES: &[&str] = &[
    "DOMPointReadOnly",
    "DOMPoint",
    // DOMRectReadOnly / DOMRect are cppgc too -- DOMRect inherits DOMRectReadOnly. Beside the
    // DOMPoint family since W5.0d (they were `dom/03-element-glue.js`'s, the phase that built rects).
    "DOMRectReadOnly",
    "DOMRect",
    // DOMQuad is cppgc and Worker-exposed; putting it here, BEFORE the registry install, makes both
    // realms' shell pass skip it, so the real ctor replaces the stub (whose `new` threw).
    "DOMQuad",
    "MutationObserver",
    // DOMMatrixReadOnly / DOMMatrix are `interface!` tables over own classes. Once
    // the op2 class went on the global here and a later verification supplied the
    // `Symbol.toStringTag` / WebIDL fn lengths / ctor-inheritance chain op2 omits; the table now builds
    // all of it in this slot. Parent before child, so the ctor-chain splice has its target.
    "DOMMatrixReadOnly",
    "DOMMatrix",
];


/// `globalThis.X = <class template>`, for each of [`NATIVE_CLASSES`], plus the `WebKitCSSMatrix` and
/// `WebKitMutationObserver` aliases, in this order, each with the interface-object descriptor
/// ([`crate::install::installer::publish_on_global`]) -- the JS assignment made them enumerable.
fn expose_native_classes(scope: &mut v8::PinScope) {
    for name in NATIVE_CLASSES {
        // The geometry interfaces are OWN classes, built from their tables in this slot.
        if super::geometry::is_geometry(name) {
            super::geometry::install_geometry_interface(scope, name);
            continue;
        }
        // `MutationObserver`: its TABLE interface, in the slot deno_core's class held once (the
        // class only reserved the global key's position; the table interface replaced it after the registry).
        if *name == "MutationObserver" {
            super::mutation_custom_elements::install_mutation_observer_interface(scope);
            continue;
        }
    }
    if let Some(m) = crate::binding::protos::iface_object(scope, "DOMMatrix") {
        let global = scope.get_current_context().global(scope);
        crate::install::installer::publish_on_global(scope, global, "WebKitCSSMatrix", m);
    }
    // `[LegacyWindowAlias=WebKitMutationObserver]`: the SAME interface object (Chrome 150: `WebKitMutationObserver ===
    // MutationObserver`, its name "MutationObserver" -- korvus built the registry's own interface under that name)
    if let Some(m) = crate::binding::protos::iface_object(scope, "MutationObserver") {
        let global = scope.get_current_context().global(scope);
        crate::install::installer::publish_on_global(scope, global, "WebKitMutationObserver", m);
    }
}

/// `host.js`'s opening interface-OBJECT block: `TextEncoder`, `TextDecoder`, then `Screen` on
/// `EventTarget`, in that order (a realm's global key order is fingerprint surface).
///
/// OWN CLASSES FROM THEIR TABLES. They were the three `deno_core` class templates
/// `korvus-runtime` registered (`JsTextEncoder`, `JsTextDecoder`, `JsScreen`) -- Web API in the kernel,
/// against invariant I9 -- put on the global here, renamed (`.name` was the Rust ident), given their
/// WebIDL `length` from the registry row, and `Screen` spliced onto `EventTarget` by hand, with the
/// registry's backfill adding `onchange` / `isExtended` to the class prototype. Each is now its table in
/// korvus-webapi (`core/encoding/text_codec.rs`, `core/html/screen.rs`): `build_newable` over the class's
/// constructor for the two codecs, the ordinary non-newable table interface on `EventTarget` for
/// `Screen`, which also makes `new Screen()` throw where the class constructor answered an object.
///
/// THE POSITION still matters for the reason W4.i found: `Symbol.toStringTag` and the rest of each
/// interface go on with it here, ahead of the registry install, so `Object.prototype.toString.call(screen)`
/// reads `[object Screen]` (`window_globals_live.rs`'s `tags=` line).
///
/// `Location` IS NOT EXPOSED, and that is not an omission: the captured baseline's
/// `Location.prototype` enumerates to 0 members (real `Location` descriptor access throws). A `Location`
/// SHELL row covers the interface object.
fn install_window_host_classes<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    install_codec_interface(scope, global, "TextEncoder");
    install_codec_interface(scope, global, "TextDecoder");
    let mut ledger = crate::install::installer::take_ledger(scope);
    let event_target = super::rows::parent_of(scope, "EventTarget");
    let built = crate::install::installer::create_interface(
        scope,
        global,
        &crate::core::html::screen::SCREEN,
        "Screen",
        event_target,
        None,
        crate::install::installer::Flavor::Table,
        &mut ledger,
    );
    for g in &built.gaps {
        eprintln!("[korvus] Screen install gap: {}.{}", g.interface, g.member);
    }
    crate::install::installer::put_ledger(scope, ledger);
}

/// `TextEncoder` or `TextDecoder` on `global`, from its table over its own class's constructor -- the one
/// install both realms run (invariant I8).
fn install_codec_interface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str) {
    use crate::core::encoding::text_codec::{JsTextDecoder, JsTextEncoder, TEXTDECODER, TEXTENCODER};
    let mut ledger = crate::install::installer::take_ledger(scope);
    match name {
        "TextEncoder" => super::rows::build_newable(scope, global, &TEXTENCODER, JsTextEncoder::__kc_ctor, None, None, &mut ledger),
        "TextDecoder" => super::rows::build_newable(scope, global, &TEXTDECODER, JsTextDecoder::__kc_ctor, None, None, &mut ledger),
        _ => {}
    }
    crate::install::installer::put_ledger(scope, ledger);
}

/// One bootstrap step, with the `try { … } catch (e) {}` its JS statement carried.
fn quietly(scope: &mut v8::PinScope, f: impl FnOnce(&mut v8::PinScope)) {
    // Its own handle scope too: a step's handles die with it, so a scavenge in a later step does not
    // walk them (the install was GC-bound on root visiting -- the per-frame install).
    v8::scope!(let hs, scope);
    v8::tc_scope!(let tc, hs);
    f(tc);
    tc.reset();
}

/// The whole webapi ESM layer, in order. Called as the FIRST statement of the runtime's
/// pre-bootstrap block -- the slot the extension's ESM entry point's last body held.
///
/// `device_oracle_json` is the persona's recorded device oracle. It is an ARGUMENT because the
/// persona lives in the kernel's `OpState`, not this crate's: the JS read it through
/// `op_persona_oracle`, now deleted, for exactly the same reason, and the caller holds the persona.
pub fn install_window_bootstrap(scope: &mut v8::PinScope, device_oracle_json: &str) {
    // NO BRIDGE HOLDER IS MINTED HERE ANY MORE. This function opened with
    // `globalThis[Symbol.for("korvus.bridge")] ??= Object.create(null)` -- the third line of the
    // deleted `host.js`, kept first because `bridge_set` wrote into it and every install below this
    // line might. All four things that wrote it are moved (a V8 Private, an `OpState` field, one dead
    // bind and one name that was never on the holder), `bridge_get`/`bridge_set` are deleted, and so
    // the WINDOW realm carries no `Symbol(korvus.bridge)` at all -- which also retired the two
    // rewritten intrinsics that used to filter it out of the global's key lists.

    // ── `src/js/events.js` ──────────────────────────────────────────────────────────
    // `globalThis.EventTarget`, minted natively -- the worker's [`mint_event_target`], unchanged,
    // because the realms were doing the identical thing. BEFORE everything: the DOM spine below
    // chains onto `EventTarget.prototype`, `install_window_host_classes` splices `Screen` onto it,
    // and the `EventTarget` table takes this prototype over IN PLACE later in the same block.
    //
    // `host.js`'s `EventTarget` import was `events.js`'s LAST consumer, so that file is deleted with
    // this line, leaves `korvus_ext`'s `esm = [...]`, and `core/korvus-runtime/src/js/` is one file.
    // See [`mint_event_target`] for the two-EventTarget-functions-in-sequence seam this closes in the
    // window, which is the same one W5.3 closed in the worker.
    //
    // (The specifier is spelled out nowhere in this crate on purpose: `check-invariants.sh` resolves
    // every `ext:<ext>/<file>` string it finds in the tree against that extension's module list, and
    // a doc comment naming a module that has just been deleted is exactly the drift it is there to
    // catch. It caught this one.)
    // THIS REALM'S BUNDLE SELECTOR, declared before anything creates an interface --
    // the window half of the pair in [`install_worker_bootstrap`]. This function is the first webapi
    // install the runtime makes in the page realm (`engine.rs`'s post-`JsRuntime::new` block opens
    // with it), so declaring it here covers every create underneath, the DOM spine's included.
    crate::install::surface::enter(scope, spec::SurfaceRealm::Window);
    // The PAGE realm records what its install builds outside every interface, for the global's
    // own template (`templates::finish_global_capture`, when `persist_templates` ends the install), and its
    // deno_core classes as deno_core made them, before anything here touches them -- both under the surface
    // just entered, which names the records, and only while this surface has none yet.
    {
        let here = scope.get_current_context().global(scope);
        if crate::core::html::browsing_context::main_global(scope) == here && crate::install::templates::records_global(scope) {
            crate::install::templates::begin_global_capture();
        }
    }
    quietly(scope, mint_event_target);

    // ── `src/js/host.js`'s interface-OBJECT block ───────────────────────────────────
    // FIRST, for two reasons that are the same reason. `host.js` is `korvus_ext`'s ESM entry point
    // and `korvus_ext` is the FIRST extension in `engine.rs`'s list, so these five statements ran
    // before `dom/00-bootstrap.js`'s -- this is their position in the order, not a new one. And
    // everything that decorates `Screen.prototype` (the registry's `Symbol.toStringTag` backfill
    // most of all) runs further down this same function, which is the half W4.i got wrong by
    // installing from the pre-bootstrap block instead.
    quietly(scope, install_window_host_classes);

    // ── THE deno_core ERROR BUILDERS ────────────────────────────────────────────────
    // `host.js` called `OPS.op_register_dom_exception_builders()` and carried W5.3's own note about
    // why the WORKER could move it: `register_error_builders` is Rust and reads
    // `Deno.core.registerErrorBuilder` off the live global, and the `Deno` strip is
    // `binding::hidden::hide_deno` at the END of each realm's Rust block -- a position the WINDOW's
    // has held since W4.f. So `Deno` is still here, and the op's own doc said this call "goes when
    // that file does". It does.
    //
    // AFTER the class assignments and before anything else in the realm, which is the worker's
    // position and the JS's. Each registered builder resolves `globalThis.DOMException` at CALL time,
    // so it does not care that the table is installed later in this same block.
    quietly(scope, crate::core::dom::dom_exception::register_error_builders);

    // ── `js/dom/00-bootstrap.js` ────────────────────────────────────────────────────────────────
    // The class-slot interfaces (the geometry tables and `MutationObserver`, where the `deno_core` class
    // templates were assigned), then the DOM inheritance spine -- the file's acts, in its order (the
    // internal-plumbing strip between them went with the classes' dead members). The spine
    // is LAST because that is where `globalThis.Document`/`Element` used to be assigned: after `host.js` (so
    // `EventTarget.prototype`, the chain's root, exists), and before every other pass and the registry's create.
    quietly(scope, expose_native_classes);
    quietly(scope, crate::core::dom::spine::install_dom_spine);

    // ── `js/generated_reflect.js` ───────────────────────────────────────────────────────────────
    // The member backfill: 266 catalogued members mounted onto prototypes `host.js` built. BEFORE
    // the registry install below, and that order was measured rather than assumed -- it reaches no
    // registry interface, because `Highlight` and friends do not exist yet when it runs, and it
    // declines to overwrite what a prototype already owns. Every comment in the tree that called
    // this pass LAST described an order the tree does not have.
    quietly(scope, |scope| {
        let global = scope.get_current_context().global(scope);
        crate::install::installer::backfill_window_members(scope, global);
    });

    // ── `js/generated_shells.js` ────────────────────────────────────────────────────────────────
    // Create every registry interface (constructor + prototype + members + chain) from the one
    // table, then the toStringTag + length finalize -- over the full surface the steps above built.
    quietly(scope, |scope| {
        let global = scope.get_current_context().global(scope);
        crate::install::installer::install_window_registry(scope, global);
    });
    // The ten crypto/messaging/cookie/XHR interfaces from their `interface!` tables.
    // Right after the registry install, which is where the deleted `dom/08f-crypto.js` block ran and
    // where the hand-written `XMLHttpRequest` chain fix-up sat: after `EventTarget` and `Event`
    // exist and after the registry has created `Crypto`.
    quietly(scope, |scope| {
        let global = scope.get_current_context().global(scope);
        super::install_window_crypto_xhr(scope, global);
    });
    // The thirteen media-family interfaces from their `interface!` tables. HERE, and
    // not later in the block, because `install_window_media_rtc` replaces five of these rows' bodies
    // in place and needs the rows to exist first; installed after it, the tables' honest stub rows
    // would win and TextTrack/textTracks/getVideoPlaybackQuality would go silent.
    quietly(scope, |scope| {
        let global = scope.get_current_context().global(scope);
        super::install_window_media(scope, global);
    });
    // The five elements whose missing members LOOK AT OTHER ELEMENTS. BEFORE the
    // canvas oracle below, because that step supplies the bodies of canvas's
    // getContext/toDataURL/toBlob rows: the table puts each name in the capture's position and the
    // oracle replaces the descriptor in place. Installed after it, the honest stub rows would win and
    // the whole canvas/WebGL surface would go silent.
    quietly(scope, |scope| {
        let global = scope.get_current_context().global(scope);
        super::install_window_element_refs(scope, global);
    });
    // Chrome gates SharedArrayBuffer behind cross-origin isolation (COOP+COEP); the default
    // non-isolated window does NOT expose it, and V8 exposes it unconditionally -- a tell. The
    // isolation-policy service takes the constructor off the global and KEEPS it, putting it back for
    // a cross-origin-isolated document at navigation, so the gate and `crossOriginIsolated` cannot
    // disagree in either direction.
    quietly(scope, crate::core::html::isolation_policy::isolation_gate);
    // GS.4 device oracle: now that the WebGL/canvas shell prototypes exist, install the NATIVE
    // recorded context methods baked with the persona's oracle. The LAST step, as it was the last
    // line of the file.
    quietly(scope, |scope| {
        crate::core::html::html_canvas_element::wire_canvas_oracle(scope, device_oracle_json);
    });
}

// ── THE WORKER REALM'S BOOTSTRAP ────────────────────────────────────────────────────
//
// `core/korvus-runtime/src/js/worker-host.js` opened with eleven `globalThis.X =` statements and an
// `Object.defineProperty(cls, "name", …)` loop -- invariants I9 and I2, kept in JS by the same
// belief `native_class`'s doc comment demolished (a route deleted with the last class):
// that a `deno_core` `objects =` class template is reachable only from ESM. It was not, and this was the
// worker half of the same route; the slot now builds the classes' tables.
//
// WHERE IT RUNS, and why that is the identical position. The eleven assignments were the FIRST
// statements of the worker's ESM entry point, and every Rust install of this realm runs after
// `JsRuntime::new` returns -- `worker.rs`'s `spawn_on_thread` block, whose first statement was
// `install_worker_shells`. So "first statement of that block" is the same slot: nothing ran between
// the last of the eleven and the first Rust install then, and nothing does now. Three consumers
// depend on that, and they are why the order inside this function is the JS's verbatim:
//
//   * `install_worker_registry` SKIPS a name the global already carries, which is what stops the
//     shell pass stubbing `DOMQuad` / `DOMRect` / `DOMMatrix` / `TextEncoder`;
//   * the geometry interfaces are BUILT here from their `interface!` tables over their own classes
//     (`geometry::install_geometry_interface`; the op2 classes were verified against
//     the tables later instead), PARENT BEFORE CHILD so the ctor-chain splice has its target;
//   * `install_worker_navigator` re-prototypes the `new JsNavigator()` instance this function mints.
//
// The own-key insertion ORDER of a worker's `globalThis` is fingerprint surface, so the list below
// is `worker-host.js`'s assignment order verbatim and not an alphabetisation of it.

/// `(Deno.core.ops key, global name)` for every `deno_core` class the worker realm puts on its
/// global, IN THE ORDER THE GLOBAL RECEIVES THEM. `navigator` is not here: it is an INSTANCE, minted
/// first by [`mint_worker_navigator`].
///
/// The key is the Rust struct ident, which is what `deno_core` registers an `objects =` entry under
/// and exactly what `worker-host.js`'s `import { JsTextEncoder, … } from "ext:core/ops"` named.
const WORKER_NATIVE_CLASSES: &[(&str, &str)] = &[
    ("JsTextEncoder", "TextEncoder"),
    ("JsTextDecoder", "TextDecoder"),
    // DOMQuad is `[Exposed=(Window,Worker)]`; putting the real class here, BEFORE the registry
    // install, is what makes the worker's shell pass skip it rather than stub it (the stub's `new` threw).
    ("DOMQuad", "DOMQuad"),
    ("DOMRectReadOnly", "DOMRectReadOnly"),
    // The `DOMMatrix` pair became `interface!` tables, which took them out of the ONE
    // registry both realms draw from -- so without this slot the worker loses them ENTIRELY (measured
    // then: 292/294 interfaces, 3053/3127 members). Now the slot builds each geometry
    // interface from its table (`geometry::install_geometry_interface`). Parent before child.
    ("DOMMatrixReadOnly", "DOMMatrixReadOnly"),
    ("DOMMatrix", "DOMMatrix"),
    ("DOMRect", "DOMRect"),
    ("DOMPointReadOnly", "DOMPointReadOnly"),
    ("DOMPoint", "DOMPoint"),
];

/// `globalThis.navigator` -- the SAME own class the window mints (`core/navigator/navigator.rs`'s
/// `JsNavigator`; a `deno_core` class of the kernel's until then).
///
/// `install_worker_navigator` points this instance's `__proto__` at the one-table `WorkerNavigator`
/// prototype later in the same block; until then it carries the plain wrapper prototype (a worker has no
/// `Navigator` interface for `binding::class::mint` to find), and no member is read in between.
fn mint_worker_navigator<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let nav = crate::binding::class::mint(scope, crate::core::navigator::navigator::JsNavigator);
    let global = scope.get_current_context().global(scope);
    let Some(k) = v8::String::new(scope, "navigator") else { return };
    global.set(scope, k.into(), nav.into());
}

/// [`WORKER_NATIVE_CLASSES`] onto the global, in order, each with the interface-object descriptor
/// ([`crate::install::installer::publish_on_global`]).
fn expose_worker_native_classes<'s>(scope: &mut v8::PinScope<'s, '_>) {
    for (key, name) in WORKER_NATIVE_CLASSES {
        // The codecs are OWN classes, built from their tables in this slot.
        if matches!(*name, "TextEncoder" | "TextDecoder") {
            let global = scope.get_current_context().global(scope);
            install_codec_interface(scope, global, name);
            continue;
        }
        // The geometry interfaces are OWN classes, built from their tables in this slot.
        if super::geometry::is_geometry(name) {
            super::geometry::install_geometry_interface(scope, name);
            continue;
        }
        let _ = key;
    }
}

/// `globalThis.EventTarget` -- the interface object and the PROTOTYPE OBJECT the rest of the realm
/// chains onto, minted NATIVELY. BOTH REALMS.
///
/// This was `worker-host.js`'s and then `host.js`'s `globalThis.EventTarget = EventTarget`, importing
/// `core/korvus-runtime/src/js/events.js` (`export class EventTarget {}`) for the object identity
/// alone. IT WAS THE FILE'S LAST CONSUMER, so `events.js` is deleted with the window half and
/// `core/korvus-runtime/src/js/` is `host.js` alone. The class carried no members:
/// `install_event_model` mounts the `EventTarget` TABLE onto this prototype IN PLACE a few statements
/// into the same block, and every later lane splices a child's prototype and constructor chain onto
/// it BY NAME.
///
/// IT REPLACED `core::events::nativize_event_target_ctor`, WHICH WAS DELETED WITH THE WORKER HALF,
/// and that deletion closed a seam the function could not. That one ran from `install_event_model` --
/// i.e. AFTER `install_worker_shells` had already chained the registry's prototypes onto the JS
/// class's `.prototype` -- and then swapped the CONSTRUCTOR for a fresh native one. The prototype
/// object survived the swap so the chains stayed intact, but for the length of that block the realm
/// had two `EventTarget` functions and the JS one was still `Function.prototype.toString`-visible.
/// Minting the native one HERE, in the slot the JS assignment held and before anything looks the name
/// up, means there is only ever one.
///
/// THE WINDOW HAD THE SAME TWO-IN-SEQUENCE SHAPE, one lane further apart, and W5.3 predicted it.
/// `host.js` assigned the JS class at ESM time; `install_event_model` (reached from
/// `install_window_html_image_element`, later in the pre-bootstrap block) then overwrote its
/// prototype from the table. Between those two moments every window lane that splices a child onto
/// `EventTarget` by name -- the DOM spine's chain root most of all -- was reading a JS class object,
/// and `String(EventTarget)` printed `class EventTarget {}` to any page script that ran in between.
/// Minting it as the first statement of the window bootstrap is the same close: `host.js` is
/// `korvus_ext`'s ESM entry point and `korvus_ext` is the first extension, so this function's opening
/// statements ARE the slot that assignment held.
///
/// A JS class's `.prototype` is a plain `Object.prototype`-rooted object owning `constructor`, so a
/// fresh `v8::Object::new` plus the `constructor` definition below is the same object shape; and
/// `overwrite_prototype` deletes every own key and re-defines `constructor` LAST from the table
/// regardless, which is the position Chrome puts it in.
fn mint_event_target<'s>(scope: &mut v8::PinScope<'s, '_>) {
    // `new EventTarget()` is legal, so the function must be constructible; V8 mints the instance
    // against `.prototype` itself, which is why the body has nothing to do on that path.
    //
    // WITHOUT `new` IT MUST THROW, and it did not. Found independently twice: by a sweep of
    // the whole class, and by the change that deleted `events.js`.
    //
    // The mechanism: a JS `class EventTarget {}` refuses a plain call by construction -- "Class
    // constructor EventTarget cannot be invoked without 'new'" -- while a `v8::Function` with
    // `ConstructorBehavior::Allow` and an empty callback simply RETURNS UNDEFINED. So `EventTarget('t')`
    // went from a throw to a silent success when W5.3 minted the worker's natively. THAT DEFECT WAS
    // LIVE IN THE WORKER FROM W5.3 UNTIL HERE, because nothing in the worker gate asked; the window's
    // `event_family_live::the_constructors_throw_what_chrome_throws` asked immediately. The asymmetry
    // in what the two realms' gates cover is the reusable lesson, not the missing guard.
    //
    // It is `cb_table_value_ctor`, the same body every other hand-table `Model::Value` placeholder
    // gets, NOT a bespoke callback with the sentence written out again. W7.3 folded 27 inline copies
    // of Chrome's message onto one spelling; a 28th here would have undone that for this row.
    // `build_interface_ctor` binds the `[name, requiredArgs]` data the body names itself from.
    //
    // Chrome's answer, measured (`google-chrome --headless=new` 150.0.7871.46) rather than guessed:
    // `TypeError: Failed to construct 'EventTarget': Please use the 'new' operator, this DOM object
    // constructor cannot be called as a function.` `EventTarget.length` is 0 and `String(EventTarget)`
    // reads `function EventTarget() { [native code] }` in both realms, unchanged by this.
    //
    // THE TABLE'S ROWS GO ON HERE TOO, not later in `events::install_event_model`: a
    // template carries an interface only whole, and the lanes between here and the event model splice
    // their children onto THIS prototype, so the pair the realm keeps has to exist, members and all,
    // from this statement on. The rows need nothing that is not already there: four `build_fn` methods.
    let def = &crate::core::events::event_target::EVENTTARGET;
    let mut ledger = crate::install::installer::take_ledger(scope);
    use crate::install::installer::{tpl_done, tpl_step, TplStep};
    let (f, _proto) = match tpl_step(scope, def, "EventTarget", None, "mint", crate::install::Exposed::Both, &mut ledger) {
        TplStep::Instantiated(f, p, _) => (f, p),
        TplStep::Build(key, mark) => {
            let f = crate::binding::protos::build_interface_ctor(scope, crate::binding::protos::cb_table_value_ctor, "EventTarget", 0);
            let proto = v8::Object::new(scope);
            // (never `None` for an ASCII literal; an early return here would drop the ledger taken above)
            let pk = v8::String::new(scope, "prototype").expect("an ASCII name");
            // `X.prototype`: non-writable, non-enumerable, non-configurable -- an interface object's shape.
            let mut pd = v8::PropertyDescriptor::new_from_value_writable(proto.into(), false);
            pd.set_enumerable(false);
            pd.set_configurable(false);
            f.define_property(scope, pk.into(), &pd);
            // `X.prototype.constructor`: writable, non-enumerable, configurable -- what a class body leaves,
            // and what `overwrite_prototype` re-defines from the table a moment later.
            if let Some(ck) = v8::String::new(scope, "constructor") {
                let mut cd = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
                cd.set_enumerable(false);
                cd.set_configurable(true);
                proto.define_property(scope, ck.into(), &cd);
            }
            if let Err(e) = crate::install::installer::overwrite_prototype(scope, f, def, &mut ledger) {
                eprintln!("[korvus] event target: {e}");
            }
            tpl_done(scope, key, mark, "EventTarget", None, f, proto, &ledger, &[]).unwrap_or((f, proto))
        }
    };
    crate::install::installer::put_ledger(scope, ledger);
    let global = scope.get_current_context().global(scope);
    crate::install::installer::publish_on_global(scope, global, "EventTarget", f.into());
}

/// The worker realm's bootstrap, in `worker-host.js`'s order. Called as the FIRST statement of
/// `worker.rs`'s post-`JsRuntime::new` block -- the slot the worker ESM entry point's opening
/// statements held. See the section header above for what each step used to be and what depends on
/// its position.
///
/// Each step is wrapped in [`quietly`] for the same reason the window's steps are: every JS
/// statement it replaces sat in a module whose throw would have taken the rest of the file with it,
/// and a failure has to stay local to its step.
pub fn install_worker_bootstrap(scope: &mut v8::PinScope) {
    // THIS REALM'S BUNDLE SELECTOR, declared first. The window and worker captures are
    // two different recordings -- 1,206 own globals against 335, and the same table is `Navigator` in
    // one and `WorkerNavigator` in the other -- so the realm has to be known before the first
    // interface is created. It is declared rather than sniffed for the reason
    // `install::surface`'s module doc gives: neither of the two ways to infer it is true this early.
    crate::install::surface::enter(scope, spec::SurfaceRealm::Worker);
    quietly(scope, mint_worker_navigator);
    quietly(scope, expose_worker_native_classes);
    quietly(scope, mint_event_target);
    // THE deno_core ERROR BUILDERS, AND THE CEILING THAT SAID THEY COULD NOT BE HERE.
    //
    // `worker-host.js` called `O.op_register_dom_exception_builders()` and argued the call could not
    // move: "`Deno.core.registerErrorBuilder` is reachable only from JS, and the tail of this file
    // deletes `globalThis.Deno` -- which happens before `JsRuntime::new` returns, i.e. before any
    // Rust install of this realm can run."
    //
    // The first half was already false when it was written: [`crate::core::dom::dom_exception::register_error_builders`]
    // IS Rust and reads `registerErrorBuilder` off the live `Deno` global, which is why the op is a
    // three-line wrapper around it. The second half was true only because of WHERE THE DELETE WAS --
    // nothing in deno_core requires the worker's ESM tail to be the thing that strips `Deno`. That
    // delete is `binding::hidden::hide_deno`, called from `worker.rs` at the END of the realm's Rust
    // block now (the position the WINDOW's has held since W4.f), so `Deno` is still on this realm's
    // global here. Measured both ways, in both realms, with `tests/fixtures/dom-exception-probe.js`'s
    // `vals.fromOp`: `atob("x")` throws `[instanceof DOMException, instanceof Error,
    // "InvalidCharacterError", 5]` before this move and after it.
    //
    // The position among the steps above is the JS's: after the class assignments, before anything
    // else in the realm runs. Each registered builder resolves `globalThis.DOMException` at CALL
    // time, so it does not care that the table is installed later in the same block.
    quietly(scope, crate::core::dom::dom_exception::register_error_builders);
}
