//! `core/dom/mutation_observer.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::protos::{get_prop, str_val};
use crate::core::events::event::evt_slot_write;


// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// ---- Observer classes. MutationObserver delivers REAL records. observe/disconnect/takeRecords are
// NATIVE ([native code], mounted from the table's rows), and SO IS EVERYTHING BEHIND THEM:
// the (observer -> registrations + queued records) registry, the microtask delivery and the record
// mint are the bottom half of this file. They used to be the JS `KB.__mutobs` helper reached through
// a `call_mutobs` bridge -- the same pattern `KB.__ranges` had before W4.e -- and `KB.__ranges` went
// the same way for the same reason. The class keeps its constructor here (it captures the callback);
// the raw-callback methods (below) can't be op2 methods because op2 forbids `#[this]` + an explicit
// `scope` together and they need both (the observer object as the registry key, and a scope).
// ResizeObserver / IntersectionObserver remain stubs (no layout engine). ----

#[repr(C)]
#[derive(CppgcBase)]
pub struct MutationObserver { cb: std::cell::RefCell<Option<v8::Global<v8::Function>>> }

pub(crate) trait MutationObserverBehavior {
   fn construct(_scope: &mut v8::PinScope, _cb: v8::Local<v8::Value>) -> MutationObserver where Self: Sized { stub!("MutationObserver::constructor") }
}

#[webapi(base, own)]
impl MutationObserver {
    #[constructor]
    #[cppgc]
    fn constructor(scope: &mut v8::PinScope, cb: v8::Local<v8::Value>) -> Result<MutationObserver, ::deno_error::JsErrorBox> { dispatch!("MutationObserver::constructor", <MutationObserver as MutationObserverBehavior>::construct(scope, cb)) }
}

impl MutationObserverBehavior for MutationObserver {
    fn construct(scope: &mut v8::PinScope, cb: v8::Local<v8::Value>) -> MutationObserver { let g = v8::Local::<v8::Function>::try_from(cb).ok().map(|f| v8::Global::new(scope, f));
        MutationObserver { cb: std::cell::RefCell::new(g) } }
}

fn mo_callback(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<v8::Global<v8::Function>> {
    deno_core::cppgc::try_unwrap_cppgc_object::<MutationObserver>(scope, this)
        .and_then(|p| unsafe { p.as_ref() }.cb.borrow().clone())
}
fn cb_mo_observe(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    // The callback captured at construction is the registry entry's callback (delivery's 1st-arg target).
    let cb = match mo_callback(scope, this.into()) { Some(g) => v8::Local::new(scope, &g), None => return };
    let target = args.get(0);
    let options = args.get(1);
    observe(scope, this, cb, target, options);
}
fn cb_mo_disconnect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    disconnect(scope, args.this());
}
fn cb_mo_take_records(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let arr = take_records(scope, args.this());
    rv.set(arr.into());
}




// -- The two interfaces, from two tables -------------------------------------------
//
// `dom/08g-observers.js` installed `observe`/`disconnect`/`takeRecords` with three `method()`
// round-trips through the `op_mo_*` builders, then a LATER pass in the same file re-wrapped the
// interface object in `op_nativize_ctor` to get the "parameter 1 is not of type 'Function'" check
// (a JS body wearing `[native code]`). [`cb_mo_ctor`] is that check as a real Rust constructor, so
// the mask goes with the file.
//
// The (observer -> registrations + queued records) registry and the microtask delivery are the bottom
// half of this file; the bodies above call them directly, with no bridge in between.

/// `new MutationObserver(cb)`. WebIDL rejects a non-callable first argument before anything else,
/// with Blink's message; then the cppgc instance is minted and re-prototyped onto the TABLE's
/// prototype (`new.target.prototype` for a subclass, so `class X extends MutationObserver` works).
pub fn cb_mo_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::binding::protos::{get_prop, throw_type_error};
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("MutationObserver"));
        return;
    }
    // WebIDL checks ARITY before type. The `op_nativize_ctor` mask this replaces only had the
    // type check, so `new MutationObserver()` answered "parameter 1 is not of type 'Function'."
    // where Chrome 150.0.7871.46 on this machine answers "1 argument required, but only 0 present."
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'MutationObserver': 1 argument required, but only 0 present.");
        return;
    }
    let cb = args.get(0);
    if !cb.is_function() {
        throw_type_error(scope, "Failed to construct 'MutationObserver': parameter 1 is not of type 'Function'.");
        return;
    }
    let inner = <MutationObserver as MutationObserverBehavior>::construct(scope, cb);
    let obj = crate::binding::class::mint(scope, inner);
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => crate::install::live::set_table_proto(scope, obj, "MutationObserver"),
    }
    rv.set(obj.into());
}

/// The body every row of [`MUTATIONOBSERVER`] names.
pub fn install_mutation_observer(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::method_row;
    match member.name {
        "disconnect" => method_row(scope, proto, member, cb_mo_disconnect),
        "observe" => method_row(scope, proto, member, cb_mo_observe),
        "takeRecords" => method_row(scope, proto, member, cb_mo_take_records),
        other => debug_assert!(false, "install_mutation_observer: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `MutationObserver` block of `install/generated_registry.rs`. Window-only:
// the bundle's `worker.surface.json` lists neither interface.
webapi::interface! {
    "MutationObserver" [value, window] {
        ctor            "MutationObserver" len 1 = stub,
        method          "disconnect"  : Object len 0            = install_mutation_observer,
        method          "observe"     : Object len 1 required 1 = install_mutation_observer,
        method          "takeRecords" : Object len 0            = install_mutation_observer
    }
}

// `MutationRecord`: nine readonly attributes over the instance's private SLOTS.
//
// EVERY ROW WAS AN HONEST `= stub` AND EVERY ROW WAS UNREACHABLE, which is the same sentence twice.
// The records `KB.__mutobs` delivered were plain JS object literals carrying the nine values as OWN
// properties, so they never touched this prototype: `Object.prototype.toString.call(record)` read
// `[object Object]`, `record instanceof MutationRecord` was false, and these nine accessors sat on a
// prototype nothing was ever an instance of. `mint_record` below builds the real thing, and the rows
// are `slot_get` -- the same mechanism `AbstractRange`'s four boundary members use.
//
// `MutationRecord` is still `[interface]`: `new MutationRecord()` is
// `TypeError: Failed to construct 'MutationRecord': Illegal constructor` in Chrome, and the records
// are minted from Rust, never constructed.
webapi::interface! {
    "MutationRecord" [interface, window] {
        ctor            "MutationRecord" len 0 = stub,
        attr            "type"               : Object readonly          = install_mutation_record,
        attr            "target"             : Object readonly          = install_mutation_record,
        attr            "addedNodes"         : Object readonly          = install_mutation_record,
        attr            "removedNodes"       : Object readonly          = install_mutation_record,
        attr            "previousSibling"    : Object readonly nullable = install_mutation_record,
        attr            "nextSibling"        : Object readonly nullable = install_mutation_record,
        attr            "attributeName"      : Object readonly nullable = install_mutation_record,
        attr            "attributeNamespace" : Object readonly nullable = install_mutation_record,
        attr            "oldValue"           : Object readonly nullable = install_mutation_record
    }
}

// ── THE MUTATION-OBSERVER REGISTRY, IN RUST ──────────────────────────────────────────
//
// This was `core/korvus-runtime/src/js/host.js`'s `KB.__mutobs`: a `Map` of observer -> {callback,
// registrations, queued records}, a microtask delivery loop, and six entry points the native
// mutation methods reached through the `call_mutobs` bridge. The bridge slot and its helper are
// deleted with it -- `core/dom/shared.rs`'s three `notify_*` functions call the three `record_*`
// functions below as plain Rust now, and the three `MutationObserver` bodies above call
// [`observe`] / [`disconnect`] / [`take_records`].
//
// # The defect this closes, which is the reason it moved
//
// `Object.prototype.toString.call(record)` read `[object Object]`. Two separate units measured that
// and both correctly said it was not theirs: the records were JS OBJECT LITERALS minted by
// `KB.__mutobs`, so they were not `MutationRecord`s in any sense a page can check -- and the
// `MutationRecord` table's nine rows, on a prototype no record ever used, were nine `= stub`s that
// nothing could reach. A JS body cannot mint a platform object without a new bridge slot, which the
// goal forbids, so the fix and the move are the same change.
//
// Measured on `google-chrome --headless=new` 150.0.7871.46 over a `file:` URL. Every one of these
// was a difference:
//
//   * `Object.prototype.toString.call(record)` is `[object MutationRecord]`, not `[object Object]`.
//   * `Object.getPrototypeOf(record) === MutationRecord.prototype` and
//     `record instanceof MutationRecord` are BOTH TRUE. Both were false.
//   * `Object.getOwnPropertyNames(record)` is `[]`. It was the nine member names -- every value was
//     an own data property, so `delete record.type` worked and `JSON.stringify(record)` serialised
//     the lot. A browser does neither.
//   * `addedNodes` / `removedNodes` are a `NodeList`, not a plain `Array`. `forEach` and `.length`
//     work in both; `record.addedNodes instanceof NodeList` did not, and `Array.isArray` answered
//     the wrong way round.
//   * the nine members answer through the prototype's ACCESSORS now. `MutationRecord.prototype.type`
//     and `get(…).call({})` still throw `TypeError: Illegal invocation` (the stub rows' receiver
//     check did that too, which is why `layers_live.rs` needs no new expectation); what changes is
//     that a REAL record answers through them instead of shadowing them with own properties.
//
// THE ALGORITHM IS UNCHANGED, deliberately: the same interested-target test (a subtree registration
// matches the target or any descendant; a non-subtree one matches only the exact target), the same
// one-record-per-interested-observer-per-mutation, the same snapshot-then-deliver microtask, the
// same four `observe()` validation TypeErrors in the same order, and the same `sync_frames` call at
// the top of a childList notification. Those were right; they are ported, not rewritten.

/// One `observe(target, options)` registration, with its normalized `MutationObserverInit` flags.
struct Registration {
    target: v8::Global<v8::Object>,
    child_list: bool,
    subtree: bool,
    attributes: bool,
    character_data: bool,
    attribute_old_value: bool,
    character_data_old_value: bool,
    /// `None` = no filter (every attribute); `Some(list)` = only these names.
    attribute_filter: Option<Vec<String>>,
}

/// One observer's registry entry -- the JS `Map`'s value, plus its key.
struct ObserverEntry {
    observer: v8::Global<v8::Object>,
    callback: v8::Global<v8::Function>,
    regs: Vec<Registration>,
    queue: Vec<v8::Global<v8::Object>>,
}

/// The realm's observer registry, in `OpState` -- which is this realm's lifetime, and so the same
/// lifetime the JS `Map` had: `host.js` evaluated once per context and a navigation reuses it.
///
/// `disconnect` CLEARS an entry rather than removing it, exactly as the JS did: the entry carries the
/// observer's identity for a later `observe()`, and the registry's strong reference with it.
#[derive(Default)]
pub(crate) struct MutationRegistry {
    entries: Vec<ObserverEntry>,
    scheduled: bool,
}

/// `OpState`'s registry, created on first use.
///
/// `f` MUST NOT call into JS: it runs while `OpState` is mutably borrowed, and every delivery path
/// here re-enters page script. Each caller takes what it needs out under this borrow, drops it, and
/// only then calls -- the same reentrancy discipline `shared.rs::set_attribute_notify` documents.
fn with_registry<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut MutationRegistry) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<MutationRegistry>().is_none() {
        st.put(MutationRegistry::default());
    }
    f(st.borrow_mut::<MutationRegistry>())
}

/// The index of `observer`'s entry, by OBJECT IDENTITY -- the JS `Map`'s key comparison.
///
/// The globals are copied out first because the identity test needs the scope and `with_registry`
/// holds the `OpState` borrow for the length of its closure.
fn find_entry(scope: &mut v8::PinScope, observer: v8::Local<v8::Object>) -> Option<usize> {
    let obs: Vec<v8::Global<v8::Object>> = with_registry(scope, |r| r.entries.iter().map(|e| e.observer.clone()).collect());
    obs.iter().position(|o| v8::Local::new(scope, o) == observer)
}

// ── the three `MutationObserver` bodies ─────────────────────────────────────────────────────────

/// A member of the CONVERTED `MutationObserverInit`: present (WebIDL "present": the page gave it), and its value.
fn opt_present(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str) -> bool {
    get_prop(scope, o, k).is_some_and(|v| !v.is_undefined())
}
fn opt_truthy(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str) -> bool {
    get_prop(scope, o, k).is_some_and(|v| v.is_true())
}

/// `MutationObserver.observe(target, options)`: normalize the init dictionary, then store or REPLACE
/// the registration for this (observer, target) pair.
///
/// The four TypeErrors are Blink's, in Blink's order, with the operation's context in front of each.
pub(crate) fn observe(
    scope: &mut v8::PinScope,
    observer: v8::Local<v8::Object>,
    callback: v8::Local<v8::Function>,
    target: v8::Local<v8::Value>,
    options: v8::Local<v8::Value>,
) {
    use crate::binding::protos::throw_type_error;
    const PREFIX: &str = "Failed to execute 'observe' on 'MutationObserver': ";
    let Ok(target) = v8::Local::<v8::Object>::try_from(target) else { return };
    // `MutationObserverInit`, converted as WebIDL converts it (Chrome 150): `attributeFilter`
    // (any iterable of strings), `attributeOldValue`, `attributes`, `characterData`, `characterDataOldValue`,
    // `childList`, `subtree`, each read ONCE. korvus read them in its own order, three of them twice, took only an
    // Array for a filter, and threw the four refusals without the operation's context.
    let Ok(opts) = crate::install::ctor_dicts::convert_dict(scope, options, "MutationObserverInit", PREFIX) else { return };
    let attributes = if opt_present(scope, opts, "attributes") {
        opt_truthy(scope, opts, "attributes")
    } else {
        opt_present(scope, opts, "attributeOldValue") || opt_present(scope, opts, "attributeFilter")
    };
    let character_data = if opt_present(scope, opts, "characterData") {
        opt_truthy(scope, opts, "characterData")
    } else {
        opt_present(scope, opts, "characterDataOldValue")
    };
    let child_list = opt_truthy(scope, opts, "childList");
    let subtree = opt_truthy(scope, opts, "subtree");
    let attribute_old_value = opt_truthy(scope, opts, "attributeOldValue");
    let character_data_old_value = opt_truthy(scope, opts, "characterDataOldValue");
    // the converted `sequence<DOMString>`: an array of strings
    let attribute_filter = get_prop(scope, opts, "attributeFilter")
        .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
        .map(|arr| {
            (0..arr.length())
                .map(|i| arr.get_index(scope, i).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default())
                .collect::<Vec<String>>()
        });

    if attribute_old_value && !attributes {
        throw_type_error(scope, &format!("{PREFIX}The options object may only set 'attributeOldValue' to true when 'attributes' is true or not present."));
        return;
    }
    if attribute_filter.is_some() && !attributes {
        throw_type_error(scope, &format!("{PREFIX}The options object may only set 'attributeFilter' when 'attributes' is true or not present."));
        return;
    }
    if character_data_old_value && !character_data {
        throw_type_error(scope, &format!("{PREFIX}The options object may only set 'characterDataOldValue' to true when 'characterData' is true or not present."));
        return;
    }
    if !child_list && !attributes && !character_data {
        throw_type_error(scope, &format!("{PREFIX}The options object must set at least one of 'attributes', 'characterData', or 'childList' to true."));
        return;
    }

    let entry = find_entry(scope, observer);
    // Replace the registration for the SAME target, else append -- the JS's
    // `st.regs.find(r => sameNode(r.target, target))`. Node wrappers are canonical (one cached
    // wrapper per arena node), so identity IS the comparison.
    let slot = match entry {
        None => None,
        Some(i) => {
            let targets: Vec<v8::Global<v8::Object>> =
                with_registry(scope, |r| r.entries[i].regs.iter().map(|g| g.target.clone()).collect());
            targets.iter().position(|t| v8::Local::new(scope, t) == target)
        }
    };
    let rec = Registration {
        target: v8::Global::new(scope, target),
        child_list,
        subtree,
        attributes,
        character_data,
        attribute_old_value,
        character_data_old_value,
        attribute_filter,
    };
    let cb = v8::Global::new(scope, callback);
    let obs = v8::Global::new(scope, observer);
    with_registry(scope, |reg| match entry {
        None => reg.entries.push(ObserverEntry { observer: obs, callback: cb, regs: vec![rec], queue: Vec::new() }),
        Some(i) => {
            reg.entries[i].callback = cb;
            match slot {
                Some(j) => reg.entries[i].regs[j] = rec,
                None => reg.entries[i].regs.push(rec),
            }
        }
    });
}

/// `MutationObserver.disconnect()`: drop the registrations and the queued records, KEEP the entry.
pub(crate) fn disconnect(scope: &mut v8::PinScope, observer: v8::Local<v8::Object>) {
    let Some(i) = find_entry(scope, observer) else { return };
    with_registry(scope, |reg| {
        reg.entries[i].regs.clear();
        reg.entries[i].queue.clear();
    });
}

/// `MutationObserver.takeRecords()`: the queued records, and the queue is emptied.
pub(crate) fn take_records<'s>(scope: &mut v8::PinScope<'s, '_>, observer: v8::Local<v8::Object>) -> v8::Local<'s, v8::Array> {
    let taken: Vec<v8::Global<v8::Object>> = match find_entry(scope, observer) {
        None => Vec::new(),
        Some(i) => with_registry(scope, |reg| std::mem::take(&mut reg.entries[i].queue)),
    };
    let arr = v8::Array::new(scope, taken.len() as i32);
    for (i, r) in taken.iter().enumerate() {
        let l = v8::Local::new(scope, r);
        arr.set_index(scope, i as u32, l.into());
    }
    arr
}

// ── minting a record ────────────────────────────────────────────────────────────────────────────

/// The nine `MutationRecord` members, in the capture's order -- which is the order the table's rows
/// are in, and the order `Object.getOwnPropertyNames(MutationRecord.prototype)` reports.
const RECORD_MEMBERS: [&str; 9] = [
    "type",
    "target",
    "addedNodes",
    "removedNodes",
    "previousSibling",
    "nextSibling",
    "attributeName",
    "attributeNamespace",
    "oldValue",
];

/// A `NodeList` over `nodes` -- what Chrome hands back from `addedNodes` / `removedNodes`.
///
/// `make_static` is the mint `querySelectorAll` and `labels` already use, so this is the realm's one
/// `NodeList` implementation rather than a second list type for one caller.
fn node_list<'s>(scope: &mut v8::PinScope<'s, '_>, nodes: &[v8::Local<v8::Value>]) -> v8::Local<'s, v8::Value> {
    let arr = v8::Array::new(scope, nodes.len() as i32);
    for (i, n) in nodes.iter().enumerate() {
        arr.set_index(scope, i as u32, *n);
    }
    crate::core::dom::collections::make_static(scope, "NodeList", arr)
}

/// The nine members of a `MutationRecord`, each read from the record's one slot by its place in [`RECORD_MEMBERS`].
///
/// A record keeps its nine values in ONE private slot, an array -- not nine: writing nine private properties onto
/// each fresh record was a third of an observed `setAttribute` (perf, 2026-10-22: `v8::Object::SetPrivate` under
/// `mint_record`, 33 %), at 4.3-5.3 us a call against Chrome 152's 0.3 us. `addedNodes` / `removedNodes` of a mutation
/// that added or removed nothing -- every `attributes` and `characterData` record -- hold no list until one is read:
/// the first read makes the empty `NodeList` and keeps it in the array, so the record answers that one list on every
/// read, as Blink's `RecordWithEmptyNodeLists` makes it lazily. Minting both lists for every record had cost another
/// 40 % of that call.
pub fn install_mutation_record(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    let Some(index) = RECORD_MEMBERS.iter().position(|m| *m == member.name) else {
        debug_assert!(false, "install_mutation_record: unexpected member {}", member.name);
        return;
    };
    let d: v8::Local<v8::Value> = v8::Integer::new(scope, index as i32).into();
    let g = crate::binding::protos::build_fn_data(scope, cb_record_get, &format!("get {}", member.name), 0, d);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}

fn cb_record_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // `webidl::receiver(this, "MutationRecord")`'s test, answered first by the template brand every record minted
    // from `MutationRecord`'s template carries (an internal-field read once the brand is cached on it), then by the
    // WebIDL brand, which every other record carries ([`mint_record`]) and which is all the old test accepted:
    // the same receivers pass, and the rest get the same `Illegal invocation`.
    let this_v: v8::Local<v8::Value> = args.this().into();
    let ok = record_id().is_some_and(|t| crate::binding::brand::implements_id(scope, this_v, t))
        || crate::install::webidl::has_brand(scope, this_v, "MutationRecord");
    if !ok {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let this = args.this();
    let index = crate::binding::protos::body_data(scope, &args).uint32_value(scope).unwrap_or(0);
    let key = crate::core::events::event::slot_key(scope, RECORD_SLOT);
    let Some(values) = this.get_private(scope, key).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
    let v = values.get_index(scope, index).unwrap_or_else(|| v8::undefined(scope).into());
    // `addedNodes` / `removedNodes` left empty at minting: the list is made now, once.
    if v.is_undefined() && (index == 2 || index == 3) {
        let list = node_list(scope, &[]);
        values.set_index(scope, index, list);
        rv.set(list);
        return;
    }
    rv.set(v);
}

/// The private slot a record keeps its nine values in ([`install_mutation_record`]).
const RECORD_SLOT: &str = "mutationRecord";
/// [`RECORD_SLOT`]'s private, as `event::slot_key` names it: declared on the record template
/// (`install::instance_shapes::declare_engine_slots`).
pub(crate) const RECORD_SLOT_PRIVATE: &str = "korvus_evt_mutationRecord";

/// `MutationRecord`'s template brand id. Remembered once known -- an id never changes once allocated -- and asked
/// again while it is not: a fresh blueprint cache builds the template (and allocates the id) at the first mint, and
/// a getter can be probed before any record exists.
fn record_id() -> Option<u32> {
    static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    if let Some(&i) = ID.get() {
        return Some(i);
    }
    let i = crate::binding::brand::id_of("MutationRecord")?;
    let _ = ID.set(i);
    Some(i)
}

/// This realm's `MutationRecord.prototype`, looked up once and kept on the realm's global: a record is minted for every
/// observed mutation, and the interface lookup by name it went through each time was a tenth of an observed
/// `setAttribute`. The interface object's `prototype` is not writable, so the one kept is the one a lookup would find.
fn record_prototype<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let global = scope.get_current_context().global(scope);
    let key = crate::core::events::event::slot_key(scope, "mutationRecordPrototype");
    if let Some(p) = global.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(p);
    }
    let proto = crate::binding::protos::iface_object(scope, "MutationRecord")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    global.set_private(scope, key, proto.into());
    Some(proto)
}

/// A REAL `MutationRecord`: an object on the table's prototype whose nine members are private SLOTS,
/// read back by the prototype's `slot_get` accessors. It owns nothing, which is what Chrome reports.
fn mint_record<'s>(scope: &mut v8::PinScope<'s, '_>, values: [v8::Local<v8::Value>; 9]) -> Option<v8::Local<'s, v8::Object>> {
    let proto = record_prototype(scope)?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let arr = v8::Array::new_with_elements(scope, &values);
    evt_slot_write(scope, obj, RECORD_SLOT, arr.into());
    // The brand the `slot_get` rows' receiver check wants: without it every accessor would answer
    // `Illegal invocation` on a genuine record, which is failure mode 2 of a table cutover. A record minted from
    // the template already carries the template's (the check reads that first, and this caches it in the
    // object's brand field); only one minted otherwise is stamped -- a SetPrivate map transition per record saved.
    if !record_id().is_some_and(|t| crate::binding::brand::implements_id(scope, obj.into(), t)) {
        crate::install::webidl::brand(scope, obj, "MutationRecord");
    }
    Some(obj)
}

/// Everything a record of one mutation carries EXCEPT the per-registration `oldValue` bit.
///
/// One struct rather than nine arguments threaded through the queue loop, because the loop mints one
/// record per interested observer and every field but `old` is the same for all of them.
struct Mutation<'a> {
    kind: &'static str,
    target: v8::Local<'a, v8::Value>,
    added: &'a [v8::Local<'a, v8::Value>],
    removed: &'a [v8::Local<'a, v8::Value>],
    prev: Sib<'a>,
    next: Sib<'a>,
    attribute_name: Option<&'a str>,
    namespace: v8::Local<'a, v8::Value>,
    /// The value before the change, made a V8 string only when a registration that wants it matched.
    old_value: Option<&'a str>,
}

/// A childList record's `previousSibling` / `nextSibling` as its caller has it: none, a node's wrapper, or a node
/// by arena id, wrapped only when an observer's registration matched ([`enqueue`]). Most insertions and removals
/// are observed by no one, and a sibling the page never touched (a parser-made node) had a wrapper minted per
/// mutation only for the record nobody made. The id resolves to the wrapper an earlier mint would have made:
/// `NodeCache` keeps one per id and an id is never reused, and nothing between the capture and the record runs
/// script -- except frame bookkeeping, before which [`record_child_list`] resolves them.
#[derive(Clone, Copy)]
pub(crate) enum Sib<'a> {
    Null,
    Id(u32),
    Node(v8::Local<'a, v8::Value>),
}

impl<'a> Sib<'a> {
    pub(crate) fn of(v: Option<v8::Local<'a, v8::Value>>) -> Self {
        v.map_or(Sib::Null, Sib::Node)
    }

    pub(crate) fn of_id(id: Option<u32>) -> Self {
        id.map_or(Sib::Null, Sib::Id)
    }

}

/// The record's value for `sib`: `null`, the node's wrapper.
fn sib_value<'c, 'a: 'c, 's: 'c>(sib: Sib<'a>, scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'c, v8::Value> {
    match sib {
        Sib::Null => v8::null(scope).into(),
        Sib::Node(v) => v,
        Sib::Id(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            crate::binding::protos::wrap_cached(scope, &op_state, id).into()
        }
    }
}

/// `sib` with an id made its wrapper now.
fn sib_resolved<'c, 'a: 'c, 's: 'c>(sib: Sib<'a>, scope: &mut v8::PinScope<'s, '_>) -> Sib<'c> {
    match sib {
        Sib::Id(_) => Sib::Node(sib_value(sib, scope)),
        Sib::Null => Sib::Null,
        Sib::Node(v) => Sib::Node(v),
    }
}

// ── queueing and delivery ───────────────────────────────────────────────────────────────────────

/// Does a registration on `reg_target` cover `target`? A subtree registration matches the target or
/// any of its descendants (ancestor-OR-self); a non-subtree one matches only the exact target.
/// A node wrapper's arena id: an element-or-other node's own, a Document's root.
fn arena_id(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<u32> {
    if let Some(p) = deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::JsElement>(scope, o.into()) {
        return Some(unsafe { p.as_ref() }.id);
    }
    let slot = deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, o.into()).map(|p| unsafe { p.as_ref() }.root)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<crate::binding::HostDom>().0;
    Some(crate::core::dom::document::resolve_doc_root(dom, slot))
}

/// `target_id` is the mutation target's arena id once a subtree registration has needed it (`None` until then):
/// one lookup per mutation, not one per registration.
fn matches_target(
    scope: &mut v8::PinScope,
    reg_target: v8::Local<v8::Object>,
    subtree: bool,
    target: v8::Local<v8::Object>,
    target_id: &mut Option<Option<u32>>,
) -> bool {
    if reg_target == target {
        return true;
    }
    if !subtree {
        return false;
    }
    // Up the ARENA, not through the page's `parentNode` getter: a page that wraps
    // `parentNode` saw a call per ancestor per mutation, and each one was a JS round trip.
    let Some(r) = arena_id(scope, reg_target) else { return false };
    let t = match *target_id {
        Some(t) => t,
        None => {
            let t = arena_id(scope, target);
            *target_id = Some(t);
            t
        }
    };
    let Some(t) = t else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<crate::binding::HostDom>().0;
    let mut cur = Some(t);
    while let Some(n) = cur {
        if n == r {
            return true;
        }
        cur = dom.parent(n);
    }
    false
}

/// Queue ONE record per interested observer (the spec's "queue a mutation record" over the node's
/// interested observers), then schedule delivery.
///
/// `gate` decides whether a registration wants this KIND of record; `wants_old` reads the old-value
/// flag that belongs to this kind, because `oldValue` is PER REGISTRATION -- only an observer that
/// asked for it sees one, and the other observers of the same mutation see `null`.
fn enqueue(
    scope: &mut v8::PinScope,
    target: v8::Local<v8::Object>,
    gate: impl Fn(&Registration) -> bool,
    wants_old: fn(&Registration) -> bool,
    mutation: &Mutation<'_>,
) {
    let hits = interested(scope, target, gate, wants_old);
    enqueue_hits(scope, &hits, mutation);
}

/// The observers interested in a mutation of `target` -- Blink's `MutationObserverInterestGroup` -- as the index of
/// each one's entry and whether its registration wants the old value. Empty for the unobserved write.
fn interested(
    scope: &mut v8::PinScope,
    target: v8::Local<v8::Object>,
    gate: impl Fn(&Registration) -> bool,
    wants_old: fn(&Registration) -> bool,
) -> Vec<(usize, bool)> {
    // Every gated registration of every observer, in order, as Locals of the registered targets, copied out of
    // the borrow: the identity walk below reads the arena and must not be holding `OpState`. A realm with no
    // registry, or none that wants this kind of record, stops here -- no registry made, no Vec, no handle: the
    // unobserved write, which is nearly every write. (A Global clone per registration and a Vec per observer,
    // made and dropped per mutation, were ~5 % of an observed setAttribute, profiled 2026-10-23.)
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let regs: Vec<(usize, v8::Local<v8::Object>, bool, bool)> = {
        let st = op_state.borrow();
        let Some(reg) = st.try_borrow::<MutationRegistry>() else { return Vec::new() };
        let mut regs = Vec::new();
        for (i, e) in reg.entries.iter().enumerate() {
            for r in e.regs.iter().filter(|r| gate(r)) {
                regs.push((i, v8::Local::new(scope, &r.target), r.subtree, wants_old(r)));
            }
        }
        regs
    };
    // The FIRST matching registration of each observer decides -- the JS's `break`.
    let mut hits: Vec<(usize, bool)> = Vec::new();
    let mut target_id = None;
    for (i, t, subtree, old) in regs {
        if hits.last().is_some_and(|&(h, _)| h == i) {
            continue;
        }
        if matches_target(scope, t, subtree, target, &mut target_id) {
            hits.push((i, old));
        }
    }
    hits
}

/// Queue `mutation`'s record for each of `hits` ([`interested`]), then schedule delivery.
fn enqueue_hits(scope: &mut v8::PinScope, hits: &[(usize, bool)], mutation: &Mutation<'_>) {
    if hits.is_empty() {
        return;
    }
    let mut old_string: Option<v8::Local<v8::Value>> = None;
    let (prev, next) = (sib_value(mutation.prev, scope), sib_value(mutation.next, scope));
    let kind = crate::binding::protos::key_val(scope, mutation.kind);
    for &(i, old) in hits {
        // One NodeList PER RECORD, as Chrome mints one per record: two observers of the same
        // mutation get two lists, and `a.addedNodes === b.addedNodes` is false in both engines.
        // An empty list is made when it is first read (`cb_record_nodes_get`), not here.
        let added = if mutation.added.is_empty() { v8::undefined(scope).into() } else { node_list(scope, mutation.added) };
        let removed = if mutation.removed.is_empty() { v8::undefined(scope).into() } else { node_list(scope, mutation.removed) };
        let null: v8::Local<v8::Value> = v8::null(scope).into();
        let name = match mutation.attribute_name {
            Some(n) => str_val(scope, n),
            None => null,
        };
        let old_v = match (old, mutation.old_value) {
            (true, Some(s)) => *old_string.get_or_insert_with(|| str_val(scope, s)),
            _ => null,
        };
        let values = [kind, mutation.target, added, removed, prev, next, name, mutation.namespace, old_v];
        let Some(rec) = mint_record(scope, values) else { continue };
        let g = v8::Global::new(scope, rec);
        with_registry(scope, |reg| reg.entries[i].queue.push(g));
    }
    schedule(scope);
}

/// Schedule the microtask checkpoint, once.
///
/// Through the realm's own `queueMicrotask`, which is what the JS did and what the dialog-close and
/// canvas-`toBlob` lanes already do from Rust: one delivery per checkpoint, drained by the engine's
/// task source at the same moment it drained the JS one.
fn schedule(scope: &mut v8::PinScope) {
    if with_registry(scope, |reg| std::mem::replace(&mut reg.scheduled, true)) {
        return;
    }
    let f = crate::binding::protos::build_fn(scope, cb_deliver, "", 0);
    let global = {
        let ctx = scope.get_current_context();
        ctx.global(scope)
    };
    // The realm's own `queueMicrotask`: a page that wraps it (zone.js) or replaces it
    // must not see -- or break -- the observer's delivery.
    let Some(qm) = crate::binding::protos::intrinsic_fn(scope, "queueMicrotask") else {
        // No `queueMicrotask` in this realm: un-schedule, so the next mutation tries again rather
        // than the queue silently never draining.
        with_registry(scope, |reg| reg.scheduled = false);
        return;
    };
    let recv: v8::Local<v8::Value> = global.into();
    let arg: v8::Local<v8::Value> = f.into();
    qm.call(scope, recv, &[arg]);
}

/// The microtask checkpoint: hand each observer the records queued UP TO NOW.
///
/// The observer set and each queue are SNAPSHOT first, because a callback may `observe()` or
/// `disconnect()` during delivery -- anything queued by a callback waits for the next checkpoint,
/// which is the JS's `[...reg]` copy and its `st.queue = []` before the call.
fn cb_deliver(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let batches: Vec<(v8::Global<v8::Object>, v8::Global<v8::Function>, Vec<v8::Global<v8::Object>>)> =
        with_registry(scope, |reg| {
            reg.scheduled = false;
            reg.entries
                .iter_mut()
                .filter(|e| !e.queue.is_empty())
                .map(|e| (e.observer.clone(), e.callback.clone(), std::mem::take(&mut e.queue)))
                .collect()
        });
    for (obs, cb, queue) in batches {
        let obs = v8::Local::new(scope, &obs);
        let cb = v8::Local::new(scope, &cb);
        let arr = v8::Array::new(scope, queue.len() as i32);
        for (i, r) in queue.iter().enumerate() {
            let l = v8::Local::new(scope, r);
            arr.set_index(scope, i as u32, l.into());
        }
        let recv: v8::Local<v8::Value> = obs.into();
        let a0: v8::Local<v8::Value> = arr.into();
        let a1: v8::Local<v8::Value> = obs.into();
        // A throwing callback is REPORTED and the next observer still runs -- the JS's
        // `try { … } catch (e) { KB.__recordUncaught(e) }`, which is `report_exception` in Rust. A
        // frame's observer runs as the frame's realm.
        crate::core::html::browsing_context::as_callback_realm(scope, cb, |scope| {
            v8::tc_scope!(let tc, scope);
            #[cfg(feature = "analysis")]
            crate::analysis_hook::call_traced(tc, "MutationCallback", cb, recv, &[a0, a1]);
            #[cfg(not(feature = "analysis"))]
            cb.call(tc, recv, &[a0, a1]);
            if tc.has_caught() {
                match tc.exception() {
                    Some(err) => {
                        // reported where it was thrown (`report::caught_site`)
                        let m = tc.message();
                        let site = crate::core::events::report::caught_site(tc, err, m);
                        tc.reset();
                        crate::core::events::report::report_exception_at(tc, err, site);
                    }
                    None => tc.reset(),
                }
            }
        });
    }
}

// ── the three notification entry points (`core/dom/shared.rs`'s `notify_*` call these) ──────────

/// A childList mutation on `target`.
///
/// `sync_frames` runs FIRST and unconditionally, exactly where the JS ran it: a frame entering or
/// leaving the tree creates or destroys a nested browsing context, and a browser reflects that in
/// `window[0]`, `window[1]`, … in the SAME turn as the mutation. MutationObserver delivery is a
/// microtask and would be too late for a page that appends a frame and reads `window[0]` in the next
/// statement. `sync_frames` returns immediately for a page with no frames.
pub(crate) fn record_child_list(
    scope: &mut v8::PinScope,
    target: v8::Local<v8::Value>,
    added: &[v8::Local<v8::Value>],
    removed: &[v8::Local<v8::Value>],
    prev: Sib,
    next: Sib,
) {
    // Frame bookkeeping below can run script (a nested context made or synced), and a sibling given by id must be
    // the wrapper it is before that: resolved first whenever it can act -- the target is outside the page's
    // document (`sync_frames_at` acts), or a frame may enter or leave (`sync_frames`). The two tests are the ones
    // the bookkeeping makes, made once: `sync_frames_at` answers false exactly when the target is not off the page.
    let off = off_page(scope, target);
    let may_change = !off && crate::core::html::browsing_context::frames_may_change(scope, added);
    let (prev, next) = if off || may_change { (sib_resolved(prev, scope), sib_resolved(next, scope)) } else { (prev, next) };
    if off {
        crate::core::html::browsing_context::sync_frames_at(scope, target, added);
    } else if may_change {
        let arr = v8::Array::new(scope, added.len() as i32);
        for (i, n) in added.iter().enumerate() {
            arr.set_index(scope, i as u32, *n);
        }
        let av: v8::Local<v8::Value> = arr.into();
        v8::tc_scope!(let tc, scope);
        crate::core::html::browsing_context::sync_frames(tc, false, av);
        tc.reset();
    }
    let Ok(target_o) = v8::Local::<v8::Object>::try_from(target) else { return };
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let m = Mutation {
        kind: "childList",
        target,
        added,
        removed,
        prev,
        next,
        attribute_name: None,
        namespace: null,
        old_value: None,
    };
    // A childList record has no old value at all, so no registration wants one.
    enqueue(scope, target_o, |r| r.child_list, |_| false, &m);
}

/// Whether `target` is a node whose tree is not the page document's (`sync_frames_at`'s test for acting).
fn off_page(scope: &mut v8::PinScope, target: v8::Local<v8::Value>) -> bool {
    let Some(t) = crate::binding::protos::unwrap_id(scope, target) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<crate::binding::HostDom>().0;
    dom.root_of(t) != dom.root()
}

/// An attributes mutation on `target`. `old` is the value BEFORE the change; only a registration
/// that asked for `attributeOldValue` receives it, and `attributeFilter` (a name allow-list) gates
/// the record entirely -- and never admits an attribute in a namespace (DOM's "queue a mutation record": the
/// filter lists names, and a namespaced attribute is skipped). Chrome 152 (2026-10-23):
/// `setAttributeNS('urn:x', 'p:foo', '1')` reaches neither `attributeFilter: ['foo']` nor `['p:foo']`.
///
/// `ns` is made a V8 value, and the record built, only for a write an observer hears: the unobserved one -- nearly
/// every attribute write -- asks the registry and returns. It made a `null` handle and the record first, out of line
/// once the inliner stopped taking this function into `shared::notify_attributes`.
#[inline]
pub(crate) fn record_attributes(
    scope: &mut v8::PinScope,
    target: v8::Local<v8::Value>,
    name: &str,
    ns: Option<&str>,
    old: Option<&str>,
) {
    let Ok(target_o) = v8::Local::<v8::Object>::try_from(target) else { return };
    let hits = interested(
        scope,
        target_o,
        |r| r.attributes && r.attribute_filter.as_ref().is_none_or(|f| ns.is_none() && f.iter().any(|n| n == name)),
        |r| r.attribute_old_value,
    );
    if hits.is_empty() {
        return;
    }
    let ns: v8::Local<v8::Value> = match ns {
        Some(n) => str_val(scope, n),
        None => v8::null(scope).into(),
    };
    let empty: [v8::Local<v8::Value>; 0] = [];
    let m = Mutation {
        kind: "attributes",
        target,
        added: &empty,
        removed: &empty,
        prev: Sib::Null,
        next: Sib::Null,
        attribute_name: Some(name),
        namespace: ns,
        old_value: old,
    };
    enqueue_hits(scope, &hits, &m);
}

/// The `style` attribute mutation a CSSOM write to element `id`'s inline block is (`el.style.x = v`, `setProperty`,
/// `removeProperty`, `cssText`, `cssFloat`): Blink's `StyleAttributeMutationScope`, opened around `write`.
///
/// korvus queued no record for any of them and ran no custom-element reaction, as if only `setAttribute('style')`
/// changed the attribute. Chrome 152 (headless, 2026-10-23):
///   * an `attributes` record for `style` exactly when the write changed the declaration block -- none for a value
///     the property rejects, the value it already holds, or a property the block does not have removed -- and for
///     `cssText` ALWAYS (the same text again, `''` on an element with no `style` attribute: one each);
///   * its `oldValue` the attribute as it was: `null` without one, `"color:red"` as `setAttribute` wrote it;
///   * an observed `style`'s `attributeChangedCallback` on EVERY write that names a property, the rejected and the
///     unchanged ones too (old and new value then alike, `null>null` on an element without the attribute); none
///     for a name that is no property, which returns before the scope opens. A Typed OM write's waits for the next
///     microtask checkpoint (`StylePropertyMap` has no [CEReactions]), so only the writers no Typed OM write goes
///     through ask for it here (`css_style_declaration::mutate_inline`).
///
/// `always`: the write is `cssText`. `reacts`: the write runs the reaction here. `text`: the block's serialization,
/// which differs after the write exactly when the block changed -- read before it only when an observer is
/// interested in this element's `style`. The unobserved write to an element that is no custom element (nearly every
/// write, on a page that defines custom elements too) reads `OpState` once more than it did and the element's tag.
pub(crate) fn style_attribute_mutation(
    scope: &mut v8::PinScope,
    id: u32,
    always: bool,
    reacts: impl FnOnce(&mut v8::PinScope) -> bool,
    text: impl Fn(&korvus_dom::Dom, u32) -> String,
    write: impl FnOnce(&mut korvus_dom::Dom),
) {
    use crate::binding::HostDom;
    let gate = |r: &Registration| r.attributes && r.attribute_filter.as_ref().is_none_or(|f| f.iter().any(|n| n == "style"));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Only an autonomous custom element -- a tag with a `-`, which every valid custom element name has -- can have a
    // definition (`custom_element_registry::attribute_changed` finds it by the tag), so a write to any other element
    // asks nothing of the registry. Asking whether the page had defined one sent every write of a page that has --
    // to a plain `<div>` -- down the reaction path: the target wrapped, the property looked up, the attribute read
    // twice, `+18.6 %` instructions for `el.style.width = v` with one definition; asked this way it is `+1.1 %` over
    // the build before the scope existed, as on a page with none (2026-10-23, r2/l3/r1/perf, 13897 -> 14049).
    let (observed, custom) = {
        let st = op_state.borrow();
        let observed = st.try_borrow::<MutationRegistry>().is_some_and(|r| r.entries.iter().any(|e| e.regs.iter().any(gate)));
        (observed, st.borrow::<HostDom>().0.tag_name(id).is_some_and(|t| t.contains('-')))
    };
    let react = custom && !crate::core::html::custom_element_registry::nothing_defined(scope) && reacts(scope);
    if !observed && !react {
        let mut st = op_state.borrow_mut();
        write(&mut st.borrow_mut::<HostDom>().0);
        return;
    }
    let target = crate::binding::protos::wrap_cached(scope, &op_state, id);
    let hits = if observed { interested(scope, target, gate, |r| r.attribute_old_value) } else { Vec::new() };
    let (old, before) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        (dom.get_attribute(id, "style").map(str::to_string), (!hits.is_empty() && !always).then(|| text(dom, id)))
    };
    write(&mut op_state.borrow_mut().borrow_mut::<HostDom>().0);
    let (changed, new) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let changed = always || before.is_some_and(|b| b != text(dom, id));
        (changed, react.then(|| dom.get_attribute(id, "style").map(str::to_string)).flatten())
    };
    if changed {
        let null: v8::Local<v8::Value> = v8::null(scope).into();
        let empty: [v8::Local<v8::Value>; 0] = [];
        let m = Mutation {
            kind: "attributes",
            target: target.into(),
            added: &empty,
            removed: &empty,
            prev: Sib::Null,
            next: Sib::Null,
            attribute_name: Some("style"),
            namespace: null,
            old_value: old.as_deref(),
        };
        enqueue_hits(scope, &hits, &m);
    }
    if react {
        crate::core::html::custom_element_registry::attribute_changed(scope, target.into(), "style", old, new);
    }
}

/// A characterData mutation on `target` (a Text / Comment / ProcessingInstruction node). `old` is
/// the data BEFORE the change; only a registration that asked for `characterDataOldValue` sees it.
pub(crate) fn record_character_data(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, old: Option<&str>) {
    let Ok(target_o) = v8::Local::<v8::Object>::try_from(target) else { return };
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let empty: [v8::Local<v8::Value>; 0] = [];
    let m = Mutation {
        kind: "characterData",
        target,
        added: &empty,
        removed: &empty,
        prev: Sib::Null,
        next: Sib::Null,
        attribute_name: None,
        namespace: null,
        old_value: old,
    };
    enqueue(scope, target_o, |r| r.character_data, |r| r.character_data_old_value, &m);
}

#[cfg(test)]
mod tests {
    #[test]
    fn record_slot_private_is_slot_keys_name() {
        assert_eq!(super::RECORD_SLOT_PRIVATE, format!("korvus_evt_{}", super::RECORD_SLOT));
    }
}
