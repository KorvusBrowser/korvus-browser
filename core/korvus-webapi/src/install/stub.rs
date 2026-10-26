//! The single D2 stub.
//!
//! One stub replaces the four that existed (`op_noop_method`, `op_null_method`, `op_stub_ctor`,
//! `behavior_rt::stub!`). When an unimplemented member is called it (1) records the hit into the
//! same coverage registry the corpus reads (`behavior_rt::record_stub_call`), and (2) returns a
//! SHAPE-CORRECT EMPTY value -- never throws, never fabricates. That is the "D2" contract: an
//! unimplemented member is observable as absent, not as a crash and not as a lie, and its call is
//! counted so the corpus worklist can prioritise it.
//!
//! The member being stubbed is passed to the callback as its associated data: a `v8::External`
//! wrapping the `&'static MemberDef`. The installer builds it with `build_fn_data` / an accessor
//! configuration; the callback reads it back here.

use deno_core::v8;

use super::{MemberDef, MemberKind, TypeTag};

/// The empty value a stubbed member returns, derived from its IDL type. A page reading a stubbed
/// attribute sees the same *kind* of value a real one would, just empty -- `""`, `0`, `false`,
/// `null`, `[]` -- so feature detection and arithmetic do not blow up on `undefined`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShapeDefault {
    Undefined,
    Null,
    False,
    Zero,
    EmptyStr,
    EmptyArray,
    EmptyPromise,
}

/// The D2 rule: map a member's kind + IDL type to the empty value it returns while unimplemented.
///
/// Pure, so it is unit-tested without a V8 isolate. The mapping is deliberately conservative --
/// nullable/object/any answer `null`, a void method answers `undefined`, and everything with a
/// concrete primitive shape answers that primitive's empty -- because a stub that returned the wrong
/// KIND (e.g. `undefined` where a `boolean` is declared) would itself be a detectable tell.
pub fn stub_default(kind: MemberKind, ty: TypeTag, nullable: bool) -> ShapeDefault {
    // An `on*` handler reads back as `null` until set, whatever its event type.
    if matches!(kind, MemberKind::Handler) {
        return ShapeDefault::Null;
    }
    // NULLABILITY OUTRANKS THE TYPE, for every type and not just `Any`. `?` is the one empty value
    // WebIDL actually gives a member -- `readonly attribute DOMString? x` with no value IS `null` in
    // every browser, not `""` -- so a row webref declares nullable answers `null` whatever its shape
    // would otherwise be. This used to be reachable only through `Any` because the registry tagged
    // every row `Any`; now that rows carry their real type (`gen_shells.type_tag`), a nullable
    // `DOMString` would otherwise have started answering `""` where it had correctly answered `null`.
    if nullable {
        return ShapeDefault::Null;
    }
    match ty {
        TypeTag::Undefined => ShapeDefault::Undefined,
        TypeTag::Boolean => ShapeDefault::False,
        TypeTag::Integer | TypeTag::Double => ShapeDefault::Zero,
        TypeTag::DomString | TypeTag::UsvString => ShapeDefault::EmptyStr,
        TypeTag::Sequence => ShapeDefault::EmptyArray,
        TypeTag::Promise => ShapeDefault::EmptyPromise,
        // A nullable object/interface reference reads as `null`. Non-nullable `Object` rows are a
        // hand table's deliberate spelling of "an object reference with nothing behind it"; the
        // registry never emits `Object` (see `gen_shells.type_tag`).
        TypeTag::Object => ShapeDefault::Null,
        // `Any` is the widest type AND the tag for every row whose type has no unambiguous empty
        // value -- an interface reference, a dictionary, a union, `object`, an enum, or a member
        // webref does not declare. `undefined` is `any`'s own "no value" spelling and is what the
        // shell stubs these rows replaced answered, so this branch reproduces the pre-W7 answer of
        // every row that is not one of the concrete shapes above.
        TypeTag::Any => ShapeDefault::Undefined,
    }
}

/// Realise a [`ShapeDefault`] as a V8 value in `scope`.
pub fn shape_value<'s>(scope: &mut v8::PinScope<'s, '_>, d: ShapeDefault) -> v8::Local<'s, v8::Value> {
    match d {
        ShapeDefault::Undefined => v8::undefined(scope).into(),
        ShapeDefault::Null => v8::null(scope).into(),
        ShapeDefault::False => v8::Boolean::new(scope, false).into(),
        ShapeDefault::Zero => v8::Integer::new(scope, 0).into(),
        ShapeDefault::EmptyStr => v8::String::empty(scope).into(),
        ShapeDefault::EmptyArray => v8::Array::new(scope, 0).into(),
        // An already-resolved promise of `undefined`: a stubbed `Promise<T>` member must be
        // awaitable without hanging, and it settles empty rather than rejecting.
        ShapeDefault::EmptyPromise => {
            let resolver = v8::PromiseResolver::new(scope).expect("promise resolver");
            let undef = v8::undefined(scope).into();
            resolver.resolve(scope, undef);
            resolver.get_promise(scope).into()
        }
    }
}

/// Recover the `&'static MemberDef` a stub callback was installed with. The installer stores it as a
/// `v8::External` in the function/accessor's associated data.
///
/// # Safety
/// The pointer was made from a `&'static MemberDef` (the members live in a `static` slice the
/// `interface!` macro emits), so it outlives every call.
fn member_from_external<'s>(data: v8::Local<'s, v8::Value>) -> Option<&'static MemberDef> {
    let ext = v8::Local::<v8::External>::try_from(data).ok()?;
    let ptr = ext.value() as *const MemberDef;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: see the doc comment -- the target is a `'static` MemberDef.
    Some(unsafe { &*ptr })
}

/// Unpack a stub callback's associated data: either the bare `External`, or `[External, prototype]`
/// when the row was mounted with a receiver to check against. Returns the member and whether the
/// receiver is one this member may answer for.
///
/// WHY THE CHECK IS HERE. A stub used to answer `undefined` for any receiver at all, so
/// `GPUAdapter.prototype.features` -- read off the PROTOTYPE, which is not an instance -- answered
/// where every browser throws: 532 recorded disagreements in one shape, and a fingerprint as much as a
/// correctness gap, because an object whose members answer for anything is not a platform object. That
/// check lived in `binding::protos`' shell stub; when the shell stub and the D2 stub became ONE stub
/// (invariant I7), the check came with it rather than being dropped.
fn member_from_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    data: v8::Local<'s, v8::Value>,
    this: v8::Local<'s, v8::Value>,
) -> (Option<&'static MemberDef>, bool) {
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(data) {
        let m = arr.get_index(scope, 0).and_then(member_from_external);
        // A third element marks the HAND-TABLE mount, whose receiver check is the chain alone -- a
        // table mints its own instances and they carry no shell brand (see `table_receiver_ok`).
        let table = arr.length() > 2;
        let ok = match arr.get_index(scope, 1) {
            Some(p) if p.is_object() => {
                if table {
                    crate::binding::protos::table_receiver_ok(scope, this, p)
                } else {
                    crate::binding::protos::shell_receiver_ok(scope, this, p)
                }
            }
            _ => true,
        };
        return (m, ok);
    }
    (member_from_external(data), true)
}

/// Wrap a `&'static MemberDef` as the associated data a stub callback reads back. The installer
/// hands the result to `build_fn_data` (methods/handlers) or an accessor configuration (attributes).
pub fn stub_data<'s>(scope: &mut v8::PinScope<'s, '_>, member: &'static MemberDef) -> v8::Local<'s, v8::Value> {
    // A template capture carries this External only because it knows what it points at.
    if crate::install::templates::capturing() {
        crate::install::templates::note_member_external(member);
    }
    v8::External::new(scope, crate::binding::refs::ext(member as *const MemberDef as *mut std::ffi::c_void)).into()
}

/// [`stub_data`] plus the interface prototype the member is mounted on, so the callback can reject a
/// foreign receiver. `None` keeps the un-guarded form -- a caller with no prototype to check against
/// (a static, or a member installed before its prototype exists) must not get a guard that would
/// reject every receiver.
pub fn stub_data_recv<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    member: &'static MemberDef,
    proto: Option<v8::Local<'s, v8::Value>>,
) -> v8::Local<'s, v8::Value> {
    stub_data_recv_kind(scope, member, proto, false)
}

/// [`stub_data_recv`], saying which receiver check the mount wants: the registry's (brand + chain) or
/// a hand table's (chain alone). See `binding::protos::table_receiver_ok` for why they differ.
pub fn stub_data_recv_kind<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    member: &'static MemberDef,
    proto: Option<v8::Local<'s, v8::Value>>,
    table: bool,
) -> v8::Local<'s, v8::Value> {
    let ext = stub_data(scope, member);
    match proto {
        Some(p) if p.is_object() => {
            let arr = v8::Array::new(scope, if table { 3 } else { 2 });
            arr.set_index(scope, 0, ext);
            arr.set_index(scope, 1, p);
            if table {
                let t = v8::Boolean::new(scope, true);
                arr.set_index(scope, 2, t.into());
            }
            arr.into()
        }
        _ => ext,
    }
}

/// A `&'static MemberDef` for a member named from JS by strings rather than reached through a table
/// row, interned once per distinct member.
///
/// NO JS CALL SITE IS LEFT, AND THE OPS THAT SERVED THEM ARE GONE. `op_noop_method` was the last
/// JS-facing spelling of "give me a stub" and was deleted, after the two
/// generated shell layers that were its remaining callers went; its nullable twin `op_null_method` went
/// with `generated_shells.js`'s `__reshell`. The invariant is therefore whole: one stub
/// callback, [`cb_stub`], reached one way, from a `Stub` row of a table.
///
/// This function is still needed, by RUST: `install::live::foreign` re-mints two accessor
/// descriptors for a foreign-realm object and `install::live::svg` re-mints `SVGStyleElement.sheet`
/// nullable, neither of which has a table row to carry the `MemberDef`.
///
/// The leak is bounded and one-time, exactly as the label interning it replaces was: one `MemberDef`
/// per distinct `(iface, name)`, and the callers ask from a fixed list.
pub fn interned_member(iface: &str, name: &str, length: u32, nullable: bool) -> &'static MemberDef {
    let key = format!("{iface}::{name}");
    let pool = interned_pool();
    let Ok(mut pool) = pool.lock() else {
        // A poisoned pool must not take the surface down: leak one throwaway row.
        return Box::leak(Box::new(make_member(Box::leak(key.clone().into_boxed_str()), Box::leak(name.to_string().into_boxed_str()), length, nullable)));
    };
    if let Some(m) = pool.get(&key) {
        return m;
    }
    let label: &'static str = Box::leak(key.clone().into_boxed_str());
    let name: &'static str = &label[iface.len() + 2..];
    let m: &'static MemberDef = Box::leak(Box::new(make_member(label, name, length, nullable)));
    pool.insert(key, m);
    m
}

fn interned_pool() -> &'static std::sync::Mutex<std::collections::HashMap<String, &'static MemberDef>> {
    static POOL: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, &'static MemberDef>>> = std::sync::OnceLock::new();
    POOL.get_or_init(Default::default)
}

/// Whether `m` is a row [`interned_member`] made -- on the heap, named by `(iface, name)` -- rather than a
/// static of an `interface!` table (a blueprint on disk stores the one by name, the other by offset).
pub fn is_interned(m: &'static MemberDef) -> bool {
    let Ok(pool) = interned_pool().lock() else { return false };
    pool.get(m.label).is_some_and(|p| std::ptr::eq(*p, m))
}

/// The row `interned_member` fabricates. `MemberKind::Method` and `TypeTag::Any` are the honest tags
/// for "named from JS, no IDL behind it": `Any` + `nullable` is exactly the `undefined`-or-`null`
/// choice the deleted `cb_shell_noop` / `cb_shell_null` pair made, and `Method` only matters to
/// [`stub_default`]'s `Handler` special case, which these call sites never ask for.
fn make_member(label: &'static str, name: &'static str, length: u32, nullable: bool) -> MemberDef {
    MemberDef {
        name,
        label,
        kind: MemberKind::Method,
        ty: TypeTag::Any,
        length,
        body: super::BodyKind::Stub,
        install: super::stub_install_sentinel,
        cval: super::ConstVal::None,
        static_member: false,
        // A stub minted for a name with no IDL behind it belongs to whichever realm asked for it.
        exposed: super::Exposed::Both,
        enumerable: true,
        configurable: true,
        nullable,
        required: 0, lenient_this: false,
        cfg: "",
        // And to whichever ORIGINS asked for it: a fabricated row carries no measurement, and a
        // condition nobody measured is not a condition.
        exposure: super::Exposure::Unconditional,
    }
}

// ── KORVUS_STUB: what a stub DOES when it is reached ────────────────────────────────────────────

/// The three things a stub can do when a page reaches it. Selected once per process by `$KORVUS_STUB`.
///
/// `quiet` IS THE DEFAULT AND MUST STAY THE DEFAULT. A stub that throws changes what every page does:
/// the D2 contract is that an unimplemented member is observable as ABSENT (a shape-correct empty
/// value), not as a crash, because an unguarded feature-detect that currently degrades would start
/// breaking pages. `throw` exists for diagnosis -- "which stub is this page actually depending on" --
/// and never for the gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StubMode {
    /// Record the hit, answer the shape default. (default)
    Quiet,
    /// Also print `Iface.member @ <script url>:<line>` once per distinct call site, to
    /// `$KORVUS_STUB_LOG` if set, else stderr. This is the diagnosis the coverage map cannot give:
    /// the map counts hits per member, and this says WHICH script asked.
    Log,
    /// Raise `NotSupportedError` instead of answering. Diagnosis only.
    Throw,
}

/// `$KORVUS_STUB`, read once. An unrecognised value is `quiet` -- a typo in a diagnostic switch must
/// not silently change what every page sees.
pub fn stub_mode() -> StubMode {
    static M: std::sync::OnceLock<StubMode> = std::sync::OnceLock::new();
    *M.get_or_init(|| parse_stub_mode(std::env::var("KORVUS_STUB").ok().as_deref()))
}

/// The `$KORVUS_STUB` grammar, split out from the environment read so it can be unit-tested: the
/// selection is the part that must not drift, and `stub_mode` caches its answer for the process.
pub fn parse_stub_mode(v: Option<&str>) -> StubMode {
    match v {
        Some("log") => StubMode::Log,
        Some("throw") => StubMode::Throw,
        _ => StubMode::Quiet,
    }
}

/// `Iface.member` from the row's `Iface::member` coverage label -- the spelling a browser uses in an
/// error message and the one a reader greps for.
fn pretty(label: &str) -> String {
    label.replacen("::", ".", 1)
}

/// The top JS frame, as `<script url>:<line>`; `<native>` when the stack has no JS frame (a call made
/// from an op or from the installer itself).
fn call_site(scope: &mut v8::PinScope) -> String {
    let st = v8::StackTrace::current_stack_trace(scope, 1);
    let Some(st) = st.filter(|s| s.get_frame_count() > 0) else { return "<native>".to_string() };
    let Some(f) = st.get_frame(scope, 0) else { return "<native>".to_string() };
    let name = f.get_script_name_or_source_url(scope);
    let url = name
        .map(|s| s.to_rust_string_lossy(scope))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "<anonymous>".to_string());
    format!("{url}:{}", f.get_line_number())
}

/// Print one `Iface.member @ url:line` line, at most once per distinct (member, site) pair -- a stub
/// inside a loop would otherwise bury the list it exists to produce.
fn log_site(scope: &mut v8::PinScope, label: &'static str) {
    let line = format!("{} @ {}", pretty(label), call_site(scope));
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    let Ok(mut seen) = seen.lock() else { return };
    if !seen.insert(line.clone()) {
        return;
    }
    match std::env::var("KORVUS_STUB_LOG") {
        Ok(path) => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{line}");
            }
        }
        Err(_) => eprintln!("stub: {line}"),
    }
}

/// The one thing every stub callback does before it answers: record the hit into the coverage registry
/// the corpus worklist reads, then apply `$KORVUS_STUB`. Returns `false` when the caller must NOT
/// answer (it threw).
fn enter_stub(scope: &mut v8::PinScope, member: &'static MemberDef) -> bool {
    crate::behavior_rt::record_stub_call(member.label);
    match stub_mode() {
        StubMode::Quiet => true,
        StubMode::Log => {
            log_site(scope, member.label);
            true
        }
        StubMode::Throw => {
            crate::binding::protos::throw_dom(scope, &format!("{} is not implemented", pretty(member.label)), "NotSupportedError");
            false
        }
    }
}

/// THE FOURTH READER of "a promise-returning member rejects rather than throws"
/// ([`crate::install::TypeTag::rejects_rather_than_throws`]) -- and until TY2 the only refusal in the
/// tree that did not ask.
///
/// `install::receiver::policy` asks it for a row with a BODY, `binding::protos::cb_arity_guard` asks
/// it for the argument count, `stub_default` asks it for the shape -- and the D2 stub's own receiver
/// check, which is where a `= stub` row's refusal lives (`put_member` skips the installer's wrapper
/// for exactly this reason: "a `stub` row carries its OWN receiver check"), threw unconditionally.
///
/// MEASURED, Google Chrome 150.0.7871.46 `--headless=new` over a loopback server, 2026-09-23, by
/// sweeping every `I.prototype` member of all 954 window interfaces with the receiver `{}` and
/// AWAITING every promise that came back: 364 members answer a REJECTED promise, none resolved.
/// Of the 235 that korvus threw for instead, **190 already carried `ty: Promise`** and threw only
/// because this refusal never read the column. The remaining 45 are mistyped rows (TY2's own half).
///
/// THE DELIVERY IS SPELLED INLINE at both call sites rather than shared, for the reason
/// `install::receiver::cb_receiver_guard` records: one extra native frame on this
/// native -> JS -> native path was enough to turn a WPT file into V8's
/// `Check failed: IsOnCentralStack()` abort in the debug build. The RULE is still produced once.
///
/// A member the data could not name (`None`) keeps throwing -- the same safe default
/// `TypeTag::from_ordinal` gives an absent slot.
/// A promise member's receiver refusal in Chrome's words (measured; as
/// `binding::protos::illegal_invocation_message` words the fused check's): "Failed to execute '<op>' on
/// '<I>': Illegal invocation", "Failed to read the '<attr>' property from '<I>': ...", "Failed to set ...".
fn promise_refusal(member: Option<&'static MemberDef>, setter: bool) -> String {
    let Some(m) = member else { return "Illegal invocation".to_string() };
    let iface = m.label.split("::").next().unwrap_or("");
    match m.kind {
        MemberKind::Method | MemberKind::StaticMethod => format!("Failed to execute '{}' on '{iface}': Illegal invocation", m.name),
        _ if setter => format!("Failed to set the '{}' property on '{iface}': Illegal invocation", m.name),
        _ => format!("Failed to read the '{}' property from '{iface}': Illegal invocation", m.name),
    }
}

#[inline(always)]
fn refuses_by_rejecting(member: Option<&'static MemberDef>) -> bool {
    member.map(|m| m.ty.rejects_rather_than_throws()).unwrap_or(false)
}

/// The promise operations a stub answers that ask the user for a permission, and Blink 150's refusal of each when no
/// user activation allows the request -- this engine has none (measured: a stub resolved them):
/// (label, DOMException name, message -- a "Must be ..." sentence carries the operation's context --, and whether the
/// refusal comes a task later: the gesture checks are the renderer's, at once; the idle-detection permission is the
/// browser's answer).
const REFUSED_WITHOUT_ACTIVATION: [(&str, &str, &str, bool); 4] = [
    ("HID::requestDevice", "SecurityError", "Must be handling a user gesture to show a permission request.", false),
    ("USB::requestDevice", "SecurityError", "Must be handling a user gesture to show a permission request.", false),
    ("Serial::requestPort", "SecurityError", "Must be handling a user gesture to show a permission request.", false),
    ("IdleDetector::start", "NotAllowedError", "Idle detection permission denied", true),
];

/// Stub getter/method callback: record the hit, return the member's shape-default. One callback
/// serves attributes, handlers, and methods -- the return shape is entirely in the `MemberDef`.
pub fn cb_stub<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let (member, recv_ok) = member_from_data(scope, __body_data, args.this().into());
    stub_get_body(scope, member, recv_ok, &args, rv);
}

/// [`cb_stub`]'s body, shared with the template form ([`cb_stub_tpl`]).
fn stub_get_body(scope: &mut v8::PinScope, member: Option<&'static MemberDef>, recv_ok: bool, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let have = args.length() as usize;
    // An overloaded promise operation counts FIRST (`install::counts_arguments_first`).
    if let Some(m) = member {
        let iface = m.label.split("::").next().unwrap_or("");
        if matches!(m.kind, MemberKind::Method | MemberKind::StaticMethod)
            && have < m.required as usize
            && crate::install::counts_arguments_first(iface, m.name, m.ty, m.required as u32)
        {
            let msg = crate::binding::protos::exec_arity_msg(iface, m.name, m.required as usize, have);
            crate::binding::protos::raise_webidl_type_error(scope, &mut rv, &msg, true);
            return;
        }
    }
    if !recv_ok {
        if refuses_by_rejecting(member) {
            let msg = promise_refusal(member, false);
            rv.set(crate::binding::protos::rejected_type_error(scope, &msg));
        } else {
            crate::binding::protos::illegal_invocation(scope);
        }
        return;
    }
    let Some(member) = member else {
        rv.set_undefined();
        return;
    };
    // THE ARGUMENT COUNT, AFTER THE RECEIVER: Blink's operation callback checks `this` (V8's signature) before it
    // counts, so `AudioData.prototype.copyTo.call({})` is `Illegal invocation`, not "2 arguments required"
    // (Chrome 150, measured 2026-10-09: 151 stub and table methods answered the count first here, 47 promise
    // ones rejected with it). The count lives in the stub for that reason -- an arity guard wrapped round it
    // counted first -- and the installer marks the stub arity-guarded so no pass wraps it again.
    if matches!(member.kind, MemberKind::Method | MemberKind::StaticMethod) && have < member.required as usize {
        let iface = member.label.split("::").next().unwrap_or("");
        let msg = crate::binding::protos::exec_arity_msg(iface, member.name, member.required as usize, have);
        crate::binding::protos::raise_webidl_type_error(scope, &mut rv, &msg, member.ty.rejects_rather_than_throws());
        return;
    }
    // THE ARGUMENTS, CONVERTED, before the operation answers -- Blink's binding converts each as the IDL types it
    // (`install::ctor_dicts::convert_method_args`); a promise operation's conversion error is a rejection
    if matches!(member.kind, MemberKind::Method | MemberKind::StaticMethod) {
        let iface = member.label.split("::").next().unwrap_or("");
        let prefix = format!("Failed to execute '{}' on '{iface}': ", member.name);
        if member.ty.rejects_rather_than_throws() {
            let caught = {
                v8::tc_scope!(let tc, scope);
                match crate::install::ctor_dicts::convert_method_args(tc, member.label, args, &prefix) {
                    Ok(_) => None,
                    Err(()) => {
                        let e = tc.exception().map(|e| v8::Global::new(tc, e));
                        tc.reset();
                        Some(e)
                    }
                }
            };
            if let Some(e) = caught {
                let e: v8::Local<v8::Value> = match e {
                    Some(g) => v8::Local::new(scope, &g),
                    None => v8::undefined(scope).into(),
                };
                rv.set(crate::binding::protos::rejected_promise(scope, e));
                return;
            }
        } else if crate::install::ctor_dicts::convert_method_args(scope, member.label, args, &prefix).is_err() {
            return;
        }
    }
    if !enter_stub(scope, member) {
        return;
    }
    // what a promise operation that asks for a permission answers when nothing the user did allows it
    if let Some((_, name, msg, later)) = REFUSED_WITHOUT_ACTIVATION.iter().find(|(l, _, _, _)| *l == member.label) {
        let iface = member.label.split("::").next().unwrap_or("");
        let text = if msg.starts_with("Must be") { format!("Failed to execute '{}' on '{iface}': {msg}", member.name) } else { msg.to_string() };
        if let Some(e) = crate::binding::protos::make_dom_exc(scope, &text, name) {
            let p = if *later {
                crate::binding::protos::settle_later(scope, e, true, crate::binding::protos::Settle::Task(0.0))
            } else {
                crate::binding::protos::rejected_promise(scope, e)
            };
            rv.set(p);
        }
        return;
    }
    // A `FrozenArray<T>` attribute's empty answer is a FROZEN empty array, the same one again or a new one as Blink's
    // rule for the attribute says (`install::frozen_array`) -- whatever type tag the row carries
    // (`PaymentAddress.addressLine` is tagged `Any`, and answered `undefined`). A nullable one stays `null`.
    if matches!(member.kind, MemberKind::Attr { .. } | MemberKind::StaticAttr { .. }) && !member.nullable {
        if let Some((iface, _)) = member.label.split_once("::").filter(|(i, _)| crate::install::frozen_array::row(i, member.name).is_some()) {
            if let Some(a) = crate::install::frozen_array::answer(scope, args.this(), iface, member.name, |_| 0.0, |_| Some(Vec::new())) {
                rv.set(a);
            }
            return;
        }
    }
    let d = stub_default(member.kind, member.ty, member.nullable);
    let v = shape_value(scope, d);
    rv.set(v);
}

/// Stub setter for a writable attribute or an `on*` handler: record the hit and swallow the write
/// (return undefined). A stubbed writable member must not throw in strict mode on assignment, and it
/// has nowhere to store the value yet -- when the member is implemented, a real setter replaces this.
pub fn cb_stub_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let (member, recv_ok) = member_from_data(scope, __body_data, args.this().into());
    stub_set_body(scope, member, recv_ok, rv);
}

/// [`cb_stub_set`]'s body, shared with the template form ([`cb_stub_set_tpl`]).
fn stub_set_body(scope: &mut v8::PinScope, member: Option<&'static MemberDef>, recv_ok: bool, mut rv: v8::ReturnValue) {
    if !recv_ok {
        // The SAME arm as the getter's, because `install::receiver::guard_installed` wraps a row's
        // getter and setter with the row's ONE policy and this stub is that row's own copy of the
        // check -- a second sub-rule ("...except on a setter") is exactly the shape this goal
        // deletes. It is unreachable today either way: no row in the tree is a WRITABLE attribute
        // typed `Promise` (0 of 3,496 registry rows, 0 of 5,193 hand rows, counted 2026-09-23), so
        // this branch is written to agree with the installer rather than on a measurement of its own.
        if refuses_by_rejecting(member) {
            let msg = promise_refusal(member, true);
            rv.set(crate::binding::protos::rejected_type_error(scope, &msg));
        } else {
            crate::binding::protos::illegal_invocation(scope);
        }
        return;
    }
    if let Some(member) = member {
        if !enter_stub(scope, member) {
            return;
        }
    }
    rv.set_undefined();
}

// ── the one stub builder, and its template form ──────────────────────────────────

/// A stub function: [`cb_stub`] (or, for `setter`, [`cb_stub_set`]) over `data` from
/// [`stub_data_recv`] / [`stub_data_recv_kind`]. While a template capture runs it also declares what the
/// function IS, so `install::templates` can give a realm the template form of it.
pub fn build_stub<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    setter: bool,
    name: &str,
    length: i32,
    data: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Function> {
    let f = if setter {
        crate::binding::protos::build_fn_data(scope, cb_stub_set, name, length, data)
    } else {
        crate::binding::protos::build_fn_data(scope, cb_stub, name, length, data)
    };
    if crate::install::templates::capturing() {
        crate::install::templates::declare(scope, f, crate::install::templates::Form::Stub { setter });
    }
    f
}

/// The template form of a receiver-checked stub's data. A template's data is the same in every realm,
/// so it cannot hold the prototype the check is against; it holds the interface's TEMPLATE, whose
/// function in the realm the call runs in is that realm's interface object (V8 caches one per context).
pub(crate) struct StubTpl {
    member: &'static MemberDef,
    table: bool,
    iface: crate::install::templates::IfaceRef,
}

/// What a stub's recorded data says, for its blueprint: the member, and whether its receiver check is a
/// hand table's. `proto` is the prototype of the interface being translated; data aimed anywhere else is
/// refused.
pub(crate) fn stub_parts<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    data: v8::Local<'s, v8::Value>,
    proto: v8::Local<'s, v8::Value>,
) -> Result<(&'static MemberDef, bool), String> {
    let arr = v8::Local::<v8::Array>::try_from(data).map_err(|_| "stub data that is not [member, prototype]".to_string())?;
    let member = arr.get_index(scope, 0).and_then(member_from_external).ok_or("stub data with no member")?;
    if arr.get_index(scope, 1) != Some(proto) {
        return Err(format!("a stub for `{}` checked against another prototype", member.name));
    }
    Ok((member, arr.length() > 2))
}

/// A stub's template form in one isolate: `(callback, record)`, the record naming the interface by its
/// template (see [`StubTpl`]).
pub(crate) fn stub_template_of(
    setter: bool,
    member: &'static MemberDef,
    table: bool,
    iface: crate::install::templates::IfaceRef,
) -> (v8::FunctionCallback, Box<StubTpl>) {
    let rec = Box::new(StubTpl { member, table, iface });
    let cb: v8::FunctionCallback = if setter {
        v8::MapFnTo::<v8::FunctionCallback>::map_fn_to(cb_stub_set_tpl)
    } else {
        v8::MapFnTo::<v8::FunctionCallback>::map_fn_to(cb_stub_tpl)
    };
    (cb, rec)
}

/// [`member_from_data`] for the template form.
fn member_from_tpl<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<'s, v8::Value>, this: v8::Local<'s, v8::Value>) -> (Option<&'static MemberDef>, bool) {
    // the record's ID (`templates::build_ft`, for the context snapshot), or (an analysis build's) an External over it
    let rec: &StubTpl = match crate::install::templates::fn_slot(scope, data) {
        // SAFETY: a `StubTpl` of this isolate's table, owned by its template cache, which outlives its functions
        Some(crate::install::templates::FnSlot::Stub(p)) => unsafe { &*p },
        _ => {
            let Ok(ext) = v8::Local::<v8::External>::try_from(data) else { return (None, true) };
            // SAFETY: the pointer is a `StubTpl` owned by the isolate's template cache, which outlives every
            // function made from these templates (`install::templates`).
            unsafe { &*(ext.value() as *const StubTpl) }
        }
    };
    let ok = match rec.iface.proto(scope) {
        Some(p) if rec.table => crate::binding::protos::table_receiver_ok(scope, this, p.into()),
        Some(p) => crate::binding::protos::shell_receiver_ok(scope, this, p.into()),
        None => true,
    };
    (Some(rec.member), ok)
}

/// [`cb_stub`] in a realm that instantiated the member from its template.
pub fn cb_stub_tpl<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let (member, recv_ok) = member_from_tpl(scope, __body_data, args.this().into());
    #[cfg(feature = "analysis")]
    if let Some(m) = member {
        spec::analysis::record_bare(spec::analysis::Kind::Stub, m.label);
    }
    stub_get_body(scope, member, recv_ok, &args, rv);
}

/// [`cb_stub_set`] in a realm that instantiated the member from its template.
pub fn cb_stub_set_tpl<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let (member, recv_ok) = member_from_tpl(scope, __body_data, args.this().into());
    #[cfg(feature = "analysis")]
    if let Some(m) = member {
        spec::analysis::record_bare(spec::analysis::Kind::Stub, m.label);
    }
    stub_set_body(scope, member, recv_ok, rv);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_defaults_match_the_idl_type() {
        // A readonly numeric attribute -> 0, not undefined (arithmetic must not NaN).
        assert_eq!(stub_default(MemberKind::Attr { readonly: true }, TypeTag::Integer, false), ShapeDefault::Zero);
        assert_eq!(stub_default(MemberKind::Attr { readonly: true }, TypeTag::Double, false), ShapeDefault::Zero);
        // Booleans read false, strings read "".
        assert_eq!(stub_default(MemberKind::Attr { readonly: false }, TypeTag::Boolean, false), ShapeDefault::False);
        assert_eq!(stub_default(MemberKind::Attr { readonly: false }, TypeTag::DomString, false), ShapeDefault::EmptyStr);
        assert_eq!(stub_default(MemberKind::Attr { readonly: false }, TypeTag::UsvString, false), ShapeDefault::EmptyStr);
        // A void method answers undefined; a Promise method answers an empty resolved promise.
        assert_eq!(stub_default(MemberKind::Method, TypeTag::Undefined, false), ShapeDefault::Undefined);
        assert_eq!(stub_default(MemberKind::Method, TypeTag::Promise, false), ShapeDefault::EmptyPromise);
        // sequence<T> -> [].
        assert_eq!(stub_default(MemberKind::Method, TypeTag::Sequence, false), ShapeDefault::EmptyArray);
        // A nullable object attribute -> null; `any` -> undefined.
        assert_eq!(stub_default(MemberKind::Attr { readonly: true }, TypeTag::Object, false), ShapeDefault::Null);
        assert_eq!(stub_default(MemberKind::Method, TypeTag::Any, false), ShapeDefault::Undefined);
        // An on* handler is null regardless of the (event) type tag.
        assert_eq!(stub_default(MemberKind::Handler, TypeTag::Object, false), ShapeDefault::Null);
        assert_eq!(stub_default(MemberKind::Handler, TypeTag::Any, false), ShapeDefault::Null);
    }

    /// NULLABILITY OUTRANKS THE TYPE. `?` is the only empty value WebIDL actually gives a member, so
    /// a nullable row answers `null` whatever its shape would otherwise be: `DOMString? x` with no
    /// value is `null` in every browser, not `""`. Before the registry carried real types this was
    /// reachable only through `Any`; the rule is written down here so that giving a row its type
    /// cannot quietly turn a correct `null` into an `""`.
    #[test]
    fn a_nullable_row_answers_null_whatever_its_type_is() {
        for ty in [TypeTag::Boolean, TypeTag::Integer, TypeTag::Double, TypeTag::DomString,
                   TypeTag::UsvString, TypeTag::Sequence, TypeTag::Promise, TypeTag::Object, TypeTag::Any] {
            assert_eq!(stub_default(MemberKind::Attr { readonly: true }, ty, true), ShapeDefault::Null, "{ty:?}");
            assert_eq!(stub_default(MemberKind::Method, ty, true), ShapeDefault::Null, "{ty:?}");
        }
    }

    /// The shapes the registry's new IDL-derived tags select, spelled out as the answers a page sees.
    /// `CompositionEvent.data` is the worked example: webref declares `DOMString`, Chrome answers `""`
    /// on a freshly constructed event, and this engine answered `undefined` for as long as every
    /// registry row was tagged `Any` (found by W4.b, 2026-09-11).
    #[test]
    fn an_idl_typed_stub_answers_its_own_kind_of_empty() {
        let attr = MemberKind::Attr { readonly: true };
        assert_eq!(stub_default(attr, TypeTag::DomString, false), ShapeDefault::EmptyStr);
        assert_eq!(stub_default(attr, TypeTag::UsvString, false), ShapeDefault::EmptyStr);
        assert_eq!(stub_default(attr, TypeTag::Boolean, false), ShapeDefault::False);
        assert_eq!(stub_default(attr, TypeTag::Integer, false), ShapeDefault::Zero);
        assert_eq!(stub_default(attr, TypeTag::Sequence, false), ShapeDefault::EmptyArray);
        assert_eq!(stub_default(MemberKind::Method, TypeTag::Promise, false), ShapeDefault::EmptyPromise);
        assert_eq!(stub_default(MemberKind::Method, TypeTag::Undefined, false), ShapeDefault::Undefined);
    }

    /// `KORVUS_STUB=throw` MUST BE OFF BY DEFAULT. A stub that throws changes what every page does --
    /// an unguarded feature-detect that currently degrades would start breaking pages -- so the
    /// absence of the variable, an empty value, and a typo must all select `quiet`, and only the two
    /// exact words select anything else. `stub_mode()` caches its answer for the process, so the
    /// grammar is tested here, where it is a pure function.
    #[test]
    fn korvus_stub_defaults_to_quiet_and_only_two_words_change_it() {
        assert_eq!(parse_stub_mode(None), StubMode::Quiet);
        assert_eq!(parse_stub_mode(Some("")), StubMode::Quiet);
        assert_eq!(parse_stub_mode(Some("quiet")), StubMode::Quiet);
        // A typo, a case variant, or a value meant for something else must NOT arm the diagnostic.
        assert_eq!(parse_stub_mode(Some("THROW")), StubMode::Quiet);
        assert_eq!(parse_stub_mode(Some("throws")), StubMode::Quiet);
        assert_eq!(parse_stub_mode(Some("1")), StubMode::Quiet);
        assert_eq!(parse_stub_mode(Some("log")), StubMode::Log);
        assert_eq!(parse_stub_mode(Some("throw")), StubMode::Throw);
    }

    /// The label a `log` line and a `throw` message carry is the browser's spelling, `Iface.member`,
    /// and only the FIRST `::` becomes a dot (a member name may contain one in principle).
    #[test]
    fn the_stub_label_reads_as_a_member_access() {
        assert_eq!(pretty("Document::featurePolicy"), "Document.featurePolicy");
        assert_eq!(pretty("WebSocket::send"), "WebSocket.send");
        assert_eq!(pretty("Iface::a::b"), "Iface.a::b");
    }
}

/// D2 stub for a named-property getter -- the exotic interceptor kind (DOMStringMap `dataset`,
/// HTMLCollection named access). A stub owns no backing store, so it resolves NO name: it returns
/// `Intercepted::kNo`, letting the lookup continue up the prototype chain, so an absent named
/// property reads `undefined` exactly as a real empty named-getter interface does -- present as an
/// interceptor, empty of data, never fabricating a value. Its firing is recorded into the same
/// coverage registry the corpus reads, so the worklist can see the interceptor was reached. The label
/// is fixed because this rusty_v8 handler API carries no per-member data channel; a live named-getter
/// interface that needs per-interface accounting records it from its own body.
pub fn cb_named_getter_stub<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue,
) -> v8::Intercepted {
    crate::behavior_rt::record_stub_call("NamedGetter::stub");
    v8::Intercepted::kNo
}
