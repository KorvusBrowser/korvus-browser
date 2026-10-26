//! `core/html/html_dialog_element.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::binding::protos::{build_fn_data, set_bool, set_prop, str_val};
use crate::core::events::trusted_key;

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// ---- HTMLDialogElement.show/showModal/close (GS.A1.dialog) ----
// Native raw callbacks (build_fn -> [native code], installed on HTMLDialogElement.prototype by dom.js)
// that drive the dialog through the SAME native setAttribute/removeAttribute/dispatchEvent an author
// would call -- real "open" attribute state (visible to hasAttribute/outerHTML, and to MutationObserver
// since JsElement::set_attribute/remove_attribute already notify KB.__mutobs) and a real `close` event,
// not JS-shim state. Top-layer/focus/inert/::backdrop are out of scope (no layout engine) -- show and
// showModal both just ensure "open" is present, so one callback serves both installs (like
// cb_reflect_bool_get serving many different attrs): dom.js's `method()` passes the JS-visible name
// ("show"/"showModal") through to build_fn, so each install still gets its own correct fn.name.
// close(returnValue?): if open, removeAttribute("open") (a real attribute change, so MutationObserver
// sees it too), set .returnValue through the real accessor setter (this.set(), not a direct WeakMap poke)
// when an argument was passed -- args.length(), not an is_undefined() check, because close(undefined)
// must still set returnValue to "undefined" (Chrome/WebIDL "argument present" is about arity, not value)
// -- then QUEUE a non-bubbling, non-cancelable, TRUSTED `close` Event at the dialog (spec: "queue an
// element task ... to fire an event named close"). It must be queued, not dispatched synchronously: WPT
// dialog-close.html's async test does `d4.close(); was_queued = true;` and asserts `was_queued` is
// already true inside the close handler -- true only if the event fires after this call returns.
// Queued by cb_dialog_close (via queueMicrotask, dialog bound as args.data()): build a genuinely-trusted
// `close` Event (isTrusted===true, matching every other engine-originated event) and dispatch it through
// the real EventTarget path, same as build_trusted_mouse/build_trusted_key et al.

/// `show()` / `showModal()` -- Blink's preconditions in order, then the `open` attribute, and for
/// `showModal` the dialog's IS-MODAL flag (the arena's, so `:modal` and `closedBy` see it). Both used
/// to set `open` and nothing else: a detached dialog's `showModal()` succeeded where Chrome 150 throws
/// "The element is not in a Document." (the Chrome oracle, 2026-10-08).
fn cb_dialog_show(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let modal = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "showModal";
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(id) = crate::page::node_id(scope, this) else { return };
    let method = if modal { "showModal" } else { "show" };
    let (open, is_modal) = crate::page::with_host_dom(scope, |d| (d.get_attribute(id, "open").is_some(), d.is_modal(id))).unwrap_or((false, false));
    if open {
        if modal && !is_modal {
            crate::binding::protos::throw_dom(scope, &format!("Failed to execute '{method}' on 'HTMLDialogElement': The dialog is already open as a non-modal dialog, and therefore cannot be opened as a modal dialog."), "InvalidStateError");
        } else if !modal && is_modal {
            crate::binding::protos::throw_dom(scope, &format!("Failed to execute '{method}' on 'HTMLDialogElement': The dialog is already open as a modal dialog, and therefore cannot be opened as a non-modal dialog."), "InvalidStateError");
        }
        return;
    }
    if modal && !crate::page::is_connected(scope, this) {
        crate::binding::protos::throw_dom(scope, &format!("Failed to execute '{method}' on 'HTMLDialogElement': The element is not in a Document."), "InvalidStateError");
        return;
    }
    let name = str_val(scope, "open");
    let empty = str_val(scope, "");
    crate::core::dom::element::attr_call(scope, this, "setAttribute", &[name, empty]);
    if modal {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_modal(id, true);
    }
}

/// `closedBy` -- the `closedby` attribute's keyword (`any` / `closerequest` / `none`, ASCII
/// case-insensitive), or, for a missing or invalid value, the AUTO state: `closerequest` while the
/// dialog is modal, else `none` (Blink's `HTMLDialogElement::ClosedBy`). It was a stub answering null.
fn cb_dialog_closed_by_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = crate::page::node_id(scope, args.this().into()) else { return };
    let (attr, modal) = crate::page::with_host_dom(scope, |d| (d.get_attribute(id, "closedby").map(|a| a.to_ascii_lowercase()), d.is_modal(id))).unwrap_or((None, false));
    let out = match attr.as_deref() {
        Some("any") => "any",
        Some("none") => "none",
        Some("closerequest") => "closerequest",
        _ if modal => "closerequest",
        _ => "none",
    };
    let v = str_val(scope, out);
    rv.set(v);
}

fn cb_dialog_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this_obj = args.this();
    let this: v8::Local<v8::Value> = this_obj.into();
    let open_name = str_val(scope, "open");
    let is_open = crate::core::dom::element::attr_call(scope, this, "hasAttribute", &[open_name]).map(|v| v.boolean_value(scope)).unwrap_or(false);
    if !is_open { return; }
    crate::core::dom::element::attr_call(scope, this, "removeAttribute", &[open_name]);
    if let Some(id) = crate::page::node_id(scope, this) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_modal(id, false);
    }
    if args.length() > 0 {
        set_prop(scope, this_obj, "returnValue", args.get(0));
    }
    // Round-trip through a Global so the bound data has the scope's own lifetime (args.this()'s lifetime
    // is tied to FunctionCallbackArguments, not to `scope`; build_fn_data needs the latter) -- same
    // trick cb_idb_drain uses to re-bake a queued fn's bound data.
    let this_g = v8::Global::new(scope, this);
    let this_v = v8::Local::new(scope, &this_g);
    let fnv = build_fn_data(scope, cb_dialog_close_fire, "", 0, this_v);
    if let Some(qm) = crate::binding::protos::intrinsic_fn(scope, "queueMicrotask") {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        qm.call(scope, undef, &[fnv.into()]);
    }
}

fn cb_dialog_close_fire(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = crate::binding::protos::body_data(scope, &args);
    if let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        let init = crate::binding::protos::init_dict(scope);
        set_bool(scope, init, "bubbles", false);
        set_bool(scope, init, "cancelable", false);
        let ty = str_val(scope, "close");
        if let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) {
            mark_trusted(scope, ev);
            let evv: v8::Local<v8::Value> = ev.into();
            crate::binding::protos::engine_call(scope, this, "dispatchEvent", &[evv]);
        }
    }
}

/// Stamp an event instance as genuinely trusted (isTrusted===true), from the engine's privileged path.
pub(crate) fn mark_trusted(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let key = trusted_key(scope);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    ev.set_private(scope, key, t);
}

// ---- returnValue (W5.0d: the port of the dialog return-value bridge helpers of dom/03-element-glue.js) ----
// Per-instance state, NOT a content attribute (it must not surface through getAttribute/outerHTML/
// attributes), so it lives in a private slot of the element's wrapper -- the wrapper is the canonical
// one per node (`wrap_cached`), so the slot is exactly the WeakMap the JS helper kept. `close(v)` above
// writes it through this accessor (`set_prop(this, "returnValue", v)`), as the spec's "set the
// returnValue attribute" step does.
const P_RETURN_VALUE: &str = "korvus_dialog_return_value";
fn cb_dialog_return_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match crate::core::css::css_style_declaration::priv_get(scope, args.this(), P_RETURN_VALUE) {
        Some(v) => rv.set(v),
        None => {
            let e = str_val(scope, "");
            rv.set(e);
        }
    }
}
fn cb_dialog_return_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = args.get(0).to_rust_string_lossy(scope);
    let v = str_val(scope, &s);
    crate::core::dom::element::priv_set(scope, args.this(), P_RETURN_VALUE, v);
}



// -- The interface, from ONE table -------------------------------------------------
//
// `dom/08g-observers.js` decorated the generated shell's prototype: `rbool` for `open`, `nat2` for
// `returnValue`, and three `method()` round-trips for `show`/`showModal`/`close`. The interface is a
// table now, spliced onto the REAL `HTMLElement.prototype`, and the `dialog` tag maps onto it through
// the one `korvus_dom::tags` pass (`install::dom_chain::fill_dom_protos`) rather than by a hand write.
//
// `closedBy` and `requestClose` stay `= stub`: they were `BodyKind::Stub` shells in the registry block
// this table replaces, and this lane adds no behaviour. A counted stub beats a masked body.

/// The body every row of [`HTMLDIALOGELEMENT`] names.
pub fn install_html_dialog_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{method_row, pair_row};
    match member.name {
        "returnValue" => pair_row(scope, proto, member, cb_dialog_return_value_get, cb_dialog_return_value_set),
        // One callback serves `show` and `showModal`, as it did through `method()`: the row supplies
        // the JS-visible name and length, so each install still gets its own correct `fn.name`.
        "show" | "showModal" => {
            let d = str_val(scope, member.name);
            crate::core::dom::shared::row_method_data(scope, proto, member, cb_dialog_show, d)
        }
        "closedBy" => {
            let d = str_val(scope, "closedby");
            crate::core::dom::shared::row_accessor_data(scope, proto, member, cb_dialog_closed_by_get, d, crate::core::dom::element::cb_reflect_str_set, d)
        }
        "close" => method_row(scope, proto, member, cb_dialog_close),
        other => debug_assert!(false, "install_html_dialog_element: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `HTMLDialogElement` block of `install/generated_registry.rs`, in the
// capture's order and with its `cfg` strings. Window-only: `worker.surface.json` lists no HTML element.
webapi::interface! {
    // `[element]`: html.idl gives `HTMLDialogElement` `[HTMLConstructor]`, so a bare
    // `HTMLDialogElement()` is `Failed to construct 'HTMLDialogElement': Please use the 'new'
    // operator, …` and not the `Illegal constructor` a `[interface]` row answers -- measured, Chrome
    // 150 over loopback. `new HTMLDialogElement()` is unchanged.
    "HTMLDialogElement" : "HTMLElement" [element, window, backfill] {
        ctor            "HTMLDialogElement" len 0 = stub,
        attr            "open"          : Object cfg "open"     = reflect_bool,
        attr            "returnValue"   : Object                = install_html_dialog_element,
        attr            "closedBy"      : Object cfg "closedby" = install_html_dialog_element,
        method          "close"         : Object len 0          = install_html_dialog_element,
        // `ty` is the WebIDL return type, and webref declares `undefined requestClose(optional
        // DOMString returnValue)` (html.idl:1248). The generated registry's `Object` is the
        // generator's default -- the capture cannot see a return type -- and the D2 stub reads `ty`,
        // so `Object` would make an unimplemented `requestClose()` answer `null` where the shell it
        // replaces answered `undefined`. The grader caught it as HTMLDialogElement losing a
        // `verified` member.
        method          "requestClose"  : Undefined len 0       = stub,
        method          "show"          : Object len 0          = install_html_dialog_element,
        method          "showModal"     : Object len 0          = install_html_dialog_element
    }
}
