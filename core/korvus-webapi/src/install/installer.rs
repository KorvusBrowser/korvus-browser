//! THE installer: every member installed exactly once, by exactly one row.
//!
//! Every own member of every interface prototype and interface object that Rust builds is mounted
//! here, by one function ([`put_member`]) walking one table type ([`InterfaceDef`] / [`MemberDef`]),
//! whether the table is a generated registry row (924 window + 289 worker interfaces) or a
//! hand-written `interface!` (DOMRect, DOMPoint, HTMLImageElement, Navigator). Every mount is recorded
//! in the context's [`InstallLedger`].
//!
//! Two shapes of table reach the installer today, and the difference is a [`Flavor`], not a second
//! code path:
//!
//! * **`Flavor::Shell`** -- the generated registry. Its passes are the shell installer's seven
//!   (`shells_installer.rs`, deleted in W2.3): create + chain, backfill, seal, order, arity,
//!   finalize, and the worker variant with phantom parents. Same native builders (`make_shell_method`,
//!   `make_et_on_*`, `make_evt_slot_*`, `make_arity_guard`), same order of operations, so the surface
//!   is identical by construction; `tests/installer_parity.rs` proves it against the golden captured
//!   from the shell installer. The field map, for a reader coming from the shell code:
//!   n -> name · p -> parent · cl -> ctor.length · t -> tag · ctor==1 -> Model::Value · w -> backfill ·
//!   sl -> slot_backed · m / s -> members split by `static_member` · k "a"/"m"/"v" ->
//!   Attr|StaticAttr|Handler / Method|StaticMethod / Const · e,c -> enumerable,configurable ·
//!   l -> length · g,s -> (accessor, !readonly) · z -> nullable · q -> required · v -> cval.
//! * **`Flavor::Table`** -- a hand-written table. A `Stub` row is the single D2 stub ([`stub`]),
//!   a non-newable interface object throws `Failed to construct 'X': Illegal constructor` on `new`
//!   (and a bare `Illegal constructor` on a call), and the members can be a
//!   bundle SELECTOR's subset in the selector's order (Navigator's worker realm) rather than the
//!   whole table.
//!
//! What a row's BODY means, in every flavor and pass: a `BodyKind::Install` row is mounted by its
//! `install` fn -- the kit or hand body the table names. A `Stub` (or `Cppgc`, on a prototype this
//! installer mints) row takes the flavor's stub: the shell stub for the registry (a writable
//! non-handler accessor gets the slot pair the pre-table shells gave it -- a body the table does not
//! yet name, which W7 makes explicit), the D2 stub for a hand table. A `cppgc` row of an OWN class's
//! table (`class X` in its header) is an `Install` row naming the member
//! `#[webapi(own)]` generated. (Until P6.2 a deno_core class template realised such rows and the
//! installer VERIFIED them in place; that verification, `verify_native`, was deleted with the last
//! op2 class that realised a table member.)
//!
//! Until W7 (one stub) the two flavors differ in the STUB FAMILY. They no longer differ in the
//! non-newable CONSTRUCTOR: every interface object of both flavors has a real
//! `[[Construct]]` that throws Chrome's two measured messages (`Failed to construct 'X': Illegal
//! constructor` on `new`, `Illegal constructor` on a bare call). W7 collapses the stubs onto the D2
//! forms after a golden re-capture.

use deno_core::v8::MapFnTo;
use deno_core::v8;

use super::generated_registry::{REGISTRY, WORKER_ORDER};
use super::receiver;
use super::stub;
use super::{BodyKind, ConstVal, Exposed, InterfaceDef, MemberDef, MemberKind, Model};
use crate::binding::protos::{build_illegal_ctor, build_interface_ctor, cb_shell_ctor, cb_table_value_ctor, define_method, is_arity_guarded, make_arity_guard, member_key};
use crate::core::events::event::{make_evt_slot_get, make_evt_slot_get_null, make_evt_slot_setter};
use crate::core::events::event_target::{make_et_on_getter, make_et_on_setter};

fn str_key<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    // INTERNALIZED, not a fresh string per call. Every key this installer writes is a
    // property name: a member name, a descriptor field, an interface name. V8 keeps one internalized
    // copy of each in the isolate's string table, so the second install of `"configurable"` allocates
    // nothing and every property access made with it takes the identity-compare fast path instead of
    // hashing the characters again. The realm install writes tens of thousands of these, and the
    // string is the SAME string either way -- a page cannot tell two equal JavaScript strings apart.
    v8::String::new_from_utf8(scope, s.as_bytes(), v8::NewStringType::Internalized)
        .expect("a property name is a valid v8 string")
}

/// Put an INTERFACE OBJECT (or a namespace-like legacy factory) on a realm's global object, with the
/// descriptor WebIDL gives it: `{writable: true, enumerable: false, configurable: true}`.
///
/// Every writer that publishes an interface object goes through here. They used a plain
/// `global.set`, which creates an ENUMERABLE property -- so `Object.keys(window)` listed all ~880
/// interface objects on this engine and ~300 names on a browser, and a `for (k in window)` loop, the
/// oldest automation check there is, saw every one. Measured on stock Chrome 150.0.7871.46, Brave
/// 151.1.93.138, Edge 151.0.4129.93 and Opera 135.0.5973.133 over a loopback page: every interface
/// object on `window` is `w=true e=false c=true` in all four, with no exception (the full list is
/// `tests/fixtures/p11a/desc.*.txt`). The shape is the same in every browser of the family, so it is
/// not a persona fact and nothing about it is read from a bundle.
///
/// A define, not a set, and that is what keeps the ORDER: redefining an existing own key changes its
/// attributes in place, and a new key is appended exactly where `set` would have appended it.
pub fn publish_on_global<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str, value: v8::Local<'s, v8::Value>) {
    let Some(k) = v8::String::new(scope, name) else { return };
    let mut d = v8::PropertyDescriptor::new_from_value_writable(value, true);
    d.set_enumerable(false);
    d.set_configurable(true);
    global.define_property(scope, k.into(), &d);
}

// ── the ledger ──────────────────────────────────────────────────────────────────────────────────

/// Which object a member was mounted on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Holder {
    Prototype,
    InterfaceObject,
}

/// Which pass put a member in the ledger: the create pass (a prototype this installer minted), the
/// transitional backfill onto a pre-existing prototype, or the verification of a row a cppgc class
/// template realised natively.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pass {
    Create,
    Backfill,
    Verify,
}

/// One install-ledger row: `(interface, member)` mounted on `holder` by `pass`, in `realm`.
///
/// `realm` is the REALM ORDINAL of the context the row was mounted into (see [`realm_ordinal`]). It
/// is part of the row because the ledger lives in `OpState`, which belongs to the `JsRuntime` and not
/// to the context: a page with an iframe installs the same interfaces twice into one `OpState`, and
/// without the ordinal every one of those would look like a second writer.
#[derive(Clone, Copy, Debug)]
pub struct LedgerEntry {
    pub realm: u32,
    pub iface: &'static str,
    pub member: &'static str,
    pub holder: Holder,
    pub pass: Pass,
}

/// The install ledger (goal invariant I2): every member this installer mounts or verifies, in
/// install order. The ops that drive a context's install keep one in `OpState`;
/// `core/korvus-runtime/tests/install_ledger.rs` reads it back and asserts that what the page SEES in
/// each slot is still what this installer put there.
///
/// IT REJECTS A SECOND RECORD. A second `(realm, interface, member, holder)` is not
/// appended: the first writer keeps the row and the collision is published to
/// [`install_duplicates_json`] naming BOTH passes. I2's own text says why an append-and-count ledger
/// could not do the job -- two writers on one member produce one own member and one ledger row, so a
/// check that every member is present passes either way.
#[derive(Default, Debug)]
pub struct InstallLedger {
    entries: Vec<LedgerEntry>,
    /// `(realm, iface, member, holder)` -> the pass that got there first. The engine's own hasher: every mount of an
    /// install asks it, and SipHash was a visible share of a warm start.
    seen: crate::install::templates::FxMap<(u32, &'static str, &'static str, Holder), Pass>,
}

impl InstallLedger {
    pub fn new() -> Self {
        Self::default()
    }
    /// Record one mount. Returns `false` when the slot was already recorded in this realm -- the row
    /// is REJECTED rather than appended, and the collision is published for the gate.
    fn record(&mut self, realm: Realm, iface: &'static str, member: &'static str, holder: Holder, pass: Pass) -> bool {
        if let Some(first) = self.seen.get(&(realm.0, iface, member, holder)) {
            record_install_duplicate(realm.1, iface, member, holder, *first, pass);
            return false;
        }
        self.seen.insert((realm.0, iface, member, holder), pass);
        self.entries.push(LedgerEntry { realm: realm.0, iface, member, holder, pass });
        true
    }
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }
    pub fn has(&self, iface: &str, member: &str, holder: Holder) -> bool {
        self.entries.iter().any(|e| e.iface == iface && e.member == member && e.holder == holder)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── the realm ordinal ───────────────────────────────────────────────────────────────────────────

/// A small integer identifying the CONTEXT a mount happened in, minted on first use and kept on the
/// context's global object as a `v8::Private` (invisible to every JS reflection path, so it is not a
/// surface tell). Nothing here knows what a realm IS -- a window, an iframe, a worker global all get
/// an ordinal the same way, which is what lets the duplicate check work in every realm without a
/// realm list.
#[derive(Clone, Copy)]
pub struct Realm(pub u32, pub &'static str);

/// The realm a mount is happening in: its ordinal (which CONTEXT) and its label (which KIND).
fn realm_of(scope: &mut v8::PinScope) -> Realm {
    Realm(realm_ordinal(scope), realm_label(scope))
}

/// A stable per-realm number, minted on first use and kept in a V8 Private on the realm's global.
///
/// `pub(crate)` because [`crate::install::surface`] keys the realm's bundle selector by it: one
/// context is one surface, and this is the only identity a v8 context in this tree has.
static NEXT_REALM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

pub(crate) fn realm_ordinal(scope: &mut v8::PinScope) -> u32 {
    use NEXT_REALM as NEXT;
    let global = scope.get_current_context().global(scope);
    let key = {
        let n = v8::String::new(scope, "korvus.realmOrdinal").unwrap();
        v8::Private::for_api(scope, Some(n))
    };
    if let Some(n) = global.get_private(scope, key).and_then(|v| v.uint32_value(scope)).filter(|n| *n != 0) {
        return n;
    }
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nv = v8::Integer::new_from_unsigned(scope, n);
    global.set_private(scope, key, nv.into());
    n
}

/// The context snapshot (slice 3): give the current realm -- a page deserialised from a snapshot, whose
/// global still carries the ordinal of the realm the snapshot was made from -- a fresh ordinal of THIS process, so
/// no other realm here (a frame's, another page's from the same snapshot) shares it.
#[cfg(not(feature = "analysis"))]
pub(crate) fn fresh_ordinal(scope: &mut v8::PinScope) -> u32 {
    let global = scope.get_current_context().global(scope);
    let key = {
        let n = v8::String::new(scope, "korvus.realmOrdinal").unwrap();
        v8::Private::for_api(scope, Some(n))
    };
    let n = NEXT_REALM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nv = v8::Integer::new_from_unsigned(scope, n);
    global.set_private(scope, key, nv.into());
    n
}

// ── the install stamp: which ledger row mounted the function a slot holds now ────────────────────
//
// THE COLUMN THE ROWS DID NOT CARRY. A ledger row says `(interface, member, holder)` was mounted. It
// says nothing about the OBJECT that ended up in the slot, so reading the ledger back could never
// tell "the installer's function is still there" from "something replaced it after the installer
// ran" -- and a JS `Object.defineProperty` makes no ledger call at all, so it leaves no other trace.
// That is why I2's enforcement as originally specified could not catch the defect it existed for.
//
// This is that column. Every function the one member mount leaves in a slot is branded with the row
// that put it there, as `Iface::member`, in a `v8::Private`. Then:
//
//   * brand == the row        -- the installer's own mount is what the page sees.
//   * brand == another row    -- a pass MOVED another table row's body here (the offscreen-canvas
//                               borrow, `09k-svg.js`'s mixin borrow). A re-aim, not a clobber.
//   * no brand, `engineFn`    -- replaced by a function this engine built through `build_fn` /
//                               `build_fn_data` but the installer did not mount: an `op_helper_*` or
//                               `op_nativize` body. Reported `clobber:engine`.
//   * no brand at all         -- replaced by a function with no engine brand either: page or glue JS,
//                               OR a Rust pass that built its function with a raw
//                               `v8::Function::builder` instead of `build_fn` (measured: that is what
//                               `binding::protos::make_constructable_with_data` does when it swaps an
//                               interface object for a constructable one). Reported
//                               `clobber:unbranded` -- the brand says WHO DID NOT write it, and
//                               claiming more than that from one bit would be a guess.
//
// A re-aim keeps the brand because the arity guard CARRIES it across the wrapper it builds
// ([`carry_install_stamp`]), as the receiver re-aim `receiver::rebind` did until it was deleted. That is the whole distinction the ledger asks for,
// and it is deliberate rather than incidental: a pass that re-aims a member declares that it is
// still that row's body, and a writer that does not declare it is a second writer.

fn install_stamp_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, "korvus.installedBy").unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// Brand `f` as the body row `tag` (`Iface::member`) mounted.
fn stamp_installed_by<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<'s, v8::Value>, tag: &str) {
    if !f.is_function() {
        return;
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(f) else { return };
    let key = install_stamp_key(scope);
    let Some(v) = v8::String::new(scope, tag) else { return };
    o.set_private(scope, key, v.into());
}

/// The row that mounted `v`, if this installer mounted it.
pub(crate) fn installed_by(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    if !v.is_function() {
        return None;
    }
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let key = install_stamp_key(scope);
    let got = o.get_private(scope, key)?;
    if !got.is_string() {
        return None;
    }
    Some(got.to_rust_string_lossy(scope))
}

/// Carry the install brand from a function onto the wrapper that now stands in its place.
///
/// Called by every step that REBUILDS a member's function after the mount -- the arity guard
/// ([`guard_member`]), and the receiver re-aim (`receiver::rebind`) until it was deleted. Without it those steps would
/// make the installer's own work indistinguishable from a clobber, and the check would be worked
/// around within a week.
pub(crate) fn carry_install_stamp<'s>(scope: &mut v8::PinScope<'s, '_>, from: v8::Local<'s, v8::Value>, to: v8::Local<'s, v8::Function>) {
    if let Some(tag) = installed_by(scope, from) {
        stamp_installed_by(scope, to.into(), &tag);
    }
}



/// Brand whatever the one member mount just left in `target[name]`, and report the descriptor flags
/// it ended up with so the ledger can hold the installer's own answer to "how was this mounted".
fn stamp_member<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Object>, name: &str, tag: &str) -> Option<(bool, bool)> {
    if !crate::behavior_rt::bookkeeping_on() {
        return None;
    }
    let key = member_key(scope, name);
    let dd = target.get_own_property_descriptor(scope, key).filter(|v| !v.is_undefined())?;
    let dd = v8::Local::<v8::Object>::try_from(dd).ok()?;
    let sk = |scope: &mut v8::PinScope<'s, '_>, s: &str| v8::String::new(scope, s).unwrap();
    for field in ["get", "set", "value"] {
        let fk = sk(scope, field);
        if let Some(v) = dd.get(scope, fk.into()) {
            stamp_installed_by(scope, v, tag);
        }
    }
    let en_k = sk(scope, "enumerable");
    let cfg_k = sk(scope, "configurable");
    let en = dd.get(scope, en_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let cfg = dd.get(scope, cfg_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    Some((en, cfg))
}

// ── what the gate reads back: the duplicate log, the flags log and the interface aliases ─────────

/// `Iface::member@holder` -> `"<first pass>+<second pass>"`, one entry per REJECTED second record.
fn duplicate_map() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, String>> {
    static D: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, String>>> = std::sync::OnceLock::new();
    D.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

fn holder_name(h: Holder) -> &'static str {
    match h {
        Holder::Prototype => "prototype",
        Holder::InterfaceObject => "interface",
    }
}

/// Which realm a scope is in, for the keys the gate reads: `window` when the global names ITSELF
/// `window` (the realm's own self-reference, which no worker global has), `worker` otherwise. Derived
/// from the global, so an iframe's realm answers `window` like the page's -- they install the same
/// surface and a finding in either is a finding in the window realm.
fn realm_label(scope: &mut v8::PinScope) -> &'static str {
    let global = scope.get_current_context().global(scope);
    let Some(k) = v8::String::new(scope, "window") else { return "worker" };
    match global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(w) if w == global => "window",
        _ => "worker",
    }
}

fn record_install_duplicate(realm: &str, iface: &str, member: &str, holder: Holder, first: Pass, second: Pass) {
    let Ok(mut m) = duplicate_map().lock() else { return };
    m.insert(format!("{realm}|{iface}::{member}@{}", holder_name(holder)), format!("{first:?}+{second:?}"));
}

/// The rejected second records, as JSON. Empty is the invariant; a non-empty map names both writers.
pub fn install_duplicates_json() -> String {
    json_of(&duplicate_map().lock().map(|m| m.clone()).unwrap_or_default())
}

/// `exposedName` -> the table name its rows are ledgered under, for the one case where a table is
/// installed under an alias (`Navigator` as the worker realm's `WorkerNavigator`). Recorded by
/// [`create_interface`] when `name != def.name`, so there is no list of aliases anywhere.
fn alias_map() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, String>> {
    static A: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, String>>> = std::sync::OnceLock::new();
    A.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// The descriptor flags the installer left on each row, `Iface::member@holder` -> `"e0c1"`.
fn flags_map() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, String>> {
    static F: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, String>>> = std::sync::OnceLock::new();
    F.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

pub(crate) fn record_install_flags(realm: &'static str, iface: &str, member: &str, holder: Holder, en: bool, cfg: bool) {
    if crate::install::templates::capturing() {
        let (iface, member) = (iface.to_string(), member.to_string());
        crate::install::templates::note_mark(crate::install::templates::Mark::Flags { realm, iface, member, holder, en, cfg });
    }
    if !crate::behavior_rt::bookkeeping_on() {
        return;
    }
    let Ok(mut m) = flags_map().lock() else { return };
    m.insert(format!("{realm}|{iface}::{member}@{}", holder_name(holder)), format!("e{}c{}", en as u8, cfg as u8));
}

fn json_of(m: &std::collections::BTreeMap<String, String>) -> String {
    let body: Vec<String> = m.iter().map(|(k, v)| format!("  {k:?}: {v:?}")).collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

/// THE AUDIT: what the page SEES in every slot the installer claims, classified.
///
/// `Iface::member@holder` -> one of
///   * `ok`            -- the installer's own mount, flags included.
///   * `reaim:<row>`   -- another table row's body, carried here by a declared re-aim/borrow pass.
///   * `flags:<was>-><now>` -- the installer's function, re-described by someone else.
///   * `clobber:engine`    -- replaced by a `build_fn` function the installer did not mount
///                            (`op_helper_*` / `op_nativize`).
///   * `clobber:unbranded` -- replaced by a function carrying no engine brand at all (glue JS, or a
///                            Rust pass using a raw `v8::Function::builder`).
///   * `gone`          -- the slot the installer mounted no longer exists.
fn audit_map() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, String>> {
    static A: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, String>>> = std::sync::OnceLock::new();
    A.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Whether the realms this process installs from now on read their ledger back against the live surface
/// ([`audit_surface`]) -- ON DEMAND. Its one reader,
/// `core/korvus-runtime/tests/install_ledger.rs`, asks for it with [`enable_install_audit`]; a page and a
/// worker no longer pay for it (a worker's start was ~8 ms of its ~22 in it, measured 2026-09-28).
static AUDIT_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask for the install audit in every realm this process installs from now on.
pub fn enable_install_audit() {
    AUDIT_ON.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn install_audit_enabled() -> bool {
    AUDIT_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// The audit's verdict per row, as JSON. `core/korvus-runtime/tests/install_ledger.rs` is the gate
/// that reads it (having asked for the audit, [`enable_install_audit`]).
pub fn install_audit_json() -> String {
    json_of(&audit_map().lock().map(|m| m.clone()).unwrap_or_default())
}

/// Take the context's install ledger out of `OpState` (a fresh one the first time). Put it back with
/// [`put_ledger`] once the pass is done -- a `RefCell` borrow must not be held across a V8 call.
pub fn take_ledger(scope: &mut v8::PinScope) -> InstallLedger {
    let state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = state.borrow_mut();
    st.try_take::<InstallLedger>().unwrap_or_default()
}

pub fn put_ledger(scope: &mut v8::PinScope, ledger: InstallLedger) {
    let state = deno_core::JsRuntime::op_state_from(scope);
    state.borrow_mut().put(ledger);
}

/// A copy of the context's install ledger so far (the I2 test reads it back through an op).
pub fn ledger_entries(scope: &mut v8::PinScope) -> Vec<LedgerEntry> {
    let state = deno_core::JsRuntime::op_state_from(scope);
    let st = state.borrow();
    st.try_borrow::<InstallLedger>().map(|l| l.entries().to_vec()).unwrap_or_default()
}

// ── the vocabulary ──────────────────────────────────────────────────────────────────────────────

/// Which conventions a table's `Stub` rows and non-newable constructor take (see the module doc).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavor {
    /// The generated registry: the shell stub. (Its interface object is the same one a table gets,
    /// -- see [`create_interface`]'s table.)
    Shell,
    /// A hand-written table: the D2 stub.
    Table,
}

/// A member the bundle selected that the union table does not carry -- an implementation gap. The
/// caller (the boot audit) reports these; the installer keeps the surface's shape by mounting a
/// plain stub so the member is present-but-empty rather than missing.
#[derive(Clone)]
pub struct Gap {
    pub interface: &'static str,
    pub member: String,
}

/// A discrepancy between a natively realised surface and its one-table declaration. Unlike a [`Gap`]
/// (a bundle-selected member the table lacks), a `Drift` is the table and the realised surface
/// disagreeing -- the tripwire the one-table rule exists to catch. The op2 verification that produced
/// most of them (`verify_native`) was deleted with the last op2 class that realised a
/// table member; the callers still log any a live install returns.
pub struct Drift {
    pub interface: &'static str,
    pub member: String,
    pub detail: String,
}

/// What [`create`] did besides building the interface object and its prototype, which a realm that
/// instantiates the interface's TEMPLATE replays (`install::templates`): the ledger rows
/// the members' mounts recorded and the join's gaps.
#[derive(Clone, Default)]
pub(crate) struct Replay {
    pub rows: Vec<LedgerEntry>,
    pub gaps: Vec<Gap>,
    /// the process-wide install bookkeeping the members' mounts recorded (`templates::Mark`), replayed
    /// once per process by a blueprint read from disk -- a process that recorded nothing itself
    pub marks: Vec<crate::install::templates::Mark>,
}

/// The key of an interface's template, in the isolate's cache and the process's blueprints: everything
/// its builder reads that can change what it builds -- the table, the exposed name, the selector, the
/// builder (`kind`: [`create`]'s flavor, or `newable`), the realm filter, the context's declared realm,
/// and the surface it installs against, by content (`templates::realm_identity`).
fn tpl_key(scope: &mut v8::PinScope, def: &'static InterfaceDef, name: &str, selector: Option<&[&str]>, kind: &str, realm: Exposed) -> String {
    let surface_realm = crate::install::surface::realm(scope);
    // A worker realm's install also reads which scope it is (dedicated, shared, service: the event
    // families, the rig's publishing gate), so its keys name that too. BEFORE the identity, which a
    // blueprint file's writer reads as the key's last segment.
    let scope_kind = match surface_realm {
        Some(spec::SurfaceRealm::Worker) => worker_scope_kind(scope),
        _ => String::new(),
    };
    let ident = crate::install::templates::realm_identity(scope);
    format!("{}|{name}|{selector:?}|{kind}|{realm:?}|{surface_realm:?}{scope_kind}|{ident}", def.name)
}

/// `/dedicated`, `/shared` or `/service`: this worker realm's scope, asked of the kernel once per realm
/// (a template key is made for every interface).
fn worker_scope_kind(scope: &mut v8::PinScope) -> String {
    use std::sync::{Mutex, OnceLock};
    static KINDS: OnceLock<Mutex<std::collections::HashMap<u32, String>>> = OnceLock::new();
    let ord = realm_ordinal(scope);
    if let Some(k) = KINDS.get_or_init(Default::default).lock().ok().and_then(|m| m.get(&ord).cloned()) {
        return k;
    }
    let k = format!("/{}", crate::core::workers::worker_global_scope::scope_kind(scope));
    if let Ok(mut m) = KINDS.get_or_init(Default::default).lock() {
        m.insert(ord, k.clone());
    }
    k
}

/// One interface's template step (`install::templates`), shared by every builder of an
/// interface object and its prototype: [`create`] and `live::rows::build_newable_with_data`.
pub(crate) enum TplStep<'s> {
    /// This realm instantiated the cached template: the pair (members, statics, `constructor`, tag),
    /// with the ledger rows replayed. The builder still publishes and splices, as it always does.
    Instantiated(v8::Local<'s, v8::Function>, v8::Local<'s, v8::Object>, Vec<Gap>),
    /// Build the ordinary way, then hand the pair to [`tpl_done`]: `Some(key)` while a capture records it.
    Build(Option<String>, usize),
}

/// Start an interface: instantiate its template if this isolate has one or the process has its blueprint
/// (`templates::from_blueprint`), or start recording it.
pub(crate) fn tpl_step<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    def: &'static InterfaceDef,
    name: &str,
    selector: Option<&[&str]>,
    kind: &str,
    realm: Exposed,
    ledger: &mut InstallLedger,
) -> TplStep<'s> {
    let key = crate::install::templates::wanted(scope).then(|| tpl_key(scope, def, name, selector, kind, realm));
    let t0 = crate::install::templates::reporting().then(std::time::Instant::now);
    if let Some(k) = key.as_deref() {
        crate::install::templates::note_key(scope, name, k);
        let t = crate::install::templates::cached(scope, k).or_else(|| crate::install::templates::from_blueprint(scope, k));
        if let Some(t) = t {
            if let Some((ctor, proto)) = crate::install::templates::instantiate(scope, &t) {
                let us = t0.map(|t| t.elapsed().as_micros()).unwrap_or(0);
                let ord = realm_ordinal(scope);
                crate::install::templates::report(&format!("{k}: instantiated"));
                crate::install::templates::report(&format!("TIME {name} r{ord} instantiated {us}us"));
                let here = realm_of(scope);
                for e in &t.replay.rows {
                    ledger.record(here, e.iface, e.member, e.holder, e.pass);
                }
                note_alias(def, name);
                return TplStep::Instantiated(ctor, proto, t.replay.gaps.clone());
            }
        }
    }
    // No capture where the process refused the key. A blueprint another isolate of this process stored
    // after the look above does NOT stop this realm recording its own: it used to, and the
    // realm then built the interface the ordinary way with no capture and no switch -- objects no template
    // made, carrying no brand, which a parallel test run showed as a fake receiver accepted. Recording it
    // twice costs that race a translation; the process keeps the first blueprint, this isolate its own.
    let key = key.filter(|k| !crate::install::templates::refused(scope, k));
    if key.is_some() {
        crate::install::templates::begin(name);
    }
    if let Some(t) = t0 {
        BUILD_T.with(|b| b.borrow_mut().push(t));
    }
    TplStep::Build(key, ledger.len())
}

thread_local! {
    /// When each interface [`tpl_step`] said to build started (a stack: a build may run inside another's
    /// member install), for `KORVUS_TPL_REPORT`'s timings.
    static BUILD_T: std::cell::RefCell<Vec<std::time::Instant>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Finish an interface [`tpl_step`] said to build: translate what the ordinary build made into its
/// template, with the ledger rows it recorded since `mark` and the join's gaps, and instantiate it here
/// too (the template is the only path): the pair is returned, the builder carries on with it --
/// publishes it, splices it -- and the objects it built while recording are left to the collector. Their
/// ledger rows, flags and stamps describe the same members (the template carries `korvus.installedBy`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn tpl_done<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: Option<String>,
    mark: usize,
    name: &str,
    parent: Option<&str>,
    ctor: v8::Local<'s, v8::Function>,
    proto: v8::Local<'s, v8::Object>,
    ledger: &InstallLedger,
    gaps: &[Gap],
) -> Option<(v8::Local<'s, v8::Function>, v8::Local<'s, v8::Object>)> {
    tpl_done_with(scope, key, mark, name, parent, ctor, proto, ledger, gaps, None)
}

/// [`tpl_done`] for a `[Global]` interface whose install also put its own members on the realm's global
/// object (`global`: that object and those keys, in order): they are translated with the interface, and a
/// realm that switches takes them from the template too.
#[allow(clippy::too_many_arguments)]
fn tpl_done_with<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: Option<String>,
    mark: usize,
    name: &str,
    parent: Option<&str>,
    ctor: v8::Local<'s, v8::Function>,
    proto: v8::Local<'s, v8::Object>,
    ledger: &InstallLedger,
    gaps: &[Gap],
    global: Option<(v8::Local<'s, v8::Object>, &[&str])>,
) -> Option<(v8::Local<'s, v8::Function>, v8::Local<'s, v8::Object>)> {
    if crate::install::templates::reporting() {
        if let Some(t) = BUILD_T.with(|b| b.borrow_mut().pop()) {
            let ord = realm_ordinal(scope);
            crate::install::templates::report(&format!("TIME {name} r{ord} built {}us", t.elapsed().as_micros()));
        }
    }
    let k = key?;
    let rows = ledger.entries()[mark..].to_vec();
    let t0 = crate::install::templates::reporting().then(std::time::Instant::now);
    crate::install::templates::finish(scope, &k, ctor, proto, parent, Replay { rows, gaps: gaps.to_vec(), marks: Vec::new() }, global.map(|(g, keys)| (g.into(), keys)));
    if let Some(t) = t0 {
        let ord = realm_ordinal(scope);
        crate::install::templates::report(&format!("TIME {name} r{ord} translated {}us", t.elapsed().as_micros()));
    }
    let t1 = crate::install::templates::reporting().then(std::time::Instant::now);
    let t = crate::install::templates::cached(scope, &k)?;
    let pair = crate::install::templates::instantiate(scope, &t)?;
    // Over the keys the ordinary install just defined, so each keeps its position.
    if let Some((g, _)) = global {
        crate::install::templates::attach_global(scope, &t, g)?;
    }
    crate::install::templates::report(&format!("{k}: switched"));
    if let Some(t) = t1 {
        let ord = realm_ordinal(scope);
        crate::install::templates::report(&format!("TIME {name} r{ord} switched {}us", t.elapsed().as_micros()));
    }
    Some(pair)
}

/// What [`create_interface`] built.
pub struct Created<'s> {
    pub proto: v8::Local<'s, v8::Object>,
    pub ctor: v8::Local<'s, v8::Function>,
    pub gaps: Vec<Gap>,
}

/// The prototype members of a table (`!static_member`) that `realm` exposes, in table order.
///
/// `Exposed::Both` as the realm means "the whole table" -- what every hand-table site passes, since an
/// `interface!` table declares its exposure once. The generated registry's two walks pass
/// `Exposed::Window` / `Exposed::Worker`, which is how ONE table installs two different member sets:
/// `DOMMatrix.setMatrixValue` is in the window's and not the worker's (goal invariant I8).
fn proto_members(def: &'static InterfaceDef, realm: Exposed) -> impl Iterator<Item = &'static MemberDef> {
    def.members.iter().filter(move |m| !m.static_member && m.exposed.covers(realm))
}

/// The interface-object members of a table (`static_member`) that `realm` exposes, in table order.
fn static_members(def: &'static InterfaceDef, realm: Exposed) -> impl Iterator<Item = &'static MemberDef> {
    def.members.iter().filter(move |m| m.static_member && m.exposed.covers(realm))
}

/// The capture's terse kind for a row: "a" accessor, "m" method, "v" const.
fn kind_letter(m: &MemberDef) -> &'static str {
    match m.kind {
        MemberKind::Attr { .. } | MemberKind::StaticAttr { .. } | MemberKind::Handler => "a",
        MemberKind::Method | MemberKind::StaticMethod => "m",
        MemberKind::Const => "v",
        // Interceptor rows are not prototype members; the registry never emits them.
        MemberKind::NamedGetter | MemberKind::IndexedGetter => "",
    }
}

/// (has getter, has setter) for an accessor row.
fn accessor_flags(m: &MemberDef) -> (bool, bool) {
    match m.kind {
        MemberKind::Attr { readonly } | MemberKind::StaticAttr { readonly } => (true, !readonly),
        MemberKind::Handler => (true, true),
        _ => (false, false),
    }
}

/// A captured constant's value as a v8 value -- what the shell code's `json_scalar` over the
/// `ShellDef` adapter's `cval_to_json` composed to, before both were deleted with the shell
/// installer: an integral number in i32 range is a v8 Integer, any other finite number a
/// Number, a non-finite number `null` (JSON could not encode it), strings/bools themselves, null/none
/// `null`.
fn cval_v8<'s>(scope: &mut v8::PinScope<'s, '_>, cv: ConstVal) -> v8::Local<'s, v8::Value> {
    match cv {
        ConstVal::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 9007199254740992.0 && (i32::MIN as f64..=i32::MAX as f64).contains(&n) {
                v8::Integer::new(scope, n as i32).into()
            } else if n.is_finite() {
                v8::Number::new(scope, n).into()
            } else {
                v8::null(scope).into()
            }
        }
        ConstVal::Bool(b) => v8::Boolean::new(scope, b).into(),
        ConstVal::Str(s) => str_key(scope, s).into(),
        ConstVal::Null | ConstVal::None => v8::null(scope).into(),
    }
}

/// A [`ConstVal`] as a v8 value for the D2 const path (a hand table's constant): a Number for every
/// number, `undefined` for `None`.
fn const_v8_value<'s>(scope: &mut v8::PinScope<'s, '_>, cv: ConstVal) -> v8::Local<'s, v8::Value> {
    match cv {
        ConstVal::Num(n) => v8::Number::new(scope, n).into(),
        ConstVal::Bool(b) => v8::Boolean::new(scope, b).into(),
        ConstVal::Str(s) => v8::String::new(scope, s).map(|v| v.into()).unwrap_or_else(|| v8::undefined(scope).into()),
        ConstVal::Null => v8::null(scope).into(),
        ConstVal::None => v8::undefined(scope).into(),
    }
}

// ── the one member mount ────────────────────────────────────────────────────────────────────────

/// Mount one row on `target`. An `Install` body is the row's own installer (a kit or a hand body);
/// everything else is the flavor's stub form -- the shell code's `put()` (a method is the
/// receiver-checked shell stub; an accessor is the event-handler pair for `Handler` rows, the slot
/// pair when writable, or a readonly shell getter; a const is its value with the row's flags) or the
/// D2 stub (a method, arity-guarded when `required > 0`; a get/set accessor over the shape default; a
/// const with the WebIDL constant descriptor). Descriptor flags come from the row.
#[allow(clippy::too_many_arguments)]
pub fn put_member<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
    m: &'static MemberDef,
    iface: &'static str,
    is_proto: bool,
    ledger: &mut InstallLedger,
    pass: Pass,
    flavor: Flavor,
) {
    let holder = if is_proto { Holder::Prototype } else { Holder::InterfaceObject };
    let realm = realm_of(scope);
    if m.body == BodyKind::Install {
        (m.install)(scope, target, m);
        // THE RECEIVER CHECK, for every row with a body. It goes HERE, in the one member
        // mount, so a row cannot be written without one: a body that never looked at `this` used to
        // answer for any object at all, and each lane was left to remember
        // (`Navigator.prototype.sendBeacon.call({}, "/x")` answered `true`; Chrome 150 throws).
        // What it does is read off the row's own columns -- see `install::receiver::policy`.
        crate::behavior_rt::record_receiver(iface, m.name, receiver::guard_installed(scope, target, m, is_proto));
        // The brand goes on AFTER the receiver check, because the check REPLACES the slot's function
        // with its wrapper -- branding the body first would brand the object the page never sees.
        if let Some((en, cfg)) = stamp_member(scope, target, m.name, &format!("{iface}::{}", m.name)) {
            record_install_flags(realm.1, iface, m.name, holder, en, cfg);
        }
        ledger.record(realm, iface, m.name, holder, pass);
        crate::behavior_rt::record_layer(iface, m.name, "native");
        return;
    }
    match flavor {
        Flavor::Shell => put_shell_stub(scope, target, m, iface, is_proto),
        Flavor::Table => put_d2_stub(scope, target, m, is_proto),
    }
    if !kind_letter(m).is_empty() {
        let layer = mounted_layer(m, flavor);
        // A `stub` row carries its OWN receiver check (`install::stub::member_from_data`), which is
        // where it has to be: the stub answers from the row rather than forwarding to a body, so
        // there is nothing to wrap. A `slot` row is different -- the event-handler pair and the
        // private-slot accessor are real storage with no check at all, and they answer `null` for a
        // foreign receiver where Chrome throws -- so those DO go through the installer's check.
        if layer != "stub" {
            crate::behavior_rt::record_receiver(iface, m.name, receiver::guard_installed(scope, target, m, is_proto));
        } else {
            // ...and a `stub` row records the check the STUB carries, which is not "no policy".
            // Leaving it out is what made the sweep read a stub row's rejected promise as a second
            // writer -- see `receiver::stub_policy_name`.
            crate::behavior_rt::record_receiver(iface, m.name, receiver::stub_policy_name(m, is_proto));
        }
        if let Some((en, cfg)) = stamp_member(scope, target, m.name, &format!("{iface}::{}", m.name)) {
            record_install_flags(realm.1, iface, m.name, holder, en, cfg);
        }
        ledger.record(realm, iface, m.name, holder, pass);
        crate::behavior_rt::record_layer(iface, m.name, layer);
    }
}

/// The LAYER a row reaching the stub path is mounted at (goal invariant I7). It is read off the row's
/// own columns, which is the whole point: before this, `status.json` inferred the layer from the
/// runtime shell inventory, which the D2 stub never registered into, so a hand table's `= stub` row
/// reported `implemented`.
///
/// Not every row that reaches the stub path is a stub:
/// * a WebIDL CONSTANT's value IS its observable surface, and both stub paths install that value from
///   the table's `cval` -- nothing is missing, so it is `native`;
/// * a registry row that is an `on*` handler, or a writable non-handler attribute, gets the event
///   handler pair / the private-slot accessor -- real storage that round-trips, so it is `slot`,
///   neither a stub (it does not answer empty) nor an implementation (nothing computes it).
///
/// Everything else answers a shape-correct empty value and records a stub hit: `stub`. A `Cppgc` row
/// that reaches HERE is one no class generated a member for (an own class's `cppgc` rows are `Install`
/// rows) -- so it is a stub in this process. `cppgc` was the layer of a row a deno_core
/// class template realised and `verify_native` checked; both were deleted in P6.2.
fn mounted_layer(m: &'static MemberDef, flavor: Flavor) -> &'static str {
    if matches!(m.kind, MemberKind::Const) {
        return "native";
    }
    if flavor == Flavor::Shell {
        if matches!(m.kind, MemberKind::Handler) {
            return "slot";
        }
        let (g, s) = accessor_flags(m);
        if g && s {
            return "slot";
        }
    }
    "stub"
}

/// The registry flavor's `put()` for a `Stub` row.
///
/// The BODY is the same single D2 stub a hand table's `= stub` row gets (goal invariant I7: there is
/// one stub). What differs, and why this is not simply `put_d2_stub`, is the SHAPE around it: a
/// registry row carries the descriptor flags the capture recorded (`m.enumerable` / `m.configurable`)
/// where a hand table's accessor is WebIDL's `{enumerable: true, configurable: true}`; its arity guard
/// is applied by a later pass over the whole registry rather than per row; and a writable non-handler
/// accessor gets the private-slot pair instead of a stub, which is the `slot` layer.
fn put_shell_stub<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Object>, m: &'static MemberDef, iface: &str, is_proto: bool) {
    let _ = iface;
    let key = member_key(scope, m.name);
    // The receiver a stub call is checked against: the prototype it is being mounted on. A static has
    // none, and passing none is what keeps a member installed before its prototype exists un-guarded
    // rather than guarded against nothing.
    let recv: Option<v8::Local<v8::Value>> = if is_proto { Some(target.into()) } else { None };
    let data = stub::stub_data_recv(scope, m, recv);
    match kind_letter(m) {
        "m" => {
            let f = stub::build_stub(scope, false, m.name, m.length as i32, data);
            // the stub counts its own arguments after its receiver check (`stub::stub_get_body`): the
            // registry's arity pass must not wrap it from outside, which would count first
            if m.required > 0 {
                crate::binding::protos::stamp_arity_guarded(scope, f);
            }
            let mut d = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
            d.set_enumerable(m.enumerable);
            d.set_configurable(m.configurable);
            target.define_property(scope, key.into(), &d);
        }
        "a" => {
            let (g, s) = accessor_flags(m);
            let get: v8::Local<v8::Value>;
            let set: v8::Local<v8::Value>;
            if g && s {
                if matches!(m.kind, MemberKind::Handler) {
                    let ty = m.name.strip_prefix("on").unwrap_or(m.name);
                    get = make_et_on_getter(scope, ty).into();
                    set = make_et_on_setter(scope, ty).into();
                } else {
                    let gname = format!("get {}", m.name);
                    get = if m.nullable {
                        make_evt_slot_get_null(scope, &gname, m.name).into()
                    } else {
                        make_evt_slot_get(scope, &gname, m.name).into()
                    };
                    let sname = format!("set {}", m.name);
                    set = make_evt_slot_setter(scope, &sname, m.name).into();
                }
            } else {
                get = if g {
                    stub::build_stub(scope, false, &format!("get {}", m.name), 0, data).into()
                } else {
                    v8::undefined(scope).into()
                };
                set = if s {
                    stub::build_stub(scope, true, &format!("set {}", m.name), 1, data).into()
                } else {
                    v8::undefined(scope).into()
                };
            }
            let mut d = v8::PropertyDescriptor::new_from_get_set(get, set);
            d.set_enumerable(m.enumerable);
            d.set_configurable(m.configurable);
            target.define_property(scope, key.into(), &d);
        }
        "v" => {
            let val = cval_v8(scope, m.cval);
            let mut d = v8::PropertyDescriptor::new_from_value_writable(val, false);
            d.set_enumerable(m.enumerable);
            d.set_configurable(m.configurable);
            target.define_property(scope, key.into(), &d);
        }
        _ => {}
    }
}

/// The D2 stub for a `Stub` (or fresh-prototype `Cppgc`) row of a hand table: attribute/handler -> a
/// getter (and, when writable, a swallowing setter) over the shape default; method -> a method
/// function, wrapped in the WebIDL arity guard when `required > 0` (a table-owned member never needs a
/// separate arity pass); const -> its value with `{ writable:false, enumerable:true,
/// configurable:false }`. Both carry the `MemberDef` as associated data so the single
/// [`stub::cb_stub`] callback knows what to record and return.
/// [`put_d2_stub`] for a ROW BODY that finds its implementation missing in this realm.
///
/// `Worker.postMessage`'s body is a `korvus-runtime` factory op the WORKER isolate does not
/// register (a nested worker is not implemented). A row body cannot just skip -- that leaves the member
/// absent, which is a surface diff -- and it must not invent an answer. This is the one
/// not-implemented mechanism (invariant I7), so the row lands as a counted stub.
pub fn put_table_stub<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, is_proto: bool) {
    // Round-trip the holder through a `Global` so it carries the SCOPE's lifetime: an `install` row
    // body is a fn pointer whose two lifetimes are independent (the macro's signature), and
    // `put_d2_stub` ties them. The same trick `core/fileapi/file_reader.rs` uses for its microtask data.
    let g = v8::Global::new(scope, proto);
    let proto = v8::Local::new(scope, &g);
    put_d2_stub(scope, proto, member, is_proto);
    // the receiver policy BEFORE the layer, as `put_member` records them: a stub row with no policy is
    // one `receiver_sweep` cannot classify, and a reader of both ledgers relies on this order
    let iface = member.label.split("::").next().unwrap_or("");
    crate::behavior_rt::record_receiver(iface, member.name, receiver::stub_policy_name(member, is_proto));
    crate::behavior_rt::record_layer(iface, member.name, "stub");
}

fn put_d2_stub<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>, member: &'static MemberDef, is_proto: bool) {
    // THE RECEIVER CHECK, which this path was missing (W4.a measured the gap on
    // `Navigator`). `put_shell_stub` has carried it since the two stubs became one, so a REGISTRY row
    // read off a foreign receiver has always thrown `Illegal invocation` the way every browser does --
    // but a HAND TABLE's `= stub` row answered for anything, which is the exact tell the check exists
    // to remove (`Notification.prototype.close.call({})` answered `undefined`; Chrome throws). A
    // static has no prototype to check against, and passing none keeps it un-guarded rather than
    // guarded against nothing.
    //
    // A `[Global]` interface's row sits on the GLOBAL OBJECT, which is an instance, not a prototype: a check
    // against it rejected the global itself -- `webkitRequestFileSystem(0, 0)` in a dedicated worker threw
    // `Illegal invocation` where Chrome 150.0.7871.46 answers `undefined` (measured). Its
    // receiver is the realm's global, which `window::guarded` checks, OUTSIDE the arity guard (Chrome's order).
    let on_global = is_proto && scope.get_current_context().global(scope) == proto;
    let recv: Option<v8::Local<v8::Value>> = if is_proto && !on_global { Some(proto.into()) } else { None };
    let data = stub::stub_data_recv_kind(scope, member, recv, true);
    match member.kind {
        MemberKind::Method | MemberKind::StaticMethod => {
            let f = stub::build_stub(scope, false, member.name, member.length as i32, data);
            // The stub counts its own arguments, after its receiver check (`stub::stub_get_body`; Chrome's
            // order). An arity guard wrapped round it counted FIRST: `Notification.prototype.close`-shaped
            // `= stub` rows answered "N arguments required" to a foreign receiver.
            if member.required > 0 {
                crate::binding::protos::stamp_arity_guarded(scope, f);
            }
            let f = if on_global { crate::core::html::window::guarded(scope, f.into(), member.name, member.length as i32) } else { f };
            define_method(scope, proto, member.name, f);
        }
        MemberKind::Attr { readonly } | MemberKind::StaticAttr { readonly } => {
            install_d2_accessor(scope, proto, member, data, readonly, on_global);
        }
        // An `on*` handler is a writable accessor whose getter reads null until set (see
        // `stub_default`'s handler case); its setter swallows the write while stubbed.
        MemberKind::Handler => install_d2_accessor(scope, proto, member, data, false, on_global),
        // A WebIDL constant: install its VALUE (from the table's `cval`) with the constant descriptor.
        // Appears on both the interface object and its prototype (this fn is called for each),
        // matching the capture's `v` members.
        MemberKind::Const => {
            let val = const_v8_value(scope, member.cval);
            let k = member_key(scope, member.name);
            let mut d = v8::PropertyDescriptor::new_from_value_writable(val, false);
            d.set_enumerable(true);
            d.set_configurable(false);
            proto.define_property(scope, k, &d);
        }
        // Named / indexed getters are a V8 instance interceptor mounted at ctor-template time (see
        // `install_named_getter_object`), NOT a prototype member -- so the prototype stub path installs
        // nothing for them. A table may still DECLARE one (for coverage / the live mount); reaching
        // here is a no-op rather than an error, and the member correctly does not appear on the
        // prototype (the capture never lists it as one).
        MemberKind::NamedGetter | MemberKind::IndexedGetter => {}
    }
}

/// Install a get(/set) accessor for an attribute-shaped member, matching WebIDL's descriptor for an
/// interface prototype attribute: `{ get, set?, enumerable: true, configurable: true }`. The getter
/// is the D2 stub; a writable member gets the swallowing stub setter so assignment does not throw.
/// `on_global`: a `[Global]` row on the global object, whose getter and setter check the receiver is the
/// realm's global (`window::guarded`, see [`put_d2_stub`]).
fn install_d2_accessor<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>, member: &'static MemberDef, data: v8::Local<'s, v8::Value>, readonly: bool, on_global: bool) {
    let gname = format!("get {}", member.name);
    let mut getter = stub::build_stub(scope, false, &gname, 0, data);
    if on_global {
        getter = crate::core::html::window::guarded(scope, getter.into(), &gname, 0);
    }
    let setter: v8::Local<v8::Value> = if readonly {
        v8::undefined(scope).into()
    } else {
        let sname = format!("set {}", member.name);
        let s = stub::build_stub(scope, true, &sname, 1, data);
        if on_global { crate::core::html::window::guarded(scope, s.into(), &sname, 1).into() } else { s.into() }
    };
    let mut desc = v8::PropertyDescriptor::new_from_get_set(getter.into(), setter);
    desc.set_enumerable(true);
    desc.set_configurable(true);
    let k = member_key(scope, member.name);
    proto.define_property(scope, k, &desc);
}

// ── THE JOIN: union table × bundle selector ────────────────────────────────────────

/// The rows one interface mounts in this realm, on each holder, in the order they must be created —
/// and the members the active capture records that no row provides.
pub(crate) struct Mount {
    pub proto: Vec<&'static MemberDef>,
    pub statics: Vec<&'static MemberDef>,
    pub gaps: Vec<Gap>,
    /// How many of [`Self::proto`] go down BEFORE `constructor` — the capture's seat,
    /// carried the last step of the way: through `explicit` in [`crate::install::surface::selection`]
    /// and through the rows this table actually has here, since a selected name with no row is a
    /// [`Gap`] that never reaches the prototype and so must not count towards the seat either.
    ///
    /// `None` is "the capture does not say", and the two creates then place `constructor` where their
    /// flavour always placed it. It is never a guess and never a constant.
    pub ctor_at: Option<usize>,
}

/// Join the union table with the ACTIVE bundle's capture: the capture decides WHICH rows this device
/// has and IN WHAT ORDER, the table decides how each behaves.
///
/// THE ONE PLACE THE SELECTOR IS ASKED. Both creates go through it ([`create_interface_in`] and
/// [`overwrite_prototype_selected`], which is the newable-table path `build_newable` uses), and so do
/// the two backfills; a filter sprinkled at call sites is how six Brave-only members
/// (`Blob.textStream`, `AnimationEvent.animation`, `WheelEvent.momentum` …) survived the first cut of
/// this change on the Chrome bundle, which is a page-readable tell.
///
/// * `exposed_name` is the name on the GLOBAL, not the table's: the `Navigator` table is
///   `WorkerNavigator` in the worker realm, and the worker capture keys it that way.
/// * `explicit` NARROWS and never widens: the DOM spine's staged build asks for no members here and
///   lets the registry's backfill supply them; `Window` asks for its two constants. It does not
///   re-order, because the order a page reads is the capture's.
/// * With NO active surface — a bare runtime, `tests/installer_parity.rs` — the whole table stands,
///   filtered by realm, in table order. That is the fallback, not the rule.
pub(crate) fn join(
    scope: &mut v8::PinScope,
    def: &'static InterfaceDef,
    exposed_name: &str,
    realm: Exposed,
    explicit: Option<&[&str]>,
) -> Mount {
    let keeps = |m: &&'static MemberDef| explicit.map(|e| e.contains(&m.name)).unwrap_or(true);
    // THE REALM IS THE CONTEXT'S, not the call site's. `create_interface` passes `Exposed::Both`,
    // which means "install this table whole, do not filter" -- right for a table that states its
    // exposure once, and wrong the moment a ROW carries its own (`Performance`'s five
    // `[Exposed=Window]` partials, `Navigator`'s fourteen Protected-Audience rows,
    // `ServiceWorkerRegistration.cookies`), because `Exposed::covers` is permissive in both directions
    // and a `window` row therefore passed a `Both` filter inside a worker. So when the caller does not
    // name a realm, this asks the CONTEXT which one it declared at bootstrap -- and a context that
    // declared none (a bare `JsRuntime`) keeps `Both`, which is the old behaviour exactly.
    let realm = match (realm, crate::install::surface::realm(scope)) {
        (Exposed::Both, Some(spec::SurfaceRealm::Window)) => Exposed::Window,
        (Exposed::Both, Some(spec::SurfaceRealm::Worker)) => Exposed::Worker,
        (r, _) => r,
    };
    let active = crate::install::surface::active(scope);
    let Some(sel) = active.as_deref().and_then(|s| crate::install::surface::selection(s, exposed_name, explicit)) else {
        let mut gaps = Vec::new();
        let (proto, statics) = match explicit {
            None => (proto_members(def, realm).collect(), static_members(def, realm).collect()),
            Some(names) => {
                // BOTH HOLDERS for one name, because a WebIDL CONSTANT is one name carried by two
                // rows and belongs in both places. A by-name lookup finds the prototype's and stops,
                // which is why `install_window_window` and `install_worker_global_scope` each had to
                // mount the static half by hand afterwards — a second writer by I2's definition.
                let mut p = Vec::new();
                let mut s = Vec::new();
                for &n in names {
                    let (a, b) = (def.member_on(n, false), def.member_on(n, true));
                    if a.is_none() && b.is_none() {
                        gaps.push(Gap { interface: def.name, member: n.to_string() });
                    }
                    p.extend(a);
                    s.extend(b);
                }
                (p, s)
            }
        };
        return Mount { proto, statics, gaps, ctor_at: None };
    };
    let mut gaps = Vec::new();
    let mut pick = |names: &[&str], statics: bool| -> Vec<&'static MemberDef> {
        names
            .iter()
            .filter_map(|n| match def.member_on(n, statics) {
                Some(m) => Some(m),
                None => {
                    gaps.push(Gap {
                        interface: def.name,
                        member: if statics { format!("{n} (static)") } else { (*n).to_string() },
                    });
                    None
                }
            })
            .collect()
    };
    let mut proto = pick(&sel.members, false);
    let mut statics = pick(&sel.statics, true);
    // The seat survives the row lookup: of the selected names in front of `constructor`, only the
    // ones this table could actually answer for are on the prototype, so only those move it along.
    let ctor_at = sel.ctor_after.map(|k| sel.members.iter().take(k).filter(|n| def.member_on(n, false).is_some()).count());
    // The rows no capture can record, appended: `Object.getOwnPropertyNames` returns string keys, so
    // the rig writes down no `Symbol(...)` member anywhere in either bundle, and filtering against
    // those lists would drop every `@@iterator` in the tree. Last is also where `Reflect.ownKeys`
    // reports every symbol, whatever order it was inserted in.
    proto.extend(proto_members(def, realm).filter(|m| crate::install::surface::unrecordable(m.name)).filter(keeps));
    statics.extend(static_members(def, realm).filter(|m| crate::install::surface::unrecordable(m.name)).filter(keeps));
    Mount { proto, statics, gaps, ctor_at }
}

// ── the one create ──────────────────────────────────────────────────────────────────────────────

/// Build an interface from its table onto `global` under `name`: the interface object (per model and
/// flavor), a fresh prototype with `constructor`, the `Symbol.toStringTag`, the members -- every
/// prototype row then every static row, in table order, or a bundle `selector`'s subset in the
/// selector's order -- and, when `parent` is given, the WebIDL inheritance splice (prototype chain AND
/// constructor chain). The registry's create pass wires its chains in a later pass by name instead.
///
/// The interface object, by model (every one of these is a REAL `[[Construct]]` that
/// throws from its body, which is what Chrome 150 has for every interface object but `NodeFilter`):
///
/// | model | `new X()` | `X()` |
/// |---|---|---|
/// | `Value` (registry) | `cb_shell_ctor` mints an instance | `Failed to construct 'X': Please use the 'new' operator, …` |
/// | `Element` (`[HTMLConstructor]`) | `Failed to construct 'X': Illegal constructor` | `Failed to construct 'X': Please use the 'new' operator, …` |
/// | `Interface` / `Singleton` | `Failed to construct 'X': Illegal constructor` | `Illegal constructor` |
/// | `Value` (hand table) | a newable placeholder (the shape gate only -- live, `build_newable` builds the real one over the class's constructor) | |
///
/// The two `Illegal constructor` rows differ ONLY in the bare-call answer, and that difference is
/// WebIDL's: an `[HTMLConstructor]` interface declares a constructor, so calling it without `new` is
/// "you forgot new" rather than "there is nothing to construct". Both were measured across the whole
/// surface of both realms -- see `binding::protos`'s block comment for the rig and the numbers.
///
/// `constructor` is the FIRST own key of a registry prototype (as the shells built it) and the LAST of a
/// hand table's (as the Phase 3 installer built it and its shape gates pin it); the ORDER pass puts the
/// live surface in the capture's order either way, and W7 settles the bare-runtime form.
/// Build a table's interface object + prototype with EVERY row the table carries — what a hand
/// `interface!` table wants, since it declares its exposure once for the whole table.
#[allow(clippy::too_many_arguments)]
pub fn create_interface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    name: &str,
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
    selector: Option<&[&str]>,
    flavor: Flavor,
    ledger: &mut InstallLedger,
) -> Created<'s> {
    create(scope, global, def, name, parent, selector, flavor, Exposed::Both, Publish::IfCaptured, ledger, &[], None)
}

/// [`create_interface`] for an interface the member backfill finishes (`InterfaceDef::backfill`: the DOM
/// spine's staged `HTML*Element` shells): the backfill's rows for `realm` go on inside the create, exactly
/// as [`backfill_members`] would put them a moment later -- after `constructor` and the tag, through
/// `put_member` with its pass and flavor -- so they are recorded with the interface, and the
/// backfill pass finds them owned.
#[allow(clippy::too_many_arguments)]
pub fn create_interface_backfilled<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    name: &str,
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
    selector: Option<&[&str]>,
    flavor: Flavor,
    realm: Exposed,
    ledger: &mut InstallLedger,
) -> Created<'s> {
    create(scope, global, def, name, parent, selector, flavor, Exposed::Both, Publish::IfCaptured, ledger, &[], Some(realm))
}

/// As [`create_interface`], but installing only the rows `realm` exposes — the generated registry's
/// two walks, which is how one table builds a window `DOMMatrix` with `setMatrixValue` and a worker
/// `DOMMatrix` without it (goal invariant I8).
#[allow(clippy::too_many_arguments)]
pub fn create_interface_in<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    name: &str,
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
    selector: Option<&[&str]>,
    flavor: Flavor,
    realm: Exposed,
    ledger: &mut InstallLedger,
) -> Created<'s> {
    create(scope, global, def, name, parent, selector, flavor, realm, Publish::IfCaptured, ledger, &[], None)
}

/// As [`create_interface`], but for the interface THE CALLING REALM'S GLOBAL OBJECT IS — `Window` in
/// the page realm, `DedicatedWorkerGlobalScope` / `SharedWorkerGlobalScope` /
/// `ServiceWorkerGlobalScope` in a worker's. Its name is published whatever the capture says.
///
/// THE CAPTURE CANNOT SPEAK FOR THIS ONE NAME, AND ITS SILENCE IS NOT A "NO". The rig
/// enters exactly two realms per device — a window and a DEDICATED worker — so the global-scope
/// interface is the one thing that differs between worker KINDS and the one thing no bundle can ever
/// record for the other two: `SharedWorkerGlobalScope` and `ServiceWorkerGlobalScope` are absent from
/// every `worker.surface.json` in `plugins/korvus-persona/bundles/` by construction, and capturing a
/// hundred more devices would not add them. That is the same blind spot [`surface::unrecordable`]
/// already names for `@@`-keyed members, one level up, and the answer is the same: a row the rig is
/// structurally unable to see is not a row a selector may drop.
///
/// It is also not a device claim in either direction. A realm cannot fail to have its own global
/// scope — it is running inside it — so publishing the name states a fact about the REALM, which is
/// the engine's own, not about the device, which is the persona's. Both realms go through here
/// (invariant I8) even though `Window` is in every capture and so would publish anyway: the rule is
/// stated once, where it is true, instead of holding by luck in the realm nobody tested.
///
/// WHAT IT COST WHEN IT WAS WRONG. `064721c8` added the capture gate to the one create, and from that
/// commit a shared worker's global carried only `WorkerGlobalScope`. `resources/testharness.js`
/// picks its environment by NAME before `instanceof`, so every shared-worker WPT file fell through to
/// the DEDICATED environment and published results down a plane a page holding a `SharedWorker` has
/// no end of: `html/infrastructure` 89 -> 88 verdicts and `html/webappapis` 248 -> 247, two files
/// that stopped reporting at all. `shared_worker_live::the_shared_worker_global_scope_is_reachable_by_name`
/// is the standing gate.
#[allow(clippy::too_many_arguments)]
pub fn create_global_scope<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
    selector: Option<&[&str]>,
    ledger: &mut InstallLedger,
) -> Created<'s> {
    create(scope, global, def, def.name, parent, selector, Flavor::Table, Exposed::Both, Publish::Always, ledger, &[], None)
}

/// [`create_global_scope`] with the interface's own `[Global]` members (`global_rows`), mounted on the
/// realm's global object in that order right after the interface object -- and recorded with the
/// interface, so a realm that instantiates its template takes them from it (the Window global).
pub fn create_global_scope_with_members<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
    selector: Option<&[&str]>,
    global_rows: &[&'static MemberDef],
    ledger: &mut InstallLedger,
) -> Created<'s> {
    create(scope, global, def, def.name, parent, selector, Flavor::Table, Exposed::Both, Publish::Always, ledger, global_rows, None)
}

/// Whether [`create`] puts the interface object on the global under its name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// Only if the ACTIVE capture records this interface — every ordinary interface, because a name
    /// the device does not have is a name a page reads with `typeof`.
    IfCaptured,
    /// Always — the calling realm's own global-scope interface. See [`create_global_scope`].
    Always,
}

#[allow(clippy::too_many_arguments)]
fn create<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    name: &str,
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
    selector: Option<&[&str]>,
    flavor: Flavor,
    realm: Exposed,
    publish_rule: Publish,
    ledger: &mut InstallLedger,
    global_rows: &[&'static MemberDef],
    backfill: Option<Exposed>,
) -> Created<'s> {
    // `install::templates`: the first realm of an isolate builds an interface the ordinary
    // way while its template is recorded; every later realm instantiates the template and replays the
    // rest. A `[Global]` interface's own rows (`global_rows`, which a document's origin can gate) name the
    // template too.
    let mut kind = if global_rows.is_empty() {
        format!("{flavor:?}")
    } else {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for m in global_rows {
            m.name.hash(&mut h);
        }
        format!("{flavor:?}+global{:016x}", h.finish())
    };
    if let Some(b) = backfill {
        kind.push_str(&format!("+backfill{b:?}"));
    }
    let publish = publishes(scope, name, publish_rule, def);
    let (tpl, ledger_mark) = match tpl_step(scope, def, name, selector, &kind, realm, ledger) {
        TplStep::Instantiated(ctor, proto, gaps) => {
            let created = finish_create(scope, global, name, parent, publish_rule, publish, ctor, proto, gaps);
            // The `[Global]` members, after the interface object, as the ordinary install puts them.
            if !global_rows.is_empty() {
                let key = tpl_key(scope, def, name, selector, &kind, realm);
                if let Some(t) = crate::install::templates::cached(scope, &key) {
                    crate::install::templates::attach_global(scope, &t, global);
                }
            }
            return created;
        }
        TplStep::Build(key, mark) => (key, mark),
    };
    let len = def.ctor.as_ref().map(|c| c.length as i32).unwrap_or(0);
    // THE ROW'S OWN CONSTRUCTOR BODY, when this engine has one for it. Twelve generated
    // rows used to get theirs by having `make_constructable_with[_data]` SWAP the interface object
    // after this function had mounted it -- which swept away the statics this same call had just
    // installed (`EventSource.CLOSED` answered `undefined` against a capture that records `2`) and
    // the `korvus.installedBy` brand with them. Built here, the interface object is the row's from
    // the start and nothing replaces it. See `install::ctors`.
    let row_ctor = if flavor == Flavor::Shell { crate::install::ctors::row_ctor(def.name) } else { None };
    let ctor = match (flavor, def.model) {
        _ if row_ctor.is_some() => {
            let rc = row_ctor.unwrap();
            let data: v8::Local<v8::Value> = if rc.data_is_name {
                str_key(scope, def.name).into()
            } else {
                v8::undefined(scope).into()
            };
            let f = v8::Function::builder_raw(crate::binding::refs::c(rc.cb))
                .length(len)
                .data(data)
                .constructor_behavior(v8::ConstructorBehavior::Allow)
                .build(scope)
                .expect("a constructor template builds");
            let n = str_key(scope, name);
            f.set_name(n);
            if crate::install::templates::capturing() {
                let recipe = crate::install::templates::Recipe {
                    cb: Some(rc.cb),
                    data: Some(v8::Global::new(scope, data)),
                    length: len,
                    name: name.to_string(),
                    ctor: true,
                    guard: crate::install::templates::Guard::None,
                    inner: None,
                };
                crate::install::templates::record(scope, f, recipe);
            }
            crate::install::templates::note_untemplated(scope, "row_ctor", name, f);
            f
        }
        (Flavor::Shell, Model::Value) => build_interface_ctor(scope, cb_shell_ctor, name, len),
        // `build_fn`'s `ConstructorBehavior::Throw` used to stand here, and it made
        // `new X()` fail inside V8 with "<the binding> is not a constructor" before the body ran --
        // a message that names the EXPRESSION, not the interface, and that Chrome answers for
        // `NodeFilter` and nothing else. Every one of these is constructible in Chrome and throws
        // from its body, so every one of them is built with `Allow` here.
        (_, Model::Element) => build_interface_ctor(scope, crate::binding::protos::cb_element_ctor, name, len),
        (Flavor::Shell, _) | (Flavor::Table, Model::Singleton | Model::Interface) => build_illegal_ctor(scope, name, len),
        // A HAND TABLE'S `Model::Value` PLACEHOLDER STILL OWES CHROME'S BARE-CALL MESSAGE.
        //
        // This used to be `fn cb_ctor_noop(..) {}` built with a plain `v8::Function::builder`, and a
        // no-op body runs on BOTH call shapes: `new X()` returned the object V8 had already made from
        // `.prototype` (which is the placeholder's whole point) and a bare `X()` returned
        // **undefined**, where Chrome throws. Measured on a live window realm at `00b4bddd`:
        // `ClipboardItem()`, `CustomElementRegistry()`, `DOMParser()`, `XMLSerializer()`,
        // `ReadableStreamDefaultReader()`, `WritableStreamDefaultWriter()` and the worker's
        // `EventTarget()` all answered `undefined`. Every interface that reaches this arm is a
        // `Model::Value` table with a `ctor` row -- i.e. its IDL DECLARES a constructor -- so WebIDL's
        // answer to a call is the prefixed "Please use the 'new' operator" sentence, which
        // `binding::protos` spells once. `build_interface_ctor` binds `[name, requiredArgs]` as the
        // `data` that sentence is built from, exactly as every other interface object's is.
        (Flavor::Table, Model::Value) => build_interface_ctor(scope, cb_table_value_ctor, name, len),
    };
    let proto = v8::Object::new(scope);
    let ctor_obj: v8::Local<v8::Object> = ctor.into();

    // ctor.prototype = proto { writable:false, enumerable:false, configurable:false }
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(proto.into(), false);
    pd.set_enumerable(false);
    pd.set_configurable(false);
    let k = str_key(scope, "prototype");
    ctor.define_property(scope, k.into(), &pd);

    // WHERE `constructor` GOES: THE CAPTURE'S SEAT.
    //
    // The two flavours used to answer this with a constant each -- the registry's shape was
    // `constructor` first, tag, members, and a hand table's was members, then `constructor`, then the
    // tag -- and on 635 of 930 interfaces the registry's constant put it at index 0 where every
    // browser in the Chromium family puts it last. Both constants are wrong for the same reason:
    // POSITION IS A FACT ABOUT THE DEVICE, so it is read off the worn bundle like every other one.
    //
    // The rule the capture is recording: **`constructor` is the last member installed at
    // interface-definition time, so a PARTIAL interface's members land after it.** That is why the
    // number is last on `Element` (152 of 152) and in the middle on everything a partial extends --
    // `Navigator` 32/66, `Performance` 13/18, `StorageManager` 2/4, `Screen` 9/11, where `onchange`
    // and `isExtended` come from partials and sit behind it. Installing "last" as a constant would be
    // right on ~83 % of the surface and wrong on exactly the interfaces a fingerprinting script reads.
    //
    // `Mount::ctor_at` is that index after `explicit` and after this table's own rows; `None` -- no
    // active capture, or a capture that does not record the position, which is every
    // `worker.surface.json` in this tree -- keeps each flavour's previous placement.
    let mount = join(scope, def, name, realm, selector);
    let ctor_at = mount.ctor_at.unwrap_or(match flavor {
        Flavor::Shell => 0,
        Flavor::Table => mount.proto.len(),
    });
    let define_ctor = |scope: &mut v8::PinScope<'s, '_>, ledger: &mut InstallLedger| {
        let mut cd = v8::PropertyDescriptor::new_from_value_writable(ctor.into(), true);
        cd.set_enumerable(false);
        cd.set_configurable(true);
        let k = str_key(scope, "constructor");
        proto.define_property(scope, k.into(), &cd);
        let realm = realm_of(scope);
        // BRANDED like every other row. `constructor` is the one member this installer
        // mounts by hand rather than through `put_member`, so it needs the brand explicitly -- and it
        // needs it, because an interface REPLACED wholesale by a JS class (`host.js` does this to
        // fourteen of them) shows up first as its `constructor` no longer being the one installed.
        if let Some((en, cfg)) = stamp_member(scope, proto, "constructor", &format!("{}::constructor", def.name)) {
            record_install_flags(realm.1, def.name, "constructor", Holder::Prototype, en, cfg);
        }
        ledger.record(realm, def.name, "constructor", Holder::Prototype, Pass::Create);
    };
    // The tag: the table's, or the exposed name when the table is installed under one of its aliases
    // (`Navigator` as the worker's `WorkerNavigator`).
    let tag: Option<&str> = if name != def.name { Some(name) } else { def.tag };
    note_alias(def, name);
    let define_tag = |scope: &mut v8::PinScope<'s, '_>| {
        if let Some(tag) = tag {
            let tag_v = str_key(scope, tag);
            let mut td = v8::PropertyDescriptor::new_from_value_writable(tag_v.into(), false);
            td.set_enumerable(false);
            td.set_configurable(true);
            let key = v8::Symbol::get_to_string_tag(scope);
            proto.define_property(scope, key.into(), &td);
        }
    };
    // The prototype's own keys go down in ONE sequence, with `constructor` seated inside it: the
    // members the capture puts in front of it, then `constructor` and the tag, then the rest. The two
    // old shapes are the two ends of this same loop (`ctor_at == 0` and `ctor_at == proto.len()`).
    for m in mount.proto.iter().take(ctor_at) {
        put_member(scope, proto, m, def.name, true, ledger, Pass::Create, flavor);
    }
    define_ctor(scope, ledger);
    define_tag(scope);
    for m in mount.proto.iter().skip(ctor_at) {
        put_member(scope, proto, m, def.name, true, ledger, Pass::Create, flavor);
    }
    for m in &mount.statics {
        put_member(scope, ctor_obj, m, def.name, false, ledger, Pass::Create, flavor);
    }
    // The member backfill's rows, when this interface is one it finishes: what `backfill_members` would
    // append a moment later, here inside the recording -- and only where that pass reaches, an interface
    // published under its own name (it reads the global). A shell the capture does not publish keeps its
    // bare prototype, as it did: `HTMLFencedFrameElement` on a device without it (the status file's layer
    // count caught the difference, 12 rows on two such shells).
    if let Some(b) = backfill {
        if name == def.name && publish {
            backfill_one(scope, def, proto, ctor_obj, b, ledger);
            // A staged create (the DOM spine's `HTML*Element` shells: an empty selector, every row from the
            // backfill) put `constructor` down before its members arrived: seat it now, inside the
            // recording, so the template has it where the capture does ([`seat_constructor`]).
            if let Some(surface) = crate::install::surface::active(scope) {
                if seat_constructor(scope, &surface, name, proto) == Some(false) {
                    eprintln!("[korvus] {name}: a non-configurable key keeps `constructor` from its seat");
                }
            }
        }
    }
    // The WebIDL arity guards on this interface's methods, here inside the recording: a registry
    // interface published under its own name. The surface-finishing pass that used to add them after the
    // install found every one guarded here (0 wraps in 45 runs: 5 bundles x every realm) and was deleted.
    if name == def.name && publish {
        if let Some(ar) = arity_pass_realm(scope, def) {
            for m in proto_members(def, ar) {
                if kind_letter(m) == "m" && m.required > 0 {
                    guard_member(scope, proto, def.name, m.name, m.required as i32, m.ty, Holder::Prototype);
                }
            }
            for m in static_members(def, ar) {
                if kind_letter(m) == "m" && m.required > 0 {
                    guard_member(scope, ctor_obj, def.name, m.name, m.required as i32, m.ty, Holder::InterfaceObject);
                }
            }
        }
    }
    // A `[Global]` interface's own members go on the GLOBAL OBJECT (WebIDL), inside the recording, after
    // the interface object is on the global -- the order its install always had.
    let mut global_names: Vec<&str> = Vec::new();
    if !global_rows.is_empty() {
        publish_on_global(scope, global, name, ctor.into());
        for m in global_rows {
            put_member(scope, global, m, def.name, true, ledger, Pass::Create, flavor);
            global_names.push(m.name);
        }
    }
    let gaps = mount.gaps;
    let on_global = (!global_names.is_empty()).then_some((global, global_names.as_slice()));
    let (ctor, proto) = tpl_done_with(scope, tpl, ledger_mark, name, def.parent, ctor, proto, ledger, &gaps, on_global).unwrap_or((ctor, proto));
    finish_create(scope, global, name, parent, publish_rule, publish, ctor, proto, gaps)
}

/// The rows are ledgered under the TABLE's name and mounted under the EXPOSED one, so the audit needs
/// the join. Derived at the one place the two names differ, rather than written down anywhere.
fn note_alias(def: &'static InterfaceDef, name: &str) {
    if name != def.name {
        if let Ok(mut a) = alias_map().lock() {
            a.insert(name.to_string(), def.name.to_string());
        }
    }
}

/// [`create`]'s tail, the same for a built interface and an instantiated one: the inheritance splice
/// and the publish.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn finish_create<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    name: &str,
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
    publish_rule: Publish,
    publish: bool,
    ctor: v8::Local<'s, v8::Function>,
    proto: v8::Local<'s, v8::Object>,
    gaps: Vec<Gap>,
) -> Created<'s> {
    if let Some((pproto, pctor)) = parent {
        proto.set_prototype(scope, pproto.into());
        ctor.set_prototype(scope, pctor.into());
    }
    // PUBLISHED ON THE GLOBAL ONLY IF THIS DEVICE HAS THIS INTERFACE.
    //
    // The member join above decides WHICH ROWS; this decides whether the interface OBJECT exists at
    // all, and it has to be here rather than only in `install_registry` because three other paths
    // create interfaces: the DOM spine's `HTML*Element` pass (which walks the registry's own parent
    // links, so it minted Chrome's `HTMLFencedFrameElement` on a Brave page), `build_newable` for the
    // newable hand tables, and every `install::live::*` domain install (which is how Brave's
    // `FontFaceSet` appeared on a Chrome one). Both were measured on the live global against each
    // capture's own `globals` list: one korvus-only name in each direction, and each is a tell a page
    // reads with `typeof`.
    //
    // The test is `members_of(name).is_some()` -- does the ACTIVE capture record this interface --
    // and not a scan of `globals`, because it is a hash lookup rather than a walk of 1,206 strings per
    // interface, and because the two answers are the same answer: every function-typed entry in
    // `interfaces` is also in `globals`, in both realms of both bundles (measured).
    //
    // The prototype is still BUILT and returned. A caller that splices children onto it keeps working,
    // and an interface object no name reaches is exactly what `worker_phantom_proto` already is: off
    // the global is not off the surface, it is simply unreachable by name -- which is what a device
    // that does not have the interface looks like.
    //
    // ONE NAME IS EXEMPT, and it is the realm's own global scope — see [`create_global_scope`] for
    // the whole of why. In short: the capture rig never enters a shared or service worker, so its
    // silence about `SharedWorkerGlobalScope` is a blind spot rather than a device answer, and a
    // realm cannot fail to have the interface it is running inside.
    //
    // AND ONLY IF ITS EXPOSURE CONDITION HOLDS FOR THIS DOCUMENT'S ORIGIN. That is the
    // SECOND question, of a different kind: the first is a persona fact off the capture, this one is a
    // platform fact carried by the row (`def.exposure`), and it passes through the same
    // `exposure::gate` FP5.7 put in front of the hand-installed globals — one gate, one recording, one
    // removal pass, one reading of `potentially_trustworthy`. 230 of the ~248 names an insecure origin
    // takes off a Chromium window come through HERE -- asked by [`create`] before it builds
    // ([`publishes`]), and handed in.
    if publish {
        publish_on_global(scope, global, name, ctor.into());
    } else if publish_rule == Publish::IfCaptured {
        // Not a global on this device -- but the engine may still mint INSTANCES on it: Chrome 150
        // has no `FontFaceSet` global and `document.fonts` is a FontFaceSet all the same.
        // The prototype is kept where `shell_proto` finds it, without a `constructor`.
        crate::binding::protos::record_hidden_proto(scope, global, name, proto);
    }
    Created { proto, ctor, gaps }
}

/// Whether [`create_interface`] (`Flavor::Table`, published under its own name, no selector) would INSTANTIATE
/// `def` here rather than build it: its template is in this isolate or in the process's blueprint, under the key
/// that create would use. The key is noted for [`crate::install::templates::tpl_by_name`], which finds a
/// blueprint only by a key some create noted -- a lazy interface's first read makes it through there.
pub(crate) fn has_table_template(scope: &mut v8::PinScope, def: &'static InterfaceDef) -> Option<Vec<LedgerEntry>> {
    has_template_for(scope, def, Flavor::Table, Exposed::Both)
}

/// Record, for an interface put LAZILY, the ledger rows its create would have recorded -- its template's
/// (`templates::template_rows`), in this realm, as [`tpl_step`] replays them on an instantiate.
pub(crate) fn record_lazy_rows(scope: &mut v8::PinScope, ledger: &mut InstallLedger, rows: &[LedgerEntry]) {
    let here = realm_of(scope);
    for e in rows {
        ledger.record(here, e.iface, e.member, e.holder, e.pass);
    }
}

/// Put table interface `def` on the PAGE global LAZILY instead of creating it -- an install loop's
/// `create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ..)` whose result it does not use, on a
/// blueprint hit -- as WebGL's (`live::canvas`) and the registry's leaves are, recording into `ledger` the rows its
/// create would have recorded ([`record_lazy_rows`]). Its first read makes it from its template
/// (`templates::realm_interface`), chained as the install that recorded it chained it (the persisted final chains).
/// `false` -- create it as ever -- in any realm but the page's, for an interface with aliases, on a miss, or when this
/// device does not publish it.
pub(crate) fn put_lazy_table<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, def: &'static InterfaceDef, ledger: &mut InstallLedger) -> bool {
    if !def.aliases.is_empty()
        || crate::install::surface::realm(scope) != Some(spec::SurfaceRealm::Window)
        || crate::core::html::browsing_context::main_global(scope) != global
    {
        return false;
    }
    let Some(rows) = has_table_template(scope, def) else { return false };
    if !(publishes_interface(scope, def) && crate::install::templates::put_lazy_global(scope, global, def.name, def.name, false, true)) {
        return false;
    }
    record_lazy_rows(scope, ledger, &rows);
    crate::install::templates::count_lazy(scope);
    if crate::install::templates::reporting() {
        crate::install::templates::report(&format!("LAZY-PAGE put {}", def.name));
    }
    true
}

/// [`has_table_template`] for a create of `flavor` in `realm` ([`create_interface_in`]: the registry's shells).
fn has_template_for(scope: &mut v8::PinScope, def: &'static InterfaceDef, flavor: Flavor, realm: Exposed) -> Option<Vec<LedgerEntry>> {
    if !crate::install::templates::wanted(scope) {
        return None;
    }
    let key = tpl_key(scope, def, def.name, None, &format!("{flavor:?}"), realm);
    crate::install::templates::note_key(scope, def.name, &key);
    crate::install::templates::template_rows(scope, &key)
}

/// [`publishes`] for an interface published under its own name with [`Publish::IfCaptured`], as
/// [`create_interface`] decides it -- for an install that puts the name itself (a lazy interface object).
pub(crate) fn publishes_interface(scope: &mut v8::PinScope, def: &'static InterfaceDef) -> bool {
    publishes(scope, def.name, Publish::IfCaptured, def)
}

/// Whether [`finish_create`] puts the interface object on the global (see its comment): asked ONCE per
/// create, because the gate records the `[SecureContext]` names it lets through.
fn publishes(scope: &mut v8::PinScope, name: &str, publish_rule: Publish, def: &'static InterfaceDef) -> bool {
    publish_rule == Publish::Always
        || crate::core::html::exposure::gate(scope, crate::core::html::exposure::Publishes::Interface, name, def.exposure)
}

/// The realm whose WebIDL arity guards [`finish_create`] puts on `def`'s methods, if any: a
/// generated-registry interface -- the window's `REGISTRY`, the worker's `WORKER_ORDER` -- that the realm
/// exposes (the reach of the surface-finishing arity pass this step replaced).
fn arity_pass_realm(scope: &mut v8::PinScope, def: &'static InterfaceDef) -> Option<Exposed> {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static WINDOW: OnceLock<HashSet<usize>> = OnceLock::new();
    static WORKER: OnceLock<HashSet<usize>> = OnceLock::new();
    let addr = def as *const InterfaceDef as usize;
    match crate::install::surface::realm(scope) {
        Some(spec::SurfaceRealm::Window) | None => {
            let set = WINDOW.get_or_init(|| crate::install::generated_registry::REGISTRY.iter().map(|d| *d as *const InterfaceDef as usize).collect());
            (set.contains(&addr) && def.exposed.covers(Exposed::Window)).then_some(Exposed::Window)
        }
        Some(spec::SurfaceRealm::Worker) => {
            let set = WORKER.get_or_init(|| crate::install::generated_registry::WORKER_ORDER.iter().map(|d| *d as *const InterfaceDef as usize).collect());
            (set.contains(&addr) && def.exposed.covers(Exposed::Worker)).then_some(Exposed::Worker)
        }
    }
}

/// The constructor `length` the generated row for `name` declares — for the three LEGACY FACTORIES
/// (`Image`, `Audio`, `Option`), which cannot go through [`create_interface_in`] at all.
///
/// A factory's `.prototype` IS another interface's, so it cannot be built until that interface
/// exists, which is long after the registry walk; each one is therefore a function built by its own
/// install and assigned over the shell. What it must NOT also do is invent the shape: `html_image.rs`
/// passed the literal `2` and `Image.length` answered 2, where the capture records **0** and headless
/// Chrome 150.0.7871.46 answers `0` (measured 2026-09-12, over a loopback server, alongside
/// `Audio.length` 0 and `Option.length` 0). The row has carried the right number the whole time —
/// `CtorDef { name: "Image", length: 0 }` — so the factories read it from there now, which is the
/// same rule the one create follows.
pub fn row_ctor_length(name: &str) -> i32 {
    REGISTRY
        .iter()
        .find(|d| d.name == name)
        .and_then(|d| d.ctor.as_ref())
        .map(|c| c.length as i32)
        .unwrap_or(0)
}

// ── the registry passes ─────────────────────────────────────────────────────────────────────────

/// Install every registry interface's create-level shape (constructor + prototype + tag + members +
/// chain) onto `global`, **in the ACTIVE bundle's own-global order**. Skips any name already present
/// (a hand-written interface the engine implements), exactly as the shell create loop did, any row the
/// realm does not expose, and — the L7b half — any row the active capture does not list among its
/// globals at all.
///
/// WHY THE ORDER IS THE CAPTURE'S AND NOT THE TABLE'S. `Object.getOwnPropertyNames(globalThis)` is a
/// SEQUENCE a page reads in three lines, and the two realms' captures do not even agree with each
/// other on it (they diverge at the 40th name), which is why the worker walk used to need a separate
/// `WORKER_ORDER` permutation of the same rows. With the active capture driving the walk, the order is
/// whatever that device does, both realms, every bundle — and `WORKER_ORDER` is only the fallback a
/// bare runtime with no persona gets.
///
/// The union carries the Chromium family's rows, so under one bundle a row of another bundle's is
/// simply not named here and is never created. That is the subtraction half of P3, and it is why
/// `navigator.brave` can be absent from a build that still carries its row.
pub fn install_registry<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, registry: &'static [&'static InterfaceDef], realm: Exposed, ledger: &mut InstallLedger) {
    crate::behavior_rt::touch_init(); // the impact recorder's flag, before any member exists
    let order = realm_order(scope, registry, realm);
    // On the PAGE, a registry interface no other registry interface inherits from -- a LEAF -- and
    // whose template this realm has (a hit) goes on the global as a lazy interface object, made from its template
    // on first read, as WebGL's are (`live::canvas`). A parent stays eager: its children's creates and chains read it.
    let lazy_page = realm == Exposed::Window && crate::core::html::browsing_context::main_global(scope) == global;
    let parents: std::collections::HashSet<&str> = if lazy_page { registry.iter().filter_map(|d| d.parent).collect() } else { Default::default() };
    for step in walk_steps(scope, registry, realm) {
        // One handle scope per interface: the walk builds ~900 of them, and every scavenge during it
        // visits every live handle, so one scope for the whole walk made the install quadratic in GC
        // time -- ~100 ms per realm, measured on a frame realm's install.
        v8::scope!(let scope, scope);
        let global = v8::Local::new(scope, global);
        let d = match step {
            WalkStep::Row(d) => d,
            WalkStep::Hand(h) => {
                (h.create)(scope, global, ledger);
                continue;
            }
        };
        // A snapshot maker leaves V8's late names to V8 ([`V8_LATE`]): a stand-in made here would be baked in
        if v8_late(d.name) {
            continue;
        }
        let name = str_key(scope, d.name);
        if global.get(scope, name.into()).map(|v| !v.is_undefined()).unwrap_or(false) {
            continue;
        }
        if lazy_page && !parents.contains(d.name) {
            if let Some(rows) = has_template_for(scope, d, Flavor::Shell, realm) {
                if publishes(scope, d.name, Publish::IfCaptured, d) && crate::install::templates::put_lazy_global(scope, global, d.name, d.name, false, true) {
                    record_lazy_rows(scope, ledger, &rows);
                    crate::install::templates::count_lazy(scope);
                    if crate::install::templates::reporting() {
                        crate::install::templates::report(&format!("LAZY-PAGE put {}", d.name));
                    }
                    continue;
                }
            }
        }
        create_interface_in(scope, global, d, d.name, None, None, Flavor::Shell, realm, ledger);
    }
    // The chains, over the same set -- including the names this walk SKIPPED because something had
    // already created them, which is the behaviour this pass has always had.
    let any_lazy = crate::install::templates::has_lazy_globals(scope);
    for d in &order {
        let Some(parent) = d.parent else { continue };
        // a lazy leaf chains when it is made (`templates::realm_interface`: its template's parent, the page's own)
        if any_lazy && crate::install::templates::lazy_global(scope, global, d.name).is_some() {
            continue;
        }
        wire_chain(scope, global, d.name, parent);
    }
}

/// The registry rows this realm installs, in the order it installs them: the ACTIVE capture's
/// own-global sequence, restricted to rows the capture names; or, with no active surface, every row
/// the realm exposes in table order.
///
/// One function because both realms' walks ask the same question, and because "which rows does this
/// device have" must have exactly one answer per realm -- `install_worker_registry` used to get its
/// order from a second generated slice over the same rows.
/// A global a HAND table owns that the registry walk creates AT ITS CAPTURE POSITION.
///
/// `Object.getOwnPropertyNames(globalThis)` is a sequence (`tests/global_order.rs`,
/// `worker_surface.rs` ratchet it), and a name created after the walk lands after every name the walk
/// created -- measured on the window: `NetworkInformation` created from the navigator mount instead of
/// by the registry put 533 names out of place against a ceiling of 531, and the ONE position at which
/// it costs nothing is between `OffscreenCanvas` and `NavigatorUAData`, i.e. inside the walk. So a
/// table leaving the registry names its creator here and the walk calls it where the row used to be.
/// (The walk's last step, [`seat_globals_in_capture_order`], places every global at
/// its capture position whenever it was created, so the position is no longer what an entry here buys;
/// what it still buys is ONE creator for the global, called at a moment the domain's later install can
/// rely on.)
/// `create` is the ONE creator of that global; the domain's later install only chains or fills what
/// the global needs from interfaces that do not exist yet at walk time.
pub struct OrderedHand {
    pub name: &'static str,
    pub realm: Exposed,
    pub create: for<'s, 'i> fn(&mut v8::PinScope<'s, 'i>, v8::Local<'s, v8::Object>, &mut InstallLedger),
}

enum WalkStep {
    Row(&'static InterfaceDef),
    Hand(&'static OrderedHand),
}

/// The walk's steps: the ACTIVE capture's own-global sequence, each name either a registry row or an
/// [`OrderedHand`] for this realm. With no active surface (a bare runtime), every exposed row in table
/// order and then every hand creator, so each still runs exactly once.
fn walk_steps(scope: &mut v8::PinScope, registry: &'static [&'static InterfaceDef], realm: Exposed) -> Vec<WalkStep> {
    let hands: Vec<&'static OrderedHand> =
        crate::install::live::ORDERED_HAND.iter().filter(|h| h.realm.covers(realm)).collect();
    let exposed: Vec<&'static InterfaceDef> = registry.iter().copied().filter(|d| d.exposed.covers(realm)).collect();
    let Some(s) = crate::install::surface::active(scope) else {
        return exposed.into_iter().map(WalkStep::Row).chain(hands.into_iter().map(WalkStep::Hand)).collect();
    };
    let rows: std::collections::HashMap<&str, &'static InterfaceDef> = exposed.iter().map(|d| (d.name, *d)).collect();
    let by_hand: std::collections::HashMap<&str, &'static OrderedHand> = hands.iter().map(|h| (h.name, *h)).collect();
    s.globals
        .iter()
        .filter_map(|g| match rows.get(g.as_str()) {
            Some(d) => Some(WalkStep::Row(d)),
            None => by_hand.get(g.as_str()).map(|h| WalkStep::Hand(h)),
        })
        .collect()
}

/// THE WALK'S LAST STEP: every own global this realm has, at its capture position.
///
/// [`walk_steps`] CREATES the registry rows and the [`OrderedHand`] globals in the active capture's
/// `globals` sequence. It does not create the rest, and the rest was the whole of the order defect:
/// measured at `fd6473b8`, 531..533 window names and 120 worker names out of place, every one of
/// them created OUTSIDE the walk -- before any install by V8 and `deno_core` (`Temporal`,
/// `SuppressedError`, `DisposableStack`, `AsyncDisposableStack`, `Float16Array`, `queueMicrotask`,
/// `WebAssembly`), before the walk by the bootstrap (`EventTarget`, `TextEncoder`, `Screen`, the
/// `DOMPoint`/`DOMRect`/`DOMMatrix` class templates, the DOM spine -- present, so the walk skips
/// them), and after it by some thirty `install_window_*` passes (`window`, `document`, `location`,
/// `chrome`, ...). Many of those passes have a MEASURED position -- see
/// `install::live`'s index -- so their creation moment cannot be re-sequenced name by name without
/// reopening defects each of them recorded.
///
/// What CAN follow the sequence is where each name ends up, because the sequence a page reads is
/// enumeration order and V8 gives a global property a new enumeration index when it is defined
/// afresh. So the walk runs once more, over the same `globals` list, at the end of the realm's
/// install, and SEATS each name: its own descriptor is read, the property deleted and defined again
/// with that exact descriptor. The value is the same object (identity is untouched), the four
/// attributes are copied, and the name now enumerates after everything seated before it. One
/// sequence decides creation (the walk) and position (this), and it is the worn bundle's.
///
/// THE PREFIX IS LEFT ALONE: names up to the first place the realm and the capture disagree are
/// already where the capture has them, and they include V8's three non-configurable values
/// (`Infinity`, `NaN`, `undefined`), which no define can move. Seating starts at the divergence.
///
/// A NON-CONFIGURABLE GLOBAL cannot be deleted, so a creator whose property must end
/// non-configurable (`[LegacyUnforgeable]` `window`/`document`/`location`/`top`) defines it
/// configurable and calls [`lock_at_seat`]; this pass seats it and makes it non-configurable in the
/// same define, and locks any it does not reach (a name the capture does not list, or a realm with no
/// active surface) where it stands. No page script can run between the creator and this pass -- it is
/// called immediately before `seal_interface_protos`, whose contract is exactly that.
///
/// Returns the names it could not move (non-configurable and not locked here) -- empty in both realms
/// under every bundle in the tree, which `global_order.rs` and `worker_surface.rs` observe as an
/// `out_of_place` of 0.
pub fn seat_globals_in_capture_order(scope: &mut v8::PinScope) -> Vec<String> {
    let s = crate::install::surface::active(scope);
    seat_globals_in_order(scope, s.as_ref().map(|s| s.globals.as_slice()), &[])
}

/// [`seat_globals_in_capture_order`] with the capture's global order given -- a restore seats by the order its
/// snapshot carries, and so never parses the surface (a megabyte of JSON) only to read it -- and the names `keep`
/// lets stay where they stand ([`kept_run`]; empty for every install).
pub(crate) fn seat_globals_in_order(scope: &mut v8::PinScope, order: Option<&[String]>, keep: &[String]) -> Vec<String> {
    let t0 = crate::install::templates::reporting().then(std::time::Instant::now);
    let global = scope.get_current_context().global(scope);
    // A snapshot maker's locks wait for the restore's seat, which runs after V8's late names ([`V8_LATE`])
    let locks = if V8_LATE.get().is_some() { Default::default() } else { take_seat_locks(scope, global) };
    let mut stuck = Vec::new();
    let mut moved = 0;
    if let Some(globals) = order {
        let own = own_names(scope, global);
        let present: std::collections::HashSet<&str> = own.iter().map(String::as_str).collect();
        let captured: std::collections::HashSet<&str> = globals.iter().map(String::as_str).collect();
        let sequence: Vec<&str> = globals.iter().map(String::as_str).filter(|g| present.contains(g)).collect();
        let realm: Vec<&str> = own.iter().map(String::as_str).filter(|g| captured.contains(g)).collect();
        let start = sequence.iter().zip(realm.iter()).take_while(|(a, b)| a == b).count();
        let kept = kept_run(&own, &sequence[start..], keep);
        for name in &sequence[start + kept..] {
            if seat_one(scope, global, name, locks.contains(*name)) {
                moved += 1;
            } else {
                stuck.push(name.to_string());
            }
        }
    }
    let mut locked = 0;
    // A lock this walk did not reach still has to hold: locked in place, which a define of the same
    // key with `configurable: false` does without moving it.
    for name in &locks {
        let k = str_key(scope, name);
        let Some(d) = global.get_own_property_descriptor(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
            continue;
        };
        if descriptor_flag(scope, d, "configurable") {
            let pd = descriptor_from(scope, d, false);
            global.define_property(scope, k.into(), &pd);
            locked += 1;
        }
    }
    crate::install::templates::report_pass(scope, "seat_globals_in_capture_order", moved);
    crate::install::templates::report_pass(scope, "seat_globals_in_capture_order/locks", locked);
    if let Some(t) = t0 {
        let ord = realm_ordinal(scope);
        crate::install::templates::report(&format!("TIME seat_globals_in_capture_order r{ord} {}us", t.elapsed().as_micros()));
    }
    stuck
}

/// Seat `proto`'s `constructor` where the capture gives it for the interface exposed as `name`:
/// for a staged create, whose members arrive after its `constructor` -- the DOM spine's
/// `HTML*Element` shells, every row from `finish_create`'s backfill -- which calls this inside the
/// recording, so the template has it in place. Every other create seats `constructor` as it goes
/// (`Mount::ctor_at`).
///
/// It moves ONE key: `constructor` comes out and goes back in behind the members the capture puts in
/// front of it, carrying its own descriptor; every other key keeps its descriptor and its position
/// relative to every other key. An end-of-install pass made this move for every interface on the global
/// (`seat_constructors_in_capture_order`, deleted): it moved these 26-28 shells in every
/// page realm, and a realm built from templates was given the move as a delta; with the seat taken here
/// it moved nothing in any realm of any bundle.
///
/// `None`: nothing to do (the capture records no seat, the prototype has no `constructor`, or it is
/// seated already); `Some(false)`: a non-configurable own key behind the seat.
fn seat_constructor<'s>(scope: &mut v8::PinScope<'s, '_>, surface: &spec::RealmSurface, name: &str, proto: v8::Local<'s, v8::Object>) -> Option<bool> {
    let ci = surface.members_of(name).and_then(|o| o.ctor_index)?;
    let before: std::collections::HashSet<&str> = surface
        .members_of(name)
        .map(|o| o.members.iter().take(ci).map(String::as_str).collect())
        .unwrap_or_default();
    let own = own_names(scope, proto);
    let cur = own.iter().position(|n| n == "constructor")?;
    // WHERE IT BELONGS, counted in the keys this realm actually has rather than read off the
    // capture's index: a member the capture records and this engine does not would otherwise
    // push `constructor` past the end of a shorter list.
    let others: Vec<String> = own.into_iter().filter(|n| n != "constructor").collect();
    let target = others.iter().filter(|n| before.contains(n.as_str())).count();
    if cur == target {
        return None;
    }
    Some(reseat_constructor(scope, proto, &others[target..]))
}

/// Lift `constructor` out of `proto` and put it back in front of `tail`, which is re-defined behind
/// it with the descriptors it had. `false` if any key involved is non-configurable, in which case
/// nothing is left half-moved: the tail is read before anything is deleted.
fn reseat_constructor<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>, tail: &[String]) -> bool {
    let ck = str_key(scope, "constructor");
    let Some(cd) = proto.get_own_property_descriptor(scope, ck.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return true;
    };
    if !descriptor_flag(scope, cd, "configurable") {
        return false;
    }
    let mut saved = Vec::with_capacity(tail.len());
    for n in tail {
        let k = str_key(scope, n);
        let Some(d) = proto.get_own_property_descriptor(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
            continue;
        };
        if !descriptor_flag(scope, d, "configurable") {
            return false;
        }
        saved.push((n.clone(), descriptor_from(scope, d, true)));
    }
    let ctor_pd = descriptor_from(scope, cd, true);
    for (n, _) in &saved {
        let k = str_key(scope, n);
        proto.delete(scope, k.into());
    }
    proto.delete(scope, ck.into());
    proto.define_property(scope, ck.into(), &ctor_pd);
    for (n, pd) in &saved {
        let k = str_key(scope, n);
        proto.define_property(scope, k.into(), pd);
    }
    true
}

/// Delete `name` from `global` and define it again with the descriptor it had (non-configurable if
/// `lock`), so it enumerates last. `false` if it is non-configurable and so cannot be moved.
fn seat_one<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str, lock: bool) -> bool {
    // A lazy interface object moves as it is -- out, and in again, lazy -- as the frame path moves its
    // rows: reading its descriptor would make the interface.
    if let Some((iface, enumerable, writable)) = crate::install::templates::lazy_global(scope, global, name) {
        let k = str_key(scope, name);
        if global.delete(scope, k.into()) != Some(true) {
            return false;
        }
        crate::install::templates::forget_lazy_global(scope, global, name);
        return crate::install::templates::put_lazy_global(scope, global, name, &iface, enumerable, writable);
    }
    let k = str_key(scope, name);
    let Some(d) = global.get_own_property_descriptor(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return true;
    };
    if !descriptor_flag(scope, d, "configurable") {
        return false;
    }
    let pd = descriptor_from(scope, d, !lock);
    if global.delete(scope, k.into()) != Some(true) {
        return false;
    }
    global.define_property(scope, k.into(), &pd);
    true
}

fn descriptor_flag(scope: &mut v8::PinScope, d: v8::Local<v8::Object>, flag: &str) -> bool {
    let k = str_key(scope, flag);
    d.get(scope, k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false)
}

/// A `PropertyDescriptor` carrying exactly what `Object.getOwnPropertyDescriptor` answered, with
/// `configurable` as given.
fn descriptor_from<'s>(scope: &mut v8::PinScope<'s, '_>, d: v8::Local<'s, v8::Object>, configurable: bool) -> v8::PropertyDescriptor {
    let gk = str_key(scope, "get");
    let mut pd = if d.has_own_property(scope, gk.into()).unwrap_or(false) {
        let get = d.get(scope, gk.into()).unwrap_or_else(|| v8::undefined(scope).into());
        let sk = str_key(scope, "set");
        let set = d.get(scope, sk.into()).unwrap_or_else(|| v8::undefined(scope).into());
        v8::PropertyDescriptor::new_from_get_set(get, set)
    } else {
        let vk = str_key(scope, "value");
        let value = d.get(scope, vk.into()).unwrap_or_else(|| v8::undefined(scope).into());
        let writable = descriptor_flag(scope, d, "writable");
        v8::PropertyDescriptor::new_from_value_writable(value, writable)
    };
    let enumerable = descriptor_flag(scope, d, "enumerable");
    pd.set_enumerable(enumerable);
    pd.set_configurable(configurable);
    pd
}

fn seat_lock_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = str_key(scope, "korvus.seat.locks");
    v8::Private::for_api(scope, Some(n))
}

/// Mark own global `name` as one [`seat_globals_in_capture_order`] makes NON-CONFIGURABLE when it
/// seats it. The creator defines the property `configurable: true` and calls this; nothing between
/// the two can observe the difference, because no page script runs before the seat.
pub fn lock_at_seat(scope: &mut v8::PinScope, global: v8::Local<v8::Object>, name: &str) {
    let key = seat_lock_key(scope);
    let list = match global.get_private(scope, key).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => a,
        None => {
            let a = v8::Array::new(scope, 0);
            global.set_private(scope, key, a.into());
            a
        }
    };
    let v = str_key(scope, name);
    list.set_index(scope, list.length(), v.into());
}

/// The context snapshot: the names V8 puts on a context's global only when its isolate is not a snapshot
/// maker's -- the shipped features `InitializeExperimentalGlobal` installs (`Temporal`, `SuppressedError`,
/// `Float16Array`, ...) and the special objects (`WebAssembly`; bootstrapper.cc). A snapshot leaves them out by design,
/// and a context made from one gets them again, after the snapshot's own names. Set in the maker only
/// ([`set_v8_late`]): its walk leaves them to V8 -- no stand-in row -- and its seat leaves the locks to the restore's,
/// which seats every name again once V8 has added its own (`snapshot::PageStatics::apply`).
static V8_LATE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// [`V8_LATE`]: the names a plain context's global has and a maker's lacks, both read before any install.
pub fn set_v8_late(plain: &[String], maker: &[String]) -> &'static [String] {
    let made: std::collections::HashSet<&str> = maker.iter().map(String::as_str).collect();
    V8_LATE.get_or_init(|| plain.iter().filter(|n| !made.contains(n.as_str())).cloned().collect())
}

/// What [`set_v8_late`] set (a maker's), or nothing.
#[cfg(not(feature = "analysis"))]
pub(crate) fn v8_late_names() -> &'static [String] {
    V8_LATE.get().map(Vec::as_slice).unwrap_or_default()
}

/// Whether `name` is one of [`V8_LATE`] (in a maker only).
fn v8_late(name: &str) -> bool {
    V8_LATE.get().is_some_and(|l| l.iter().any(|n| n == name))
}

/// A global's own names (strings, every attribute), as the seat reads them.
pub fn global_names(scope: &mut v8::PinScope) -> Vec<String> {
    let global = scope.get_current_context().global(scope);
    own_names(scope, global)
}

/// How many names at the head of `rest` (the capture's order after the divergence) may stay where they stand, which
/// is a restore's: names `keep` lists (V8's late names, which V8 appended after the snapshot's own -- among
/// them `Temporal`, whose descriptor read builds the whole namespace), standing in that order, and only when every own
/// name after the first of them is one the seat keeps or defines again (`rest`) -- so defining the rest after them
/// gives the capture's order. 0 when `keep` is empty: every install seats every name, as ever.
fn kept_run(own: &[String], rest: &[&str], keep: &[String]) -> usize {
    if keep.is_empty() || rest.is_empty() {
        return 0;
    }
    let at: std::collections::HashMap<&str, usize> = own.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    let mut last = None;
    let mut n = 0;
    for name in rest {
        let Some(&p) = at.get(name) else { break };
        if !keep.iter().any(|k| k == name) || last.is_some_and(|l| p <= l) {
            break;
        }
        last = Some(p);
        n += 1;
    }
    if n == 0 {
        return 0;
    }
    let seated: std::collections::HashSet<&str> = rest.iter().copied().collect();
    if own[at[rest[0]]..].iter().all(|o| seated.contains(o.as_str())) { n } else { 0 }
}

/// The names [`lock_at_seat`] recorded, and the record deleted -- the seat runs once per realm.
fn take_seat_locks(scope: &mut v8::PinScope, global: v8::Local<v8::Object>) -> std::collections::BTreeSet<String> {
    let key = seat_lock_key(scope);
    let mut out = std::collections::BTreeSet::new();
    if let Some(a) = global.get_private(scope, key).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        for i in 0..a.length() {
            if let Some(v) = a.get_index(scope, i) {
                out.insert(v.to_rust_string_lossy(scope));
            }
        }
    }
    global.delete_private(scope, key);
    out
}

pub(crate) fn realm_order(scope: &mut v8::PinScope, registry: &'static [&'static InterfaceDef], realm: Exposed) -> Vec<&'static InterfaceDef> {
    let exposed: Vec<&'static InterfaceDef> = registry.iter().copied().filter(|d| d.exposed.covers(realm)).collect();
    let Some(s) = crate::install::surface::active(scope) else { return exposed };
    let by_name: std::collections::HashMap<&str, &'static InterfaceDef> = exposed.iter().map(|d| (d.name, *d)).collect();
    s.globals.iter().filter_map(|g| by_name.get(g.as_str()).copied()).collect()
}

fn wire_chain<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str, parent: &str) {
    let Some(proto) = ctor_proto(scope, global, name) else { return };
    if let Some(pproto) = ctor_proto(scope, global, parent) {
        proto.set_prototype(scope, pproto.into());
    }
    // ...AND THE INTERFACE OBJECT (WebIDL "create an interface object": its [[Prototype]] is the inherited
    // interface's interface object), as `finish_create`'s splice does: this pass linked only the prototypes, so 206
    // window interfaces -- `CSSMediaRule`, `Accelerometer`, `SVGSVGElement` -- answered `Object.getPrototypeOf(X)`
    // with `Function.prototype` where Chrome 150 answers the parent (found by a registry probe).
    if let (Some(c), Some(pc)) = (ctor_obj(scope, global, name), ctor_obj(scope, global, parent)) {
        c.set_prototype(scope, pc.into());
    }
}

/// `globalThis[name]` as an interface object (a function), if present.
fn ctor_obj<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    let k = str_key(scope, name);
    global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()).map(Into::into)
}

/// `globalThis[name].prototype` as an object, if present.
fn ctor_proto<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    let k = str_key(scope, name);
    let ctor = global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let pk = str_key(scope, "prototype");
    ctor.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// The member backfill: on every `backfill` interface's (pre-existing, natively implemented or
/// hand-written) prototype, additively install the catalogued members it does not already OWN. A
/// hand-written implementation always wins (`has_own_property`). Each row lands through `put_member`,
/// so a slot-backed row reads the instance's private slot, an `on*` row is the event-handler pair and a
/// `[Reflect]` row is the reflected accessor -- the bodies the table names.
pub fn backfill_members<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, registry: &'static [&'static InterfaceDef], realm: Exposed, ledger: &mut InstallLedger) {
    for d in registry {
        if !d.backfill || !d.exposed.covers(realm) {
            continue;
        }
        let name = str_key(scope, d.name);
        let Some(cv) = global.get(scope, name.into()) else { continue };
        let Ok(cfun) = v8::Local::<v8::Function>::try_from(cv) else { continue };
        let c_obj: v8::Local<v8::Object> = cfun.into();
        let pk = str_key(scope, "prototype");
        let Some(proto) = c_obj.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { continue };
        backfill_one(scope, d, proto, c_obj, realm, ledger);
    }
}

/// [`backfill_members`] for one interface: every selected row its prototype (then its interface object)
/// does not already OWN, appended through `put_member`. Also run inside a create that the backfill
/// finishes ([`create_interface_backfilled`]).
fn backfill_one<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    d: &'static InterfaceDef,
    proto: v8::Local<'s, v8::Object>,
    c_obj: v8::Local<'s, v8::Object>,
    realm: Exposed,
    ledger: &mut InstallLedger,
) {
    // THE SAME JOIN THE ONE CREATE DOES. This pass ADDS members, so it selects like
    // any other install: the active capture decides which of the catalogued rows this device has
    // and in what order they are appended, and a row it does not name is not backfilled.
    for m in selected_rows(scope, d, d.name, realm, false) {
        if m.name == "constructor" {
            continue;
        }
        let key = member_key(scope, m.name);
        if proto.has_own_property(scope, key).unwrap_or(false) {
            continue;
        }
        put_member(scope, proto, m, d.name, true, ledger, Pass::Backfill, Flavor::Shell);
    }
    for m in selected_rows(scope, d, d.name, realm, true) {
        let key = member_key(scope, m.name);
        if c_obj.has_own_property(scope, key).unwrap_or(false) {
            continue;
        }
        put_member(scope, c_obj, m, d.name, false, ledger, Pass::Backfill, Flavor::Shell);
    }
}

/// The rows of one holder the ACTIVE capture selects, in its order -- the join, for the passes that
/// ADD members to a prototype that already exists (the two backfills). The one create does the same
/// join inline, because it also has to place `constructor` and the tag around it.
///
/// With no active surface this is exactly [`proto_members`] / [`static_members`]: the whole table,
/// filtered by realm, in table order.
///
/// Symbol-keyed rows are always included, for the reason [`crate::install::surface::unrecordable`]
/// states: no capture in this tree records one, so no capture can be evidence one is absent.
fn selected_rows(
    scope: &mut v8::PinScope,
    def: &'static InterfaceDef,
    exposed_name: &str,
    realm: Exposed,
    statics: bool,
) -> Vec<&'static MemberDef> {
    let m = join(scope, def, exposed_name, realm, None);
    if statics { m.statics } else { m.proto }
}

/// `Object.getOwnPropertyNames(obj)` semantics: all own STRING keys, non-enumerable included, in
/// insertion order.
fn own_names<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Object>) -> Vec<String> {
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
        .index_filter(v8::IndexFilter::IncludeIndices)
        .build();
    let mut out = Vec::new();
    if let Some(names) = obj.get_own_property_names(scope, args) {
        for i in 0..names.length() {
            if let Some(k) = names.get_index(scope, i) {
                out.push(k.to_rust_string_lossy(scope));
            }
        }
    }
    out
}


/// Re-read a slot's descriptor flags and update the installer's own record of them.
///
/// Called by the passes that DELIBERATELY re-describe a row after the mount (the arity guard, the
/// constant seal). Without it the audit would read the installer's own later work as a second writer
/// re-describing the slot, which is the false positive that would make the check useless.
fn note_flags<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Object>, iface: &str, name: &str, holder: Holder) {
    let key = member_key(scope, name);
    let Some(dd) = target.get_own_property_descriptor(scope, key).filter(|v| !v.is_undefined()) else { return };
    let Ok(dd) = v8::Local::<v8::Object>::try_from(dd) else { return };
    let en_k = str_key(scope, "enumerable");
    let cfg_k = str_key(scope, "configurable");
    let en = dd.get(scope, en_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let cfg = dd.get(scope, cfg_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let realm = realm_label(scope);
    record_install_flags(realm, iface, name, holder, en, cfg);
}

/// Wrap one member in an arity guard if it is a configurable function owning `holder[name]`.
///
/// `ty` is the ROW's return type, which decides both how a WebIDL `undefined` return is rendered and
/// -- since ARITY1 -- whether the argument-count refusal is thrown or REJECTED. It used to be hard
/// coded `false` here, so every member this pass reached threw where a promise-returning one must
/// reject. A caller with no row behind the member passes `TypeTag::Any`.
pub(crate) fn guard_member<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    iface: &str,
    name: &str,
    need: i32,
    ty: super::TypeTag,
    which: Holder,
) -> bool {
    if need <= 0 {
        return false;
    }
    let key = str_key(scope, name);
    let Some(dd) = holder.get_own_property_descriptor(scope, key.into()).filter(|v| !v.is_undefined()) else { return false };
    let Ok(dd) = v8::Local::<v8::Object>::try_from(dd) else { return false };
    let val_k = str_key(scope, "value");
    let Some(value) = dd.get(scope, val_k.into()) else { return false };
    if !value.is_function() {
        return false;
    }
    let cfg_k = str_key(scope, "configurable");
    if !dd.get(scope, cfg_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false) {
        return false;
    }
    if is_arity_guarded(scope, value) {
        return false;
    }
    let len_k = str_key(scope, "length");
    let length = v8::Local::<v8::Object>::try_from(value)
        .ok()
        .and_then(|o| o.get(scope, len_k.into()))
        .and_then(|v| v.integer_value(scope))
        .unwrap_or(0) as i32;
    // A row the installer receiver-checked gets its count INSIDE that check, as Chrome orders them: the guard
    // is rebuilt round an arity guard over its own inner function (`receiver::inner_of`). Wrapped from
    // outside, a foreign receiver with too few arguments heard "N arguments required" where Chrome says
    // `Illegal invocation`.
    // (An overloaded promise operation counts first, `install::counts_arguments_first`: its guard stays outside.)
    let count_first = crate::install::counts_arguments_first(iface, name, ty, need as u32);
    let g = match crate::install::receiver::inner_of(scope, value).filter(|_| matches!(which, Holder::Prototype) && !count_first) {
        Some((inner, ord)) => {
            let a = make_arity_guard(scope, iface, name, need, length, inner.into(), ty);
            let g = crate::install::receiver::rewrap(scope, holder.into(), a, ord);
            // the guard this replaces is no longer the install's: no template may replay it (`templates::forget`)
            if crate::install::templates::capturing() {
                if let Ok(old) = v8::Local::<v8::Function>::try_from(value) {
                    crate::install::templates::forget(scope, old);
                }
            }
            g
        }
        None => make_arity_guard(scope, iface, name, need, length, value, ty),
    };
    // The guard REPLACES the slot's function, so the install brand moves onto it -- the arity pass is
    // the installer finishing its own row, not a second writer. See [`carry_install_stamp`].
    carry_install_stamp(scope, value, g);
    let wr_k = str_key(scope, "writable");
    let en_k = str_key(scope, "enumerable");
    let writable = dd.get(scope, wr_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let enumerable = dd.get(scope, en_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(g.into(), writable);
    pd.set_enumerable(enumerable);
    pd.set_configurable(true);
    holder.define_property(scope, key.into(), &pd);
    note_flags(scope, holder, iface, name, which);
    true
}

/// Force a native function's `.length` to the WebIDL value when it differs (op2 counts the OpState
/// param and ignores optional args, so a hand-written native can report the wrong arity where Chrome
/// uses the WebIDL length). `fn.length` is configurable, so this is legal and idempotent.
fn set_len<'s>(scope: &mut v8::PinScope<'s, '_>, fnv: v8::Local<'s, v8::Value>, n: i32) {
    if !fnv.is_function() {
        return;
    }
    let Ok(f) = v8::Local::<v8::Object>::try_from(fnv) else { return };
    let len_k = str_key(scope, "length");
    let cur = f.get(scope, len_k.into()).and_then(|v| v.integer_value(scope)).unwrap_or(i64::MIN);
    if cur == n as i64 {
        return;
    }
    let val = v8::Integer::new(scope, n);
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(val.into(), false);
    pd.set_enumerable(false);
    pd.set_configurable(true);
    f.define_property(scope, len_k.into(), &pd);
}

/// For each catalogued member the holder OWNS, correct its function length -- a method to the row's
/// `length`, an accessor's get/set to 0/1 (the ECMAScript invariant).
fn fix_lens<'s, I: Iterator<Item = &'static MemberDef>>(scope: &mut v8::PinScope<'s, '_>, holder: v8::Local<'s, v8::Object>, members: I) {
    for m in members {
        let key = member_key(scope, m.name);
        if !holder.has_own_property(scope, key).unwrap_or(false) {
            continue;
        }
        match kind_letter(m) {
            "m" => {
                if let Some(fv) = holder.get(scope, key.into()) {
                    set_len(scope, fv, m.length as i32);
                }
            }
            "a" => {
                let Some(dd) = holder.get_own_property_descriptor(scope, key.into()).filter(|v| !v.is_undefined()) else { continue };
                let Ok(dd) = v8::Local::<v8::Object>::try_from(dd) else { continue };
                let get_k = str_key(scope, "get");
                if let Some(g) = dd.get(scope, get_k.into()) {
                    set_len(scope, g, 0);
                }
                let set_k = str_key(scope, "set");
                if let Some(s) = dd.get(scope, set_k.into()) {
                    set_len(scope, s, 1);
                }
            }
            _ => {}
        }
    }
}

/// The two module-body backfills that run right after create: the captured `Symbol.toStringTag` on any
/// interface prototype with no own one (a hand-written interface the create loop skipped), and the
/// WebIDL lengths of every OWN native member whose length disagrees with the capture.
pub fn finalize_shapes<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, registry: &'static [&'static InterfaceDef], realm: Exposed) {
    let any_lazy = crate::install::templates::has_lazy_globals(scope);
    for d in registry {
        if !d.exposed.covers(realm) {
            continue;
        }
        // a lazy leaf: reading it would make it; what this finalize gives it, its template's
        // deltas give it when it is made (`templates::realm_interface` -> `apply_deltas`), as a frame's get them
        if any_lazy && crate::install::templates::lazy_global(scope, global, d.name).is_some() {
            continue;
        }
        let key = str_key(scope, d.name);
        let Some(cv) = global.get(scope, key.into()) else { continue };
        let Ok(cfun) = v8::Local::<v8::Function>::try_from(cv) else { continue };
        let c_obj: v8::Local<v8::Object> = cfun.into();
        let pk = str_key(scope, "prototype");
        let proto = c_obj.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());

        if let (Some(tag), Some(proto)) = (d.tag, proto) {
            let tag_key = v8::Symbol::get_to_string_tag(scope);
            let has_own = proto.get_own_property_descriptor(scope, tag_key.into()).map(|v| !v.is_undefined()).unwrap_or(false);
            if !has_own {
                let tv = str_key(scope, tag);
                let mut pd = v8::PropertyDescriptor::new_from_value_writable(tv.into(), false);
                pd.set_enumerable(false);
                pd.set_configurable(true);
                proto.define_property(scope, tag_key.into(), &pd);
            }
        }
        if let Some(proto) = proto {
            fix_lens(scope, proto, proto_members(d, realm));
        }
        fix_lens(scope, c_obj, static_members(d, realm));
    }
}

// ── the registry passes (worker) ────────────────────────────────────────────────────────────────

/// A phantom parent prototype (worker realm): a base interface named in the recorded chain that the
/// worker does NOT expose as a global (`CSSTransformComponent`, the base of CSSSkewX/Y). A stub ctor kept
/// OFF the global with a `{}` prototype carrying the right `constructor`.
///
/// OFF the global is not off the surface: a page reaches this object as
/// `Object.getPrototypeOf(CSSSkewX.prototype).constructor`, and Chrome's answers there are the same
/// two as for any other non-newable interface object -- measured IN A REAL WORKER, 2026-09-12:
/// `new C()` is `Failed to construct 'CSSTransformComponent': Illegal constructor` and `C()` is
/// `Illegal constructor`, with `C` on no global. So it gets the same builder as the exposed ones.
fn worker_phantom_proto<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, cache: &mut std::collections::HashMap<String, v8::Local<'s, v8::Object>>) -> v8::Local<'s, v8::Object> {
    if let Some(p) = cache.get(name) {
        return *p;
    }
    let ctor = build_illegal_ctor(scope, name, 0);
    let proto = v8::Object::new(scope);
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(proto.into(), false);
    pd.set_enumerable(false);
    pd.set_configurable(false);
    let k = str_key(scope, "prototype");
    ctor.define_property(scope, k.into(), &pd);
    let mut cd = v8::PropertyDescriptor::new_from_value_writable(ctor.into(), true);
    cd.set_enumerable(false);
    cd.set_configurable(true);
    let k = str_key(scope, "constructor");
    proto.define_property(scope, k.into(), &cd);
    cache.insert(name.to_string(), proto);
    proto
}

/// The worker's slot-backed member backfill: for each `slot_backed` worker interface, add the rows its
/// prototype does not already OWN (each through `put_member`, so a slot row reads the private slot).
fn worker_slot_backfill<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, registry: &'static [&'static InterfaceDef], realm: Exposed, ledger: &mut InstallLedger) {
    for d in registry {
        if !d.slot_backed || !d.exposed.covers(realm) {
            continue;
        }
        let name = str_key(scope, d.name);
        let Some(cv) = global.get(scope, name.into()) else { continue };
        let Ok(cfun) = v8::Local::<v8::Function>::try_from(cv) else { continue };
        let c_obj: v8::Local<v8::Object> = cfun.into();
        let pk = str_key(scope, "prototype");
        let Some(proto) = c_obj.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { continue };
        for m in selected_rows(scope, d, d.name, realm, false) {
            if m.name == "constructor" {
                continue;
            }
            let key = member_key(scope, m.name);
            if proto.has_own_property(scope, key).unwrap_or(false) {
                continue;
            }
            put_member(scope, proto, m, d.name, true, ledger, Pass::Backfill, Flavor::Shell);
        }
    }
}

/// Interfaces the worker installer leaves to the hand-written JS glue (a LIVE handshake it cannot yet
/// reproduce): XMLHttpRequest's native-ctor wiring, and the WorkerGlobalScope family that
/// worker-finalize.js reparents the worker global onto. W6 replaces the handshake.
pub fn worker_js_owned(name: &str) -> bool {
    // The list is DATA (`tools/sync/data/worker_js_owned.json`), emitted into the registry by the same
    // generator run that builds the rows, so the installer and the generator cannot disagree (W8.2b).
    crate::install::generated_registry::WORKER_JS_OWNED.contains(&name)
}

/// Install the WORKER realm's registry surface: create every missing interface (skipping the
/// `worker_js_owned` ones), chain ONLY what this call created (a phantom parent for a base the realm
/// does not expose), finalize shapes, and run the slot backfill.
pub fn install_worker_registry<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, registry: &'static [&'static InterfaceDef], ledger: &mut InstallLedger) {
    let realm = Exposed::Worker;
    let mut created: std::collections::HashSet<&str> = std::collections::HashSet::new();
    // The ACTIVE worker capture's own-global order, or the registry's `WORKER_ORDER` view when no
    // surface is active. This is what retired the second-table question "whose worker order?": the
    // device being worn answers it.
    let order = realm_order(scope, registry, realm);
    for step in walk_steps(scope, registry, realm) {
        let d = match step {
            WalkStep::Row(d) => d,
            WalkStep::Hand(h) => {
                (h.create)(scope, global, ledger);
                continue;
            }
        };
        if worker_js_owned(d.name) {
            continue;
        }
        let name = str_key(scope, d.name);
        if global.get(scope, name.into()).map(|v| !v.is_undefined()).unwrap_or(false) {
            continue;
        }
        create_interface_in(scope, global, d, d.name, None, None, Flavor::Shell, realm, ledger);
        created.insert(d.name);
    }
    let mut phantom: std::collections::HashMap<String, v8::Local<v8::Object>> = std::collections::HashMap::new();
    for d in &order {
        if !created.contains(d.name) {
            continue;
        }
        let Some(parent) = d.parent else { continue };
        let Some(proto) = ctor_proto(scope, global, d.name) else { continue };
        let pproto = match ctor_proto(scope, global, parent) {
            Some(p) => p,
            None => worker_phantom_proto(scope, parent, &mut phantom),
        };
        proto.set_prototype(scope, pproto.into());
        // the interface object chains to the parent's too -- the phantom's own constructor when the parent is not
        // exposed here (Chrome 150's worker: `Object.getPrototypeOf(CSSSkewX).name` is "CSSTransformComponent"; 33
        // worker interfaces answered `Function.prototype`)
        let pctor = ctor_obj(scope, global, parent).or_else(|| {
            let k = str_key(scope, "constructor");
            pproto.get(scope, k.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()).map(Into::into)
        });
        if let (Some(c), Some(pc)) = (ctor_obj(scope, global, d.name), pctor) {
            c.set_prototype(scope, pc.into());
        }
    }
    finalize_shapes(scope, global, registry, realm);
    worker_slot_backfill(scope, global, registry, realm, ledger);
}

// ── a table over an existing constructor ────────────────────────────────────────────────────────

/// Install a hand table onto the prototype of an EXISTING constructor -- an interface whose constructor
/// the runtime still provides as a JS class (`Event`, `EventTarget` until W4.b) while its prototype's
/// members come from the table. The prototype is made the table's: every own string key except
/// `constructor` is deleted, every prototype row is mounted in table order (`Flavor::Table`, so an
/// `Install` row runs its body and a `Stub` row is the D2 stub), `constructor` is re-defined LAST (Chrome's
/// position), the static rows go onto the constructor, `Symbol.toStringTag` is set when the prototype
/// has none, and the constructor's `length` is corrected to the table's `ctor` row. Rows are ledgered as
/// `Pass::Create`. Returns the prototype, or why the table's order is unreachable on this prototype (a
/// non-configurable own key the delete pass cannot remove).
///
/// This is the Rust form of what `dom/09g-event.js` did for `Event.prototype` ("delete the whole set
/// and reinstall it in the captured order"): the only way an existing prototype takes the recorded
/// member order, since a define on a key that already exists keeps its position.
pub fn overwrite_prototype<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ctor: v8::Local<'s, v8::Function>,
    def: &'static InterfaceDef,
    ledger: &mut InstallLedger,
) -> Result<v8::Local<'s, v8::Object>, String> {
    overwrite_prototype_selected(scope, ctor, def, None, ledger).map(|(p, _)| p)
}

/// [`overwrite_prototype`] over a MEMBER SELECTOR -- the union-table form, and the only difference
/// between the two.
///
/// A table's name is what removes an interface from BOTH generated registries, so when the two
/// captures disagree about an interface's member list ONE table has to serve both realms.
/// `MediaSource` is the case this exists for: `worker.surface.json` records a 13th prototype member,
/// `handle`, that `surface.json` does not. The table carries the WORKER's list (the superset, in the
/// worker's order) and the WINDOW install passes the window's names, exactly the way
/// [`create_interface`] already lets a `Model::Singleton` table be installed under a per-realm
/// selector (`Navigator` / `WorkerNavigator`).
///
/// `None` installs every row in table order -- byte-for-byte what `overwrite_prototype` always did.
/// `Some(names)` installs only those names, IN THE SELECTOR'S ORDER, routing a `static_member` row to
/// the constructor the way the unselected path does; a name the table does not carry is returned as a
/// [`Gap`] rather than silently skipped, so a selector that drifts from the table is visible.
pub fn overwrite_prototype_selected<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ctor: v8::Local<'s, v8::Function>,
    def: &'static InterfaceDef,
    selector: Option<&[&str]>,
    ledger: &mut InstallLedger,
) -> Result<(v8::Local<'s, v8::Object>, Vec<Gap>), String> {
    let pk = str_key(scope, "prototype");
    let proto = ctor
        .get(scope, pk.into())
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .ok_or_else(|| format!("{}: the constructor has no prototype object", def.name))?;
    for name in own_names(scope, proto) {
        if name == "constructor" {
            continue;
        }
        let k = str_key(scope, &name);
        proto.delete(scope, k.into());
    }
    // A NON-configurable own key (a sealed WebIDL constant, say) survives the delete and cannot be moved
    // to its table position, so the recorded order is unreachable: say so instead of installing a
    // prototype whose order is half the table's.
    let survivors: Vec<String> = own_names(scope, proto).into_iter().filter(|n| n != "constructor").collect();
    if !survivors.is_empty() {
        return Err(format!("{}: non-configurable own keys survive the delete pass and cannot take their table position: {:?}", def.name, survivors));
    }
    let ctor_obj: v8::Local<v8::Object> = ctor.into();
    // THE SAME JOIN THE OTHER CREATE DOES. This path deletes the class body's own keys
    // and re-installs from the table, so it decides presence and order for every newable hand table --
    // `Blob`, `Request`, `Response`, the event family. Before it asked the selector, six Brave-only
    // members stayed mounted under the Chrome bundle.
    let mount = join(scope, def, def.name, Exposed::Both, selector);
    // `constructor` AT THE CAPTURE'S SEAT, which for a hand table is LAST on ~83 % of
    // interfaces and is not last on the ones a partial interface extends. The class body put it
    // first, so it comes off before any row goes down and is re-defined at its index -- same value,
    // WebIDL's { writable, non-enumerable, configurable }. `None` keeps this path's old answer, last.
    let ctor_at = mount.ctor_at.unwrap_or(mount.proto.len());
    let ck = str_key(scope, "constructor");
    proto.delete(scope, ck.into());
    // The tag BEFORE the rows: a symbol-keyed row (`@@iterator`) set down first came out ahead
    // of `Symbol.toStringTag` in `Reflect.ownKeys` -- `Headers`, `Highlight` -- where Brave 151 lists the tag
    // first, as `create_interface` already does. String keys keep their order whatever the tag's place.
    if let Some(tag) = def.tag {
        let tag_key = v8::Symbol::get_to_string_tag(scope);
        let has_own = proto.get_own_property_descriptor(scope, tag_key.into()).map(|v| !v.is_undefined()).unwrap_or(false);
        if !has_own {
            let tv = str_key(scope, tag);
            let mut pd = v8::PropertyDescriptor::new_from_value_writable(tv.into(), false);
            pd.set_enumerable(false);
            pd.set_configurable(true);
            proto.define_property(scope, tag_key.into(), &pd);
        }
    }
    for m in mount.proto.iter().take(ctor_at) {
        put_member(scope, proto, m, def.name, true, ledger, Pass::Create, Flavor::Table);
    }
    // The statics keep their place in the sequence: they go on the interface OBJECT, so where they
    // fall relative to `constructor` cannot be read off the prototype, and leaving them here means an
    // interface whose seat IS last records exactly the ledger it always did.
    for m in &mount.statics {
        put_member(scope, ctor_obj, m, def.name, false, ledger, Pass::Create, Flavor::Table);
    }
    let gaps = mount.gaps;
    let mut cd = v8::PropertyDescriptor::new_from_value_writable(ctor.into(), true);
    cd.set_enumerable(false);
    cd.set_configurable(true);
    proto.define_property(scope, ck.into(), &cd);
    let realm = realm_of(scope);
    if let Some((en, cfg)) = stamp_member(scope, proto, "constructor", &format!("{}::constructor", def.name)) {
        record_install_flags(realm.1, def.name, "constructor", Holder::Prototype, en, cfg);
    }
    ledger.record(realm, def.name, "constructor", Holder::Prototype, Pass::Create);
    for m in mount.proto.iter().skip(ctor_at) {
        put_member(scope, proto, m, def.name, true, ledger, Pass::Create, Flavor::Table);
    }
    if let Some(c) = &def.ctor {
        set_len(scope, ctor.into(), c.length as i32);
    }
    Ok((proto, gaps))
}

// ── the hand-table entry points ─────────────────────────────────────────────────────────────────
//
// Thin forms of the core above for the hand tables and their gates: each takes the context's ledger
// from `OpState`, so a live install (`live.rs`, `navigator_table.rs`) and a bare-runtime gate record
// the same way.

/// Mount a `Model::Singleton` interface (Navigator, WorkerNavigator): a constructor-less interface
/// object plus its prototype, with the selected members installed in the selector's order. Returns
/// the prototype (so the caller can point the one live instance -- e.g. `navigator` -- at it) and any
/// gaps. `iface_name` is the exposed name (so one table can install as `Navigator` in the window and
/// `WorkerNavigator` in a worker), independent of `def.name`; the tag is that name.
pub fn install_singleton<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, def: &'static InterfaceDef, iface_name: &str, selected: &[&str]) -> (v8::Local<'s, v8::Object>, Vec<Gap>) {
    let mut ledger = take_ledger(scope);
    let c = create_interface(scope, global, def, iface_name, None, Some(selected), Flavor::Table, &mut ledger);
    put_ledger(scope, ledger);
    (c.proto, c.gaps)
}

/// Mount a constructible interface (Model::Value like DOMRect, or an element interface) from a
/// selector: a newable interface object + prototype, instance members on the prototype in selector
/// order, static members on the constructor, and -- when `parent` is given -- the WebIDL inheritance
/// splice. Returns the prototype, the constructor and the gaps. The shape gates use it on a bare
/// runtime; the live HTMLImageElement install uses [`create_interface`] with the table's own order.
#[allow(clippy::too_many_arguments)]
pub fn install_constructable<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    iface_name: &str,
    instance_selected: &[&str],
    static_selected: &[&str],
    parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
) -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>, Vec<Gap>) {
    let selected: Vec<&str> = instance_selected.iter().chain(static_selected.iter()).copied().collect();
    let mut ledger = take_ledger(scope);
    let c = create_interface(scope, global, def, iface_name, parent, Some(&selected), Flavor::Table, &mut ledger);
    put_ledger(scope, ledger);
    (c.proto, c.ctor, c.gaps)
}

/// Build the instance object for an interface whose SHAPE includes a named-property getter -- the
/// exotic interceptor kind (DOMStringMap `dataset`, HTMLCollection named
/// access). Only a native `v8::ObjectTemplate` can carry a property handler, so the instance is minted
/// from a template configured with the D2 named-getter stub, then the interface's DECLARED prototype
/// members (its non-interceptor rows -- `length`, `item`, ...) are installed on it in selector order.
/// Returns the instance whose property reads route through the interceptor, and any [`Gap`]s.
pub fn install_named_getter_object<'s>(scope: &mut v8::PinScope<'s, '_>, def: &'static InterfaceDef, selected: &[&str]) -> (v8::Local<'s, v8::Object>, Vec<Gap>) {
    let tmpl = v8::ObjectTemplate::new(scope);
    // Only the getter is configured for the D2 stub: a `query`/`enumerator` that claimed names would
    // fabricate presence (a stub owns no store), so `in`/enumeration correctly find nothing.
    let config = v8::NamedPropertyHandlerConfiguration::new().getter_raw(crate::binding::refs::c(icpt!(named_get, holder, stub::cb_named_getter_stub).map_fn_to()));
    tmpl.set_named_property_handler(config);
    let obj = tmpl.new_instance(scope).unwrap();
    // A NamedGetter/IndexedGetter row IS the interceptor (installed above), not a prototype member.
    let members: Vec<&str> = selected
        .iter()
        .copied()
        .filter(|n| !matches!(def.member(n).map(|m| m.kind), Some(MemberKind::NamedGetter) | Some(MemberKind::IndexedGetter)))
        .collect();
    // The exotic object IS the instance -- its members are mounted directly on it, not on a
    // prototype it is the [[Prototype]] of -- so the rows go up WITHOUT the receiver check
    // `install_members` applies (which rejects "the target itself", correctly, when the target is a
    // prototype). Same reason the registry's `put_shell_stub` passes no receiver for a static.
    let mut ledger = take_ledger(scope);
    let mut gaps = Vec::new();
    for &name in &members {
        match def.member(name) {
            Some(m) => put_member(scope, obj, m, def.name, false, &mut ledger, Pass::Create, Flavor::Table),
            None => gaps.push(Gap { interface: def.name, member: name.to_string() }),
        }
    }
    put_ledger(scope, ledger);
    (obj, gaps)
}

/// Mount `def`'s selected members onto `proto`, in `selected` order (the bundle's order), as a hand
/// table's rows. Returns the gaps (selected names the table lacks) for the audit. Members the bundle
/// did NOT select are left compiled-but-unmounted -- union membership is not exposure.
pub fn install_members<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>, def: &'static InterfaceDef, selected: &[&str]) -> Vec<Gap> {
    let mut ledger = take_ledger(scope);
    let mut gaps = Vec::new();
    for &name in selected {
        match def.member(name) {
            Some(m) => put_member(scope, proto, m, def.name, !m.static_member, &mut ledger, Pass::Create, Flavor::Table),
            None => gaps.push(Gap { interface: def.name, member: name.to_string() }),
        }
    }
    put_ledger(scope, ledger);
    gaps
}

/// Every row of a table, prototype rows then statics, in table order.
pub fn table_rows(def: &'static InterfaceDef) -> Vec<&'static MemberDef> {
    proto_members(def, Exposed::Both).chain(static_members(def, Exposed::Both)).collect()
}

// ── the ops the bootstrap calls ─────────────────────────────────────────────────────────────────

/// The bootstrap's registry install: every window interface's create-level shape + chain, then the
/// tag/length finalize over the whole surface the steps before it built.
///
/// It was `js/generated_shells.js`'s first statement, reached through `op_install_registry`; then
/// that file was deleted and this is called straight from
/// [`crate::install::live::install_window_bootstrap`] instead -- the same moment, no op, no JS. The
/// worker half is [`install_worker_shells`], which lost its op the same way in W6.2.
pub fn install_window_registry<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = take_ledger(scope);
    install_registry(scope, global, REGISTRY, Exposed::Window, &mut ledger);
    finalize_shapes(scope, global, REGISTRY, Exposed::Window);
    put_ledger(scope, ledger);
}

/// The member BACKFILL: every catalogued member a pre-existing prototype does not already own --
/// a reflected accessor, an event-handler pair, a slot getter, or a shape-correct native stub --
/// mounted from the row that names it. The reflected accessors are ROWS of the one registry now, so
/// the separate reflect table this used to walk is long gone.
///
/// It was `js/generated_reflect.js`, reached through `op_backfill_members`; then the
/// file and the op were deleted, and [`crate::install::live::install_window_bootstrap`] calls this at the same
/// point -- AFTER the `deno_core` class templates and the DOM spine, and BEFORE
/// [`install_window_registry`], which is the order that was MEASURED (2026-09-11, by probing the ops
/// from inside): it reaches no registry interface, because `Highlight` and friends do not exist yet
/// when it runs, and it declines to overwrite what a prototype already owns.
pub fn backfill_window_members<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = take_ledger(scope);
    backfill_members(scope, global, REGISTRY, Exposed::Window, &mut ledger);
    put_ledger(scope, ledger);
}

/// The end of the window realm's install: the install audit, on demand.
///
/// THERE WERE THREE SURFACE-FINISHING PASSES HERE, and a measurement took each one's effect in every
/// realm of every bundle (`KORVUS_TPL_REPORT`'s `PASS` lines). The ARITY pass wrapped every registry
/// method with its WebIDL required-argument count after the install; each interface's create does that
/// inside the recording now, and the pass found nothing left to guard (0 wraps in 45 runs), so
/// it was deleted. The CONSTANT SEAL narrowed a constant row's `configurable` on whichever of the
/// constructor or its prototype owned the key -- and its only effect left was a defect: it applied the
/// PROTOTYPE rows `Array::length` and `String::length` to the constructors' own function `length`, which
/// ECMAScript makes configurable and Chrome 150 answers configurable (B23, F-ctor-length-configurable);
/// every other constant comes sealed from its template. It was deleted with its frame-realm twin
/// `seal_intrinsic_constants`. And an enumeration-ORDER pass that walked `generated_order::ORDER`, a
/// second generated table of 407 interfaces x their recorded own-prototype member order, and restored
/// each one by deleting and redefining its members. The registry's own rows were already in that
/// order for all 407 (AGREE 407, DISAGREE 0), and the pass was measured at runtime across the whole
/// korvus-runtime suite -- 773 realm assemblies -- reordering exactly ONE interface: `Screen`, whose
/// prototype a `deno_core` class template realises in the declaration order of `#[op2] impl JsScreen`
/// (`core/korvus-runtime/src/lib.rs`). Those getters are in the capture's order now, the count is 0,
/// and the table and the pass went together. The comment that stood here blamed
/// `src/js/dom/09k-svg.js`, a file `46307a2c` deleted; the arity pass, the other writer it named, is
/// not a reorder source at all, because `Object.defineProperty` on an already-existing own property
/// does not move it in V8.
///
/// CALLED TWICE PER CONTEXT, from Rust both times: once at the end of the extension's
/// bootstrap ([`crate::install::live::install_window_postshells`], the first thing the runtime does after
/// `JsRuntime::new` returns -- the slot `dom/09-postshells.js`'s last line held), and once behind the
/// runtime's own bootstrap (`Engine::install_document`, the slot the `KB.__finalizeSurface` bridge
/// held). The second run existed for the arity half: `BOOTSTRAP`'s CSSOM block assigned
/// `CSSStyleSheet.prototype.replace`/`replaceSync` after the first, and an assignment REPLACES the
/// function the pass guarded. Those members come from their templates, guarded,.
///
/// THE SCAR THIS SIGNATURE CLOSES: the second call used to be a JS arrow behind a bridge slot, inside
/// a `try { } catch (e) {}`. It read a bare `Deno.core.ops` at INSTALL_DOCUMENT time, after the
/// bootstrap had already deleted `globalThis.Deno`, so it threw `ReferenceError` into that catch and
/// silently never ran -- `CSSStyleSheet.replace`/`replaceSync`, `MediaQueryList.addListener`/
/// `removeListener` and `StyleSheetList.item` shipped with NO arity guard from the Phase 4 port until
/// The status ledger found it. A Rust call cannot be swallowed that way: there is no
/// global to delete and no catch to fall into, and
/// `core/korvus-runtime/tests/postshells_live.rs` reads those guards back off the live surface, so a
/// member that loses its guard fails the gate instead of going quiet.
pub fn finalize_surface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    // LAST, because the audit's question is "what does the page see NOW": a verdict taken before the
    // install is over would be about a surface no page ever loads. The PAGE realm's alone: a frame realm
    // must not overwrite the page's recorded verdicts.
    if crate::core::html::browsing_context::main_global(scope) == global && install_audit_enabled() {
        audit_surface(scope, global);
    }
}

// ── the audit: what the page sees in every slot the ledger claims ─────────────────────

/// Read the install ledger back against the LIVE surface of this realm and record a verdict per row
/// into [`install_audit_json`].
///
/// This is the half of invariant I2 that the ledger alone structurally cannot do. The ledger records
/// what the INSTALLER did; a second writer that is not the installer -- a JS `Object.defineProperty`,
/// an `op_helper_*` body -- makes no ledger call, so it leaves no trace there at all. What it cannot
/// avoid leaving is a different OBJECT in the slot, or different descriptor flags on it, and that is
/// what this reads. See the install-stamp section above for the four verdicts and why a declared
/// re-aim keeps its brand.
///
/// It walks the LEDGER rather than the surface on purpose: a row nobody claims is not this
/// invariant's business (V8's own intrinsics are on these prototypes too), while a row the installer
/// claims and no longer owns is exactly the defect.
pub fn audit_surface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let realm = realm_of(scope);
    let rows: Vec<LedgerEntry> = ledger_entries(scope).into_iter().filter(|e| e.realm == realm.0).collect();
    if rows.is_empty() {
        return;
    }
    // Where each table's rows are EXPOSED in this realm: its own name, plus any alias it was
    // installed under (`Navigator` -> `WorkerNavigator`). Derived from `create_interface`.
    let mut exposed: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    if let Ok(a) = alias_map().lock() {
        for (exp, table) in a.iter() {
            exposed.entry(table.clone()).or_default().push(exp.clone());
        }
    }
    let flags: std::collections::BTreeMap<String, String> = flags_map().lock().map(|m| m.clone()).unwrap_or_default();
    let mut verdicts: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    for e in &rows {
        let mut names: Vec<String> = vec![e.iface.to_string()];
        if let Some(alts) = exposed.get(e.iface) {
            names.extend(alts.iter().cloned());
        }
        let key = format!("{}|{}::{}@{}", realm.1, e.iface, e.member, holder_name(e.holder));
        let want_tag = format!("{}::{}", e.iface, e.member);
        let mut verdict: Option<String> = None;
        for n in &names {
            let holders = holder_candidates(scope, global, n, e.holder);
            if holders.is_empty() {
                continue;
            }
            // The candidate that OWNS the member answers; `gone` only when none of them does.
            let mut v: Option<String> = None;
            for h in holders {
                match classify_slot(scope, h, e.member, &want_tag, flags.get(&key).map(String::as_str)) {
                    Some(x) => {
                        v = Some(x);
                        break;
                    }
                    None => v = Some("gone".to_string()),
                }
            }
            let Some(v) = v else { continue };
            // A table exposed under two names is audited under both; the WORST answer is the realm's,
            // because a member the page can reach through either name is only as owned as its weakest
            // copy.
            verdict = Some(match verdict {
                Some(prev) if rank(&prev) >= rank(&v) => prev,
                _ => v,
            });
        }
        // A row whose interface this realm does not expose at all is not a finding: the window
        // ledger and the worker ledger share one `OpState` only when they share a runtime, and an
        // interface that is simply absent here was never this realm's to own.
        if let Some(v) = verdict {
            verdicts.insert(key, v);
        }
    }
    // THE WEAKEST ANSWER WINS, for the reason `behavior_rt::record_layer` gives about layers: one
    // process can assemble several realms of the same kind (a page, an iframe, a second Engine a test
    // built), and a member a second writer owns in ANY of them is owned by a second writer as far as
    // a page is concerned. Merging by "last one wins" instead made the answer depend on which realm
    // was assembled last -- measured at 41 rows in one order and 36 in another, with rows appearing
    // AND disappearing. A ledger that flatters is the failure mode W7 spent a whole track undoing.
    if let Ok(mut m) = audit_map().lock() {
        for (k, v) in verdicts {
            match m.get(&k) {
                Some(prev) if rank(prev) >= rank(&v) => {}
                _ => {
                    m.insert(k, v);
                }
            }
        }
    }
}

/// Worst-first ordering over the verdict vocabulary, so two views of one row collapse to the weaker.
fn rank(v: &str) -> u8 {
    match v.split(':').next().unwrap_or("") {
        "ok" => 0,
        "reaim" => 1,
        "flags" => 2,
        "gone" => 3,
        "clobber" => 4,
        _ => 5,
    }
}

/// Where a ledger row's member could be sitting: `globalThis[name]` for an interface-object row, its
/// `.prototype` for a prototype row -- plus the realm's GLOBAL as a second candidate whenever that
/// prototype is in the global's own chain.
///
/// THE GLOBAL IS BOTH AN INSTANCE AND AN INHERITOR, and one candidate is wrong for either half. The
/// `Window` table mounts most of its rows straight onto the global object (`window` is an INSTANCE,
/// not a prototype) while leaving `constructor` on `Window.prototype`; and every interface the global
/// INHERITS -- `EventTarget` is `Window`'s own ancestor -- would be redirected to the global by a
/// chain rule that answered with one object, and then read as `gone` while its five members sat
/// untouched on their own prototype. (Measured: that was this audit's first false positive, ten rows
/// of it.) So both are offered and the caller takes whichever OWNS the member. Derived from the
/// chain, never from the name.
fn holder_candidates<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str, holder: Holder) -> Vec<v8::Local<'s, v8::Object>> {
    let Some(k) = v8::String::new(scope, name) else { return Vec::new() };
    let Some(ctor) = global.get(scope, k.into()).filter(|v| v.is_function()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return Vec::new();
    };
    if holder == Holder::InterfaceObject {
        return vec![ctor];
    }
    let Some(pk) = v8::String::new(scope, "prototype") else { return Vec::new() };
    let Some(proto) = ctor.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return Vec::new();
    };
    let pv: v8::Local<v8::Value> = proto.into();
    let mut cur = global.get_prototype(scope);
    while let Some(o) = cur.and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        let ov: v8::Local<v8::Value> = o.into();
        if ov == pv {
            return vec![proto, global];
        }
        cur = o.get_prototype(scope);
    }
    vec![proto]
}

/// A list interface's iteration member holding its realm's own `Array.prototype` function, as WebIDL gives it
/// (`@@iterator` is `values`; an iterable's `entries` / `forEach` / `keys` / `values`). That function is ONE
/// object for every list interface of the realm, so a brand on it names whichever row mounted it last -- and in a
/// realm whose list interfaces all came from blueprints (a template makes the slot an intrinsic data property)
/// no row mounted it, and it carries none: `clobber:unbranded` in one run of four, by which isolate recorded
/// first (2026-10-09; at the commit before too). Its identity is the answer the brand cannot give.
fn is_realms_array_iteration_fn(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, member: &str) -> bool {
    let name = match member {
        "@@iterator" => "values",
        "entries" | "forEach" | "keys" | "values" => member,
        _ => return false,
    };
    let global = scope.get_current_context().global(scope);
    let Some(k) = v8::String::new(scope, "Array") else { return false };
    let Some(array) = global.get(scope, k.into()).and_then(|a| v8::Local::<v8::Object>::try_from(a).ok()) else { return false };
    let Some(k) = v8::String::new(scope, "prototype") else { return false };
    let Some(proto) = array.get(scope, k.into()).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok()) else { return false };
    let Some(k) = v8::String::new(scope, name) else { return false };
    proto.get(scope, k.into()).is_some_and(|f| f == v)
}

/// The verdict for one slot: is what the page sees still what the installer put there?
fn classify_slot<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    member: &str,
    want_tag: &str,
    want_flags: Option<&str>,
) -> Option<String> {
    let key = member_key(scope, member);
    let dd = holder.get_own_property_descriptor(scope, key).filter(|v| !v.is_undefined())?;
    let dd = v8::Local::<v8::Object>::try_from(dd).ok()?;
    let sk = |scope: &mut v8::PinScope<'s, '_>, s: &str| v8::String::new(scope, s).unwrap();

    let mut worst: Option<String> = None;
    let mut saw_fn = false;
    for field in ["get", "set", "value"] {
        let fk = sk(scope, field);
        let Some(v) = dd.get(scope, fk.into()).filter(|v| v.is_function()) else { continue };
        saw_fn = true;
        let tag = installed_by(scope, v);
        let this = match tag {
            _ if is_realms_array_iteration_fn(scope, v, member) => "ok".to_string(),
            Some(t) if t == want_tag => "ok".to_string(),
            Some(t) => format!("reaim:{t}"),
            None if crate::binding::protos::is_engine_fn(scope, v) => "clobber:engine".to_string(),
            None => "clobber:unbranded".to_string(),
        };
        worst = Some(match worst {
            Some(prev) if rank(&prev) >= rank(&this) => prev,
            _ => this,
        });
    }
    // A constant, or any other data value: there is no function to brand, so the descriptor flags are
    // the whole of what can be checked.
    let mut out = worst.unwrap_or_else(|| "ok".to_string());
    let _ = saw_fn;
    if rank(&out) <= 1 {
        if let Some(want) = want_flags {
            let en_k = sk(scope, "enumerable");
            let cfg_k = sk(scope, "configurable");
            let en = dd.get(scope, en_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
            let cfg = dd.get(scope, cfg_k.into()).map(|v| v.boolean_value(scope)).unwrap_or(false);
            let now = format!("e{}c{}", en as u8, cfg as u8);
            if now != want {
                // The flags say what the brand cannot: a writer that keeps the installer's own
                // function and only RE-DESCRIBES the slot (`host.js`'s `enumerateProto` flips a
                // table-mounted member enumerable) leaves the object identical and the descriptor
                // different.
                out = format!("flags:{want}->{now}");
            }
        }
    }
    Some(out)
}

/// The WORKER realm's registry install, called from `core/korvus-runtime/src/worker.rs` as the FIRST
/// of that realm's Rust installs. It walks `WORKER_ORDER` -- the same rows as `REGISTRY`,
/// permuted into the WORKER capture's order, because a realm's `globalThis` insertion order is a
/// fingerprint surface and the two captures do not share it.
///
/// It used to be `op_install_worker_registry`, called from `generated_worker_shells.js` through an
/// ESM-imported binding (not `Deno.core.ops`, since `worker-host.js` had already deleted the `Deno`
/// global by then). That file is gone and so is the op: the worker realm is assembled entirely from
/// Rust, which is what invariant I8's "no second shell set" means in practice.
pub fn install_worker_shells<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = take_ledger(scope);
    install_worker_registry(scope, global, WORKER_ORDER, &mut ledger);
    put_ledger(scope, ledger);
}

// THIS FILE DECLARES NO OPS. `op_install_registry` and `op_backfill_members` existed
// because the callers were two JS lines in the webapi extension's ESM layer; that layer is deleted
// and both are plain Rust functions called from `install::live::install_window_bootstrap`.
