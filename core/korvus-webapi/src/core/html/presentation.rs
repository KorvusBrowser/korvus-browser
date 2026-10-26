//! `core/html/presentation.rs` -- the Presentation API's page-facing pair, `PresentationRequest` and
//! `PresentationAvailability`, as `interface!` tables.
//!
//! WHY THEY ARE NOT STUBS. Google's Cast sender SDK (`cast_sender.js`, loaded by every page with a
//! Cast button -- pocketcasts.com in the replay set) builds a `PresentationRequest` and writes
//! `availability.onchange = ...` on whatever `getAvailability()` resolves. The registry's D2 stubs
//! resolved all three promise methods with `undefined`, so that write threw a TypeError inside the
//! SDK's promise chain and two more rejections went unhandled.
//!
//! WHAT A BROWSER WITH NO CAST DEVICE ANSWERS, measured on `tools/realweb/seed/pr` in Chrome 150
//! (headless and headed) and Brave 151, which agree:
//!
//! ```text
//!   start()            rejects InvalidAccessError "Failed to execute 'start' on 'PresentationRequest':
//!                      PresentationRequest::start() requires user gesture."  -- at once
//!   getAvailability()  resolves a PresentationAvailability, value false      -- a task later
//!   reconnect("abc")   rejects NotFoundError "Route not found"               -- a task after that
//! ```
//!
//! The order is part of the answer: `start()` fails before the call returns, while the other two
//! settle after a round trip to the browser process, in the order they were asked. They settle here
//! on queued tasks, one per call, for the same reason.

use deno_core::v8;

use crate::binding::protos::{
    brand_shell_instance, make_dom_exc, rejected_promise, require_ctor_new_target, shell_proto, table_receiver_ok,
    throw_dom, throw_type_error,
};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;

/// The request's URLs, as the constructor received them.
const PR_URLS: &str = "korvus.presentation.urls";
/// The request's one `PresentationAvailability` (HTML's "presentation display availability").
const PR_AVAIL: &str = "korvus.presentation.availability";

/// `new PresentationRequest(urls)`: `(USVString or sequence<USVString>) urls`. An empty list is
/// refused (Chrome: NotSupportedError "Do not support empty sequence of URLs.").
pub(crate) fn cb_presentation_request_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !require_ctor_new_target(scope, &args, "PresentationRequest") {
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'PresentationRequest': 1 argument required, but only 0 present.");
        return;
    }
    let a = args.get(0);
    let urls: Vec<v8::Local<v8::Value>> = match v8::Local::<v8::Array>::try_from(a) {
        Ok(arr) => (0..arr.length()).filter_map(|i| arr.get_index(scope, i)).collect(),
        Err(_) => vec![a],
    };
    if urls.is_empty() {
        throw_dom(scope, "Failed to construct 'PresentationRequest': Do not support empty sequence of URLs.", "NotSupportedError");
        return;
    }
    let list = v8::Array::new(scope, urls.len() as i32);
    for (i, u) in urls.into_iter().enumerate() {
        let Some(s) = u.to_string(scope) else { return }; // a throwing toString propagates
        list.set_index(scope, i as u32, s.into());
    }
    let this = args.this();
    let Some(proto) = shell_proto(scope, "PresentationRequest") else { return };
    this.set_prototype(scope, proto.into());
    brand_shell_instance(scope, this);
    priv_set(scope, this, PR_URLS, list.into());
}

/// The receiver as a `PresentationRequest`, or a rejected promise (WebIDL: a promise-returning
/// operation reports a bad receiver by rejecting, not by throwing).
fn request_recv<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>, rv: &mut v8::ReturnValue) -> bool {
    let ok = shell_proto(scope, "PresentationRequest").is_some_and(|p| table_receiver_ok(scope, this.into(), p.into()));
    if !ok {
        let m = v8::String::new(scope, "Illegal invocation").unwrap();
        let e = v8::Exception::type_error(scope, m);
        let p = rejected_promise(scope, e);
        rv.set(p);
    }
    ok
}

#[derive(Default)]
struct PendingSettles {
    next: u64,
    open: std::collections::HashMap<u64, (v8::Global<v8::PromiseResolver>, v8::Global<v8::Value>, bool)>,
}

/// A promise that settles -- fulfilled with `value`, or rejected with it -- on a task queued now.
fn settle_on_task<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<v8::Value>, fulfil: bool) -> v8::Local<'s, v8::Value> {
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise: v8::Local<v8::Value> = resolver.get_promise(scope).into();
    let held = (v8::Global::new(scope, resolver), v8::Global::new(scope, value), fulfil);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<PendingSettles>().is_none() {
            st.put(PendingSettles::default());
        }
        let p = st.borrow_mut::<PendingSettles>();
        p.next += 1;
        p.open.insert(p.next, held);
        p.next
    };
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_settle, id, 0.0);
    promise
}

fn cb_settle(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u64) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let pending = op_state.borrow_mut().try_borrow_mut::<PendingSettles>().and_then(|p| p.open.remove(&id));
    let Some((resolver, value, fulfil)) = pending else { return };
    let resolver = v8::Local::new(scope, &resolver);
    let value = v8::Local::new(scope, &value);
    if fulfil {
        resolver.resolve(scope, value);
    } else {
        resolver.reject(scope, value);
    }
}

/// `start()`: with no user gesture, rejected before the call returns (see the module note).
fn cb_pr_start<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !request_recv(scope, args.this(), &mut rv) {
        return;
    }
    let msg = "Failed to execute 'start' on 'PresentationRequest': PresentationRequest::start() requires user gesture.";
    let Some(e) = make_dom_exc(scope, msg, "InvalidAccessError") else { return };
    let p = rejected_promise(scope, e);
    rv.set(p);
}

/// `reconnect(presentationId)`: there is no presentation to reconnect to, so NotFoundError, a task later.
fn cb_pr_reconnect<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !request_recv(scope, args.this(), &mut rv) {
        return;
    }
    if args.get(0).to_string(scope).is_none() {
        return; // `DOMString presentationId`: a throwing toString propagates
    }
    let Some(e) = make_dom_exc(scope, "Route not found", "NotFoundError") else { return };
    let p = settle_on_task(scope, e, false);
    rv.set(p);
}

/// `getAvailability()`: the request's one `PresentationAvailability` (`value` false: no display),
/// a task later. Every call answers the same object.
fn cb_pr_get_availability<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !request_recv(scope, this, &mut rv) {
        return;
    }
    let avail = match priv_get(scope, this, PR_AVAIL).filter(|v| v.is_object()) {
        Some(a) => a,
        None => {
            let Some(proto) = shell_proto(scope, "PresentationAvailability") else { return };
            let o = crate::binding::protos::mint_on(scope, proto.into());
            brand_shell_instance(scope, o);
            priv_set(scope, this, PR_AVAIL, o.into());
            o.into()
        }
    };
    let p = settle_on_task(scope, avail, true);
    rv.set(p);
}

/// `PresentationAvailability.value`: false -- this engine has no presentation display.
fn cb_pa_value<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let ok = shell_proto(scope, "PresentationAvailability").is_some_and(|p| table_receiver_ok(scope, this, p.into()));
    if !ok {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    rv.set_bool(false);
}

pub fn install_presentation_request(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getAvailability" => method_row(scope, proto, member, cb_pr_get_availability),
        "reconnect" => method_row(scope, proto, member, cb_pr_reconnect),
        "start" => method_row(scope, proto, member, cb_pr_start),
        other => debug_assert!(false, "install_presentation_request: unexpected member {other}"),
    }
}

pub fn install_presentation_availability(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "value" => getter_row(scope, proto, member, cb_pa_value),
        other => debug_assert!(false, "install_presentation_availability: unexpected member {other}"),
    }
}

/// `CtorDef::install` is not read by the installer; `install::live::postshells` passes
/// [`cb_presentation_request_ctor`] to `build_newable` directly. This lets the row NAME its body.
pub fn install_presentation_request_ctor(_scope: &mut v8::PinScope, _proto: v8::Local<v8::Object>, _member: &'static MemberDef) {}

// Rows from the `PresentationRequest` / `PresentationAvailability` blocks the generated registry
// carried, in the capture's order, with their lengths and `required` counts; `[SecureContext]` as
// `tools/sync/data/secure_context.json` measured it (`secure`).
webapi::interface! {
    "PresentationRequest" : "EventTarget" [value, window, secure] {
        ctor    "PresentationRequest"                    len 1 = install_presentation_request_ctor,
        handler "onconnectionavailable" nullable               = event_handler,
        method  "getAvailability" : Promise len 0              = install_presentation_request,
        method  "reconnect"       : Promise len 1 required 1   = install_presentation_request,
        method  "start"           : Promise len 0              = install_presentation_request
    }
}

webapi::interface! {
    "PresentationAvailability" : "EventTarget" [interface, window, secure] {
        ctor    "PresentationAvailability"          len 0 = stub,
        attr    "value" : Boolean readonly                = install_presentation_availability,
        handler "onchange" nullable                       = event_handler
    }
}
