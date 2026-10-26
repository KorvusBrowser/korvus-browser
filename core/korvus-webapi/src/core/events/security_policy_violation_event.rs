//! `core/events/security_policy_violation_event.rs` — `SecurityPolicyViolationEvent` (CSP.idl §5):
//! an `Event` subclass built ENTIRELY from its constructor's init dictionary. Korvus has no CSP
//! engine (no policy parser, no violation detector), so the engine itself never fires one of these —
//! every instance comes from page script calling `new SecurityPolicyViolationEvent(type, init)`.
//! What IS honest to implement is exactly that: the WebIDL constructor steps (the shared `Event`
//! init fields + dictionary-member coercion/defaults for the 12 read-only members, with a real enum
//! check on `disposition` and ToUint16/ToUint32 clamping on the 3 numeric members) and native
//! prototype accessors reading each value back. No violation data is invented anywhere here.
//!
//! The constructor is a raw v8 callback built with `ConstructorBehavior::Allow` (the same shape
//! `cb_html_element_ctor` is built with), so `SecurityPolicyViolationEvent.toString()` reports
//! `[native code]` — the baseline records `ctorNative: true`, and a JS `class ... extends Event`
//! would fail that check. `args.this()` already carries the right prototype (V8's ordinary
//! [[Construct]] uses `new_target.prototype` to build the receiver before the callback runs, exactly
//! as `cb_html_element_ctor`'s comment notes), so construction is just: validate, run the `Event`
//! init steps on `this`, then stash each of the 12 members on a `v8::Private` slot (hidden from
//! enumeration — no own-property tell, matching Chrome's instances) for one shared native getter
//! ([`cb_spve_get`], parameterized by member name via `build_fn_data`; it was the op `op_spve_get`
//! until the 49 builders were deleted the nine deleted JS files were the only callers of) to
//! read back — the `op_reflect_*`
//! pattern (element.rs) applied to per-instance construction state instead of a DOM content attribute.

use deno_core::v8;
use crate::binding::protos::{get_prop, str_val, throw_type_error};

/// The 8 plain DOMString/USVString members (WebIDL default `""`). `disposition` (enum, default
/// `"enforce"`) and the 3 numeric members (default `0`) are written separately below.
const STR_MEMBERS: [&str; 8] = [
    "documentURI", "referrer", "blockedURI", "violatedDirective",
    "effectiveDirective", "originalPolicy", "sourceFile", "sample",
];

fn spve_set(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, value: v8::Local<v8::Value>) {
    let pname = v8::String::new(scope, name).unwrap();
    let key = v8::Private::for_api(scope, Some(pname));
    obj.set_private(scope, key, value);
}

// (`Event.timeStamp` is `core/events/ctor.rs`'s `now_ms` -- `spec::PerfClock` out of OpState.
//  This file had a copy that went through a JS `performance.now()` round trip; it was deleted
//  with the rest of the hand-copied Event init steps.)
pub fn cb_spve_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // Called without `new` (new_target undefined) -> the standard Blink message for a DOM
    // constructor invoked as a plain function.
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("SecurityPolicyViolationEvent"));
        return;
    }
    // `type` is a required DOMString (WebIDL): 0 arguments -> TypeError, matching the `Event` base
    // constructor's own arity error (event.rs `evt_init_common` callers / host.js `Event`).
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'SecurityPolicyViolationEvent': 1 argument required, but only 0 present.");
        return;
    }
    // The arguments as WebIDL converts them, in order (Chrome 150): `type` to a string, then
    // `SecurityPolicyViolationEventInit` -- `bubbles`, `cancelable`, `composed`, then its twelve members sorted, each
    // read once and converted as it is read, `disposition` checked as the enum it is. korvus read `disposition`
    // first and the rest in its own order, answered a non-object and a bad `disposition` in messages of its own,
    // and read `lineNumber`/`columnNumber` as `unsigned long` where Blink declares `long` (-1 read back 4294967295).
    let Some(type_str) = crate::core::events::ctor::arg_string(scope, &args, 0, "SecurityPolicyViolationEvent") else { return };
    let Ok(dict) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "SecurityPolicyViolationEventInit", "Failed to construct 'SecurityPolicyViolationEvent': ") else { return };

    let this = args.this();
    // ---- The shared `Event(type, init)` steps ----
    //
    // ONE copy, `core/events/ctor.rs`'s. This block used to reproduce them by hand -- nine
    // slot writes, the three boolean init members, the clock read and the `[LegacyUnforgeable]`
    // `isTrusted` install -- with a comment saying it mirrored `host.js`'s `Event` constructor, which
    // is precisely the duplicate truth the goal exists to remove. `init_event` also stamps the `Event`
    // BRAND that `Event.prototype`'s rows check, so `spve.type` answers rather than throwing
    // `Illegal invocation`, and the timestamp is `spec::PerfClock` read at call time.
    crate::core::events::ctor::init_event(scope, this, &type_str, Some(dict));

    // ---- SecurityPolicyViolationEvent's own 12 members, on hidden v8::Private slots, from the converted
    // dictionary (every member has an IDL default, so each is present). ----
    for name in STR_MEMBERS {
        let sv = get_prop(scope, dict, name).filter(|v| v.is_string()).unwrap_or_else(|| str_val(scope, ""));
        spve_set(scope, this, name, sv);
    }
    let dv = get_prop(scope, dict, "disposition").filter(|v| v.is_string()).unwrap_or_else(|| str_val(scope, "enforce"));
    spve_set(scope, this, "disposition", dv);
    for name in ["statusCode", "lineNumber", "columnNumber"] {
        let n = get_prop(scope, dict, name).filter(|v| v.is_number()).unwrap_or_else(|| v8::Number::new(scope, 0.0).into());
        spve_set(scope, this, name, n);
    }

    rv.set(this.into());
}

// Shared native getter for all 12 members: `args.data()` carries the member name (bound at install
// time, the same `op_reflect_*` "one callback serves many attrs" trick), read back off the per-instance
// v8::Private slot the constructor stashed it on.
pub(crate) fn cb_spve_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Ok(name) = v8::Local::<v8::String>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let key = v8::Private::for_api(scope, Some(name));
    if let Some(v) = args.this().get_private(scope, key) { rv.set(v); }
}

// This file registers no `op_*`: its constructor and its shared member getter are raw callbacks
// the `SecurityPolicyViolationEvent` table names directly, so nothing needs a JS-facing builder.

// ── the table ───────────────────────────────────────────────────────────────
//
// `dom/08o-csp-event.js` (37 lines, 3 `Object.defineProperty` sites over the interface object, the
// prototype's `constructor` and the twelve accessors) is gone. The twelve members are the rows
// below, mounted by THE installer (`install::live::install_window_small_elements`) onto a prototype whose
// interface object is `cb_spve_ctor` itself, so `SecurityPolicyViolationEvent.toString()` still
// reports `[native code]` and the chain to `Event` is the installer's WebIDL splice rather than a
// hand `Object.defineProperty` of `prototype`.
//
// Names, order, kinds and descriptor flags are VERBATIM from the `SecurityPolicyViolationEvent`
// block of `install/generated_registry.rs` -- the recorded baseline's own-property order, which is
// NOT the IDL declaration order (the shape axis faults an out-of-order list, so this follows the
// browser rather than the spec text). All twelve are readonly; one shared native getter reads each
// one back off the `v8::Private` slot the constructor stashed it on, with the member name as the
// row's install-time data.

pub fn install_security_policy_violation_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    crate::core::html::html_input_element::acc1(scope, proto, member, cb_spve_get, Some(member.name));
}

webapi::interface! {
    "SecurityPolicyViolationEvent" : "Event" [value, window] {
        ctor   "SecurityPolicyViolationEvent"  len 1              = stub,
        attr "documentURI"        : Object readonly = install_security_policy_violation_event,
        attr "referrer"           : Object readonly = install_security_policy_violation_event,
        attr "blockedURI"         : Object readonly = install_security_policy_violation_event,
        attr "violatedDirective"  : Object readonly = install_security_policy_violation_event,
        attr "effectiveDirective" : Object readonly = install_security_policy_violation_event,
        attr "originalPolicy"     : Object readonly = install_security_policy_violation_event,
        attr "disposition"        : Object readonly = install_security_policy_violation_event,
        attr "sourceFile"         : Object readonly = install_security_policy_violation_event,
        attr "statusCode"         : Object readonly = install_security_policy_violation_event,
        attr "lineNumber"         : Object readonly = install_security_policy_violation_event,
        attr "columnNumber"       : Object readonly = install_security_policy_violation_event,
        attr "sample"             : Object readonly = install_security_policy_violation_event
    }
}
