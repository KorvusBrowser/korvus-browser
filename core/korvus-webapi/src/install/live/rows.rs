//! The row-mount helpers every table install shares, so one table cannot mount a member a
//! different way from another's.
//!
//! (The sentence above is `dom/08f-crypto.js`'s replacement's, which wrote the
//! first six of them for its own six interface files; every domain uses them now.)
//!
//! [`parent_of`] finds the splice point; [`getter_row`] / [`pair_row`] / [`method_row`] mount one
//! row each; [`build_newable`] builds a table interface that has a real constructor;
//! [`rechain_orphans`] repairs
//! a registry child whose WebIDL parent did not exist when the chain pass ran; [`set_table_proto`]
//! and [`strip_own`] are the two mint-site helpers.

use deno_core::v8;

use crate::install::installer;
use crate::install::InterfaceDef;

/// `(prototype, constructor)` of an interface already on this realm's global -- the `parent` argument
/// [`installer::create_interface`] splices onto. Once the realm is sealed, the interface object as its install
/// left it (`protos::iface_object`): the receiver checks that ask this on every call (`history.state`) read
/// `window.History` as a page could have replaced it.
pub fn parent_of<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)> {
    use crate::binding::protos::get_prop;
    let ctor = crate::binding::protos::iface_object(scope, name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let proto = get_prop(scope, ctor.into(), "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    Some((proto, ctor))
}

// `worker_global_scope_proto` was here: `parent_of(scope, "WorkerGlobalScope")`, so a lane could be
// handed the holder a `WorkerGlobalScope` row belongs on and define the member itself. Deleted with
// `core/workers/worker_global_scope.rs::scope_proto`, its only callee -- see the note
// there. The rows are a table and the installer picks the holder, so nothing asks.

/// Re-prototype a Rust-minted instance onto the TABLE's prototype for `iface`. Every mint site of a
/// cppgc value whose interface is a table calls this, so `instanceof`, `constructor` and
/// `Object.getPrototypeOf` answer the interface object the page can see; the cppgc unwrap reads the
/// object's wrapper tag, never its prototype, so the brand is untouched.
pub(crate) fn set_table_proto(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, iface: &str) {
    // and the interface's brand: the table's instances are made here
    if let Some(p) = crate::binding::protos::shell_proto(scope, iface) {
        crate::binding::protos::adopt_as(scope, obj, p.into());
    }
}

/// A new instance of table interface `iface`, MADE FROM ITS TEMPLATE in this realm:
/// `binding::protos::mint_on` on the realm's prototype for `iface`, so the object is the interface's from
/// birth rather than an ordinary object handed the prototype and the brand afterwards, as `Object::new` +
/// [`set_table_proto`] made it. A realm without the interface gets an ordinary object, as that pair did
/// (every such pair became this call).
pub(crate) fn mint_table<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Object> {
    match crate::binding::protos::shell_proto(scope, iface) {
        Some(p) => crate::binding::protos::mint_on(scope, p.into()),
        None => v8::Object::new(scope),
    }
}

/// Mount one readonly attribute row: a native getter named `get <member>`, length 0.
pub(crate) fn getter_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    use crate::binding::protos::build_fn;
    let g = build_fn(scope, cb, &format!("get {}", member.name), 0);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}

/// Mount one read/write attribute row (`XMLHttpRequest.timeout` and friends).
pub(crate) fn pair_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    get_cb: impl v8::MapFnTo<v8::FunctionCallback>,
    set_cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    use crate::binding::protos::build_fn;
    let g = build_fn(scope, get_cb, &format!("get {}", member.name), 0);
    let s = build_fn(scope, set_cb, &format!("set {}", member.name), 1);
    crate::install::kits::define_accessor(scope, proto, member, g, Some(s));
}

/// Mount one method row: the native callback with the row's WebIDL name and length, wrapped in the
/// arity guard when the row declares required arguments (a table-owned member never sees the
/// registry's separate arity pass, so the guard is applied here). The interface name in the guard's
/// message is the row's own label, which is what Blink's message names.
pub(crate) fn method_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    use crate::binding::protos::{build_fn, make_arity_guard};
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let f = build_fn(scope, cb, member.name, member.length as i32);
    let v: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, iface, member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    crate::install::kits::define_value(scope, target, member, v, true);
}

/// Build one NEWABLE table interface: a native constructor over the table's constructor body, a
/// fresh `{writable:false, enumerable:false, configurable:false}` `.prototype` the table's rows are
/// installed onto by [`installer::overwrite_prototype`] (rows in table order, `constructor` last, the
/// tag, the constructor's WebIDL `length`), and the WebIDL inheritance splice onto `parent`.
///
/// `create_interface` cannot express this: a hand table's `Model::Value` interface object is a no-op
/// placeholder there, and a table row cannot yet name a constructor body (`CtorDef::install` is
/// unread). This is the shape `events::install_event_model` uses for `Observable`.
///
/// `selector` is the UNION-TABLE parameter (W5.1 L4b). `None` -- what every caller but one passes --
/// installs every row in table order, which is what this function always did; `Some(names)` installs
/// only those names, in the selector's order, for a table whose two realms disagree about the member
/// list. `MediaSource` is the only such table today: the worker capture has a 13th prototype member
/// (`handle`) the window capture does not, so the table carries the worker's list and the WINDOW
/// install selects the window's twelve. See [`installer::overwrite_prototype_selected`].
pub(crate) fn build_newable<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    parent: Option<&str>,
    selector: Option<&[&str]>,
    ledger: &mut installer::InstallLedger,
) {
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    build_newable_with_data(scope, global, def, cb, u, parent, selector, ledger)
}

/// [`build_newable`] with a value bound as the constructor's `data` — `build_fn` is to
/// `build_fn_data` what this is to `build_newable`, and for the same reason.
///
/// Web Audio needed it: the twenty Web Audio node constructors are ONE callback that
/// dispatches on the interface name (`new OscillatorNode(ctx)` and `new GainNode(ctx)` differ only in
/// which `AudioNodeKind` they record), and `new AudioContext()` has to know the persona's sample rate
/// without re-parsing the device oracle per `new`. Both were `make_constructable_with_data` before
/// the cutover, which is this shape over a prototype the shell layer had already built.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_newable_with_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: v8::Local<'s, v8::Value>,
    parent: Option<&str>,
    selector: Option<&[&str]>,
    ledger: &mut installer::InstallLedger,
) {
    let len = def.ctor.as_ref().map(|c| c.length as i32).unwrap_or(0);
    // The same template step as `installer::create` (`install::templates`). A realm that
    // instantiates the template gets the whole pair -- members, statics, `constructor`, tag -- and
    // publishes and splices below exactly as a built one does.
    let (ctor, proto) = match installer::tpl_step(scope, def, def.name, selector, "newable", crate::install::Exposed::Both, ledger) {
        installer::TplStep::Instantiated(ctor, proto, gaps) => {
            publish_newable(scope, global, def, ctor);
            for g in &gaps {
                eprintln!("[korvus] newable install gap: {}.{}", g.interface, g.member);
            }
            (ctor, proto)
        }
        installer::TplStep::Build(key, mark) => {
            let raw = cb.map_fn_to();
            let ctor = v8::Function::builder_raw(crate::binding::refs::c(raw))
                .length(len)
                .data(data)
                .constructor_behavior(v8::ConstructorBehavior::Allow)
                .build(scope)
                .expect("a constructor template builds");
            if let Some(n) = v8::String::new(scope, def.name) {
                ctor.set_name(n);
            }
            if crate::install::templates::capturing() {
                let recipe = crate::install::templates::Recipe {
                    cb: Some(raw),
                    data: Some(v8::Global::new(scope, data)),
                    length: len,
                    name: def.name.to_string(),
                    ctor: true,
                    guard: crate::install::templates::Guard::None,
                    inner: None,
                };
                crate::install::templates::record(scope, ctor, recipe);
            }
            crate::install::templates::note_untemplated(scope, "build_newable", def.name, ctor);
            let proto = v8::Object::new(scope);
            if let Some(pk) = v8::String::new(scope, "prototype") {
                ctor.define_own_property(
                    scope,
                    pk.into(),
                    proto.into(),
                    v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::DONT_DELETE,
                );
            }
            // Published BEFORE the rows are mounted, as this path always has: a row's body may read the
            // global it is being installed under.
            publish_newable(scope, global, def, ctor);
            let gaps = match installer::overwrite_prototype_selected(scope, ctor, def, selector, ledger) {
                Ok((_, gaps)) => {
                    for g in &gaps {
                        eprintln!("[korvus] newable install gap: {}.{}", g.interface, g.member);
                    }
                    gaps
                }
                Err(e) => {
                    eprintln!("[korvus] newable install: {e}");
                    Vec::new()
                }
            };
            match installer::tpl_done(scope, key, mark, def.name, parent, ctor, proto, ledger, &gaps) {
                // the template's pair replaces the one published above (`None`: no template, a refused interface)
                Some((c, p)) => {
                    publish_newable(scope, global, def, c);
                    (c, p)
                }
                None => (ctor, proto),
            }
        }
    };
    // `None` is an interface that inherits nothing (`DataTransfer`, W5.1 L8): its prototype's
    // [[Prototype]] stays `Object.prototype` and its constructor's `Function.prototype`, which is what
    // the capture records.
    match parent {
        None => {}
        Some(p) => match parent_of(scope, p) {
            Some((pp, pc)) => {
                proto.set_prototype(scope, pp.into());
                ctor.set_prototype(scope, pc.into());
            }
            // THE PARENT IS NAMED BUT NOT INSTALLED YET. This used to fall through silently, leaving
            // the child on `Object.prototype` and the page with a broken `instanceof` that nothing
            // read until a live gate happened to look -- the third failure mode of a table cutover,
            // in the install path itself.
            //
            // It is not hypothetical and it is not fixable by ordering alone. `OfflineAudioCompletionEvent`
            // (lane L17) names `Event` and is built during the webapi extension's ESM evaluation, from
            // `wire_canvas_oracle`; W4.b moved `Event` out of the generated registry into a hand
            // table installed in the PRE-BOOTSTRAP block, which is after all ESM. Both lanes recorded a
            // correct reason for their position and the two reasons cannot both be honoured by moving
            // one of them. `rechain_orphans` cannot help either: it walks the generated REGISTRY, and
            // these children are hand tables that left it.
            //
            // So the unresolved edge is REMEMBERED and spliced when the parent arrives. Recorded, not
            // guessed: only a pair this function was actually asked for.
            None => pending_chains_push(def.name, p),
        },
    }
}

/// PUBLISHED ON THE GLOBAL ONLY IF THIS DEVICE HAS THIS INTERFACE, the same rule `installer::create`
/// applies and for the same reason (extended to this path by P4a): a name the device does
/// not have is a name a page reads with `typeof`. The prototype is still built and still chained -- an
/// unpublished interface object is unreachable, not absent -- so a caller that holds the pair keeps
/// working. ... and only under its own EXPOSURE CONDITION, through the same one gate --
/// the row carries it, so this path cannot drift from `create_interface_in`'s.
fn publish_newable<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, def: &'static InterfaceDef, ctor: v8::Local<'s, v8::Function>) {
    if crate::core::html::exposure::gate(scope, crate::core::html::exposure::Publishes::Interface, def.name, def.exposure) {
        crate::install::installer::publish_on_global(scope, global, def.name, ctor.into());
    }
}

thread_local! {
    /// `(child, parent)` edges whose parent did not exist when the child was built. One realm per
    /// thread in this engine (a Worker is a real OS thread with its own isolate), so this is
    /// per-realm state; [`drain_pending_chains`] empties it.
    static PENDING_CHAINS: std::cell::RefCell<Vec<(&'static str, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn pending_chains_push(child: &'static str, parent: &str) {
    PENDING_CHAINS.with(|c| c.borrow_mut().push((child, parent.to_string())));
}

/// Splice every remembered edge whose parent now exists. Call it after installing a family that other
/// tables inherit from; it is cheap and idempotent when nothing is pending.
///
/// As narrow as [`rechain_orphans`] deliberately is: a child is re-pointed ONLY while its prototype is
/// still the bare `Object.prototype`, so an edge left over from an earlier realm on the same thread, or
/// a child some lane chained itself in the meantime, cannot be disturbed. An edge whose parent is still
/// absent stays pending rather than being dropped.
///
/// `_global` is unread: every lookup here goes through `parent_of`/`global_get`, which read the scope's
/// CURRENT context global — the same object in either realm at the one moment this is called. The
/// parameter stays so both realms' call sites read alike and so a future per-realm lookup has it.
pub(crate) fn drain_pending_chains<'s>(scope: &mut v8::PinScope<'s, '_>, _global: v8::Local<'s, v8::Object>) {
    let pending: Vec<(&'static str, String)> = PENDING_CHAINS.with(|c| std::mem::take(&mut *c.borrow_mut()));
    if pending.is_empty() {
        return;
    }
    let obj_proto = crate::binding::protos::global_get(scope, "Object")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|o| crate::binding::protos::get_prop(scope, o, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let mut still_pending = Vec::new();
    for (child, parent) in pending {
        let Some((pp, pc)) = parent_of(scope, &parent) else {
            still_pending.push((child, parent));
            continue;
        };
        let Some((cproto, cctor)) = parent_of(scope, child) else { continue };
        let on_bare_root = match (cproto.get_prototype(scope), obj_proto) {
            (Some(cur), Some(op)) => cur == v8::Local::<v8::Value>::from(op),
            _ => false,
        };
        if !on_bare_root {
            continue;
        }
        cproto.set_prototype(scope, pp.into());
        cctor.set_prototype(scope, pc.into());
    }
    PENDING_CHAINS.with(|c| *c.borrow_mut() = still_pending);
}


// `take_prototype` was here: fetch an existing class's constructor off the global and hand its
// prototype to that class's table (`installer::overwrite_prototype`). Deleted, because
// the classes it took FROM are gone. It existed for the window's `host.js` JS classes -- both former
// call sites say so in place, `install/live/abort_style.rs:165` ("this call was `take_prototype`: the
// table…") and `install/live/ranges.rs:51` -- and each now builds its interface from the table
// outright. `installer::overwrite_prototype` keeps its other callers.


/// Chain the registry interfaces whose WebIDL parent the chain pass could not find.
///
/// The chain pass (`install_registry` / `install_worker_registry`) runs during the ESM layer, BEFORE
/// this install, and cannot resolve a parent the global does not name yet: the window leaves the child
/// on the bare `Object.prototype` its shell was minted with, the worker on a member-less PHANTOM
/// carrying only `constructor`. Nine of this lane's ten still have a JS class in the WINDOW, so the
/// pass found the very prototype the table then takes over in place and their children are already
/// right; `CSSStyleDeclaration`, which has no class, left `CSSPositionTryDescriptors` and
/// `CSSFunctionDescriptors` orphaned. In the WORKER six of the eight have no class, so `TaskSignal`,
/// `TaskController` and the `PerformanceEntry` family were all left on phantoms.
///
/// Three more were added, and they are why this sweep matters beyond its own lane: `TextEvent`
/// and `CompositionEvent` (parent `UIEvent`) and `DragEvent` (parent `MouseEvent`) are registry
/// interfaces whose parents became `interface!` tables built in the runtime's pre-bootstrap block --
/// after the ESM chain pass, where `host.js`'s classes used to have them ready. `installer_parity`,
/// which installs the registry ALONE in a bare runtime with no table beside it, records all three on
/// `Object.prototype` for exactly that reason; at a real boot this repair runs and the release binary
/// answers Chrome's `TextEvent -> UIEvent -> Event -> Object` (verified with `--dump-dom`).
///
/// Nothing here is written down: it walks the realm's registry, applies the chain pass's own rule
/// (`globalThis[parent].prototype`), and acts ONLY on a child whose current prototype is EMPTY -- the
/// `Object.prototype` root, or an object whose sole own key is `constructor`. So it can repair a chain
/// the pass could not make and can never disturb one another pass set deliberately.
pub(crate) fn rechain_orphans<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, registry: &'static [&'static InterfaceDef]) {
    use crate::binding::protos::get_prop;
    let proto_of = |scope: &mut v8::PinScope<'s, '_>, name: &str| -> Option<v8::Local<'s, v8::Object>> {
        let k = v8::String::new(scope, name)?;
        let c = global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    };
    let Some(objp) = proto_of(scope, "Object") else { return };
    let objpv: v8::Local<v8::Value> = objp.into();
    let any_lazy = crate::install::templates::has_lazy_globals(scope);
    for d in registry {
        let Some(parent) = d.parent else { continue };
        // a page interface put lazily: chained from its template when it is made
        if any_lazy && crate::install::templates::lazy_global(scope, global, d.name).is_some() {
            continue;
        }
        let (Some(child), Some(pp)) = (proto_of(scope, d.name), proto_of(scope, parent)) else { continue };
        let Some(cur) = child.get_prototype(scope) else { continue };
        let ppv: v8::Local<v8::Value> = pp.into();
        if cur == ppv || pp == objp {
            continue;
        }
        // Either the shell root, or `worker_phantom_proto`'s stand-in: an object whose ONE own key is
        // `constructor` and whose constructor is a function named for the parent. Deliberately narrow.
        // "No own keys" is NOT the test: the Window named-properties object (core/dom/window_named.rs)
        // answers everything through a v8 interceptor and owns nothing, and splicing it out of
        // `Window.prototype`'s chain is exactly the four-deep-vs-five-deep tell `dom/09h-window-named.js`
        // exists to fix -- `tests/automation_tells.rs` caught it.
        let phantom = v8::Local::<v8::Object>::try_from(cur)
            .ok()
            .map(|o| {
                let args = v8::GetPropertyNamesArgsBuilder::new()
                    .mode(v8::KeyCollectionMode::OwnOnly)
                    .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
                    .index_filter(v8::IndexFilter::IncludeIndices)
                    .build();
                let only_ctor = match o.get_own_property_names(scope, args) {
                    Some(n) => n.length() == 1 && n.get_index(scope, 0).map(|k| k.to_rust_string_lossy(scope) == "constructor").unwrap_or(false),
                    None => false,
                };
                only_ctor
                    && get_prop(scope, o, "constructor")
                        .and_then(|c| v8::Local::<v8::Function>::try_from(c).ok())
                        .map(|c| c.get_name(scope).to_rust_string_lossy(scope) == parent)
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        if cur == objpv || phantom {
            child.set_prototype(scope, pp.into());
            // ...AND THE INTERFACE OBJECT. WebIDL §3.7: an inherited interface object's [[Prototype]]
            // is the PARENT INTERFACE OBJECT, not `Function.prototype` --
            // `Object.getPrototypeOf(CSSImageValue) === CSSStyleValue` in Chrome 150, recorded.
            // `create_interface`'s own splice has always done both halves; this sweep did only the
            // prototype, so every child it repaired kept `Function.prototype` on its constructor --
            // a second, independent chain a page can read with one `getPrototypeOf`. Found by
            // asserting it for the seven children of the two tables
            // (`tests/typed_om_view_transition_live.rs`); the fix is here, so L4's, L12's and L14's
            // orphans get it too.
            let (Some(cc), Some(pc)) = (
                v8::String::new(scope, d.name)
                    .and_then(|k| global.get(scope, k.into()))
                    .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()),
                v8::String::new(scope, parent)
                    .and_then(|k| global.get(scope, k.into()))
                    .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()),
            ) else {
                continue;
            };
            cc.set_prototype(scope, pc.into());
        }
    }
}


/// Strip a singleton bare: delete every own property, string- and symbol-keyed.
///
/// A real singleton OWNS NOTHING -- every member is inherited from its interface prototype. Measured
/// on Brave 151 and re-probed on headless Chrome 150.0.7871.46 (2026-09-11, loopback server):
/// `Object.getOwnPropertyNames(crypto)`, `(performance)` and `(history)` are all `[]`. An internal
/// that rode along as an own property would otherwise be a permanently visible invented member; a
/// `Symbol.toStringTag` left behind would shadow the prototype's with a JS function a page can print.
///
/// This is the Rust form of the delete loop `dom/09i-singletons.js`'s `rehome` ran, and the singleton
/// re-homes share it.
pub fn strip_own(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) {
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::ALL_PROPERTIES)
        .index_filter(v8::IndexFilter::IncludeIndices)
        .key_conversion(v8::KeyConversionMode::KeepNumbers)
        .build();
    if let Some(keys) = obj.get_own_property_names(scope, args) {
        for i in 0..keys.length() {
            if let Some(k) = keys.get_index(scope, i) {
                obj.delete(scope, k);
            }
        }
    }
}

// ── the cppgc class templates' interface objects -- DELETED ──────────────────────────────
// `guard_native_class_ctors` / `class_interface_object` gave a deno_core class template on the global the
// interface object a page should see. Every class is an own class built from its table, so no
// global holds a class template any more.
