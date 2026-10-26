//! THE receiver check, derived from the table and applied by THE installer.
//!
//! # The defect this closes
//!
//! A table row installed as a plain METHOD accepted any receiver; a row whose body is a Rust ACCESSOR
//! brand-checked — not because anyone decided it should, but as a side effect of reading a slot off
//! `this`. Nothing forced a method to do the same, so the property held or not per row:
//!
//! * W4.a measured `Navigator.prototype.sendBeacon.call({}, "/x")` answering `true`; Chrome
//!   150.0.7871.46 throws `TypeError: Illegal invocation` (re-measured for this unit, below).
//! * W5.1 L17 measured eleven WebGPU members answering for any receiver and fixed those eleven by
//!   hand — which is the shape of the problem: eleven at a time, per lane, forever.
//! * On the merged release binary, `CanvasRenderingContext2D.prototype.canvas`'s getter threw for
//!   `{}` while `CanvasRenderingContext2D.prototype.fillRect.call({}, 0,0,1,1)` did not.
//!
//! The fix belongs HERE, in the one installer, so a row cannot be written without a receiver check
//! rather than each lane remembering. Nothing in this file names an interface or a member: the
//! policy is read off the row's own columns ([`policy`]).
//!
//! # What Chrome answers, and therefore what the policy is
//!
//! Two sources, both authoritative and both consulted before anything was switched on.
//!
//! **The capture bundle** (`brave-151-linux-20260907T1006`, truth source 1) already records 832
//! receiver-identity answers in its oracle — every one of them a real browser answering "what does
//! this member do when called on an object that is not an instance". The tally:
//!
//! ```text
//!     receiver {}                         receiver Object.create(I.prototype)
//!       334  TypeError: Illegal invocation    432  TypeError: Illegal invocation
//!        19  returned [object Promise]         41  returned [object Promise]
//!         3  returned [object Array Iterator]   3  returned [object Array Iterator]
//! ```
//!
//! **Headless Chrome 150.0.7871.46** (`--headless=new --dump-dom`, truth source 2), asked for this
//! unit about the classes the oracle's device families do not cover:
//!
//! ```text
//!   Navigator.prototype.sendBeacon.call({}, "/x")             TypeError: Illegal invocation
//!   CanvasRenderingContext2D.prototype.fillRect.call({},...)  TypeError: Illegal invocation
//!   get CanvasRenderingContext2D.prototype.canvas .call({})   TypeError: Illegal invocation
//!   get HTMLImageElement.prototype.alt .call({})              TypeError: Illegal invocation
//!   set HTMLImageElement.prototype.alt .call({}, "x")         TypeError: Illegal invocation
//!   get HTMLAnchorElement.prototype.href .call({})            TypeError: Illegal invocation
//!   Node.prototype.appendChild.call({}, el)                   TypeError: Illegal invocation
//!   Element.prototype.getAttribute.call({}, "id")             TypeError: Illegal invocation
//!   EventTarget.prototype.addEventListener.call({}, ...)      TypeError: Illegal invocation
//!   get HTMLElement.prototype.onclick .call({})               TypeError: Illegal invocation
//!   set HTMLElement.prototype.onclick .call({}, null)         TypeError: Illegal invocation
//!   HTMLElement.prototype.onclick            (read off proto) TypeError: Illegal invocation
//!   Navigator.prototype.requestMIDIAccess.call({})            returned [object Promise]
//!   Clipboard.prototype.readText.call({})                     returned [object Promise]
//!   get FontFace.prototype.loaded .call({})                   returned [object Promise]
//!   get ServiceWorkerContainer.prototype.ready .call({})      returned [object Promise]
//!   URL.createObjectURL.call({}, new Blob())                  returned [object String]
//!   get Document.prototype.onreadystatechange .call({})       returned [object Undefined]
//!   get HTMLElement.prototype.onmouseenter .call({})          returned [object Undefined]
//!   get HTMLElement.prototype.onmouseleave .call({})          returned [object Undefined]
//!   NodeList.prototype.values === Array.prototype.values      true
//! ```
//!
//! Four rules come out of that, and every one of them is a COLUMN of the row, never a list:
//!
//! 1. **A static has no receiver.** `URL.createObjectURL.call({})` answers. `m.static_member` /
//!    `is_proto` says so, and the installer already computes it for the D2 stub's own check.
//! 2. **A `Promise`-typed member REJECTS, it does not throw.** WebIDL turns a `TypeError` raised
//!    while invoking a promise-returning operation into a rejected promise, and the browser's own
//!    answers say so 60 times. `m.ty == TypeTag::Promise` is the column.
//! 3. **`[LegacyLenientThis]` answers `undefined`.** Exactly three members in all of webref carry it
//!    and all three are `EventHandler` attributes; `m.lenient_this` is the column, emitted by
//!    `tools/sync/gen_shells.py` from the same webref parse that answers nullability, and spelled
//!    `lenient` by a hand table.
//! 4. **A function this engine did not build is left alone.** `kits::array_proto_fn` mounts
//!    `Array.prototype`'s own function object on purpose; `protos::is_engine_fn` answers that
//!    without naming it.
//!
//! # Which receiver, and why not the strict brand
//!
//! The check is the prototype-chain one (`protos::table_receiver_ok`): the receiver must have the
//! interface prototype in its chain and must not BE it. It is deliberately the WEAKER of the two
//! checks this tree has.
//!
//! `protos::shell_receiver_ok` additionally demands the brand a shell constructor stamps (or a cppgc
//! wrapper). That is what Chrome really does — the bundle records `Object.create(I.prototype)`
//! throwing 432 times — but a HAND TABLE mints its own instances (`install::live::set_table_proto`,
//! `core/animation::new_on_proto`, `core/html/webaudio.rs::new_shell`) and they wear no brand, so
//! requiring it rejects GENUINE instances: `anim_live` and `rtc_graph_live` both threw
//! `Illegal invocation` reading a row off a real one when W7 tried it. A check that is too strict
//! breaks real instances; a check that is too loose is the defect. The chain cannot make the first
//! mistake — every mint site puts the interface prototype in the chain, which is what minting IS —
//! and it still rejects every receiver the measured defects used: `{}`, a primitive, and the
//! interface prototype read as a value.
//!
//! What it does NOT reject is `Object.create(I.prototype)`, and that residue is RECORDED rather than
//! hidden: `core/korvus-runtime/tests/receiver_sweep.rs` counts it every run. Closing it means
//! branding the hand tables' mint sites, which is each table's own work (W7 said the same).

use deno_core::v8;

use super::{MemberDef, MemberKind};
use crate::binding::protos::{
    build_fn_data, is_engine_fn, is_receiver_guarded, member_key, rejected_type_error, table_receiver_ok, throw_type_error,
};

/// Is the installer's receiver check ON?
///
/// The rollout was measured, not hoped: the mechanism landed (`ccab8651`) with this `false` and the
/// sweep in `core/korvus-runtime/tests/receiver_sweep.rs` counting how many rows would change answer
/// — 2,752 of the 6,589 it probes — so the commit that flipped it had a before and an after over
/// exactly the same set of rows.
pub const ENABLED: bool = true;

/// What the TABLE says happens when this row is reached through a receiver that is not an instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// No check: a static (no receiver at all), a constant (a data property, not a function), or an
    /// interceptor row (the exotic object IS the instance).
    None,
    /// `TypeError: Illegal invocation`, which is what a browser answers for 766 of the 832 receiver
    /// answers the bundle records.
    Throw,
    /// A promise rejected with that `TypeError`: WebIDL converts a throw out of a promise-returning
    /// member into a rejection, and the bundle records 60 members doing exactly that.
    Reject,
    /// `undefined`, for `[LegacyLenientThis]`. Three members in webref, all `EventHandler`
    /// attributes; Chrome 150 answers `undefined` to both the getter and the setter.
    Lenient,
}

/// The row's receiver policy, read off its own columns. No interface or member is named here.
pub fn policy(m: &MemberDef, is_proto: bool) -> Policy {
    // A member on the interface OBJECT has no receiver to check, and neither has a row the installer
    // mounted somewhere that is not a prototype (the named-getter exotic object mounts its rows on
    // the INSTANCE; `install_named_getter_object` passes `is_proto: false` for that reason).
    if !is_proto || m.static_member {
        return Policy::None;
    }
    match m.kind {
        // A constant is a data property whose value IS its surface; an interceptor row is not a
        // property at all.
        MemberKind::Const | MemberKind::NamedGetter | MemberKind::IndexedGetter => return Policy::None,
        MemberKind::StaticMethod | MemberKind::StaticAttr { .. } => return Policy::None,
        _ => {}
    }
    if m.lenient_this {
        return Policy::Lenient;
    }
    // ONE production of "a promise-returning operation rejects rather than throws"
    // (`TypeTag::rejects_rather_than_throws`), shared with the argument-count guard. This used to
    // spell `m.ty == TypeTag::Promise` here and the arity guard did not know the rule at all.
    if m.ty.rejects_rather_than_throws() {
        return Policy::Reject;
    }
    Policy::Throw
}

// ── the wrapper ─────────────────────────────────────────────────────────────────────────────────

/// data = `[interfacePrototype, originalFn, policyOrdinal]`.
///
/// THE THREE ARMS BELOW ARE SPELLED INLINE, AND THAT IS A MEASURED DECISION, not an oversight.
/// `binding::protos::raise_webidl_type_error` is the shared throw-or-reject DELIVERY that
/// `cb_arity_guard` uses, and routing these arms through it is one extra NATIVE FRAME per call.
/// This callback sits on the deeply recursive native -> JS -> native path that already aborts the
/// process with V8's `Check failed: IsOnCentralStack()` when it runs out of native stack (see
/// `core::events::event_target::cb_et_dispatch`, which exists because a page could reach that abort
/// in three lines). The extra frame was enough, in the DEBUG build the WPT runner uses:
///
/// ```text
///   html/webappapis/dynamic-markup-insertion/opening-the-input-stream/
///     bailout-side-effects-ignore-opens-during-unload.window.js
///       master                                 runs to a verdict (0/3)
///       with the shared helper on these arms   SIGABRT, Check failed: IsOnCentralStack()
///       with these arms inline again           runs to a verdict (0/3)
/// ```
///
/// Measured 2026-09-23 (ARITY1) by running that one file with `KORVUS_WPT_ONLY`, three ways. The
/// RULE is still produced once -- [`policy`] asks `TypeTag::rejects_rather_than_throws`, at install
/// time, where there is no recursion. Only the delivery is duplicated here, and only because a
/// frame on this path is not free.
fn cb_receiver_guard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(proto) = d.get_index(scope, 0) else { return };
    if crate::behavior_rt::touch_enabled() {
        touch_interface(scope, proto);
    }
    let Some(orig) = d.get_index(scope, 1).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    // the policy is read only when the check fails, as it always was: a call that passes pays nothing
    receiver_guard_run(scope, &args, rv, proto, orig, |scope| d.get_index(scope, 2).and_then(|v| v.integer_value(scope)).unwrap_or(0));
}

/// The receiver guard's body, [`cb_receiver_guard`]'s (the install's wrapper; a template fuses the same
/// check, `binding::protos::table_receiver_ok`): `this` must have `proto` on its chain, then the inner function runs. `ord`
/// (the refusal's form) is asked for only when the check fails.
pub(crate) fn receiver_guard_run(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
    proto: v8::Local<v8::Value>,
    orig: v8::Local<v8::Function>,
    ord: impl FnOnce(&mut v8::PinScope) -> i64,
) {
    let this: v8::Local<v8::Value> = args.this().into();
    if !table_receiver_ok(scope, this, proto) {
        match ord(scope) {
            1 => rv.set(rejected_type_error(scope, "Illegal invocation")),
            2 => rv.set(v8::undefined(scope).into()),
            _ => throw_type_error(scope, "Illegal invocation"),
        }
        return;
    }
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    if let Some(r) = orig.call(scope, this, &argv) {
        rv.set(r);
    }
}

/// The impact recorder's hook (`behavior_rt::touch`): the interface a guarded member belongs to is the
/// name stamped on the prototype its check is bound to.
#[cold]
fn touch_interface(scope: &mut v8::PinScope, proto: v8::Local<v8::Value>) {
    let Ok(p) = v8::Local::<v8::Object>::try_from(proto) else { return };
    if let Some(n) = crate::binding::protos::iface_name_of(scope, p) {
        crate::behavior_rt::touch(&n.to_rust_string_lossy(scope));
    }
}

/// Wrap `orig` in the receiver check, keeping its own `name` and `length` — both are read off the
/// function rather than off the row, because an accessor's are `"get x"`/0 and `"set x"`/1 while the
/// row carries the attribute's name and the method's arity. A receiver check changes what a member
/// does when it is called wrongly, never its descriptor, name, length or kind.
fn wrap<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Value>, orig: v8::Local<'s, v8::Function>, p: Policy) -> v8::Local<'s, v8::Function> {
    let name = orig.get_name(scope).to_rust_string_lossy(scope);
    let len_k = v8::String::new(scope, "length").unwrap();
    let obj: v8::Local<v8::Object> = orig.into();
    let length = obj.get(scope, len_k.into()).and_then(|v| v.integer_value(scope)).unwrap_or(0) as i32;
    let ord = match p {
        Policy::Reject => 1,
        Policy::Lenient => 2,
        _ => 0,
    };
    let d = v8::Array::new(scope, 3);
    d.set_index(scope, 0, proto);
    d.set_index(scope, 1, orig.into());
    let ordi = v8::Integer::new(scope, ord);
    d.set_index(scope, 2, ordi.into());
    let g = build_fn_data(scope, cb_receiver_guard, &name, length, d.into());
    if crate::install::templates::capturing() {
        let inner = Some(v8::Global::new(scope, orig));
        let guard = crate::install::templates::Guard::Receiver { ord: ord.into(), proto: v8::Global::new(scope, proto) };
        crate::install::templates::record(scope, g, crate::install::templates::Recipe { cb: None, data: None, length, name: name.clone(), ctor: false, guard, inner });
    }
    // The guarded pair still performs whatever check `orig` did, so a wrapper over an arity guard is
    // itself arity-guarded: the registry's later arity pass must skip it rather than wrap AGAIN from
    // the outside, which would put the argument count before the receiver and reverse Chrome's order
    // (`Event.prototype.initEvent.call({})` is `Illegal invocation`, not "1 argument required").
    if crate::binding::protos::is_arity_guarded(scope, orig.into()) {
        crate::binding::protos::stamp_arity_guarded(scope, g);
    }
    crate::binding::protos::stamp_receiver_guarded(scope, g);
    // (The wrapper also carried its ingredients -- inner function, policy, prototype -- as privates, so
    // `rebind` could rebuild it for another prototype. Nothing moves a member between prototypes any
    // more, and `rebind` and the three privates with it are deleted.)
    //
    // It carries two of them again, for ONE reader inside the install: the registry's arity pass
    // (`installer::guard_member`), which must put the argument count INSIDE this check -- Chrome checks the
    // receiver first (`CSSNumericValue.prototype.to.call({})` is `Illegal invocation`, not "1 argument
    // required"; 198 rows answered the count first, measured 2026-10-09) -- and so rebuilds this guard round
    // an arity guard over `orig` ([`inner_of`], [`rewrap`]) instead of wrapping it from outside.
    let (ik, ok) = (inner_key(scope), ord_key(scope));
    let o: v8::Local<v8::Object> = g.into();
    o.set_private(scope, ik, orig.into());
    let ordv = v8::Integer::new(scope, ord);
    o.set_private(scope, ok, ordv.into());
    g
}

fn inner_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    crate::binding::protos::private_key(scope, "korvus.receiverInner")
}

fn ord_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    crate::binding::protos::private_key(scope, "korvus.receiverOrd")
}

/// The function a receiver guard built by this install wraps, and its refusal's form -- `None` for any other
/// value.
pub(crate) fn inner_of<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<(v8::Local<'s, v8::Function>, i32)> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let (ik, ok) = (inner_key(scope), ord_key(scope));
    let inner = o.get_private(scope, ik).and_then(|f| v8::Local::<v8::Function>::try_from(f).ok())?;
    let ord = o.get_private(scope, ok).and_then(|n| n.int32_value(scope)).unwrap_or(0);
    Some((inner, ord))
}

/// A receiver guard against `proto` round `inner`, refusing in the form `ord` names -- [`inner_of`]'s inverse.
pub(crate) fn rewrap<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Value>, inner: v8::Local<'s, v8::Function>, ord: i32) -> v8::Local<'s, v8::Function> {
    let p = match ord {
        1 => Policy::Reject,
        2 => Policy::Lenient,
        _ => Policy::Throw,
    };
    wrap(scope, proto, inner, p)
}

// ── the installer hook ──────────────────────────────────────────────────────────────────────────

/// Apply the row's receiver policy to the member its body just mounted on `target`.
///
/// Called by `installer::put_member` for every `BodyKind::Install` row, so a row cannot be written
/// without a receiver check. It reads the descriptor back rather than taking the function from the
/// body, because a body mounts its own property (a kit's `define_accessor`, a hand body's
/// `define_method`) and the installer must not learn a second way to do that.
///
/// Returns the OUTCOME it recorded, so the caller can ledger it and the sweep can classify every
/// row that still answers a foreign receiver without guessing why. `"none:…"` always says which of
/// the reasons applied — an unexplained `none` is what made W7's 157 need a hand investigation.
pub fn guard_installed<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
    m: &'static MemberDef,
    is_proto: bool,
) -> &'static str {
    let p = policy(m, is_proto);
    if p == Policy::None {
        // WHY there is no policy, from the row: a member on the interface object (or on the exotic
        // instance) has no receiver; a constant is a value; an interceptor row is not a property.
        return if !is_proto || m.static_member { "none:static" } else { "none:kind" };
    }
    // THE GLOBAL IS NOT A PROTOTYPE. The `Window` table mounts every non-constant row onto the GLOBAL
    // OBJECT with `is_proto: true` (`core/html/window.rs::install_window_window`) — `window` is an
    // INSTANCE, not a prototype, so checking a receiver against it would reject `window` itself and
    // take the surface down. Chrome does check these (`atob.call({})` and
    // `getComputedStyle.call({})` both throw `Illegal invocation`, measured), against `Window.prototype`
    // — which this call does not have, because the re-parent that puts it in the global's chain runs
    // AFTER the member loop. So they are left un-guarded and COUNTED as such by the sweep, rather
    // than guarded against the wrong object.
    let ctx = scope.get_current_context();
    if ctx.global(scope) == target {
        return "none:global";
    }
    let key = member_key(scope, m.name);
    let Some(dd) = target.get_own_property_descriptor(scope, key).filter(|v| !v.is_undefined()) else { return "none:absent" };
    let Ok(dd) = v8::Local::<v8::Object>::try_from(dd) else { return "none:absent" };
    let sk = |scope: &mut v8::PinScope<'s, '_>, s: &str| v8::String::new(scope, s).unwrap();

    let cfg_k = sk(scope, "configurable");
    let configurable = dd.get(scope, cfg_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let en_k = sk(scope, "enumerable");
    let enumerable = dd.get(scope, en_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    if !configurable {
        // A non-configurable slot cannot be redefined; leaving it is the only honest option.
        return "none:sealed";
    }
    let proto: v8::Local<v8::Value> = target.into();

    let get_k = sk(scope, "get");
    let set_k = sk(scope, "set");
    let has_accessor = dd.get(scope, get_k.into()).map(|v| !v.is_undefined()).unwrap_or(false)
        || dd.get(scope, set_k.into()).map(|v| !v.is_undefined()).unwrap_or(false);
    if has_accessor {
        let mut touched = false;
        let one = |scope: &mut v8::PinScope<'s, '_>, k: v8::Local<v8::String>, touched: &mut bool| -> v8::Local<'s, v8::Value> {
            match dd.get(scope, k.into()) {
                Some(v) if is_engine_fn(scope, v) && !is_receiver_guarded(scope, v) => {
                    let f = v8::Local::<v8::Function>::try_from(v).unwrap();
                    *touched = true;
                    wrap(scope, proto, f, p).into()
                }
                Some(v) => v,
                None => v8::undefined(scope).into(),
            }
        };
        let g = one(scope, get_k, &mut touched);
        let s = one(scope, set_k, &mut touched);
        if !touched {
            return "none:generic";
        }
        if !ENABLED {
            return policy_name(p);
        }
        let mut d = v8::PropertyDescriptor::new_from_get_set(g, s);
        d.set_enumerable(enumerable);
        d.set_configurable(configurable);
        target.define_property(scope, key, &d);
        return policy_name(p);
    }

    let val_k = sk(scope, "value");
    let Some(value) = dd.get(scope, val_k.into()) else { return "none:absent" };
    // AN ALIAS IS NOT A SECOND MEMBER: a row whose value is the SAME function as the
    // prototype's own `cfg` member (`@@iterator` over a setlike's `values`, `kits::own_alias`) already
    // answers with that member's check -- its receiver wrapper, or the stub's own. Wrapping it again made
    // `AudioParamMap.prototype[Symbol.iterator] !== AudioParamMap.prototype.entries` (Brave 151: `===`).
    if !m.cfg.is_empty() {
        let ck = member_key(scope, m.cfg);
        if target.get(scope, ck.into()).is_some_and(|v| v == value) {
            return "none:alias";
        }
    }
    if !is_engine_fn(scope, value) || is_receiver_guarded(scope, value) {
        // A constant, a data value, or the shared `Array.prototype` function one kit mounts on
        // purpose — not ours to guard; or a slot this installer has already guarded, which a second
        // pass over the same prototype must not wrap twice.
        return "none:generic";
    }
    if !ENABLED {
        return policy_name(p);
    }
    let f = v8::Local::<v8::Function>::try_from(value).unwrap();
    let wr_k = sk(scope, "writable");
    let writable = dd.get(scope, wr_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let mut g = wrap(scope, proto, f, p);
    // An overloaded promise operation counts its arguments BEFORE this check (`install::counts_arguments_first`):
    // its count goes round the guard, not inside it.
    let iface = m.label.split("::").next().unwrap_or("");
    if crate::install::counts_arguments_first(iface, m.name, m.ty, m.required as u32) {
        let len = v8::Local::<v8::Object>::try_from(g).ok().and_then(|o| {
            let lk = sk(scope, "length");
            o.get(scope, lk.into())
        });
        let length = len.and_then(|v| v.integer_value(scope)).unwrap_or(0) as i32;
        g = crate::binding::protos::make_arity_guard(scope, iface, m.name, m.required as i32, length, g.into(), m.ty);
        // the slot still carries this check: a second pass over the prototype must not wrap it again
        crate::binding::protos::stamp_receiver_guarded(scope, g);
    }
    let mut d = v8::PropertyDescriptor::new_from_value_writable(g.into(), writable);
    d.set_enumerable(enumerable);
    d.set_configurable(configurable);
    target.define_property(scope, key, &d);
    policy_name(p)
}

/// The policy's name for the ledger the sweep joins on.
pub fn policy_name(p: Policy) -> &'static str {
    match p {
        Policy::None => "none",
        Policy::Throw => "throw",
        Policy::Reject => "reject",
        Policy::Lenient => "lenient",
    }
}

/// The ledger entry for a row the installer mounts through the D2 STUB, whose receiver check is the
/// stub's own (`install::stub::member_from_data` + `cb_stub`) rather than [`wrap`]'s.
///
/// WHY IT EXISTS. `put_member` skipped `record_receiver` entirely for a `stub` row, so the sweep in
/// `core/korvus-runtime/tests/receiver_sweep.rs` had no policy to join against and classified every
/// stub row that answered as "a second writer overwrote it". That was harmless only while every
/// stub row THREW — a throw is not an answer, so the bucket stayed empty. The moment a stub row
/// correctly answered a REJECTED promise (TY2), 194 rows landed in "second writer", a bucket whose
/// whole meaning is "a file is redefining this member after the installer ran". The grader would
/// have reported a fix as 194 overwrites.
///
/// It reads the SAME two things the stub reads and nothing else — whether the mount passed a
/// prototype to check against (`put_d2_stub`/`put_shell_stub`: `is_proto`), and the row's `ty` — so
/// it cannot claim a check the stub does not perform. It deliberately does NOT go through [`policy`]:
/// `policy` also answers `lenient`, and `cb_stub` has no lenient arm. (No row reaches both today —
/// a `[LegacyLenientThis]` member is an `EventHandler` attribute, which `mounted_layer` calls `slot`,
/// not `stub` — and this returning `throw` for one would be the honest report of what it would do.)
pub fn stub_policy_name(m: &MemberDef, is_proto: bool) -> &'static str {
    // A static, or a member mounted somewhere that is not a prototype: `stub_data_recv` was handed
    // `None`, so the callback gets the bare `External` and never looks at `this`.
    if !is_proto || m.static_member {
        return "none:static";
    }
    match m.kind {
        // A constant is installed as its VALUE; the interceptor kinds mount nothing on the prototype.
        MemberKind::Const | MemberKind::NamedGetter | MemberKind::IndexedGetter => "none:kind",
        MemberKind::StaticMethod | MemberKind::StaticAttr { .. } => "none:static",
        // The one rule, asked once more: a promise-returning member refuses by rejecting.
        _ if m.ty.rejects_rather_than_throws() => "reject",
        _ => "throw",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::{BodyKind, ConstVal, TypeTag, stub_install_sentinel};

    fn row(kind: MemberKind, ty: TypeTag, static_member: bool, lenient_this: bool) -> MemberDef {
        MemberDef {
            name: "x",
            label: "I::x",
            kind,
            ty,
            length: 0,
            body: BodyKind::Install,
            install: stub_install_sentinel,
            cval: ConstVal::None,
            static_member,
            exposed: crate::install::Exposed::Both,
            enumerable: true,
            configurable: true,
            nullable: false,
            required: 0,
            lenient_this,
            cfg: "",
            // These rows exercise the RECEIVER policy, which is orthogonal to origin exposure, so
            // `Unconditional` is the macro default a member that states nothing gets.
            //
            // A change added this column to `MemberDef` and missed this one `#[cfg(test)]`
            // constructor. FP5.5 reported that as a GATE HOLE -- "the fast gate runs `--test all`
            // and never noticed" -- and that part is WRONG, checked rather than adopted: `gate.sh`
            // steps 2/3 and 3/3 are `cargo test --workspace`, which builds lib targets, and the
            // merge gate at 23:38 failed on exactly this line with `could not compile
            // korvus-webapi (lib test)`. On master the break lived through ZERO intervening commits
            // and twelve minutes. What FP5.5 actually saw was true of its own worktree, pinned at
            // `6a344f7f` and never rebased -- a real observation with the wrong mechanism attached.
            exposure: crate::core::html::exposure::Exposure::Unconditional,
        }
    }

    /// Each of the four rules, as a column read rather than a name.
    #[test]
    fn the_policy_is_read_off_the_row() {
        let meth = row(MemberKind::Method, TypeTag::Undefined, false, false);
        assert_eq!(policy(&meth, true), Policy::Throw);
        // A member on the interface object has no receiver, whichever way it says so.
        assert_eq!(policy(&meth, false), Policy::None);
        assert_eq!(policy(&row(MemberKind::Method, TypeTag::Undefined, true, false), true), Policy::None);
        assert_eq!(policy(&row(MemberKind::StaticMethod, TypeTag::Any, false, false), true), Policy::None);
        // WebIDL turns a throw out of a promise-returning member into a rejection.
        assert_eq!(policy(&row(MemberKind::Method, TypeTag::Promise, false, false), true), Policy::Reject);
        assert_eq!(policy(&row(MemberKind::Attr { readonly: true }, TypeTag::Promise, false, false), true), Policy::Reject);
        // `[LegacyLenientThis]` outranks everything: Chrome answers `undefined`, not a rejection.
        assert_eq!(policy(&row(MemberKind::Handler, TypeTag::Any, false, true), true), Policy::Lenient);
        // A constant is a value, and an interceptor row is not a property.
        assert_eq!(policy(&row(MemberKind::Const, TypeTag::Integer, false, false), true), Policy::None);
        assert_eq!(policy(&row(MemberKind::NamedGetter, TypeTag::Any, false, false), true), Policy::None);
        assert_eq!(policy(&row(MemberKind::IndexedGetter, TypeTag::Any, false, false), true), Policy::None);
        // An ordinary handler IS checked -- Chrome 150 throws for `HTMLElement.prototype.onclick`.
        assert_eq!(policy(&row(MemberKind::Handler, TypeTag::Any, false, false), true), Policy::Throw);
    }

    /// THE STUB MOUNT READS THE SAME COLUMN, and that is asserted rather than assumed.
    ///
    /// A `= stub` row is not wrapped by [`wrap`]; `install::stub::cb_stub` performs its own receiver
    /// check, and until TY2 that check threw for every row whatever its type -- so the ONE rule
    /// (`TypeTag::rejects_rather_than_throws`) was true of a row with a body and false of the same
    /// row spelled `= stub`. 190 rows, 176 of them in the generated registry, refused the wrong way
    /// because of it. This pins the two mounts to the same answer for every tag.
    #[test]
    fn the_stub_mount_and_the_wrapper_refuse_the_same_way_for_every_type() {
        for ty in TypeTag::ALL {
            let m = row(MemberKind::Method, ty, false, false);
            let wrapped = policy(&m, true);
            let stubbed = stub_policy_name(&m, true);
            assert_eq!(
                policy_name(wrapped) == "reject",
                stubbed == "reject",
                "{ty:?}: the wrapper says {wrapped:?} and the stub says {stubbed:?} -- one rule, two answers"
            );
            // And an attribute, which is the shape `ViewTransition::ready` and
            // `WritableStreamDefaultWriter::closed` are.
            let a = row(MemberKind::Attr { readonly: true }, ty, false, false);
            assert_eq!(policy_name(policy(&a, true)) == "reject", stub_policy_name(&a, true) == "reject", "{ty:?} attr");
        }
        // The `none` reasons the stub mount has, spelled as the ledger records them: a mount that
        // passed no prototype checks nothing, and the kinds that are not a prototype property.
        let m = row(MemberKind::Method, TypeTag::Promise, false, false);
        assert_eq!(stub_policy_name(&m, false), "none:static");
        assert_eq!(stub_policy_name(&row(MemberKind::Method, TypeTag::Promise, true, false), true), "none:static");
        assert_eq!(stub_policy_name(&row(MemberKind::Const, TypeTag::Integer, false, false), true), "none:kind");
        assert_eq!(stub_policy_name(&row(MemberKind::NamedGetter, TypeTag::Any, false, false), true), "none:kind");
        // Exactly one tag rejects, so a guard that rejected everything cannot pass the loop above.
        assert_eq!(stub_policy_name(&row(MemberKind::Method, TypeTag::Object, false, false), true), "throw");
        assert_eq!(stub_policy_name(&m, true), "reject");
    }
}
