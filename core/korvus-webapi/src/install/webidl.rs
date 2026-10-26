//! `install/webidl.rs` — the BRAND and the WebIDL argument machinery every hand table shares.
//!
//! This code was written for the event family and lived in `core/events/ctor.rs`. The rest
//! needs the identical mechanism for `DOMException`, `Blob`, `File`, `Request`, `Response`,
//! `ImageData`, `AbortController`, `Worker` and `SharedWorker`, so it lives HERE now and
//! `core/events/ctor.rs` re-exports it — one implementation, not a second copy per domain. Nothing
//! about it changed in the move except the brand private's prefix (`korvus_evt_brand_` ->
//! `korvus_brand_`), which is stamped and read only by the two functions below.
//!
//! WHY A BRAND, and why one per interface. Every member of these families reads a private slot off
//! `this`. A slot getter over a receiver that never ran the constructor answers `undefined` — which
//! is how, before this mechanism, `Event.prototype.type.call({})` returned `undefined` where Chrome
//! throws `TypeError: Illegal invocation`, and `JSON.stringify(Event.prototype)` serialised the whole
//! prototype instead of throwing. Measured against `google-chrome --headless=new`: all seventeen
//! `Event.prototype` rows, all four `EventTarget.prototype` rows and every subclass row throw
//! `Illegal invocation` on `{}`, and a cross-interface receiver is rejected the same way
//! (`UIEvent.prototype.view.call(new Event("t"))` throws; so does
//! `MouseEvent.prototype.clientX.call(new UIEvent("u"))`). `DOMException` answers the same way and was
//! re-measured for W4.c: `DOMException.prototype.name`, `.message` and `.code` are all
//! `Illegal invocation` on the prototype itself. One brand per interface, stamped by that interface's
//! own init step, reproduces exactly that: a subclass runs every init in its chain, so it carries
//! every brand in its chain and no others.
//!
//! The brand is a `v8::Private`, so page JS can neither read it, copy it onto a forgery, nor delete
//! it — the same mechanism `isTrusted` uses.

use deno_core::v8;

use crate::binding::protos::{get_prop, illegal_invocation, throw_type_error};

// ── the brand ───────────────────────────────────────────────────────────────────────────────────

fn brand_key<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Private>> {
    Some(crate::binding::protos::brand_private(scope, iface))
}

/// Stamp `iface`'s brand on a freshly constructed instance. Called by that interface's init step, so
/// every instance carries the brand of every interface in its WebIDL chain.
pub(crate) fn brand(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str) {
    let Some(key) = brand_key(scope, iface) else { return };
    let t = v8::Boolean::new(scope, true);
    this.set_private(scope, key, t.into());
    // The D2 stub's own receiver check (`shell_receiver_ok`) wants the brand a SHELL constructor
    // stamps, or a cppgc wrapper; a table-built instance had neither, so a `stub` row on one of these
    // interfaces threw `Illegal invocation` on a genuine instance -- `notification.close()` did
    // exactly that. Stamping it here makes an honest not-implemented row answer its shape default (the
    // `undefined` Chrome answers for `close`) instead of throwing, while `{}` still fails both checks.
    crate::binding::protos::brand_shell_instance(scope, this);
}

/// Is `v` an instance of `iface` (or of one of its subclasses)? A primitive, a plain object, a
/// prototype and `Object.create(X.prototype)` are all false — only something an `iface` constructor
/// (or a Rust mint site that ran its init) produced is true.
pub(crate) fn has_brand(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str) -> bool {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let Some(key) = brand_key(scope, iface) else { return false };
    obj.has_private(scope, key).unwrap_or(false)
}

/// The receiver guard every row of this family runs first: `Illegal invocation` when `this` is not an
/// instance of the interface the row belongs to. Returns the receiver when it passes.
/// (The `this` parameter's lifetime is ELIDED, not tied to the scope's: a `#[op2]`-shaped callback
/// hands `args.this()` a lifetime of its own, and pinning the two together makes every call site a
/// borrow error. The `html_meter_element.rs` template records the same gotcha.)
pub(crate) fn receiver<'a>(
    scope: &mut v8::PinScope,
    this: v8::Local<'a, v8::Value>,
    iface: &str,
) -> Option<v8::Local<'a, v8::Object>> {
    if has_brand(scope, this, iface) {
        return v8::Local::<v8::Object>::try_from(this).ok();
    }
    illegal_invocation(scope);
    None
}

/// The wrapper `event::evt_branded_method` mounts: the receiver check, then the WebIDL arity check,
/// then the row's own body. Chrome checks the receiver FIRST — `Event.prototype.initEvent.call({})`
/// is `Illegal invocation`, not "1 argument required" — and the generic arity guard cannot express
/// that because it wraps from the outside knowing nothing about the interface.
///
/// data = `["Iface", "member", need, originalFn, isVoid]`, packed at install time.
fn cb_brand_guard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let iface = d.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let need = d.get_index(scope, 2).and_then(|v| v.integer_value(scope)).unwrap_or(0) as usize;
    let is_void = d.get_index(scope, 4).map(|v| v.is_true()).unwrap_or(false);
    let Some(orig) = d.get_index(scope, 3).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    // the member's name is read only for the arity message, as it always was
    let member = |scope: &mut v8::PinScope| d.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    brand_guard_run(scope, &args, rv, &iface, member, need, is_void, orig);
}

/// [`cb_brand_guard`]'s body (the install's wrapper; a template fuses [`brand_guard_check`] instead). `member` is
/// asked for only when the argument count fails.
#[allow(clippy::too_many_arguments)]
pub(crate) fn brand_guard_run(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
    iface: &str,
    member: impl FnOnce(&mut v8::PinScope) -> String,
    need: usize,
    is_void: bool,
    orig: v8::Local<v8::Function>,
) {
    if !brand_guard_check(scope, args, iface, member, need) {
        return;
    }
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let this: v8::Local<v8::Value> = args.this().into();
    let r = orig.call(scope, this, &argv);
    if is_void {
        return;
    }
    if let Some(r) = r {
        rv.set(r);
    }
}

/// [`brand_guard_run`]'s checks, shared with a fused member (`install::templates`): the brand, then
/// WebIDL's count. `false`: refused, the exception thrown.
pub(crate) fn brand_guard_check(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    iface: &str,
    member: impl FnOnce(&mut v8::PinScope) -> String,
    need: usize,
) -> bool {
    if receiver(scope, args.this().into(), iface).is_none() {
        return false;
    }
    if (args.length() as usize) < need {
        let member = member(scope);
        crate::binding::protos::require_args(scope, args, need, iface, &member);
        return false;
    }
    true
}

/// Build the wrapper above for one row. Keeps the member's own `name` and WebIDL `length`, so the
/// mounted function is indistinguishable from the body it guards.
pub(crate) fn brand_guard<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    member: &'static crate::install::MemberDef,
    orig: v8::Local<'s, v8::Function>,
) -> v8::Local<'s, v8::Value> {
    use crate::binding::protos::build_fn_data;
    let d = v8::Array::new(scope, 5);
    let i = v8::String::new(scope, iface).unwrap();
    let m = v8::String::new(scope, member.name).unwrap();
    let n = v8::Integer::new(scope, member.required as i32);
    d.set_index(scope, 0, i.into());
    d.set_index(scope, 1, m.into());
    d.set_index(scope, 2, n.into());
    d.set_index(scope, 3, orig.into());
    let is_void = member.ty == crate::install::TypeTag::Undefined;
    let vd: v8::Local<v8::Value> = v8::Boolean::new(scope, is_void).into();
    d.set_index(scope, 4, vd);
    let g = build_fn_data(scope, cb_brand_guard, member.name, member.length as i32, d.into());
    if crate::install::templates::capturing() {
        let guard = crate::install::templates::Guard::Brand { iface: iface.to_string(), member: member.name.to_string(), need: member.required as usize, is_void };
        crate::install::templates::record_guard(scope, g, member.name, member.length as i32, guard, orig);
    }
    g.into()
}

/// The accessor equivalent of [`brand_guard`]: a native getter/setter that checks the receiver and
/// then forwards to the bespoke body. data = `["Iface", originalFn]`.
fn cb_brand_accessor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let iface = d.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let Some(orig) = d.get_index(scope, 1).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    brand_accessor_run(scope, &args, rv, &iface, orig);
}

/// [`cb_brand_accessor`]'s body (the install's wrapper; a template fuses [`brand_accessor_check`] instead).
pub(crate) fn brand_accessor_run(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
    iface: &str,
    orig: v8::Local<v8::Function>,
) {
    if !brand_accessor_check(scope, args, iface) {
        return;
    }
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let this: v8::Local<v8::Value> = args.this().into();
    if let Some(r) = orig.call(scope, this, &argv) {
        rv.set(r);
    }
}

/// [`brand_accessor_run`]'s check, shared with a fused member: the brand (`false`: thrown).
pub(crate) fn brand_accessor_check(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str) -> bool {
    receiver(scope, args.this().into(), iface).is_some()
}

pub(crate) fn brand_accessor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    name: &str,
    length: i32,
    orig: v8::Local<'s, v8::Function>,
) -> v8::Local<'s, v8::Function> {
    use crate::binding::protos::build_fn_data;
    let d = v8::Array::new(scope, 2);
    let i = v8::String::new(scope, iface).unwrap();
    d.set_index(scope, 0, i.into());
    d.set_index(scope, 1, orig.into());
    let g = build_fn_data(scope, cb_brand_accessor, name, length, d.into());
    if crate::install::templates::capturing() {
        let guard = crate::install::templates::Guard::BrandAccessor { iface: iface.to_string() };
        crate::install::templates::record_guard(scope, g, name, length, guard, orig);
    }
    g
}

// ── the argument guards ─────────────────────────────────────────────────────────────────────────

/// `new X()` must be `new`, not a call — the event family's spelling of the one guard.
///
/// This used to be a SECOND copy of [`crate::binding::protos::require_ctor_new_target`],
/// with its own inline copy of Chrome's sentence, and a second spelling of a Chrome message is how
/// the first one drifts. It forwards now, so the event constructors and every other WebIDL
/// constructor answer the same bytes by construction.
pub(crate) fn require_new(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str) -> bool {
    crate::binding::protos::require_ctor_new_target(scope, args, iface)
}

/// The WebIDL required-argument count. Chrome's message, verbatim.
pub(crate) fn require_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str, n: i32) -> bool {
    if args.length() >= n {
        return true;
    }
    let present = args.length();
    // The PLURALISATION is Chrome's and it is observable: `new File(["a"])` answers "2 arguments
    // required, but only 1 present." while `new Event()` answers "1 argument required". Every
    // interface this helper served earlier required exactly one argument, so the bug could
    // not show; `File` is the first with two, and `fileapi_live` caught it on its first run.
    // `binding::protos::require_args`, the METHOD form, has always pluralised.
    let plural = if n == 1 { "argument" } else { "arguments" };
    throw_type_error(scope, &format!("Failed to construct '{iface}': {n} {plural} required, but only {present} present."));
    false
}

/// The optional dictionary at argument `idx` of `iface`'s constructor, CONVERTED as WebIDL converts it:
/// `undefined` and `null` are the empty dictionary, anything else that is not an object Chrome's
/// TypeError (`new Event("t", 5)` -> "The provided value is not of type 'EventInit'."), and the members are read
/// in WebIDL's order from Blink 150's IDL, each once, each converted as it is read
/// ([`crate::install::ctor_dicts::convert_dict`]). The answer is the converted dictionary, which the constructor
/// reads in place of the page's object -- so the readers below only ever see converted values, and a getter on the
/// page's object has run exactly once, in Chrome's order, before any of them.
///
/// It handed the constructors the page's object until a stock Chrome 150 sweep (`fixtures/dict-conversion`) found
/// them reading it in their own orders: `new MouseEvent(t, d)` read 18 members in a hand order where Chrome reads
/// 26 (the modifier keys, `sourceCapabilities`, `region`), and a getter's throw was swallowed -- `new Event('e',
/// {get bubbles() { throw 1 }})` constructed.
pub(crate) fn init_dict<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
    idx: i32,
    iface: &str,
    dict_name: &str,
) -> Result<Option<v8::Local<'s, v8::Object>>, ()> {
    let prefix = format!("Failed to construct '{iface}': ");
    crate::install::ctor_dicts::convert_dict(scope, args.get(idx), dict_name, &prefix).map(Some)
}

/// A required DOMString/USVString argument.
///
/// A Symbol is a TypeError, and the MESSAGE is Chrome's: "Failed to construct 'Event': Cannot convert
/// a Symbol value to a string". V8's own `to_string` failure raises the same sentence WITHOUT the
/// "Failed to construct" prefix, and a page reading `e.message` can tell the two apart -- so the
/// Symbol case is checked here rather than left to propagate.
pub(crate) fn arg_string(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, idx: i32, iface: &str) -> Option<String> {
    let v = args.get(idx);
    if v.is_symbol() {
        throw_type_error(scope, &format!("Failed to construct '{iface}': Cannot convert a Symbol value to a string"));
        return None;
    }
    let s = v.to_string(scope)?;
    Some(s.to_rust_string_lossy(scope))
}

// ── the WebIDL dictionary readers ───────────────────────────────────────────────────────────────
//
// Each returns `Err(())` after throwing. `Ok(None)` is "the member is absent" (use the default);
// `Ok(Some(x))` is the converted value. A dictionary member whose value is `undefined` is absent,
// which is why `{detail: undefined}` and `{}` give a `CustomEvent` the same `detail`.

fn present<'s>(scope: &mut v8::PinScope<'s, '_>, d: Option<v8::Local<v8::Object>>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    d.and_then(|d| get_prop(scope, d, key)).filter(|v| !v.is_undefined())
}

/// `boolean`: a plain truthiness coercion ("hah" is true, 0 is false).
pub(crate) fn dict_bool(scope: &mut v8::PinScope, d: Option<v8::Local<v8::Object>>, key: &str) -> bool {
    present(scope, d, key).map(|v| v.boolean_value(scope)).unwrap_or(false)
}

/// `DOMString`: `ToString`, so a number becomes its decimal text and a Symbol throws.
pub(crate) fn dict_string(scope: &mut v8::PinScope, d: Option<v8::Local<v8::Object>>, key: &str) -> Result<Option<String>, ()> {
    let Some(v) = present(scope, d, key) else { return Ok(None) };
    let Some(s) = v.to_string(scope) else { return Err(()) };
    Ok(Some(s.to_rust_string_lossy(scope)))
}

/// `double` (NOT `unrestricted double`): NaN and the infinities are a TypeError, which is what
/// `new MouseEvent("m", {clientX: NaN})` answers in Chrome.
pub(crate) fn dict_double(
    scope: &mut v8::PinScope,
    d: Option<v8::Local<v8::Object>>,
    key: &str,
    iface: &str,
) -> Result<Option<f64>, ()> {
    let Some(v) = present(scope, d, key) else { return Ok(None) };
    let Some(n) = v.number_value(scope) else { return Err(()) };
    if !n.is_finite() {
        throw_type_error(
            scope,
            &format!(
                "Failed to construct '{iface}': Failed to read the '{key}' property from '{iface}Init': The provided double value is non-finite."
            ),
        );
        return Err(());
    }
    Ok(Some(n))
}

/// WebIDL `ToInt32`/`ToUint32`/`ToInt16`/`ToUint16`: the modular integer conversions. NaN and the
/// infinities convert to 0 here (they are not an error for an integer type), and a fractional value
/// truncates toward zero — `{lineno: 2.9}` is `2`, as Chrome reports.
fn modular(n: f64, bits: u32, signed: bool) -> f64 {
    if !n.is_finite() {
        return 0.0;
    }
    let m = 2f64.powi(bits as i32);
    let t = n.trunc();
    let mut r = t % m;
    if r < 0.0 {
        r += m;
    }
    if signed && r >= m / 2.0 {
        r -= m;
    }
    r
}

pub(crate) fn dict_int(
    scope: &mut v8::PinScope,
    d: Option<v8::Local<v8::Object>>,
    key: &str,
    bits: u32,
    signed: bool,
) -> Result<Option<f64>, ()> {
    let Some(v) = present(scope, d, key) else { return Ok(None) };
    let Some(n) = v.number_value(scope) else { return Err(()) };
    Ok(Some(modular(n, bits, signed)))
}

/// `any`: the value as given, present or not. Distinguishes an absent member from an explicit
/// `null` (`new ErrorEvent("e").error` is `undefined`, `new ErrorEvent("e", {error: null}).error` is
/// `null`), which is why this one does NOT filter `undefined` away for the "absent" test only.
pub(crate) fn dict_any<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    d: Option<v8::Local<v8::Object>>,
    key: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    d.and_then(|d| get_prop(scope, d, key)).filter(|v| !v.is_undefined())
}

/// A nullable interface-typed member (`relatedTarget`, `dataTransfer`, `source`). `null`/absent are
/// fine; anything that is not an instance of `iface_check` is a TypeError, exactly as Chrome answers
/// `new MouseEvent("m", {relatedTarget: {}})`.
pub(crate) fn dict_interface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    d: Option<v8::Local<v8::Object>>,
    key: &str,
    iface: &str,
    dict_name: &str,
    accepted: &[&str],
) -> Result<Option<v8::Local<'s, v8::Value>>, ()> {
    let Some(v) = present(scope, d, key) else { return Ok(None) };
    if v.is_null() {
        return Ok(Some(v));
    }
    if accepted.iter().any(|n| is_instance_of(scope, v, n)) {
        return Ok(Some(v));
    }
    let want = accepted.join("' or '");
    throw_type_error(
        scope,
        &format!(
            "Failed to construct '{iface}': Failed to read the '{key}' property from '{dict_name}': The provided value is not of type '{want}'."
        ),
    );
    Err(())
}

/// Whether `v` is an instance of interface `name` (WebIDL's interface-type conversion, for the dictionary's
/// interface-typed members): its brand, from any realm (`binding::brand`), or -- for a name no
/// template brands, a JavaScript builtin such as `Uint8ClampedArray` -- `instanceof` this realm's constructor.
/// It never calls into page JS.
pub(crate) fn is_instance_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, name: &str) -> bool {
    crate::binding::protos::implements(scope, v, name)
}

/// A WebIDL `enum` member: one of `allowed`, or a TypeError naming the offending value AND the enum
/// type. Chrome's message, verbatim — measured on 150.0.7871.46 for three different enums, which is
/// what the `enum_name` parameter is for:
///
/// ```text
/// new Notification("t", {dir: "nope"})   ... is not a valid enum value of type NotificationDirection.
/// new Blob(["a"], {endings: "nope"})     ... is not a valid enum value of type EndingType.
/// new Request("/x", {mode: "nope"})      ... is not a valid enum value of type RequestMode.
/// ```
///
/// The trailing " of type X." was once missing, so a page comparing the message could
/// tell the engine apart on the one code path whose whole job is to be read by a `catch`.
pub(crate) fn dict_enum(
    scope: &mut v8::PinScope,
    d: Option<v8::Local<v8::Object>>,
    key: &str,
    iface: &str,
    dict_name: &str,
    enum_name: &str,
    allowed: &[&str],
) -> Result<Option<String>, ()> {
    let Some(s) = dict_string(scope, d, key)? else { return Ok(None) };
    if allowed.contains(&s.as_str()) {
        return Ok(Some(s));
    }
    throw_type_error(
        scope,
        &format!(
            "Failed to construct '{iface}': Failed to read the '{key}' property from '{dict_name}': The provided value '{s}' is not a valid enum value of type {enum_name}."
        ),
    );
    Err(())
}

// ── the branded row mounts ──────────────────────────────────────────────────────────────────────
//
// The three shapes a table row takes when its interface carries a brand. Each is
// `install::live::rows`' plain mount with [`receiver`] in front of the body: the brand FIRST, then
// the WebIDL arity, then the row. The interface the receiver is checked against is the row's own
// label prefix (`"DOMException::code"` -> `DOMException`), so a table names the body and nothing
// else — there is no per-interface wiring to keep in step with the table.

/// Mount one method row behind the interface's brand.
pub(crate) fn branded_method_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    use crate::binding::protos::build_fn;
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let f = build_fn(scope, cb, member.name, member.length as i32);
    let guard = brand_guard(scope, iface, member, f);
    let guard = stamp(scope, guard);
    crate::install::kits::define_value(scope, target, member, guard, true);
}

/// Mark a guard this module built as ALREADY receiver-checking, so `install::receiver`'s derived pass
/// leaves it alone.
///
/// W9.17 landed after this file branched and gave the installer its own receiver check, keyed off the
/// row's columns. It skips anything already stamped -- and these guards were not stamping, so a
/// promise-returning row got wrapped a second time FROM THE OUTSIDE and threw synchronously where
/// Chrome rejects (`Blob.prototype.arrayBuffer.call({})` answered `SYNC-THROW: Illegal invocation`
/// instead of a rejected promise). Both mechanisms were right on their own branch; only the pair was
/// wrong. Caught by this unit's own `no_row_accepts_a_foreign_receiver` tests at merge.
fn stamp<'a>(scope: &mut v8::PinScope, v: v8::Local<'a, v8::Value>) -> v8::Local<'a, v8::Value> {
    if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
        crate::binding::protos::stamp_receiver_guarded(scope, f);
    }
    v
}

/// Mount one PROMISE-RETURNING method row behind the interface's brand.
///
/// A promise-returning WebIDL operation never throws synchronously: every failure, the receiver check
/// included, becomes a REJECTION. Measured on `google-chrome --headless=new` 150.0.7871.46 —
/// `Blob.prototype.text.call({})` returns a `Promise` that rejects with
/// `TypeError: Failed to execute 'text' on 'Blob': Illegal invocation`, while the non-promise
/// `Blob.prototype.slice.call({})` throws `TypeError: Illegal invocation` on the spot. A plain
/// `branded_method_row` on a promise row would turn a rejection every page's `.catch` is written for
/// into a synchronous throw it is not.
pub(crate) fn branded_promise_method_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    use crate::binding::protos::{build_fn, build_fn_data};
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let inner = build_fn(scope, cb, member.name, member.length as i32);
    let d = v8::Array::new(scope, 3);
    let i = v8::String::new(scope, iface).unwrap();
    let m = v8::String::new(scope, member.name).unwrap();
    d.set_index(scope, 0, i.into());
    d.set_index(scope, 1, m.into());
    d.set_index(scope, 2, inner.into());
    let guard = build_fn_data(scope, cb_brand_promise_guard, member.name, member.length as i32, d.into());
    if crate::install::templates::capturing() {
        let g = crate::install::templates::Guard::BrandPromise { iface: iface.to_string(), member: member.name.to_string() };
        crate::install::templates::record_guard(scope, guard, member.name, member.length as i32, g, inner);
    }
    let guard = stamp(scope, guard.into());
    crate::install::kits::define_value(scope, target, member, guard.into(), true);
}

/// data = `["Iface", "member", originalFn]`.
fn cb_brand_promise_guard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let iface = d.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let Some(orig) = d.get_index(scope, 2).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    // the member's name is read only for the refusal, as it always was
    let member = |scope: &mut v8::PinScope| d.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    brand_promise_run(scope, &args, rv, &iface, member, orig);
}

/// [`cb_brand_promise_guard`]'s body (the install's wrapper; a template fuses [`brand_promise_check`] instead).
/// `member` is asked for only when the brand check fails.
pub(crate) fn brand_promise_run(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
    iface: &str,
    member: impl FnOnce(&mut v8::PinScope) -> String,
    orig: v8::Local<v8::Function>,
) {
    if !brand_promise_check(scope, args, &mut rv, iface, member) {
        return;
    }
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let this: v8::Local<v8::Value> = args.this().into();
    if let Some(r) = orig.call(scope, this, &argv) {
        rv.set(r);
    }
}

/// [`brand_promise_run`]'s check, shared with a fused member: the brand, refused by REJECTING (`false`:
/// the rejected promise is the return value).
pub(crate) fn brand_promise_check(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    rv: &mut v8::ReturnValue,
    iface: &str,
    member: impl FnOnce(&mut v8::PinScope) -> String,
) -> bool {
    use crate::binding::protos::rejected_type_error;
    if !has_brand(scope, args.this().into(), iface) {
        let member = member(scope);
        let p = rejected_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': Illegal invocation"));
        rv.set(p);
        return false;
    }
    true
}

/// Mount one readonly attribute row behind the interface's brand.
///
/// READONLY IS THE ONLY SHAPE THIS FAMILY HAS. A `branded_pair_row` twin took a setter callback too
/// and threaded it through a shared `_inner`; nothing ever called it, because no interface that
/// carries a WebIDL brand in this tree (`DOMException`, `File`, `MediaQueryList`, `Range`,
/// `Selection`) has a read/WRITE attribute — every branded attribute the capture records is
/// readonly. Both are deleted rather than kept as an unexercised path: writing the
/// setter half again, against the row that needs it, is a smaller job than trusting a branch that
/// has never run. `define_accessor` already takes `Option<setter>`, which is where it would go.
pub(crate) fn branded_getter_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    use crate::binding::protos::build_fn;
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let g = build_fn(scope, cb, &format!("get {}", member.name), 0);
    let gg = brand_accessor(scope, iface, &format!("get {}", member.name), 0, g);
    crate::install::kits::define_accessor(scope, proto, member, gg, None);
}
