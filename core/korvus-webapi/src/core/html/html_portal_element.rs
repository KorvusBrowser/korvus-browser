//! `core/html/html_portal_element.rs` — `HTMLPortalElement` as one `interface!` table.
//!
//! `dom/08j-portal.js` (48 lines, four `Object.defineProperty` sites, six op-builder round trips) is
//! gone. The table below is the interface's only definition site; the five `op_portal_*` builders the
//! JS called went with the file, because a row names its callback directly.
//!
//! A beyond-Brave corpus interface: the target browser does not ship portals, so there is no surface
//! baseline to mirror — every expectation below is derived from the SPECS:
//!   - Portals (WICG): `src` "must reflect the src content attribute"; `referrerPolicy` "must reflect
//!     the referrerpolicy content attribute, limited to only known values"; `activate`/`postMessage`
//!     both start with "Let portalBrowsingContext be the guest browsing context of this. If
//!     portalBrowsingContext is null, throw an InvalidStateError DOMException". Korvus has no nested
//!     portal browsing contexts, so that throw is the eternal answer here — and it is the spec-correct
//!     one for any portal element without a guest (never inserted, or `src` unset).
//!   - HTML: the `[HTMLConstructor]` constructor throws TypeError "Illegal constructor" when invoked
//!     outside a custom-element upgrade (same as every HTML*Element interface) — which is exactly what
//!     `installer::create_interface` builds for a `Flavor::Table` + `Model::Element` row set, so the
//!     table gets it without naming a constructor body; `referrerpolicy` is an enumerated attribute —
//!     the getter returns the keyword (ASCII case-insensitive match, canonical lowercase) or "" when
//!     absent/unknown, the setter stores the string as given, which is the shared `reflect_enum` kit
//!     with both defaults empty; event-handler IDL attributes (`onmessage`/`onmessageerror`) are the
//!     shared `event_handler` kit over the native EventTarget handler store, so a dispatched
//!     `MessageEvent` at the element still invokes `onmessage`.
//!   - WebIDL: `postMessage(any message, ...)` has one REQUIRED argument, so a zero-arg call is a
//!     TypeError before the body runs — the row says `required 1` and [`super::super::super::install::live::method_row`]
//!     mounts the WebIDL arity guard from it, which is why the body no longer calls `require_args`
//!     itself; `activate(optional ...)` is arity-0.
//!
//! NOT INSTALLED IN THIS BUNDLE, deliberately, and this is the union-table model working as intended
//! (docs/ONE-TABLE-ARCHITECTURE.md D3/D4). The captured global list has no `HTMLPortalElement` — WICG
//! Portals never shipped and Chrome removed the flag — so publishing one would be an extra global no
//! Brave has, a structural diff and a worse fingerprint than the absence it would fill. Before this
//! lane that decision was expressed by leaving the `.js` file out of `dom.js`'s import list; it is
//! expressed now by no `install/live/*.rs` naming [`HTMLPORTALELEMENT`]. The TABLE is the tree's
//! knowledge of the interface; the BUNDLE decides which of it this build exposes.
//!
//! RECEIVER CHECKS: none are added here, and none were dropped. `src` and `referrerPolicy` are the
//! shared reflect kits, whose bodies call `getAttribute`/`setAttribute` on the receiver and so answer
//! for any object that has them; `activate` and `postMessage` throw `InvalidStateError` for every
//! receiver alike, which is what they did as JS-mounted shells. This is the tree-wide method-receiver
//! gap (goal §W9 item 17), not a change of this lane's making.

use deno_core::v8;

use crate::binding::protos::throw_dom;
use crate::install::live::method_row;
use crate::install::MemberDef;

/// `activate(optional PortalActivateOptions)` — the spec's guest-browsing-context check comes FIRST
/// (before serializing options), so every call throws InvalidStateError in an engine without portal
/// guests, whatever arguments were passed.
fn cb_portal_activate(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    throw_dom(scope, "The HTMLPortalElement does not have a guest browsing context.", "InvalidStateError");
}

/// `postMessage(any message, optional StructuredSerializeOptions)` — `message` is a REQUIRED
/// argument, so a zero-arg call is an arity TypeError raised by the row's own guard before this body
/// runs; with it present, the same guest check as `activate` throws InvalidStateError (before any
/// serialization).
fn cb_portal_post_message(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    throw_dom(scope, "The HTMLPortalElement does not have a guest browsing context.", "InvalidStateError");
}

pub fn install_html_portal_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "activate" => method_row(scope, proto, member, cb_portal_activate),
        "postMessage" => method_row(scope, proto, member, cb_portal_post_message),
        other => debug_assert!(false, "install_html_portal_element: unexpected member {other}"),
    }
}

webapi::interface! {
    // WebIDL source order (gen/webref/portals.idl): src, referrerPolicy, activate, postMessage,
    // onmessage, onmessageerror. Enumeration order is itself a fingerprint signal, so it is the
    // table's order and not a convenience order.
    "HTMLPortalElement" : "HTMLElement" [element, window] {
        ctor    "HTMLPortalElement"                 len 0           = stub,
        // [CEReactions] attribute USVString src — reflects the src content attribute verbatim (no URL
        // resolution: the deleted JS used op_reflect_str_get, not op_reflect_url_get).
        attr    "src"            : UsvString  cfg "src"             = reflect_str,
        // [CEReactions] attribute DOMString referrerPolicy — enumerated, limited to known values, with
        // "" for both the missing- and the invalid-value default (Referrer Policy §"referrer policy
        // attribute"). The keyword list IS the canonical lowercase spelling the getter returns.
        attr    "referrerPolicy" : DomString
            cfg "referrerpolicy|no-referrer,no-referrer-when-downgrade,origin,origin-when-cross-origin,same-origin,strict-origin,strict-origin-when-cross-origin,unsafe-url||"
                                                                    = reflect_enum,
        // [NewObject] Promise<undefined> activate(optional PortalActivateOptions options = {}).
        method  "activate"       : Promise    len 0                 = install_html_portal_element,
        // undefined postMessage(any message, optional StructuredSerializeOptions options = {}).
        method  "postMessage"    : Undefined  len 1 required 1      = install_html_portal_element,
        handler "onmessage"                                         = event_handler,
        handler "onmessageerror"                                    = event_handler
    }
}
