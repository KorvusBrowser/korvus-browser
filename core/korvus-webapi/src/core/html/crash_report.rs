//! `core/html/crash_report.rs` — `CrashReportContext` and the `crashReport` singleton.
//!
//! # What it is
//!
//! `window.crashReport` is Chromium's page-supplied crash-report annotation store: a page calls
//! `initialize(size)` to ask for a buffer of `size` bytes and then `set(key, value)` / `delete(key)`
//! to put key/value pairs in it, and whatever is in the buffer is attached to a crash dump. The
//! INTERFACE was already in every capture's own-globals list and the registry published it; what was
//! missing was the instance, and FP5 left it out rather than mint one whose three rows were D2 stubs.
//!
//! # WHAT THE DEVICE ANSWERS — Chrome 150.0.7871.46 and Brave 151.1.93.138, headed over CDP under
//! Xvfb, 2026-09-19. Identical, reading for reading.
//!
//! ```text
//! crashReport                       own accessor, `get crashReport` only, e=1 c=1 (no setter)
//! Object.prototype.toString.call    [object CrashReportContext]
//! prototype chain                   CrashReportContext -> Object      (NOT an EventTarget)
//! crashReport === crashReport       true
//!
//! set / delete BEFORE initialize    InvalidStateError
//!   "Failed to execute 'set' on 'CrashReportContext': CrashReportContext is not initialized.
//!    Call initialize() and wait for it to resolve."
//! initialize()                      TypeError, 1 argument required, but only 0 present
//! initialize(n)                     a Promise; it RESOLVES, and `set` in the same synchronous
//!                                   block still reports "not initialized" -- the state flips when
//!                                   the promise settles, not when the call returns (measured: the
//!                                   probe that did not await it read "not initialized" for every
//!                                   subsequent op)
//! initialize(n) a SECOND time       the promise REJECTS (it does not throw):
//!   "Failed to execute 'initialize' on 'CrashReportContext': The initialize() method has already
//!    been called."
//! set/delete that does not fit      NotAllowedError
//!   "Failed to execute 'set' on 'CrashReportContext': The crash report data is too large to be
//!    stored in the requested buffer."
//! foreign receiver, set/delete      TypeError: Illegal invocation          (the bare sentence)
//! foreign receiver, initialize      a REJECTED promise, TypeError:
//!   "Failed to execute 'initialize' on 'CrashReportContext': Illegal invocation"
//!   -- a `Promise`-returning operation converts its receiver and arity errors into rejections,
//!      which is why `CrashReportContext.prototype.initialize.call({}, 1)` hands back a Promise.
//! new CrashReportContext()          TypeError: Failed to construct ...: Illegal constructor
//! ```
//!
//! # THE BUDGET IS A RULE, AND IT WAS MEASURED RATHER THAN GUESSED
//!
//! `initialize(0)` resolves and then every `set` AND every `delete` answers "too large" — including a
//! `delete` of a key that is not there, which is how one learns that the FRAMING itself has to fit.
//! One `initialize` is allowed per DOCUMENT, so the sweep used one same-origin IFRAME per buffer
//! size: sixteen sizes from 0 to 128, four operations each, Chrome 150 and Brave 151 agreeing on all
//! sixty-four. The boundaries it pins:
//!
//! ```text
//! size 1   delete('zz') on an empty map  NotAllowedError   -- the framing does not fit
//! size 2   delete('zz') on an empty map  ok                -- FRAMING = 2 bytes
//! size 7   set('', '')                   ok                -- 2 + (0 + 0 + oh) <= 7  =>  oh <= 5
//! size 6   set('', '')                   NotAllowedError   -- 2 + oh > 6            =>  oh > 4
//! size 16  set('a','b') ok, then set('c','de')  NotAllowedError
//!                                                          -- 2+(2+oh)+(3+oh) > 16  =>  oh > 4.5
//! ```
//!
//! so **the stored size is `2 + SUM over entries of (key.len + value.len + 5)`, and an operation
//! that would leave it above the requested buffer size fails with `NotAllowedError`** — which
//! reproduces every one of the sixteen rows exactly, with no free parameter left.
//!
//! **THE ONE READING IT DOES NOT REPRODUCE**, recorded rather than fitted away: in a 4096-byte
//! buffer already holding three small entries, 66 further entries of a 4-or-5-byte key and a 50-byte
//! value fit and the 67th did not; this rule predicts 67. Every other large-buffer boundary agrees
//! (a 4050-byte value fits and a 4090-byte one does not; a 4000-byte key fits and a 5000-byte one
//! does not). So the per-entry cost is right to within one entry in 67 and the framing is exact, and
//! that one entry of slack is what is not understood. Byte lengths are counted in UTF-8; every string
//! the sweep used was ASCII, so the non-ASCII case is measured by nobody and is named here for the
//! next lane rather than assumed.
//!
//! # Exposure
//!
//! **Unconditional**, measured (FP5.7/FP5.9): `crashReport` is an own property of `window` on
//! `http://web-platform.test:<p>/` — `isSecureContext` false — on all four browsers. It is absent
//! from `tools/sync/data/secure_context.json`, and that file's silence IS the reading.

use std::cell::RefCell;
use std::collections::BTreeMap;

use deno_core::v8;

use crate::binding::protos::{build_fn_data, make_dom_exc, require_args, throw_dom, throw_type_error};
use crate::install::installer::{self, Flavor};
use crate::install::MemberDef;
use crate::install::kits::branded;

/// The FRAMING: the two bytes an empty context still occupies, from the `size 1` / `size 2` boundary.
const FRAMING: u64 = 2;
/// The PER-ENTRY overhead, from the `size 6` / `size 7` and `size 16` boundaries — see the header.
const PER_ENTRY: u64 = 5;

/// This realm's crash-report context: the buffer it was given, and what is in it.
///
/// `budget` is `None` until `initialize`'s promise SETTLES, which is the state the device's
/// "not initialized. Call initialize() and wait for it to resolve." sentence is about, and `called`
/// is separate because a second `initialize` must reject even while the first is still pending.
#[derive(Default)]
struct CrashReportStore {
    called: RefCell<bool>,
    budget: RefCell<Option<u64>>,
    entries: RefCell<BTreeMap<String, String>>,
}

/// Every realm's context, at the index the realm keeps under [`REALM_SLOT`]
/// ([`crate::binding::protos::realm_slot`]).
///
/// ONE PER DOCUMENT, as the device keeps it -- measured 2026-09-28 on Chrome 150.0.7871.46 with a
/// same-origin frame, after the page's `initialize(100)` had resolved: the frame's `crashReport` is its
/// own object, its `set` before ITS `initialize` is `InvalidStateError`, its `initialize` resolves, and
/// the page's second `initialize` rejects. One per runtime made a frame's context the page's, once
/// frames had realms of their own.
#[derive(Default)]
struct CrashReportRealms {
    realms: RefCell<Vec<std::rc::Rc<CrashReportStore>>>,
}

const REALM_SLOT: &str = "korvus_crash_report";

/// Run `f` with the CURRENT realm's context: the realm of the method being called, which the brand
/// check has already tied to the receiver's.
fn with_store<R>(scope: &mut v8::PinScope, f: impl FnOnce(&CrashReportStore) -> R) -> R {
    let held = crate::binding::protos::realm_slot(scope, REALM_SLOT).filter(|v| v.is_number()).and_then(|v| v.uint32_value(scope));
    let rc = deno_core::JsRuntime::op_state_from(scope);
    if rc.borrow().try_borrow::<CrashReportRealms>().is_none() {
        rc.borrow_mut().put(CrashReportRealms::default());
    }
    let store = {
        let st = rc.borrow();
        let mut realms = st.borrow::<CrashReportRealms>().realms.borrow_mut();
        match held.and_then(|i| realms.get(i as usize).cloned()) {
            Some(s) => Ok(s),
            None => {
                realms.push(Default::default());
                Err(((realms.len() - 1) as u32, realms[realms.len() - 1].clone()))
            }
        }
    };
    let store = match store {
        Ok(s) => s,
        Err((i, s)) => {
            let n = v8::Integer::new_from_unsigned(scope, i);
            crate::binding::protos::set_realm_slot(scope, REALM_SLOT, n.into());
            s
        }
    };
    f(&store)
}

/// The stored size of a map, by the rule the header derives.
fn stored_size(entries: &BTreeMap<String, String>) -> u64 {
    entries.iter().fold(FRAMING, |n, (k, v)| n + k.len() as u64 + v.len() as u64 + PER_ENTRY)
}


const NOT_INIT: &str = "CrashReportContext is not initialized. Call initialize() and wait for it to resolve.";
const TOO_LARGE: &str = "The crash report data is too large to be stored in the requested buffer.";

/// The `key`/`value` arguments, as WebIDL `DOMString` (so `set(1, 2)` stores `"1"`/`"2"` and
/// `set(null, null)` stores `"null"`/`"null"` — both measured `ok`).
fn arg_string(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> String {
    args.get(i).to_rust_string_lossy(scope)
}

/// `set(key, value)` — the budget check comes after the receiver and the arity, and it is applied to
/// the map the operation WOULD leave behind, which is why replacing a key with a shorter value can
/// succeed where adding it would not.
fn cb_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if !require_args(scope, &args, 2, "CrashReportContext", "set") {
        return;
    }
    let key = arg_string(scope, &args, 0);
    let value = arg_string(scope, &args, 1);
    let Some(budget) = with_store(scope, |s| *s.budget.borrow()) else {
        throw_dom(scope, &format!("Failed to execute 'set' on 'CrashReportContext': {NOT_INIT}"), "InvalidStateError");
        return;
    };
    let fits = with_store(scope, |s| {
        let mut next = s.entries.borrow().clone();
        next.insert(key.clone(), value.clone());
        if stored_size(&next) > budget {
            return false;
        }
        *s.entries.borrow_mut() = next;
        true
    });
    if !fits {
        throw_dom(scope, &format!("Failed to execute 'set' on 'CrashReportContext': {TOO_LARGE}"), "NotAllowedError");
    }
}

/// `delete(key)` — a key that is not there is not an error (measured `ok`), but the FRAMING still has
/// to fit, which is why a zero-byte buffer fails this too.
fn cb_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if !require_args(scope, &args, 1, "CrashReportContext", "delete") {
        return;
    }
    let key = arg_string(scope, &args, 0);
    let Some(budget) = with_store(scope, |s| *s.budget.borrow()) else {
        throw_dom(scope, &format!("Failed to execute 'delete' on 'CrashReportContext': {NOT_INIT}"), "InvalidStateError");
        return;
    };
    let fits = with_store(scope, |s| {
        let mut next = s.entries.borrow().clone();
        next.remove(&key);
        if stored_size(&next) > budget {
            return false;
        }
        *s.entries.borrow_mut() = next;
        true
    });
    if !fits {
        throw_dom(scope, &format!("Failed to execute 'delete' on 'CrashReportContext': {TOO_LARGE}"), "NotAllowedError");
    }
}

/// WebIDL `unsigned long long` without `[EnforceRange]`: modulo 2^64, and a non-finite number is 0.
fn to_u64(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u64 {
    let n = v.number_value(scope).unwrap_or(0.0);
    if !n.is_finite() {
        return 0;
    }
    let t = n.trunc();
    let m = 18_446_744_073_709_551_616f64; // 2^64
    let wrapped = t.rem_euclid(m);
    if wrapped >= m {
        0
    } else {
        wrapped as u64
    }
}

/// The MICROTASK that flips this realm's context to INITIALIZED.
///
/// Separate from [`cb_initialize`] because the device's own sentence says so: a `set` in the same
/// synchronous block as the `initialize` call reports "not initialized. **Call initialize() and wait
/// for it to resolve.**", so the state cannot flip inside the call — measured, by a probe that did
/// not await the promise and read "not initialized" from every operation after it.
///
/// The promise is resolved in the call and the flip is queued there, which is the ordering a page
/// sees: `await` queues its continuation when the `await` runs, which is after this microtask was
/// enqueued, so `await cr.initialize(n); cr.set(...)` finds the budget and
/// `cr.initialize(n); cr.set(...)` does not.
fn cb_settle(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let size = to_u64(scope, __body_data);
    with_store(scope, |s| *s.budget.borrow_mut() = Some(size));
}

/// Reject `r` with a `TypeError` carrying `msg`, or with a DOMException of `kind`.
fn reject_with(scope: &mut v8::PinScope, r: v8::Local<v8::PromiseResolver>, msg: &str, kind: Option<&str>) {
    let e = match kind {
        Some(k) => make_dom_exc(scope, msg, k),
        // Blink's context applied after the error is made
        None => Some(deno_core::error::native_error(scope, "TypeError", msg)),
    };
    if let Some(e) = e {
        r.reject(scope, e);
    }
}

/// `initialize(size)` — a promise, and every failure is a REJECTION rather than a throw, which is
/// what a `Promise`-returning WebIDL operation does with its receiver and arity errors too
/// (measured: `CrashReportContext.prototype.initialize.call({}, 1)` hands back a Promise).
fn cb_initialize<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue<'s>) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    rv.set(resolver.get_promise(scope).into());
    if !branded(scope, &args) {
        reject_with(scope, resolver, "Failed to execute 'initialize' on 'CrashReportContext': Illegal invocation", None);
        return;
    }
    if args.length() < 1 {
        reject_with(
            scope,
            resolver,
            "Failed to execute 'initialize' on 'CrashReportContext': 1 argument required, but only 0 present.",
            None,
        );
        return;
    }
    if with_store(scope, |s| std::mem::replace(&mut *s.called.borrow_mut(), true)) {
        reject_with(
            scope,
            resolver,
            "Failed to execute 'initialize' on 'CrashReportContext': The initialize() method has already been called.",
            Some("InvalidStateError"),
        );
        return;
    }
    let size = to_u64(scope, args.get(0));
    let n: v8::Local<v8::Value> = v8::Number::new(scope, size as f64).into();
    let f = build_fn_data(scope, cb_settle, "", 0, n);
    scope.enqueue_microtask(f);
    let u = v8::undefined(scope);
    resolver.resolve(scope, u.into());
}

/// Mount one `CrashReportContext` row. Each body carries the interface prototype as its `data`, which
/// is the brand.
pub fn install_crash_report(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let f = match member.name {
        "delete" => crate::install::kits::proto_data_fn(scope, proto, cb_delete, member.name, member.length as i32),
        "initialize" => {
            // A PROMISE-returning row does its own receiver check and must say so: `install::receiver`'s
            // derived pass wraps anything un-stamped with a guard that THROWS `Illegal invocation`,
            // where the device hands back a REJECTED promise. `install::webidl`'s own promise rows
            // stamp for the same reason and record the same defect.
            let f = crate::install::kits::proto_data_fn(scope, proto, cb_initialize, member.name, member.length as i32);
            crate::binding::protos::stamp_receiver_guarded(scope, f);
            f
        }
        "set" => crate::install::kits::proto_data_fn(scope, proto, cb_set, member.name, member.length as i32),
        other => {
            debug_assert!(false, "install_crash_report: unexpected member {other}");
            return;
        }
    };
    crate::install::kits::define_value(scope, proto, member, f.into(), true);
}

/// The interface from its table and the one `crashReport` singleton, a readonly own accessor.
pub fn install_window_crash_report<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::exposure::{gate, Exposure, Publishes};
    use crate::core::html::window_globals::{define_window_singleton, mint_shell, Mint, WindowSetter};

    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(
        scope,
        global,
        &CRASHREPORTCONTEXT,
        CRASHREPORTCONTEXT.name,
        None,
        None,
        Flavor::Table,
        &mut ledger,
    );
    for g in &built.gaps {
        eprintln!("[korvus] crash-report install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);

    if !gate(scope, Publishes::Global, "crashReport", Exposure::Unconditional) {
        return;
    }
    let Some(o) = mint_shell(scope, "CrashReportContext") else { return };
    define_window_singleton(scope, global, "crashReport", o.into(), WindowSetter::None, true, Mint::Shell("CrashReportContext"));
}

// The table. All four captures record these three prototype methods in this order — `delete`,
// `initialize`, `set`, alphabetical, which is not the IDL's order — with lengths 1/1/2, all
// enumerable, configurable and writable, and `ctorIndex` 3. `new CrashReportContext()` is
// `Illegal constructor` on all four, which `Model::Interface` plus a `stub` ctor row spells.
webapi::interface! {
    "CrashReportContext" [interface, window] {
    ctor   "CrashReportContext"                       len 0                = stub,
    method "delete"      : Undefined len 1 required 1                      = install_crash_report,
    // `required 1` matches the IDL and the row this replaces; the arity is NOT enforced by the
    // installer's guard here (the rows mount through `kits::define_value`, like
    // `core/feature_policy/feature_policy.rs`'s four), because a `Promise`-returning operation must
    // REJECT for a missing argument rather than throw, and `cb_initialize` does that itself.
    method "initialize"  : Promise   len 1 required 1                      = install_crash_report,
    method "set"         : Undefined len 2 required 2                      = install_crash_report
    }
}
