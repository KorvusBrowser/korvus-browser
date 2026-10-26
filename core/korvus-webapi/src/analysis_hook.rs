//! The call recorder's capture (korvus-analysis Layer 1; compiled only with the `analysis` feature).
//!
//! A page call that enters a template function comes through [`traced`]: the receiver, the arguments,
//! the JS stack and the realm are read HERE, on the JS thread, because a V8 value can be read nowhere
//! else -- then the body runs, and its result (or its throw) is read the same way. The record is handed
//! to `spec::analysis`, whose thread-local batch and non-blocking channel take it off this thread
//! (`core/korvus-kernel/src/analysis.rs` says how a full writer drops rather than waits).
//!
//! Reading a value never runs JS: no `toString`, no getter, no proxy trap. A primitive is copied (a long
//! string cut at `STR_CAP`), an object is its constructor name and identity hash, and a proxy is named
//! a proxy without being touched.
use deno_core::v8;
use spec::analysis::{self, Frame, Kind, STR_CAP};

/// A binding call's value, digested by the patched V8 at the call's exit (`korvus_v8_call_exit`,
/// tools/v8-trace): the kinds of `korvus_v8_classify`, plus 13 for a string the buffer could not take.
#[cfg(feature = "analysis-v8")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Digest {
    kind: i32,
    hash: i32,
    num: f64,
    len: u32,
    off: u32,
    value: *const v8::Value,
}

/// Arguments digested in the one exit call (more take the general path, one by one).
#[cfg(feature = "analysis-v8")]
const DIGEST_ARGS: usize = 16;
/// The digest's string buffer (bytes): a string past it takes the general path.
#[cfg(feature = "analysis-v8")]
const DIGEST_STR: usize = 2048;

/// Run a template callback's body as one recorded call (the patched-V8 build).
///
/// Two calls into V8, both in one place: `korvus_v8_call_entry` (is a page script the caller, which
/// realm) and, after the body, `korvus_v8_call_exit` (the receiver, the arguments, the throw, the result,
/// digested). No V8 scope is made unless a value needs the general path (a class not yet cached, a
/// function's name, a BigInt), a realm is new, or the call site is sampled: with a large binding beside
/// it, every separate call across the library was a cold instruction fetch (+14 L1i
/// misses a recorded setter call).
///
/// # Safety
/// `info` is the `FunctionCallbackInfo` V8 handed the calling callback, valid for this call; `body`
/// runs the member on that same info.
#[cfg(feature = "analysis-v8")]
pub(crate) unsafe fn traced(info: *const v8::FunctionCallbackInfo, kind: Kind, label: &'static str, sited: &std::cell::Cell<u32>, body: impl FnOnce()) {
    // the recorder's own time, kept out of the page's clocks (goal T): ONE clock read a call, the exact
    // time every CALL_TIME_EXACT-th (analysis::CallTime); the body runs outside it
    let mut time = analysis::CallTime::enter();
    let mut fp = 0usize;
    let outer_fp = CALL_FP.with(|c| c.get());
    // SAFETY: the info V8 handed this callback; `fp` is this frame's.
    let realm = unsafe { korvus_v8_call_entry(info, outer_fp, &mut fp) };
    // 0: not the page's -- no script beneath it, or the ENGINE calling an API function from inside the
    // recorded binding around it (V8 runs that from C++ with no exit frame of its own, so the binding's
    // frame, and the script under it, are still the innermost: measured, `make_dom_exc`'s
    // `new DOMException(...)` was recorded inside every throwing `querySelector`). V8 compares the
    // frames first, before its frame walk and the realm.
    if realm == 0 {
        time.abandon();
        body();
        return;
    }
    CALL_FP.with(|c| c.set(fp));
    // SAFETY: valid for the duration of the callback (caller's contract).
    let i = unsafe { &*info };
    if LAST_REALM.with(|c| c.get()) != realm {
        // SAFETY: as above.
        let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
        note_realm(&mut scope.init(), realm);
    }
    // the call takes its seq now (the calls the body makes nest under it); its record is written at the
    // exit. The call site is the CALLER's stack, so it is read now -- when this label still samples one.
    let (seq, parent) = analysis::call_open();
    let (site, stack) = if wants_site(sited) {
        // SAFETY: as above.
        let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
        call_site(&mut scope.init(), sited)
    } else {
        (None, Vec::new())
    };
    let t = time.before_body();
    body();
    time.after_body();
    CALL_FP.with(|c| c.set(outer_fp));
    let mut digests = std::mem::MaybeUninit::<[Digest; DIGEST_ARGS + 2]>::uninit();
    let mut strs = std::mem::MaybeUninit::<[u8; DIGEST_STR]>::uninit();
    let (mut argc, mut threw) = (0i32, false);
    // SAFETY: the info V8 handed this callback; V8 writes at most DIGEST_ARGS + 2 digests and at most
    // DIGEST_STR bytes, and the count it returns says how many digests it wrote.
    let w = unsafe {
        korvus_v8_call_exit(info, digests.as_mut_ptr() as *mut Digest, DIGEST_ARGS as i32, strs.as_mut_ptr() as *mut u8, DIGEST_STR as u32, STR_CAP as u32, &mut argc, &mut threw)
    } as usize;
    // SAFETY: the first `w` digests were written.
    let d = unsafe { std::slice::from_raw_parts(digests.as_ptr() as *const Digest, w) };
    let strs = strs.as_ptr() as *const u8;
    let argc = argc as usize;
    let inline = argc.min(DIGEST_ARGS);
    analysis::call_close(seq, parent, t, kind, label, realm, |e| {
        e.some();
        enc_digest(e, i, &d[0], strs);
        e.count(argc as u8);
        for k in 0..argc {
            if k < inline {
                enc_digest(e, i, &d[1 + k], strs);
            } else {
                // SAFETY: as above.
                let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
                let scope = &mut scope.init();
                let args = v8::FunctionCallbackArguments::from_function_callback_info(i);
                enc_val(e, scope, args.get(k as i32));
            }
        }
        if threw {
            e.none();
        } else {
            e.some();
            enc_digest(e, i, &d[1 + inline], strs);
        }
        e.tail(threw, site, &stack);
    });
    time.done();
}

/// Whether this call reads its call site (the sampled first calls of a label, or every call with full
/// stacks): [`call_site`]'s own test, made before a scope exists.
#[cfg(feature = "analysis-v8")]
fn wants_site(sited: &std::cell::Cell<u32>) -> bool {
    analysis::stack_depth() > 0 && (analysis::full_stacks() || sited.get() < analysis::sites_per_label())
}

/// One digested value, encoded as [`enc_val`] encodes it; the kinds V8 left to the embedder (a callable,
/// a BigInt, a string past the buffer) and an object whose class is not cached take [`enc_digest_slow`].
#[cfg(feature = "analysis-v8")]
#[inline]
fn enc_digest(e: &mut analysis::Enc, i: &v8::FunctionCallbackInfo, d: &Digest, strs: *const u8) {
    let n = (d.len as usize).min(STR_CAP);
    match d.kind {
        0 => e.undefined(),
        1 => e.null(),
        2 => e.bool(false),
        3 => e.bool(true),
        4 => e.num(d.num),
        6 => {
            // SAFETY: V8 wrote the string's first `n` bytes at `off`.
            let bytes = unsafe { std::slice::from_raw_parts(strs.add(d.off as usize), n) };
            e.latin1_bytes(d.len as usize, bytes);
        }
        7 => {
            // SAFETY: V8 wrote its first `n` UTF-16 units at `off`, an even offset.
            let units = unsafe { std::slice::from_raw_parts(strs.add(d.off as usize) as *const u16, n) };
            e.utf16(d.len as usize, units);
        }
        8 => e.symbol(),
        10 => e.obj(engine_object(d.hash).filter(|c| !c.is_empty()).unwrap_or("Proxy"), d.hash),
        9 if let Some(c) = recent_class(d.hash) => e.obj(c, d.hash),
        _ => enc_digest_slow(e, i, d),
    }
}

/// [`enc_digest`]'s rare kinds, through a V8 scope and [`enc_val`] (kept out of line: the common path
/// stays small).
#[cfg(feature = "analysis-v8")]
#[cold]
#[inline(never)]
fn enc_digest_slow(e: &mut analysis::Enc, i: &v8::FunctionCallbackInfo, d: &Digest) {
    // SAFETY: the callback's info; the digest's handle lives as long as the callback.
    let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
    let scope = &mut scope.init();
    // SAFETY: `value` is a handle slot of this callback (its receiver, an argument, the return value),
    // non-null, and `Local` is that one pointer (`repr(C)`).
    let v = unsafe { std::mem::transmute::<*const v8::Value, v8::Local<v8::Value>>(d.value) };
    if d.kind == 9 {
        // SAFETY: kind 9 is a JSReceiver that is neither a proxy nor callable: an Object.
        let o = unsafe { v8::Local::<v8::Object>::cast_unchecked(v) };
        let c = class_of_id(scope, o, d.hash);
        e.obj(c, d.hash);
    } else {
        enc_val(e, scope, v);
    }
}

/// Run a template callback's body as one recorded call.
///
/// # Safety
/// `info` is the `FunctionCallbackInfo` V8 handed the calling callback, valid for this call; `body`
/// runs the member on that same info.
#[cfg(not(feature = "analysis-v8"))]
pub(crate) unsafe fn traced(info: *const v8::FunctionCallbackInfo, kind: Kind, label: &'static str, sited: &std::cell::Cell<u32>, body: impl FnOnce()) {
    // the recorder's own time, kept out of the page's clocks (goal T): from the very top, so the scope
    // set-up below counts too; the body runs outside it
    let spent = analysis::Spent::new();
    // SAFETY: valid for the duration of the callback (caller's contract).
    let i = unsafe { &*info };
    let args = v8::FunctionCallbackArguments::from_function_callback_info(i);
    // SAFETY: as above.
    let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
    let scope = &mut scope.init();
    if !from_script(scope) {
        drop(spent);
        body();
        return;
    }
    let realm = realm_id(scope);
    note_realm(scope, realm);
    // Nothing is encoded before the body: the call takes its seq (so the calls the body makes nest
    // under it), and the whole record -- `this` and the arguments too, handles that live for the whole
    // callback -- is written at the exit, straight into the thread's batch (one pass, no
    // scratch copy). The call site is the CALLER's stack, so it is read now.
    let (seq, parent) = analysis::call_open();
    let (site, stack) = call_site(scope, sited);
    let t = spent.at();
    drop(spent);
    // the diagnostic steering run (KORVUS_STEER): Chrome's answer where it differs -- see [`steer`]
    match steer::active() {
        Some(st) => steer::steered(st, scope, i, label, realm, &stack, body),
        None => body(),
    }
    let _spent = analysis::Spent::new();
    // The body's throw is SEEN, not caught: the exception it left pending is read, and propagates
    // exactly as without the recorder (Chromium's recorder does the same). A TryCatch + ReThrow here
    // unwound every throw twice: +~260 ns on a throwing call (2026-10-06, P0 `op throwing`).
    let threw = has_pending_exception(scope);
    analysis::call_close(seq, parent, t, kind, label, realm, |e| {
        e.some();
        enc_val(e, scope, args.this().into());
        let n = args.length().clamp(0, 255);
        e.count(n as u8);
        for k in 0..n {
            enc_val(e, scope, args.get(k));
        }
        if threw {
            e.none();
        } else {
            e.some();
            let mut r = v8::ReturnValue::from_function_callback_info(i).get(scope);
            // a construct call that left the slot alone returns the object V8 made: `this` (what the
            // page receives -- Blink sets the slot to it; measured: `new OffscreenCanvas` recorded undefined)
            if r.is_undefined() && !args.new_target().is_undefined() {
                r = args.this().into();
            }
            enc_val(e, scope, r);
        }
        e.tail(threw, site, &stack);
    });
}

unsafe extern "C" {
    // `bool v8::Isolate::HasPendingException()`: public V8 API the v8 crate does not bind; present in
    // the prebuilt archive and the patched build alike (a member function: `this` is the first argument)
    #[link_name = "_ZN2v87Isolate19HasPendingExceptionEv"]
    fn v8_isolate_has_pending_exception(isolate: *mut v8::RealIsolate) -> bool;
}

/// The C++ isolate under rusty_v8's one-pointer wrapper (`repr(transparent)`): what V8's own entry
/// points are handed, not the wrapper's address.
fn raw_isolate(scope: &mut v8::PinScope) -> *mut v8::RealIsolate {
    let iso: &mut v8::Isolate = scope;
    // SAFETY: the wrapper's only field is the C++ isolate this scope runs on.
    unsafe { *(iso as *mut v8::Isolate as *const *mut v8::RealIsolate) }
}

/// The current JS stack, `depth` frames, through V8's public API -- which leaves the source-position
/// tables it builds behind. korvus's own throw path builds them anyway (a DOMException carries the
/// caller's stack): clearing them after a site capture (`korvus_v8_stack_trace`, what Chromium's recorder
/// uses) made the next throw re-parse the function, ~+150 ns a throwing call over a 25k-throw loop
/// (measured 2026-10-06), where keeping them costs nothing korvus does not already pay.
fn current_stack<'s>(scope: &mut v8::PinScope<'s, '_>, depth: usize) -> Option<v8::Local<'s, v8::StackTrace>> {
    v8::StackTrace::current_stack_trace(scope, depth)
}

/// The realm a call runs in: the identity hash of the current context's global.
fn realm_id(scope: &mut v8::PinScope) -> i32 {
    #[cfg(feature = "analysis-v8")]
    {
        // SAFETY: the isolate this callback runs on; the patched V8 reads its current context.
        unsafe { korvus_v8_realm_id(raw_isolate(scope)) }
    }
    #[cfg(not(feature = "analysis-v8"))]
    {
        scope.get_current_context().global(scope).get_identity_hash().get()
    }
}

/// Whether the calling binding left an exception pending (it threw).
pub(crate) fn has_pending_exception(scope: &mut v8::PinScope) -> bool {
    // SAFETY: the isolate this callback runs on.
    unsafe { v8_isolate_has_pending_exception(raw_isolate(scope)) }
}

/// The engine calling INTO the page (`Kind::Callback`): a listener, a timer, a frame callback, a
/// microtask, an observer. `label` is the IDL callback type invoked, as Chromium's
/// `CallbackInvokeHelper` names it (`EventListener`, `EventHandlerNonNull`, `Function`, ...). Recorded
/// in the realm of the FUNCTION invoked -- its creation context, the realm Blink's callback carries as
/// its script state -- not the context current when the engine calls it: korvus's engine tasks run in
/// the page's context, so a frame's timer and listener callbacks were recorded in the page's realm.
/// The calls the page makes inside it nest under it. Pair with [`callback_end`]. Returns 0 (nothing
/// to end) when the recorder is off.
pub(crate) fn callback_begin(scope: &mut v8::PinScope, label: &'static str, f: v8::Local<v8::Function>, this: v8::Local<v8::Value>, args: &[v8::Local<v8::Value>]) -> u64 {
    if !analysis::enabled() {
        return 0;
    }
    let _spent = analysis::Spent::new();
    let realm = match f.get_creation_context(scope) {
        Some(ctx) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, ctx);
            let realm = realm_id(scope);
            note_realm(scope, realm);
            realm
        }
        None => {
            let realm = realm_id(scope);
            note_realm(scope, realm);
            realm
        }
    };
    analysis::call_begin_with(Kind::Callback, label, realm, |e| {
        e.some();
        enc_val(e, scope, this);
        let n = args.len().min(255);
        e.count(n as u8);
        for a in &args[..n] {
            enc_val(e, scope, *a);
        }
    })
}

/// The callback [`callback_begin`] opened returned `result` (`None`: it threw).
pub(crate) fn callback_end(scope: &mut v8::PinScope, seq: u64, result: Option<v8::Local<v8::Value>>) {
    if seq == 0 {
        return;
    }
    let _spent = analysis::Spent::new();
    analysis::call_end_with(seq, |e| {
        match result {
            Some(r) => {
                e.some();
                enc_val(e, scope, r);
            }
            None => e.none(),
        }
        e.tail(result.is_none(), None, &[]);
    });
}

/// The IDL callback type an observer invokes: its interface's name + `Callback` (`IntersectionObserver`
/// -> `IntersectionObserverCallback`), the name Chromium's generated callback carries.
pub(crate) fn observer_callback_label(scope: &mut v8::PinScope, observer: v8::Local<v8::Object>) -> &'static str {
    let c = class_of(scope, observer);
    analysis::intern_str(&format!("{c}Callback"))
}

/// `f.call(this, args)` as a recorded [`callback_begin`]/[`callback_end`] pair: the one call shape the
/// engine's task sources use (timers, frame callbacks, microtasks, observers). `None` when it threw.
pub fn call_traced<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    label: &'static str,
    f: v8::Local<v8::Function>,
    this: v8::Local<v8::Value>,
    args: &[v8::Local<v8::Value>],
) -> Option<v8::Local<'s, v8::Value>> {
    let seq = callback_begin(scope, label, f, this, args);
    let r = f.call(scope, this, args);
    callback_end(scope, seq, r);
    r
}

/// Whether a script is running: some JS frame is on the stack. A binding reached with none was called by
/// the ENGINE -- korvus's Rust reading `window`/`document` through the same accessors the page sees, a
/// task pump, a style read -- which Chromium's C++ never does through its bindings: not a page call, not
/// recorded (on the coverage page these were 47 % of all records, measured 2026-10-06).
///
/// The patched V8 (`analysis-v8`, tools/v8-trace) answers from raw frame types: is there a JavaScript
/// frame before the nearest entry frame. Without it, V8's referrer-script lookup stands in -- correct,
/// but it summarises the caller's frame, which for optimized code runs the deoptimizer's translation
/// (the largest single cost of a recorded call in a profile, 2026-10-06).
pub(crate) fn from_script(scope: &mut v8::PinScope) -> bool {
    #[cfg(feature = "analysis-v8")]
    {
        // SAFETY: the isolate this callback runs on.
        unsafe { korvus_v8_from_script(raw_isolate(scope)) }
    }
    #[cfg(not(feature = "analysis-v8"))]
    {
        scope.get_current_host_defined_options().is_some()
    }
}

/// An interceptor call's entry (`icpt!`): the holder as `this`, the name or index (and the value) as
/// arguments. Interceptors carry no call site: they are reached from property syntax, and the calls
/// around them carry theirs.
pub(crate) fn icpt_begin(scope: &mut v8::PinScope, label: &'static str, holder: v8::Local<v8::Object>, args: &[v8::Local<v8::Value>]) -> u64 {
    let _spent = analysis::Spent::new();
    let realm = realm_id(scope);
    note_realm(scope, realm);
    analysis::call_begin_with(Kind::Call, label, realm, |e| {
        e.some();
        enc_val(e, scope, holder.into());
        e.count(args.len() as u8);
        for a in args {
            enc_val(e, scope, *a);
        }
    })
}

/// An interceptor call's exit: the return-value slot (undefined when the callback declined, as
/// Chromium's recorder reads it too), or `None` when it threw.
pub(crate) fn icpt_end(scope: &mut v8::PinScope, seq: u64, result: Option<v8::Local<v8::Value>>) {
    let _spent = analysis::Spent::new();
    analysis::call_end_with(seq, |e| {
        match result {
            Some(r) => {
                e.some();
                enc_val(e, scope, r);
            }
            None => e.none(),
        }
        e.tail(result.is_none(), None, &[]);
    });
}

/// An interceptor's return-value slot as a value: what a getter, query, deleter or enumerator left there;
/// a setter's or definer's slot carries nothing to compare (`undefined`, as Chromium's recorder writes).
pub(crate) trait RvValue {
    fn value<'s>(&self, scope: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value>;
}
macro_rules! rv_value_of {
    ($($t:ty),*) => {$(
        impl RvValue for v8::ReturnValue<'_, $t> {
            fn value<'s>(&self, scope: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
                self.get(scope)
            }
        }
    )*};
}
rv_value_of!(v8::Value, v8::Integer, v8::Boolean, v8::Array);
impl RvValue for v8::ReturnValue<'_, ()> {
    fn value<'s>(&self, scope: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
        v8::undefined(scope).into()
    }
}

thread_local! {
    /// interceptor labels by (holder class, operation): both `&'static str`, keyed by address
    static ICPT_LABELS: std::cell::RefCell<korvus_dom::FxHashMap<(usize, usize), &'static str>> = Default::default();
}

/// The label of an interceptor several interfaces share (`icpt!(…, holder, …)`): `op` on the interface
/// that DECLARES the getter for the holder's class, which is where Chromium installs (and labels) it.
pub(crate) fn interceptor_label(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, op: &'static str, indexed: bool) -> &'static str {
    let class = class_of(scope, holder);
    let key = (class.as_ptr() as usize ^ class.len(), op.as_ptr() as usize);
    if let Some(l) = ICPT_LABELS.with(|m| m.borrow().get(&key).copied()) {
        return l;
    }
    let l = analysis::intern(declaring_iface(class, indexed), op);
    ICPT_LABELS.with(|m| m.borrow_mut().insert(key, l));
    l
}

/// The interface whose IDL declares the indexed (or named) getter a `class` instance answers through:
/// itself, unless it inherits the getter (WebIDL; Chromium generates the interceptor on the declaring
/// interface only).
pub(crate) fn declaring_iface(class: &'static str, indexed: bool) -> &'static str {
    match (class, indexed) {
        // (HTMLFormControlsCollection's indexed getter: Chromium generates its own, measured)
        ("HTMLOptionsCollection", false) => "HTMLCollection",
        ("RadioNodeList", _) => "NodeList",
        _ => class,
    }
}

thread_local! {
    /// the exit frame of the recorded binding call this thread is inside (0: none): a binding entered
    /// with the SAME innermost exit frame was called by the engine from inside it, not by the page
    #[cfg(feature = "analysis-v8")]
    static CALL_FP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// the realm the thread's last record was in: a run of calls in one realm checks nothing else
    static LAST_REALM: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    /// realms described before they had a URL
    static URL_PENDING: std::cell::RefCell<std::collections::HashSet<i32>> = Default::default();
    /// the URL each realm was last described with: a frame's realm is made on `about:blank` and described there,
    /// then shows its navigated document -- described again when that changes, or a side-by-side cannot pair the
    /// challenge frame with Chrome's (its realm read `about:blank` for the whole run)
    static URL_SEEN: std::cell::RefCell<std::collections::HashMap<i32, String>> = Default::default();
}

/// Describe `realm` (its global's class, its URL) the first time this thread records in it, so a diff
/// pairs realms by identity, not by the order they appeared.
///
/// A realm first seen before its document has a URL (the engine's bootstrap) is described again, on each
/// new call, until it has one; the reader keeps a realm's last record.
fn note_realm(scope: &mut v8::PinScope, realm: i32) {
    if LAST_REALM.with(|c| c.get()) == realm {
        return;
    }
    let first = analysis::realm_is_new(realm);
    if !first && !URL_PENDING.with(|p| p.borrow().contains(&realm)) {
        // described with a URL already: again, with its role, only when its document's URL changed
        let url = crate::core::html::browsing_context::realm_url_for_analysis(scope);
        if !url.is_empty() && URL_SEEN.with(|s| s.borrow().get(&realm).is_some_and(|u| *u != url)) {
            let global = scope.get_current_context().global(scope);
            let class = global_class(scope, global);
            analysis::realm(realm, class, &url);
            #[cfg(not(feature = "analysis-v8"))]
            steer::note_role(realm, class, &url);
            URL_SEEN.with(|s| s.borrow_mut().insert(realm, url));
        }
        LAST_REALM.with(|c| c.set(realm));
        return;
    }
    let global = scope.get_current_context().global(scope);
    let class = global_class(scope, global);
    let url = crate::core::html::browsing_context::realm_url_for_analysis(scope);
    if url.is_empty() {
        if first {
            analysis::realm(realm, class, &url);
            URL_PENDING.with(|p| p.borrow_mut().insert(realm));
        }
        return;
    }
    analysis::realm(realm, class, &url);
    URL_SEEN.with(|s| s.borrow_mut().insert(realm, url.clone()));
    #[cfg(not(feature = "analysis-v8"))]
    steer::note_role(realm, class, &url);
    URL_PENDING.with(|p| p.borrow_mut().remove(&realm));
    LAST_REALM.with(|c| c.set(realm));
}

/// The label of a function built outside every interface template (`build_fn` and kin), found on its
/// first call: `Holder.name`, where the holder is the first object on the receiver's prototype chain
/// that owns the property the function is installed under -- the global's own members are `Window`'s,
/// a prototype's its interface's (the seal's stamp), a constructor's (a static) its own name. A call
/// that constructs (`new Image()`) is labelled on the interface of the object it makes. The lookups
/// skip interceptors and stop at a proxy, so no page code runs. `?.name` when no holder is found
/// (the caller then tries again next call).
///
/// # Safety
/// `info` is the `FunctionCallbackInfo` V8 handed the calling callback, valid for this call.
pub(crate) unsafe fn resolve_label(info: *const v8::FunctionCallbackInfo, name: &'static str) -> &'static str {
    let _spent = analysis::Spent::new();
    // SAFETY: valid for the duration of the callback (caller's contract).
    let i = unsafe { &*info };
    let args = v8::FunctionCallbackArguments::from_function_callback_info(i);
    // SAFETY: as above.
    let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(i) });
    let scope = &mut scope.init();
    let global = scope.get_current_context().global(scope);
    let this = v8::Local::<v8::Object>::try_from(args.this()).unwrap_or(global);
    // an engine stand-in's functions: a trap of its handler is engine machinery ("?": never recorded); a
    // member taken out of the stand-in is the platform object's ("Window.postMessage", as Chrome's)
    match engine_object(this.get_identity_hash().get()) {
        Some("") => return "?",
        Some(class) => return analysis::intern(class, name),
        None => {}
    }
    if !args.new_target().is_undefined() {
        let iface = this
            .get_prototype(scope)
            .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())
            .and_then(|p| crate::binding::protos::iface_name_of(scope, p))
            .map(|n| analysis::intern_str(&string_or_empty(scope, n)));
        return match iface {
            Some(i) if !i.is_empty() => analysis::intern(i, name),
            _ => analysis::intern("?", name),
        };
    }
    let key_name = name.strip_prefix("get ").or_else(|| name.strip_prefix("set ")).unwrap_or(name);
    let Some(key) = v8::String::new(scope, key_name) else { return analysis::intern("?", name) };
    let mut obj = this;
    let on_global = this.strict_equals(global.into());
    for depth in 0..64 {
        if obj.is_proxy() {
            break;
        }
        if obj.has_real_named_property(scope, key.into()).unwrap_or(false) {
            // the global PROXY's first step is V8's global object behind it, which carries no stamp: a
            // property found there is the global's own
            let iface = if on_global && depth <= 1 && crate::binding::protos::iface_name_of(scope, obj).is_none() {
                global_class(scope, global)
            } else {
                holder_iface(scope, obj, global)
            };
            return analysis::intern(iface, name);
        }
        match obj.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok()) {
            Some(p) => obj = p,
            None => break,
        }
    }
    analysis::intern("?", name)
}

/// A realm global's interface (`Window`, `DedicatedWorkerGlobalScope`): the stamp on its prototype. The
/// global PROXY's constructor name is `Object`, which names nothing.
fn global_class(scope: &mut v8::PinScope, global: v8::Local<v8::Object>) -> &'static str {
    let stamped = global
        .get_prototype(scope)
        .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())
        .and_then(|p| crate::binding::protos::iface_name_of(scope, p))
        .map(|n| string_or_empty(scope, n));
    match stamped {
        Some(n) if !n.is_empty() => analysis::intern_str(&n),
        // Window.prototype need not carry the seal's stamp; a browsing context's global is a Window
        // (measured: the window getter was labelled `Object.get window` on every page)
        _ if is_window(scope, global) => "Window",
        _ => class_of(scope, global),
    }
}

/// Whether `global` is a browsing context's (the page's or a frame's) window.
fn is_window(scope: &mut v8::PinScope, global: v8::Local<v8::Object>) -> bool {
    crate::core::html::browsing_context::main_global(scope).strict_equals(global.into())
        || crate::core::html::browsing_context::ctx_of_window(scope, global).is_some()
}

/// The interface a holder object stands for (see [`resolve_label`]).
fn holder_iface(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, global: v8::Local<v8::Object>) -> &'static str {
    if obj.strict_equals(global.into()) {
        return global_class(scope, global);
    }
    if let Some(n) = crate::binding::protos::iface_name_of(scope, obj) {
        let n = string_or_empty(scope, n);
        if !n.is_empty() {
            return analysis::intern_str(&n);
        }
    }
    if let Ok(f) = v8::Local::<v8::Function>::try_from(obj) {
        let n: v8::Local<v8::Value> = f.get_name(scope).into();
        return analysis::intern_str(&string_or_empty(scope, n));
    }
    // a realm's global OBJECT (V8's, behind the proxy -- some lookups hand it over as the receiver):
    // unstamped, but its prototype is its global interface's (measured: `window` labelled
    // `Object.get window` on a replayed page)
    let g = global_class(scope, obj);
    if g == "Window" || g.ends_with("GlobalScope") {
        return g;
    }
    class_of(scope, obj)
}

// ---- C2: builtins and constants, reported by V8 itself (tools/v8-trace) ----------------------------------

#[cfg(feature = "analysis-v8")]
type V8TraceFn = unsafe extern "C" fn(
    *mut v8::RealIsolate,
    *const std::ffi::c_char,
    i32,
    i32,
    v8::Local<'static, v8::Value>,
    i32,
    *const v8::Local<'static, v8::Value>,
    i32,
);

#[cfg(feature = "analysis-v8")]
unsafe extern "C" {
    /// the patched V8's entry point (`src/runtime/runtime-test.cc`, tools/v8-trace)
    fn korvus_v8_set_trace(f: Option<V8TraceFn>);
    /// V8's `Date` leaves the recorder's time out (goal T)
    fn korvus_v8_set_recorder_clock(f: Option<extern "C" fn() -> i64>);
    /// is the innermost caller script (a JS frame before the nearest entry frame)
    fn korvus_v8_from_script(isolate: *mut v8::RealIsolate) -> bool;
    /// TSC ticks the patched V8 has spent in its korvus hooks (timed, plus counted early exits x cost)
    fn korvus_v8_hook_ticks() -> u64;
    /// hooks paused for bytecode generated meanwhile (the calibration loop)
    fn korvus_v8_pause_hooks(paused: bool);
    /// one hook early exit's cost, TSC ticks
    fn korvus_v8_set_exit_cost(ticks: u64);
    /// forget the early exits counted so far
    fn korvus_v8_reset_exit_counts();
    /// a binding call's entry: its realm, 0 when no page script made it
    fn korvus_v8_call_entry(info: *const v8::FunctionCallbackInfo, outer_fp: usize, exit_fp: *mut usize) -> i32;
    /// a binding call's exit: its receiver, arguments, throw and result, digested
    fn korvus_v8_call_exit(info: *const v8::FunctionCallbackInfo, out: *mut Digest, max_args: i32, sbuf: *mut u8, scap: u32, str_cap: u32, argc: *mut i32, threw: *mut bool) -> i32;
    /// the current context's global's identity hash, in one call
    fn korvus_v8_realm_id(isolate: *mut v8::RealIsolate) -> i32;
    /// a value's kind and digest fields in one call (kinds in runtime-test.cc and `enc_val`)
    fn korvus_v8_classify(isolate: *mut v8::RealIsolate, value: *const v8::Value, hash: *mut i32, num: *mut f64, len: *mut u32, buf: *mut u8, cap: u32) -> i32;
    /// rusty_v8's binding.cc shim (linked with every rusty_v8)
    fn v8__Isolate__GetCurrentContext(isolate: *mut v8::RealIsolate) -> *const v8::Context;
}

/// The ticks V8 has spent in its korvus hooks (goal T).
#[cfg(feature = "analysis-v8")]
fn v8_hook_ticks() -> u64 {
    // SAFETY: a plain read of a counter the patched V8 exports.
    unsafe { korvus_v8_hook_ticks() }
}

/// [`analysis::recorder_ns`] for V8's `Date` (goal T).
#[cfg(feature = "analysis-v8")]
extern "C" fn recorder_ns() -> i64 {
    analysis::recorder_ns() as i64
}

/// Turn V8's own reports on (`--korvus-trace-builtins` + the callback) when the recorder will be: every
/// script compiled from here on reports its builtin calls and constant reads. V8's flags stay writable
/// after init in korvus (`v8_features`), so this may run at each runtime's creation.
#[cfg(feature = "analysis-v8")]
pub fn install_builtin_trace() {
    // The recorder opens with the plugins, AFTER the first runtime is made; a script compiled before the
    // flag is set carries no hooks (measured: a page's top-level code reported nothing). So this keys on
    // what the plugin opens from, `KORVUS_ANALYSIS_OUT`; the callback still checks the recorder per call.
    if std::env::var_os("KORVUS_ANALYSIS_OUT").is_none_or(|v| v.is_empty()) {
        return;
    }
    let _ = deno_core::v8_set_flags(vec!["korvus".into(), "--korvus-trace-builtins".into()]);
    // SAFETY: plain function pointers handed to the patched V8, which calls them on the JS thread.
    unsafe {
        korvus_v8_set_trace(Some(builtin_trace));
        korvus_v8_set_recorder_clock(Some(recorder_ns));
    }
    // V8's own hook time is the recorder's too (goal T)
    analysis::add_recorder_ticks(v8_hook_ticks);
}

/// Measure what one of V8's post-call hook EARLY EXITS costs on this machine, once per process, and hand
/// it to V8 (goal T): an exit is too cheap to time, so V8 counts them and the recorder clock credits
/// count x cost. Measured as the same loop of a million-ish script calls compiled with the hooks paused
/// and with them on, in a context of its own (the page never sees it), the cheapest of three each.
/// Untimed, the exits were what a traced page's own clock still saw: ~10-15 ns a script call
/// (2026-10-06, the timing page's getAttribute and querySelector rows).
#[cfg(feature = "analysis-v8")]
const CALIBRATION_JS: &str = include_str!("../../../plugins/korvus-analysis/calibrate.js");

#[cfg(feature = "analysis-v8")]
pub fn calibrate_hook_exits(rt: &mut deno_core::JsRuntime) {
    if std::env::var_os("KORVUS_ANALYSIS_OUT").is_none_or(|v| v.is_empty()) {
        return;
    }
    static DONE: std::sync::Once = std::sync::Once::new();
    let mut first = false;
    DONE.call_once(|| first = true);
    if !first {
        return;
    }
    const N: u64 = 200_000;
    deno_core::scope!(scope, rt);
    let fresh = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, fresh);
    let run = |tag: &str, k: u32| -> u64 {
        // a source of its own per run: no compilation cache hands back the other variant's bytecode. The
        // script is analysis tooling and lives with the analysis plugin, outside the kernel
        let text = CALIBRATION_JS.replace("__N__", &N.to_string()).replace("__TAG__", &format!("{tag} {k}"));
        let Some(src) = v8::String::new(scope, &text) else { return u64::MAX };
        let Some(script) = v8::Script::compile(scope, src, None) else { return u64::MAX };
        #[cfg(target_arch = "x86_64")]
        // SAFETY: RDTSC has no preconditions on x86_64.
        let t0 = unsafe { core::arch::x86_64::_rdtsc() };
        let _ = script.run(scope);
        #[cfg(target_arch = "x86_64")]
        // SAFETY: as above.
        return unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0);
        #[allow(unreachable_code)]
        u64::MAX
    };
    let (mut off, mut on) = (u64::MAX, u64::MAX);
    for k in 0..3 {
        // SAFETY: plain switches of the patched V8 (process-wide atomics).
        unsafe { korvus_v8_pause_hooks(true) };
        off = off.min(run("off", k));
        unsafe { korvus_v8_pause_hooks(false) };
        on = on.min(run("on", k));
    }
    let cost = if on > off && off != u64::MAX { (on - off) / N } else { 0 };
    // SAFETY: as above.
    unsafe {
        korvus_v8_set_exit_cost(cost);
        korvus_v8_reset_exit_counts();
    }
}

#[cfg(feature = "analysis-v8")]
thread_local! {
    /// script id -> whether it is the engine's own (an `ext:`/`korvus:` module): its builtin calls are not
    /// the page's
    static ENGINE_SCRIPT: std::cell::RefCell<korvus_dom::FxHashMap<i32, bool>> = Default::default();
}

/// V8's report: `vals` = receiver (a constant's holder), result (its value), then the arguments.
#[cfg(feature = "analysis-v8")]
unsafe extern "C" fn builtin_trace(
    isolate: *mut v8::RealIsolate,
    label: *const std::ffi::c_char,
    kind: i32,
    script_id: i32,
    script_name: v8::Local<'static, v8::Value>,
    _position: i32,
    vals: *const v8::Local<'static, v8::Value>,
    n: i32,
) {
    if !analysis::enabled() || n < 2 {
        return;
    }
    // (no Spent here: V8 times its whole hook, this included -- korvus_v8_hook_ticks)
    // SAFETY: V8 calls this on the isolate's thread, inside the script's execution, with `n` values.
    // the calling script's context, through rusty_v8's own exported shim (a Local is one non-null
    // pointer, the layout this callback already receives its values in)
    // SAFETY: V8 calls this inside the script's execution, so a context is current.
    let ctx: v8::Local<'static, v8::Context> = unsafe {
        let p = v8__Isolate__GetCurrentContext(isolate);
        if p.is_null() {
            return;
        }
        std::mem::transmute::<*const v8::Context, v8::Local<'static, v8::Context>>(p)
    };
    v8::callback_scope!(unsafe scope, ctx);
    let vals = unsafe { std::slice::from_raw_parts(vals, n as usize) };
    let engine = ENGINE_SCRIPT.with(|m| m.borrow().get(&script_id).copied()).unwrap_or_else(|| {
        let name = string_or_empty(scope, script_name);
        let e = traceio_internal(&name);
        ENGINE_SCRIPT.with(|m| m.borrow_mut().insert(script_id, e));
        e
    });
    if engine {
        return;
    }
    // SAFETY: V8 interns every label for the process (`KorvusIntern`), UTF-8.
    let label: &'static str = unsafe { std::str::from_utf8_unchecked(std::ffi::CStr::from_ptr(label).to_bytes()) };
    let realm = realm_id(scope);
    note_realm(scope, realm);
    let k = if kind == 2 { Kind::Get } else { Kind::Builtin };
    let seq = analysis::call_begin_with(k, label, realm, |e| {
        e.some();
        enc_val(e, scope, vals[0]);
        let na = (vals.len() - 2).min(255);
        e.count(na as u8);
        for a in &vals[2..2 + na] {
            enc_val(e, scope, *a);
        }
    });
    analysis::call_end_with(seq, |e| {
        e.some();
        enc_val(e, scope, vals[1]);
        e.tail(false, None, &[]);
    });
}

/// The engine's own scripts, by name.
#[cfg(feature = "analysis-v8")]
fn traceio_internal(name: &str) -> bool {
    ["korvus:", "ext:", "node:", "chrome://", "chrome-extension://", "devtools://"].iter().any(|p| name.starts_with(p))
}

/// `v`'s digest, encoded (see the module header: never runs JS). A string keeps its first `STR_CAP`
/// UTF-16 units, read as UTF-16 into a stack array and encoded to UTF-8 in place -- the same rule the
/// Chromium recorder applies.
pub(crate) fn enc_val(e: &mut analysis::Enc, scope: &mut v8::PinScope, v: v8::Local<v8::Value>) {
    // the patched V8 digests the value in one call; callables (their name) and the rare kinds take the
    // general path below
    #[cfg(feature = "analysis-v8")]
    {
        let (mut hash, mut num, mut len) = (0i32, 0f64, 0u32);
        let mut buf = std::mem::MaybeUninit::<[u16; STR_CAP]>::uninit();
        let bp = buf.as_mut_ptr() as *mut u8;
        // SAFETY: `v` is a live handle of this isolate; the out-pointers and the STR_CAP-unit buffer are
        // this frame's, and V8 writes at most `cap` units into it.
        let kind = unsafe { korvus_v8_classify(raw_isolate(scope), &*v, &mut hash, &mut num, &mut len, bp, STR_CAP as u32) };
        let n = (len as usize).min(STR_CAP);
        match kind {
            0 => return e.undefined(),
            1 => return e.null(),
            2 => return e.bool(false),
            3 => return e.bool(true),
            4 => return e.num(num),
            6 => {
                // SAFETY: V8 wrote the first `n` bytes.
                let bytes = unsafe { std::slice::from_raw_parts(bp, n) };
                return e.latin1(len as usize, n, |out| out.copy_from_slice(bytes));
            }
            7 => {
                // SAFETY: V8 wrote the first `n` units.
                let units = unsafe { std::slice::from_raw_parts(bp as *const u16, n) };
                return e.utf16(len as usize, units);
            }
            8 => return e.symbol(),
            9 => {
                // SAFETY: kind 9 is a JSReceiver that is neither a proxy nor callable: an Object.
                let o = unsafe { v8::Local::<v8::Object>::cast_unchecked(v) };
                let c = class_of_id(scope, o, hash);
                return e.obj(c, hash);
            }
            10 => return e.obj("Proxy", hash),
            _ => {}
        }
    }
    // OBJECTS FIRST: a receiver is always one and most results are, and each `is_*` below is a call into
    // V8 -- an object used to pass seven tests before reaching its own (the profile)
    if v.is_object() {
        if v.is_proxy() {
            let id = v8::Local::<v8::Object>::try_from(v).map(|o| o.get_identity_hash().get()).unwrap_or(0);
            e.obj(engine_object(id).filter(|c| !c.is_empty()).unwrap_or("Proxy"), id)
        } else if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
            // `Function::GetName` answers a VALUE (rusty_v8 types it a String): a function without a
            // string name made `to_rust_string_lossy` abort in V8's flattening (measured on the surface walk).
            let n: v8::Local<v8::Value> = f.get_name(scope).into();
            e.func(&string_or_empty(scope, n), f.get_identity_hash().get())
        } else if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            // one identity-hash read serves the class cache and the record
            let id = o.get_identity_hash().get();
            let c = class_of_id(scope, o, id);
            e.obj(c, id)
        } else {
            e.undefined()
        }
        return;
    }
    if v.is_undefined() {
        e.undefined()
    } else if v.is_null() {
        e.null()
    } else if v.is_boolean() {
        e.bool(v.is_true())
    } else if v.is_number() {
        e.num(v.number_value(scope).unwrap_or(f64::NAN))
    } else if v.is_big_int() {
        // its first STR_CAP digits, as the Chromium recorder keeps (A1: both cut at the same place)
        let d = v.to_rust_string_lossy(scope);
        e.bigint(&d[..d.len().min(STR_CAP)])
    } else if let Ok(s) = v8::Local::<v8::String>::try_from(v) {
        // the units are copied RAW (Latin-1 bytes or UTF-16): the writer makes UTF-8, not the JS thread
        let len = s.length();
        let n = len.min(STR_CAP);
        if s.is_onebyte() {
            e.latin1(len, n, |out| {
                if n > 0 {
                    s.write_one_byte_v2(scope, 0, out, v8::WriteFlags::empty());
                }
            });
        } else {
            let mut units = [0u16; STR_CAP];
            if n > 0 {
                s.write_v2(scope, 0, &mut units[..n], v8::WriteFlags::empty());
            }
            e.utf16(len, &units[..n]);
        }
    } else if v.is_symbol() {
        e.symbol()
    } else {
        e.undefined()
    }
}

/// `v` as a Rust string when it IS a string, else empty -- never a conversion that could run JS or abort.
fn string_or_empty(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> String {
    if v.is_string() { v.to_rust_string_lossy(scope) } else { String::new() }
}

thread_local! {
    /// constructor name by (prototype, prototype's prototype) identity hashes: two 31-bit hashes, so a
    /// collision that would misname an object needs both to collide. Only interned strings are kept --
    /// never a V8 handle, which must not outlive its isolate.
    static CLASSES: std::cell::RefCell<korvus_dom::FxHashMap<u64, &'static str>> = Default::default();
    /// the last few objects digested, by identity hash: a receiver is usually the same object call after
    /// call, and this skips its two prototype reads
    static RECENT: std::cell::RefCell<[(i32, &'static str); 4]> = const { std::cell::RefCell::new([(0, ""); 4]) };
}

/// `o`'s constructor name, from the cache when its prototype chain head was seen before (the
/// lookup and its string conversion were a large share of the per-call cost).
fn class_of(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> &'static str {
    let id = o.get_identity_hash().get();
    class_of_id(scope, o, id)
}

/// The class of the object `id`, when it is one of the last few digested (no V8 read at all).
fn recent_class(id: i32) -> Option<&'static str> {
    RECENT.with(|r| r.borrow().iter().find(|e| e.0 == id).map(|e| e.1))
}

thread_local! {
    /// Objects the ENGINE minted to stand for a platform object it cannot express natively, by identity
    /// hash: korvus's cross-origin WindowProxy and Location are `v8::Proxy`s (Chrome's are V8
    /// access-checked objects) -> the class Chrome's recorder digests them as ("Window", "Location");
    /// their trap HANDLERS -> "" (engine machinery: a trap call is not a page call).
    static ENGINE_OBJECTS: std::cell::RefCell<korvus_dom::FxHashMap<i32, &'static str>> = Default::default();
}

/// Register an engine-minted stand-in object (see [`ENGINE_OBJECTS`]): `class` is what it is recorded as,
/// "" for a trap handler whose functions are never recorded.
pub(crate) fn note_engine_object(o: v8::Local<v8::Object>, class: &'static str) {
    let id = o.get_identity_hash().get();
    ENGINE_OBJECTS.with(|m| m.borrow_mut().insert(id, class));
}

/// The class an engine stand-in object is recorded as ("" = a trap handler), if `id` is one.
fn engine_object(id: i32) -> Option<&'static str> {
    ENGINE_OBJECTS.with(|m| m.borrow().get(&id).copied())
}

/// [`class_of`] for an object whose identity hash the caller already read.
fn class_of_id(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, id: i32) -> &'static str {
    if let Some(c) = recent_class(id) {
        return c;
    }
    let c = class_by_prototype(scope, o);
    RECENT.with(|r| {
        let mut r = r.borrow_mut();
        r.rotate_right(1);
        r[0] = (id, c);
    });
    c
}

fn class_by_prototype(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> &'static str {
    let p1 = o.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
    let key = p1.map(|p| {
        let h1 = p.get_identity_hash().get() as u32 as u64;
        let h2 = p.get_prototype(scope).and_then(|q| v8::Local::<v8::Object>::try_from(q).ok()).map_or(0, |q| q.get_identity_hash().get() as u32 as u64);
        (h1 << 32) | h2
    });
    if let Some(k) = key {
        if let Some(c) = CLASSES.with(|m| m.borrow().get(&k).copied()) {
            return c;
        }
    }
    let n: v8::Local<v8::Value> = o.get_constructor_name().into();
    let c = analysis::intern_str(&string_or_empty(scope, n));
    if let Some(k) = key {
        CLASSES.with(|m| m.borrow_mut().insert(k, c));
    }
    c
}

/// The call's site, for the first `sites_per_label()` calls of a label on this thread: the top frame's
/// (script id, line, column) in the site table, whose full stack is recorded the first time the site is
/// seen. Every other call carries no stack at all: reading even the innermost script's URL
/// cost ~350 ns a call (measured), and whether a call is the page's is a property of its REALM, which
/// the sampled sites (and, from C1, the realm's own kind and origin) settle. `STACK=full` keeps a stack
/// per call; `STACK=0` records none.
fn call_site(scope: &mut v8::PinScope, sited: &std::cell::Cell<u32>) -> (Option<u32>, Vec<Frame>) {
    let depth = analysis::stack_depth();
    if depth == 0 {
        return (None, Vec::new());
    }
    if analysis::full_stacks() {
        return (None, frames(scope, depth));
    }
    // the member's own counter (in its FusedRec): no lookup per call
    let n = sited.get();
    if n >= analysis::sites_per_label() {
        return (None, Vec::new());
    }
    // `get_frame` does not bounds-check: with no JS frame on the stack (a binding called from V8's own
    // stack formatting, deno_core's prepareStackTrace) index 0 read past the trace and crashed (measured).
    let Some(top) = current_stack(scope, 1).filter(|st| st.get_frame_count() > 0).and_then(|st| st.get_frame(scope, 0)) else {
        return (None, Vec::new());
    };
    sited.set(n + 1);
    let key = (top.get_script_id() as i32, top.get_line_number() as u32, top.get_column() as u32);
    let (id, new) = analysis::site_id(key);
    if new {
        let stack = frames(scope, depth);
        analysis::site(id, stack);
    }
    (Some(id), Vec::new())
}

fn frames(scope: &mut v8::PinScope, depth: usize) -> Vec<Frame> {
    let Some(st) = current_stack(scope, depth) else { return Vec::new() };
    let mut out = Vec::with_capacity(st.get_frame_count());
    for k in 0..st.get_frame_count() {
        let Some(f) = st.get_frame(scope, k) else { continue };
        out.push(Frame {
            script: f.get_script_name(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default(),
            func: f.get_function_name(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default(),
            line: f.get_line_number() as u32,
            col: f.get_column() as u32,
        });
    }
    out
}

#[cfg(not(feature = "analysis-v8"))]
/// Chrome-answer STEERING: a DIAGNOSTIC mode of the analysis build (the owner's method) --
/// never the shipped engine, never a fix.
///
/// A challenge diverges at its first wrong answer, and everything after it runs a path Chrome never ran: a
/// side-by-side then shows the cascade, not the next bug, and each bug costs a live round. Steering replays the
/// SAME recorded flow Chrome passed and hands korvus Chrome's own answers wherever they differ: every binding call
/// is keyed as the traced Chrome's were (`steer_index.py`: realm role, label, the normalised call stack, the
/// occurrence), korvus's answer is compared with Chrome's, and a differing primitive answer is REPLACED by Chrome's
/// (a throw korvus made where Chrome answered is swallowed) -- so the page keeps to Chrome's path and every
/// divergence along the whole flow is listed in one run, each one an answer that differed for the SAME inputs.
/// An answer that cannot be handed over (Chrome threw, or answered an object of another class, or a string past
/// the recorder's cap) is listed as unsteerable. `KORVUS_STEER=<index>` turns it on, `KORVUS_STEER_OUT=<file>`
/// takes the list; it needs `KORVUS_ANALYSIS_STACK=full` (the key is the call's stack).
pub(crate) mod steer {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};

    pub(crate) struct Steer {
        answers: HashMap<String, Vec<Value>>,
        next: Mutex<HashMap<String, usize>>,
        roles: Mutex<(HashMap<i32, String>, HashMap<String, usize>)>,
        out: Mutex<std::io::BufWriter<std::fs::File>>,
    }

    static STEER: OnceLock<Option<Steer>> = OnceLock::new();

    pub(crate) fn active() -> Option<&'static Steer> {
        STEER.get_or_init(load).as_ref()
    }

    fn load() -> Option<Steer> {
        let path = std::env::var("KORVUS_STEER").ok()?;
        let out = std::env::var("KORVUS_STEER_OUT").unwrap_or_else(|_| format!("{path}.out.jsonl"));
        let text = std::fs::read_to_string(&path).ok()?;
        let mut answers = HashMap::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if let (Some(k), Some(a)) = (v.get("k").and_then(Value::as_str), v.get("a").and_then(Value::as_array)) {
                answers.insert(k.to_string(), a.clone());
            }
        }
        let file = std::fs::File::create(&out).ok()?;
        eprintln!("korvus steering: {} keys from {path}; divergences -> {out}", answers.len());
        Some(Steer { answers, next: Mutex::default(), roles: Mutex::default(), out: Mutex::new(std::io::BufWriter::new(file)) })
    }

    /// `steer_index.py`'s `norm`, byte for byte: cut at the first `?` / `#`; `/rch/<5>/` -> `/rch/_/`; a blob:
    /// URL's last segment -> `_`.
    pub(crate) fn norm(u: &str) -> String {
        let mut u = u.to_string();
        if let Some(i) = u.find(['?', '#']) {
            u.truncate(i);
        }
        if let Some(i) = u.find("/rch/") {
            let b = u.as_bytes();
            if b.len() >= i + 11 && b[i + 10] == b'/' && b[i + 5..i + 10].iter().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()) {
                u = format!("{}_{}", &u[..i + 5], &u[i + 10..]);
            }
        }
        if u.starts_with("blob:") {
            if let Some(j) = u.rfind('/') {
                u = format!("{}_", &u[..j + 1]);
            }
        }
        u
    }

    /// A realm's role, fixed when its URL is first known: `global norm(url)#n` (`steer_index.py`). A realm
    /// whose document's URL changed after (a frame's, made on `about:blank` before its navigation commits;
    /// Chrome makes the navigated document a realm of its own) takes its new URL's role, and hands its old
    /// number back when no later realm took one.
    pub(crate) fn note_role(realm: i32, global: &str, url: &str) {
        let Some(s) = active() else { return };
        if url.is_empty() {
            return;
        }
        let base = format!("{global} {}", norm(url));
        let mut r = s.roles.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = r.0.get(&realm).cloned() {
            let Some((old_base, old_n)) = old.rsplit_once('#') else { return };
            if old_base == base {
                return;
            }
            let last = old_n.parse::<usize>().ok().and_then(|n| n.checked_add(1));
            if let Some(c) = r.1.get_mut(old_base) {
                if Some(*c) == last {
                    *c -= 1;
                }
            }
        }
        let n = r.1.entry(base.clone()).or_insert(0);
        let role = format!("{base}#{n}");
        *n += 1;
        r.0.insert(realm, role);
    }

    /// A value as the trace writes it (`analysis::Val`'s JSON): a string's first STR_CAP units and its length, an
    /// object's class.
    fn val_json(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Value {
        if v.is_undefined() {
            json!({"t": "undefined"})
        } else if v.is_null() {
            json!({"t": "null"})
        } else if v.is_boolean() {
            json!({"t": "bool", "v": v.is_true()})
        } else if v.is_number() {
            json!({"t": "num", "v": v.number_value(scope).unwrap_or(f64::NAN)})
        } else if let Ok(s) = v8::Local::<v8::String>::try_from(v) {
            let len = s.length();
            let n = len.min(STR_CAP);
            let mut units = vec![0u16; n];
            if n > 0 {
                s.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
            }
            json!({"t": "str", "v": String::from_utf16_lossy(&units), "len": len})
        } else if v.is_symbol() {
            json!({"t": "symbol"})
        } else if v.is_big_int() {
            json!({"t": "bigint"})
        } else if v.is_function() {
            json!({"t": "fn"})
        } else if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            json!({"t": "obj", "class": class_of(scope, o)})
        } else {
            json!({"t": "undefined"})
        }
    }

    fn same(k: &Value, c: &Value) -> bool {
        let (kt, ct) = (k["t"].as_str(), c["t"].as_str());
        match (kt, ct) {
            (Some("obj"), Some("obj")) => k["class"] == c["class"],
            (Some("fn"), Some("fn")) => true,
            (Some("num"), Some("num")) => match (k["v"].as_f64(), c["v"].as_f64()) {
                (Some(a), Some(b)) => a == b || (a.is_nan() && b.is_nan()),
                (a, b) => a.is_none() && b.is_none(),
            },
            (Some("str"), Some("str")) => k["v"] == c["v"] && k["len"] == c["len"],
            _ => kt == ct && k.get("v") == c.get("v"),
        }
    }

    /// Chrome's answer as a value this isolate can hand the page: a primitive, a string only when the trace holds
    /// all of it.
    fn to_v8<'s>(scope: &mut v8::PinScope<'s, '_>, a: &Value) -> Option<v8::Local<'s, v8::Value>> {
        Some(match a["t"].as_str()? {
            "undefined" => v8::undefined(scope).into(),
            "null" => v8::null(scope).into(),
            "bool" => v8::Boolean::new(scope, a["v"].as_bool()?).into(),
            "num" => v8::Number::new(scope, a["v"].as_f64().unwrap_or(f64::NAN)).into(),
            "str" => {
                let v = a["v"].as_str()?;
                if v.encode_utf16().count() as u64 != a["len"].as_u64()? {
                    return None;
                }
                v8::String::new(scope, v)?.into()
            }
            _ => return None,
        })
    }

    /// Answers that are HANDLES into korvus's own state -- an object URL its blob store resolves, a timer id its
    /// clearTimeout looks up -- are never replaced: Chrome's handle names nothing here (a steered blob: URL made the
    /// worker fetch it over the network). They are listed as `handle`.
    const HANDLES: &[&str] = &[
        "URL.createObjectURL",
        "Window.setTimeout",
        "Window.setInterval",
        "Window.requestAnimationFrame",
        "Window.requestIdleCallback",
        "WorkerGlobalScope.setTimeout",
        "WorkerGlobalScope.setInterval",
        "DedicatedWorkerGlobalScope.requestAnimationFrame",
    ];

    fn log(s: &Steer, line: Value) {
        let mut o = s.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    }

    /// Run `body` (a recorded binding call, its caller's `frames` already read) under a TryCatch, compare its answer
    /// with Chrome's and hand the page Chrome's where they differ and it can be handed over.
    pub(crate) fn steered(s: &Steer, scope: &mut v8::PinScope, i: &v8::FunctionCallbackInfo, label: &'static str, realm: i32, frames: &[Frame], body: impl FnOnce()) {
        let role = s.roles.lock().unwrap_or_else(|e| e.into_inner()).0.get(&realm).cloned();
        let tc = std::pin::pin!(v8::TryCatch::new(scope));
        let tc = &mut tc.init();
        body();
        let threw = tc.has_caught();
        let Some(role) = role else {
            if threw {
                tc.rethrow();
            }
            return;
        };
        let stack = frames.iter().map(|f| format!("{}|{}|{}", norm(&f.script), f.line, f.col)).collect::<Vec<_>>().join(";");
        let key = format!("{role}\u{1f}{label}\u{1f}{stack}");
        let occ = {
            let mut n = s.next.lock().unwrap_or_else(|e| e.into_inner());
            let e = n.entry(key.clone()).or_insert(0);
            *e += 1;
            *e - 1
        };
        let args = v8::FunctionCallbackArguments::from_function_callback_info(i);
        let mut rv = v8::ReturnValue::from_function_callback_info(i);
        let mine = if threw {
            json!({"t": "threw"})
        } else {
            let mut r = rv.get(tc);
            if r.is_undefined() && !args.new_target().is_undefined() {
                r = args.this().into();
            }
            val_json(tc, r)
        };
        let top: Vec<String> = frames.iter().take(3).map(|f| format!("{}:{}:{}", norm(&f.script), f.line, f.col)).collect();
        let expected = s.answers.get(&key).map(|v| v.get(occ).cloned());
        match expected {
            None => log(s, json!({"kind": "korvus-only", "role": role, "label": label, "occ": occ, "top": top, "korvus": mine})),
            Some(None) => log(s, json!({"kind": "korvus-extra", "role": role, "label": label, "occ": occ, "top": top, "korvus": mine})),
            Some(Some(c)) if same(&mine, &c) => {}
            Some(Some(c)) => {
                let argv: Vec<Value> = (0..args.length().min(6)).map(|k| val_json(tc, args.get(k))).collect();
                let handle = HANDLES.contains(&label);
                let injected = match to_v8(tc, &c).filter(|_| !handle) {
                    Some(v) => {
                        rv.set(v);
                        true
                    }
                    None => false,
                };
                let kind = if handle { "handle" } else if injected { "steered" } else { "unsteerable" };
                log(s, json!({"kind": kind, "role": role, "label": label, "occ": occ, "top": top, "args": argv, "korvus": mine, "chrome": c}));
                if injected {
                    // korvus's throw, if it made one, is swallowed: the page goes on with Chrome's answer
                    return;
                }
            }
        }
        if threw {
            tc.rethrow();
        }
    }
}
