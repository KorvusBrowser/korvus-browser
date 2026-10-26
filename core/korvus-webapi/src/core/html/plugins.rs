//! `core/html/plugins.rs` -- `PluginArray` / `Plugin` / `MimeTypeArray` / `MimeType`, their four
//! `interface!` tables and the two singletons `navigator.plugins` and `navigator.mimeTypes` hand back.
//!
//! WHAT THIS REPLACES. The `navigator.plugins` block of `dom/09i-singletons.js`: nine
//! `Object.defineProperty` sites, three `op_nativize` masks and two `KB.__evtSlotSet` bridge calls,
//! building the four prototypes by hand over the shells the capture had shaped and left answering
//! `undefined`.
//!
//! WHY IT MATTERED. `navigator.plugins` was an Array with `item` and `namedItem` bolted on, which
//! makes `Array.isArray(navigator.plugins)` TRUE where every browser says false -- one line, on the
//! single most-read surface in browser fingerprinting. `Object.prototype.toString` said
//! `[object Array]` against `[object PluginArray]`, nothing was an instance of anything, and the four
//! interfaces the shells had built from the capture sat beside them with the right members and no
//! implementation.
//!
//! THE MEASURED SHAPE, from Brave 151: a `PluginArray` owns its indices AND a named key per plugin,
//! with `length`/`item`/`namedItem`/`refresh` on the prototype; a `Plugin` owns its indices and the
//! two mime essences by name, with `name`/`filename`/`description`/`length`/`item`/`namedItem` on the
//! prototype; a `MimeType` owns nothing at all. The indices and names are NOT own data properties:
//! Blink answers them from interceptors, the names `[LegacyUnenumerableNamedProperties]` --
//! `{writable:false, enumerable:FALSE, configurable:true}`, refused by `defineProperty` and `delete` --
//! so `Object.keys(navigator.plugins[0])` is `["0","1"]` and a `for-in` plugin string lists each mime
//! type once (Chrome 150). Here the three lists are
//! `core/dom/collections.rs`'s static named lists, which answer exactly that. Every row below is VERBATIM from the `PluginArray` /
//! `Plugin` / `MimeTypeArray` / `MimeType` blocks of `install/generated_registry.rs` -- the capture's
//! order, kinds, lengths, `required` counts and descriptor flags -- with a body in place of `stub`.
//!
//! THE DATA is the persona's (`ClientHints::plugins`), read from `PersonaHandle` here rather than
//! through `op_persona_hints` as the JS did (that op is gone), paired with the two PDF mime essences -- a Chrome
//! structural constant, identical across every Chrome build.
//!
//! RECEIVERS. Every body checks that its `this` really inherits from the interface's prototype and
//! throws Chrome's `Illegal invocation` otherwise. The JS bodies did `this[i] || null`, so
//! `PluginArray.prototype.item.call({}, 0)` answered `null` where headless Chrome 150.0.7871.46 on
//! this machine throws (probed 2026-09-11, over a loopback server); the slot getters answered
//! `undefined` for the same reason.

use deno_core::v8;

use crate::binding::protos::{str_val, throw_type_error};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::live::{method_row, parent_of};
use crate::install::{installer, MemberDef};

// ── receiver, storage and mint helpers ───────────────────────────────────────────────────────────

/// Is `this` a real instance of `iface` -- of ANY realm: the interface's prototype is in its chain and is not
/// `this` itself (`protos::table_receiver_ok`, which also takes another realm's instance, as Chrome does: a
/// frame's `PluginArray.prototype.item` on the page's `navigator.plugins` -- B26). Anything else throws
/// Chrome's `Illegal invocation` and answers false.
///
/// Returns a BOOL rather than the receiver because a returned `Local` would have to borrow the
/// scope's lifetime, which a `FunctionCallback`'s `args.this()` does not carry -- the elided-lifetime
/// gotcha `core/html/html_meter_element.rs` documents.
fn recv(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str) -> bool {
    let ok = parent_of(scope, iface).is_some_and(|(proto, _)| crate::binding::protos::table_receiver_ok(scope, this.into(), proto.into()));
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// Make `o` the `iface` list of `items`, each of `names` answering the value at its position in `named` -- the
/// interceptors' state (`collections::adopt_static_named`). They were own data properties, the named ones
/// enumerable too: `Object.keys(plugins[0])` read `["0","1","application/pdf","text/pdf"]` where Chrome 150 reads
/// `["0","1"]`, and a page's `for (k in plugin)` collected each mime type twice.
fn adopt<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
    iface: &str,
    items: &[v8::Local<'s, v8::Value>],
    names: &[&str],
    named: &[v8::Local<'s, v8::Value>],
) {
    let items = v8::Array::new_with_elements(scope, items);
    let nv: Vec<v8::Local<v8::Value>> = names.iter().map(|n| str_val(scope, n)).collect();
    let names = v8::Array::new_with_elements(scope, &nv);
    let named = v8::Array::new_with_elements(scope, named);
    crate::core::dom::collections::adopt_static_named(scope, o, iface, items, names, named);
}

/// A fresh instance of `proto` with no own properties (`MK(P)` in the deleted JS).
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Object> {
    let o = crate::binding::protos::mint_on(scope, proto.into());
    o
}

// ── the shared bodies ────────────────────────────────────────────────────────────────────────────

/// A readonly slot getter WITH the receiver check: `args.data()` carries the member name, bound at
/// install time, so one callback serves every scalar row of all four interfaces. `data` is
/// `[member, interface]`.
fn cb_slot(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some((iface, member)) = d.split_once('|') else { return };
    if !recv(scope, args.this(), iface) {
        return;
    }
    if let Some(v) = evt_slot_read(scope, args.this(), member) {
        rv.set(v);
    }
}

/// `item(index)` -- `this[index >>> 0] ?? null`, over the instance's own indexed properties. The
/// receiver check is [`wrap_recv`]'s, OUTSIDE the arity guard (see there).
fn cb_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let i = args.get(0).uint32_value(scope).unwrap_or(0);
    match args.this().get_index(scope, i).filter(|v| !v.is_undefined() && !v.is_null()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `namedItem(name)` -- `this[String(name)] ?? null`, over the instance's own named properties.
fn cb_named_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = args.get(0).to_rust_string_lossy(scope);
    let Some(k) = v8::String::new(scope, &name) else { return };
    match args.this().get(scope, k.into()).filter(|v| !v.is_undefined() && !v.is_null()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `PluginArray.refresh()` -- re-scans the plugin list. There is nothing to re-scan (the list is the
/// persona's and fixed for the life of the process), and Chrome's own answer for an unchanged list is
/// to do nothing and return `undefined`. Not a stub standing in for something unimplemented.
fn cb_refresh(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    recv(scope, args.this(), "PluginArray");
}

/// Mount one scalar row as a branded readonly slot getter.
/// The getter's data is ONE string, `"<interface>|<member>"`: a primitive, which a V8 template can hold
/// (it used to be a two-element Array).
fn slot_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, iface: &'static str) {
    use crate::binding::protos::build_fn_data;
    let d = str_val(scope, &format!("{iface}|{}", member.name));
    let g = build_fn_data(scope, cb_slot, &format!("get {}", member.name), 0, d);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}

/// The receiver check as an OUTER wrapper. `data` is `[interface, inner]`.
fn cb_recv_guard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let iface = d.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let Some(inner) = d.get_index(scope, 1).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    recv_guard_run(scope, &args, rv, &iface, inner);
}

/// [`cb_recv_guard`]'s body (the install's wrapper; a template fuses [`recv_guard_check`] instead).
pub(crate) fn recv_guard_run(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
    iface: &str,
    inner: v8::Local<v8::Function>,
) {
    if !recv_guard_check(scope, args, iface) {
        return;
    }
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let this: v8::Local<v8::Value> = args.this().into();
    // `inner` throwing returns None with the exception pending; leaving `rv` untouched propagates it.
    if let Some(r) = inner.call(scope, this, &argv) {
        rv.set(r);
    }
}

/// [`recv_guard_run`]'s check, shared with a fused member (`false`: thrown).
pub(crate) fn recv_guard_check(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str) -> bool {
    recv(scope, args.this(), iface)
}

/// Wrap `inner` in the receiver check, keeping the member's WebIDL `name` and `length` -- the wrapper
/// is what the page sees, so its identity has to be the member's.
fn wrap_recv<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, inner: v8::Local<'s, v8::Value>, name: &str, length: i32) -> v8::Local<'s, v8::Function> {
    let d = v8::Array::new(scope, 2);
    let i = str_val(scope, iface);
    d.set_index(scope, 0, i);
    d.set_index(scope, 1, inner);
    let g = crate::binding::protos::build_fn_data(scope, cb_recv_guard, name, length, d.into());
    if crate::install::templates::capturing() {
        if let Ok(f) = v8::Local::<v8::Function>::try_from(inner) {
            let guard = crate::install::templates::Guard::PluginRecv { iface: iface.to_string() };
            crate::install::templates::record_guard(scope, g, name, length, guard, f);
        }
    }
    g
}

/// Mount `item` / `namedItem`: the native body, then the WebIDL arity guard, then the receiver check
/// OUTSIDE it.
///
/// That order is Chrome's, probed rather than assumed -- headless Chrome 150.0.7871.46 on this
/// machine, over a loopback server (2026-09-11): `PluginArray.prototype.item.call({})` is
/// `Illegal invocation`, NOT "1 argument required", while `navigator.plugins.item()` -- a real
/// receiver, no argument -- is the arity TypeError. Building the check into the body instead put it
/// inside the guard and got the first case wrong.
fn collection_method_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, iface: &'static str) {
    use crate::binding::protos::{build_fn, make_arity_guard};
    // Each branch passes the fn ITEM, not a `fn` pointer: `MapFnTo` only accepts a zero-sized
    // callback type, and hoisting the two into one variable coerces them to a pointer (a const-eval
    // "size must be 0" failure, not a type error, which is why this reads more verbosely than it must).
    let f = if member.name == "item" {
        build_fn(scope, cb_item, member.name, member.length as i32)
    } else {
        build_fn(scope, cb_named_item, member.name, member.length as i32)
    };
    let guarded: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, iface, member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    let v = wrap_recv(scope, iface, guarded, member.name, member.length as i32);
    crate::install::kits::define_value(scope, proto, member, v.into(), true);
}

// ── the four row installers ──────────────────────────────────────────────────────────────────────

/// The body every row of [`PLUGINARRAY`] names.
pub fn install_plugin_array(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => slot_row(scope, proto, member, "PluginArray"),
        "item" | "namedItem" => collection_method_row(scope, proto, member, "PluginArray"),
        "refresh" => method_row(scope, proto, member, cb_refresh),
        other => debug_assert!(false, "install_plugin_array: unexpected member {other}"),
    }
}

/// The body every row of [`PLUGIN`] names.
pub fn install_plugin(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "name" | "filename" | "description" | "length" => slot_row(scope, proto, member, "Plugin"),
        "item" | "namedItem" => collection_method_row(scope, proto, member, "Plugin"),
        other => debug_assert!(false, "install_plugin: unexpected member {other}"),
    }
}

/// The body every row of [`MIMETYPEARRAY`] names.
pub fn install_mime_type_array(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => slot_row(scope, proto, member, "MimeTypeArray"),
        "item" | "namedItem" => collection_method_row(scope, proto, member, "MimeTypeArray"),
        other => debug_assert!(false, "install_mime_type_array: unexpected member {other}"),
    }
}

/// The body every row of [`MIMETYPE`] names.
pub fn install_mime_type(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "type" | "suffixes" | "description" | "enabledPlugin" => slot_row(scope, proto, member, "MimeType"),
        other => debug_assert!(false, "install_mime_type: unexpected member {other}"),
    }
}

// ── the singletons ───────────────────────────────────────────────────────────────────────────────

/// `(type, suffixes, description)` for the two PDF mime essences Chrome ships with its PDF viewer.
/// A structural constant of the Chromium family, not a persona value: every Chrome build reports
/// exactly these two, and the persona's plugin list is what varies.
const PDF_MIMES: [(&str, &str, &str); 2] = [
    ("application/pdf", "pdf", "Portable Document Format"),
    ("text/pdf", "pdf", "Portable Document Format"),
];

/// The persona's plugin list, or empty in a persona-less isolate (a bare test runtime).
fn persona_plugins(scope: &mut v8::PinScope) -> Vec<spec::PluginInfo> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::binding::protos::persona_handle(&st)
        .map(|p| p.0.client_hints().plugins)
        .unwrap_or_default()
}

/// Build the whole family and hand back `(pluginArray, mimeTypeArray)`.
///
/// The two arrays are cross-linked exactly as a browser's are: each `MimeType`'s `enabledPlugin` is
/// the FIRST plugin (the PDF viewer), and each `Plugin` carries both mime essences by index and by
/// name. `enabledPlugin` identity is the check `navigator.mimeTypes[0].enabledPlugin ===
/// navigator.plugins[0]` makes, so the same objects go into both.
fn build_family<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
    let (pa_proto, _) = parent_of(scope, "PluginArray")?;
    let (p_proto, _) = parent_of(scope, "Plugin")?;
    let (ma_proto, _) = parent_of(scope, "MimeTypeArray")?;
    let (m_proto, _) = parent_of(scope, "MimeType")?;

    // The mime types: own NOTHING, every member in a private slot behind the prototype's getters.
    let mimes: Vec<v8::Local<v8::Object>> = PDF_MIMES
        .iter()
        .map(|(ty, suf, desc)| {
            let o = mint(scope, m_proto);
            for (k, v) in [("type", *ty), ("suffixes", *suf), ("description", *desc)] {
                let sv = str_val(scope, v);
                evt_slot_write(scope, o, k, sv);
            }
            o
        })
        .collect();

    // The plugins: their mime types by index and by essence; scalars in slots.
    let mime_values: Vec<v8::Local<v8::Value>> = mimes.iter().map(|m| (*m).into()).collect();
    let mime_names: Vec<&str> = PDF_MIMES.iter().map(|(ty, _, _)| *ty).collect();
    let infos = persona_plugins(scope);
    let plugins: Vec<v8::Local<v8::Object>> = infos
        .iter()
        .map(|p| {
            let o = mint(scope, p_proto);
            adopt(scope, o, "Plugin", &mime_values, &mime_names, &mime_values);
            let n = v8::Number::new(scope, mimes.len() as f64);
            evt_slot_write(scope, o, "length", n.into());
            for (k, v) in [("name", &p.name), ("filename", &p.filename), ("description", &p.description)] {
                let sv = str_val(scope, v);
                evt_slot_write(scope, o, k, sv);
            }
            o
        })
        .collect();

    for m in &mimes {
        let ep: v8::Local<v8::Value> = match plugins.first() {
            Some(p) => (*p).into(),
            None => v8::null(scope).into(),
        };
        evt_slot_write(scope, *m, "enabledPlugin", ep);
    }

    // The plugin array: its plugins by index and by name, the first of a repeated name answering it.
    let plugin_array = mint(scope, pa_proto);
    let plugin_values: Vec<v8::Local<v8::Value>> = plugins.iter().map(|p| (*p).into()).collect();
    let mut plugin_names: Vec<&str> = Vec::new();
    let mut named_plugins: Vec<v8::Local<v8::Value>> = Vec::new();
    for (p, info) in plugins.iter().zip(infos.iter()) {
        if !info.name.is_empty() && !plugin_names.contains(&info.name.as_str()) {
            plugin_names.push(info.name.as_str());
            named_plugins.push((*p).into());
        }
    }
    adopt(scope, plugin_array, "PluginArray", &plugin_values, &plugin_names, &named_plugins);
    let n = v8::Number::new(scope, plugins.len() as f64);
    evt_slot_write(scope, plugin_array, "length", n.into());

    let mime_array = mint(scope, ma_proto);
    adopt(scope, mime_array, "MimeTypeArray", &mime_values, &mime_names, &mime_values);
    let n = v8::Number::new(scope, mimes.len() as f64);
    evt_slot_write(scope, mime_array, "length", n.into());

    Some((plugin_array, mime_array))
}

/// The plugin family this realm hands back, built ONCE and remembered in two v8 Privates on the
/// global (invisible to `getOwnPropertyNames`). `navigator.plugins` and `navigator.mimeTypes` are
/// separate table rows, read independently, and they must be CROSS-LINKED: `navigator.mimeTypes[0]
/// .enabledPlugin === navigator.plugins[0]` is a check pages make, so whichever row is read first
/// builds both halves. The deleted `nav-glue.js` memoised one `pluginBundle` closure for exactly
/// this reason.
pub fn navigator_plugin_family<'s>(scope: &mut v8::PinScope<'s, '_>, member: &str) -> v8::Local<'s, v8::Value> {
    let global = scope.get_current_context().global(scope);
    let slot = if member == "plugins" { "navPluginArray" } else { "navMimeTypeArray" };
    if let Some(v) = evt_slot_read(scope, global, slot) {
        return v;
    }
    let Some((plugin_array, mime_array)) = build_family(scope) else { return v8::null(scope).into() };
    evt_slot_write(scope, global, "navPluginArray", plugin_array.into());
    evt_slot_write(scope, global, "navMimeTypeArray", mime_array.into());
    if member == "plugins" { plugin_array.into() } else { mime_array.into() }
}

/// Build the four interfaces from their tables.
///
/// THE TWO `navigator` ACCESSORS ARE NOT WRITTEN HERE ANY MORE. This function used to end by
/// defining `plugins` and `mimeTypes` over the Navigator table's own rows -- the same re-point
/// `dom/09i-singletons.js` did, and the standing cost this lane's own ruling recorded ("when W4.c
/// lands, the Navigator table's two rows name `build_family` directly and this re-point goes away").
/// W4.a is where that happened: the two rows name [`navigator_plugin_family`], so each member has
/// exactly one body and nothing overwrites anything (invariants I1 / I2).
pub fn install_window_plugins<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::install::installer::Flavor;
    let mut ledger = installer::take_ledger(scope);
    for def in [&PLUGINARRAY, &PLUGIN, &MIMETYPEARRAY, &MIMETYPE] {
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] plugins install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
}

// ── the four tables ──────────────────────────────────────────────────────────────────────────────
//
// Rows VERBATIM from `install/generated_registry.rs` (window-only interfaces: the worker capture
// lists none of the four), in the capture's order, with the capture's kinds, lengths, `required`
// counts and descriptor flags. `ty` is `Any` on every row, as the registry has it.

webapi::interface! {
    "PluginArray" [interface, window] {
        ctor   "PluginArray"                        len 0 = stub,
        attr   "length"    : Any readonly                 = install_plugin_array,
        method "item"      : Any len 1 required 1 nullable = install_plugin_array,
        method "namedItem" : Any len 1 required 1 nullable = install_plugin_array,
        method "refresh"   : Any len 0                    = install_plugin_array,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "Plugin" [interface, window] {
        ctor   "Plugin"                             len 0 = stub,
        attr   "name"        : Any readonly               = install_plugin,
        attr   "filename"    : Any readonly               = install_plugin,
        attr   "description" : Any readonly               = install_plugin,
        attr   "length"      : Any readonly               = install_plugin,
        method "item"        : Any len 1 required 1 nullable = install_plugin,
        method "namedItem"   : Any len 1 required 1 nullable = install_plugin,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "MimeTypeArray" [interface, window] {
        ctor   "MimeTypeArray"                      len 0 = stub,
        attr   "length"    : Any readonly                 = install_mime_type_array,
        method "item"      : Any len 1 required 1 nullable = install_mime_type_array,
        method "namedItem" : Any len 1 required 1 nullable = install_mime_type_array,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "MimeType" [interface, window] {
        ctor   "MimeType"                           len 0 = stub,
        attr   "type"          : Any readonly             = install_mime_type,
        attr   "suffixes"      : Any readonly             = install_mime_type,
        attr   "description"   : Any readonly             = install_mime_type,
        attr   "enabledPlugin" : Any readonly             = install_mime_type
    }
}
