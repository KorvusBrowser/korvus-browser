//! `core/html/html_body_element.rs` — `HTMLBodyElement` as one `interface!` table, and the
//! window-forwarding event handlers that are most of it.
//!
//! `js/dom/08q-body-element.js` (165 lines, two `Object.defineProperty` sites, 48 `op_nativize`
//! native masks, one `KB.__` body) is deleted. `<body>` has one member of its own and
//! twenty-nine the capture records; the interesting half is the handlers.
//!
//! THE FORWARDING, AND WHAT CHROME ACTUALLY DOES. A `<body>`'s `on<type>` properties are not its own
//! event handlers: the spec has them FORWARD to the Window, which is why Chrome puts
//! `onblur`/`onerror`/`onfocus`/`onload`/`onresize`/`onscroll` on THIS prototype even though
//! `GlobalEventHandlers` already defines them on `HTMLElement` — the body's own copies shadow the
//! inherited ones. Measured in headless Chrome 151 rather than taken from the spec, because the spec
//! does not say which names, and the answer is not the whole list:
//!
//!   * `document.body.onload = f` makes `window.onload === f`, and `window.onload = g` makes
//!     `document.body.onload === g`. One slot, both directions, for TWENTY-TWO names — the six
//!     `GlobalEventHandlers` the body shadows and the sixteen `WindowEventHandlers`.
//!   * `ongamepadconnected` and `ongamepaddisconnected` are NOT forwarded, in EITHER direction.
//!     `document.body.ongamepadconnected = h` reads back `h` on the body and leaves
//!     `window.ongamepadconnected` null, and setting the window's does not change the body's. They
//!     are on this prototype because the Gamepad API adds them to `WindowEventHandlers`, but Blink
//!     does not mark them `[WindowEventHandler]`, so they are ordinary element-local handlers. The
//!     deleted JS forwarded all twenty-four; those two rows are the shared `event_handler` kit now,
//!     which is the element's own handler store.
//!   * A NON-CALLABLE clears: `window.onload = f; document.body.onload = 42` leaves
//!     `window.onload === null`.
//!   * The getter and the setter BRAND-CHECK. `Object.getOwnPropertyDescriptor(HTMLBodyElement
//!     .prototype, "onload").get.call({})` throws `TypeError: Illegal invocation`, and so does
//!     calling it on a `<div>` — which the deleted JS answered for, because its `winFor` looked only
//!     at `ownerDocument`. Every row here checks first.
//!   * `<body onload="init()">` in MARKUP puts the compiled function on `window.onload`
//!     (`typeof window.onload === "function"` with no script having assigned one, and it is the same
//!     object `document.body.onload` reads back).
//!
//! THE FORWARDING TARGET is the element's node document's Window, which here means: only when
//! `ownerDocument` is the ACTIVE document. Measured: a body from `new DOMParser().parseFromString(…)`
//! reads `null` and swallows an assignment — it neither forwards nor stores — because it has no
//! browsing context; `document.createElement("body")` DOES forward while unattached, because its node
//! document is the active one.
//!
//! WHAT THIS DELIBERATELY DOES NOT DO: put `body` on the load event's dispatch path. That would also
//! make `<body onload>` run, and it would be wrong in three observable ways at once — `event.target`
//! and `event.currentTarget` would be the body rather than the Window, and the handler would take a
//! position among the window's listeners that it does not have. The spec's mechanism is handler
//! forwarding into the SAME slot `window.onload` uses, which is also why a later `window.onload = g`
//! correctly REPLACES a markup `<body onload>` instead of running alongside it.
//!
//! Three handlers the WebIDL declares are deliberately ABSENT: `onorientationchange`, `onpagereveal`
//! and `onpageswap`. All three are recorded as `undefined` in the bundle, so installing them would be
//! a tell, not a feature — and the rows below are the bundle's list, so they are absent by being
//! absent from it.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, global_get, str_val, throw_type_error};
use crate::core::dom::element::el_id_of;
use crate::install::kits::define_accessor;
use crate::install::MemberDef;

/// The twenty-two `on<type>` names a `<body>` forwards to the Window: the six `GlobalEventHandlers`
/// it shadows, then `WindowEventHandlers`. NOT the two the Gamepad API adds — see the header.
const FORWARDED: [&str; 22] = [
    "blur", "error", "focus", "load", "resize", "scroll",
    "afterprint", "beforeprint", "beforeunload", "hashchange", "languagechange",
    "message", "messageerror", "offline", "online", "pagehide", "pageshow",
    "popstate", "rejectionhandled", "storage", "unhandledrejection", "unload",
];

fn is_forwarded(ty: &str) -> bool {
    FORWARDED.contains(&ty)
}

/// `true` when the receiver is an `HTMLBodyElement`; otherwise Chrome's `Illegal invocation`
/// TypeError (measured for `{}` AND for a `<div>`).
fn this_body(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    let this = args.this();
    let ok = crate::page::tag_name_of(scope, this.into()).is_some_and(|t| t.eq_ignore_ascii_case("body"))
        && el_id_of(scope, this).is_some();
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// The Window this element's handlers forward to, or `None` when there is none: the node document has
/// to be the ACTIVE document, which `document.createElement("body")` satisfies and a `DOMParser` or
/// frame document does not.
///
/// The node document read off the tree and the Window's handler slot read and written directly, as Blink's
/// forwarding does -- not through the page's `ownerDocument` getter and `window.on<type>` accessor
/// (`body.onload = f` called both).
fn win_for<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let id = el_id_of(scope, el)?;
    let os = deno_core::JsRuntime::op_state_from(scope);
    let doc = global_get(scope, "document")?;
    let owner = crate::core::dom::node::node_document(scope, &os, id).unwrap_or(doc);
    if owner != doc {
        return None;
    }
    let ctx = scope.get_current_context();
    Some(ctx.global(scope))
}

/// `get on<type>` -- the Window's slot, never the element's. `data` is the bare event type.
fn cb_forward_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_body(scope, &args) {
        return;
    }
    let ty = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(w) = win_for(scope, args.this()) else { rv.set_null(); return };
    let v = crate::core::events::event_target::et_on_get(scope, w.into(), &ty);
    if v.is_function() { rv.set(v) } else { rv.set_null() }
}

/// `set on<type>` -- writes the Window's slot. Anything not callable CLEARS the handler, which is what
/// the event-handler IDL attribute setter does for a non-Function value (measured: `body.onload = 42`
/// leaves `window.onload` null).
fn cb_forward_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !this_body(scope, &args) {
        return;
    }
    let ty = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(w) = win_for(scope, args.this()) else { return };
    let v = args.get(0);
    let value: v8::Local<v8::Value> = if v.is_function() { v } else { v8::null(scope).into() };
    crate::core::events::event_target::et_on_set(scope, w.into(), &ty, value);
}

/// One forwarding accessor pair, shaped exactly like the storage pair it replaces: `get on<type>` /
/// `set on<type>`, lengths 0 and 1, enumerable and configurable.
pub fn install_html_body_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let ty = member.name.strip_prefix("on").unwrap_or(member.name);
    debug_assert!(is_forwarded(ty), "install_html_body_element: {} is not a forwarded handler", member.name);
    let d = str_val(scope, ty);
    let g = build_fn_data(scope, cb_forward_get, &format!("get {}", member.name), 0, d);
    let d = str_val(scope, ty);
    let s = build_fn_data(scope, cb_forward_set, &format!("set {}", member.name), 1, d);
    define_accessor(scope, proto, member, g, Some(s));
}

// ── the event handler CONTENT attributes: `<body onload="init()">` ──────────────────────────────
//
// The IDL attributes above only forward what a SCRIPT assigns. A handler written in markup has to be
// compiled and assigned too, and nothing reads `body.onload` to trigger a lazy compile -- the whole
// point is that it runs without anyone asking for it. So this is driven, not polled: from the native
// attribute choke point for a `setAttribute`/reflected write (`core/dom/shared.rs` ->
// `core/html/subresource.rs::resource_attr`), and from the runtime's `ACTIVATE_BODY_HANDLERS` driver
// for the attributes the PARSER wrote, which go through no JS seam at all. The parser pass runs
// before the page's own scripts, which is both when a browser reaches the body start tag and the only
// order under which a later `window.onload = f` replaces the markup handler rather than the reverse.

/// `onerror` on a Window is the one handler with a different shape -- HTML's `OnErrorEventHandler`
/// takes `(event, source, lineno, colno, error)` rather than the event alone -- and its content
/// attribute is compiled with that parameter list, matching what the window's listener passes.
const ERROR_ARGS: [&str; 5] = ["event", "source", "lineno", "colno", "error"];

/// The attribute's handler, compiled as every content attribute's is
/// ([`crate::core::events::event_target::compile_content_handler`]: the body's own function, scoped by the body
/// element and its document), named after its attribute, or `None` on a syntax error: a content attribute that does
/// not parse is NO handler at all -- never a throw out of an attribute write.
fn compile<'s>(scope: &mut v8::PinScope<'s, '_>, body: Option<u32>, ty: &str, code: &str) -> Option<v8::Local<'s, v8::Value>> {
    // a sandboxed frame document's `<body onload>` compiles nothing ([`event_target::compile_handler`]'s rule)
    if crate::core::html::browsing_context::realm_scripts_sandboxed(scope) {
        return None;
    }
    let params: &[&str] = if ty == "error" { &ERROR_ARGS } else { &["event"] };
    let realm = scope.get_current_context();
    crate::core::events::event_target::compile_content_handler(scope, realm, body, ty, params, code)
}

/// One `on<type>` CONTENT attribute of a `<body>`, compiled and forwarded to the Window. A no-op for
/// every other element, for a name that is not a forwarded handler, and for a body with no Window.
///
/// Called from Rust, by both of its callers: `core/html/subresource.rs::resource_attr` in-crate for a
/// `setAttribute`/reflected write, and `korvus-runtime/src/navigation.rs::activate_body_handlers`
/// through `webapi::page::body_handler_attr` for the attributes the parser wrote. It reached the
/// second of those through a bridge NAME; the name is deleted and the callers are
/// unchanged, because they already called this function.
pub(crate) fn body_handler_attr(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, name: &str) {
    let Ok(el) = v8::Local::<v8::Object>::try_from(el) else { return };
    let is_body = crate::page::tag_name_of(scope, el.into()).is_some_and(|t| t == "BODY");
    if !is_body {
        return;
    }
    let Some(ty) = name.strip_prefix("on").filter(|t| is_forwarded(t)) else { return };
    let ty = ty.to_string();
    let Some(w) = win_for(scope, el) else { return };
    let a = str_val(scope, name);
    let code = crate::core::dom::element::attr_call(scope, el.into(), "getAttribute", &[a]).filter(|v| !v.is_null_or_undefined());
    // The attribute being gone is a removal, and a removal clears the handler.
    let Some(code) = code else {
        let null: v8::Local<v8::Value> = v8::null(scope).into();
        crate::core::events::event_target::et_on_set(scope, w.into(), &ty, null);
        return;
    };
    let code = code.to_rust_string_lossy(scope);
    let body = crate::binding::protos::unwrap_id(scope, el.into());
    let Some(f) = compile(scope, body, &ty, &code) else { return };
    crate::core::events::event_target::et_on_set(scope, w.into(), &ty, f);
}

webapi::interface! {
    // Rows VERBATIM from the `HTMLBodyElement` block of `install/generated_registry.rs`, which is the
    // capture's own prototype order: the six presentation attributes, then the six
    // `GlobalEventHandlers` the body shadows, then `WindowEventHandlers`, then the two the Gamepad
    // API adds to that mixin.
    //
    // The six colour/background reflections are the presentational attributes HTML kept for
    // compatibility. They are PLAIN STRING reflections -- the browser does NOT normalise them, which
    // Chrome 151 confirms: `body.bgColor = "x"` reads back "x" and writes `bgcolor="x"`, not a parsed
    // colour, and `background = "b.png"` is not resolved against the base URL. The IDL names are
    // camelCase and the content attributes are all-lowercase.
    "HTMLBodyElement" : "HTMLElement" [element, window] {
        ctor    "HTMLBodyElement"                        len 0     = stub,
        attr    "text"                 : DomString cfg "text"      = reflect_str,
        attr    "link"                 : DomString cfg "link"      = reflect_str,
        attr    "vLink"                : DomString cfg "vlink"     = reflect_str,
        attr    "aLink"                : DomString cfg "alink"     = reflect_str,
        attr    "bgColor"              : DomString cfg "bgcolor"   = reflect_str,
        attr    "background"           : DomString cfg "background" = reflect_str,
        handler "onblur"                                           = install_html_body_element,
        handler "onerror"                                          = install_html_body_element,
        handler "onfocus"                                          = install_html_body_element,
        handler "onload"                                           = install_html_body_element,
        handler "onresize"                                         = install_html_body_element,
        handler "onscroll"                                         = install_html_body_element,
        handler "onafterprint"                                     = install_html_body_element,
        handler "onbeforeprint"                                    = install_html_body_element,
        handler "onbeforeunload"                                   = install_html_body_element,
        handler "onhashchange"                                     = install_html_body_element,
        handler "onlanguagechange"                                 = install_html_body_element,
        handler "onmessage"                                        = install_html_body_element,
        handler "onmessageerror"                                   = install_html_body_element,
        handler "onoffline"                                        = install_html_body_element,
        handler "ononline"                                         = install_html_body_element,
        handler "onpagehide"                                       = install_html_body_element,
        handler "onpageshow"                                       = install_html_body_element,
        handler "onpopstate"                                       = install_html_body_element,
        handler "onrejectionhandled"                               = install_html_body_element,
        handler "onstorage"                                        = install_html_body_element,
        handler "onunhandledrejection"                             = install_html_body_element,
        handler "onunload"                                         = install_html_body_element,
        // NOT forwarded -- Chrome keeps these two on the element's own handler store (see the
        // header), so they are the shared kit over the native EventTarget store.
        handler "ongamepadconnected"                               = event_handler,
        handler "ongamepaddisconnected"                            = event_handler
    }
}
