//! THE BUNDLE SELECTOR — which of the union's rows this realm installs, and in what order.
//!
//! # The join
//!
//! `install/mod.rs` has described the installer as joining "that union of descriptors with the active
//! persona bundle's `surface.json` selector at context-creation time" since the one-table skeleton was
//! written. Once that sentence described a world with one bundle in it, where union ==
//! selection and the join was the identity — so the engine's whole surface was Brave's, compiled in,
//! and swapping `KORVUS_BUNDLE` changed what the browser SAID without changing what it HAD. Measured:
//! Four observables that differ between the two
//! CAPTURES and did not differ between the two ENGINES.
//!
//! This module is the selector half. The tables say HOW a member behaves; [`spec::RealmSurface`] —
//! read off the bundle the persona was loaded from — says WHICH members the device has and in what
//! sequence, which is a fact about the device. A row the active capture does not name is NOT
//! INSTALLED; it stays in its table for the bundle that does name it.
//!
//! # Where the realm comes from, and why it is declared rather than sniffed
//!
//! A surface is per-realm: the window capture records 1,206 own globals and the worker 335, and the
//! same table is `Navigator` in one and `WorkerNavigator` in the other. The realm therefore has to be
//! known before the first interface is created, and the two candidates for inferring it are both
//! wrong at that moment: `globalThis.window === globalThis` is not true yet (the self-alias is
//! `install_window_globals`, which runs AFTER the bootstrap), and a thread identity says nothing about
//! nested realms. So each realm's FIRST install entry declares it — [`enter`] from
//! `install::live::bootstrap`'s two functions — and everything underneath reads it back.
//!
//! An undeclared realm has NO selector, and the installer then mounts the table's own rows in table
//! order. That is deliberate and is the bare-runtime contract: `tests/installer_parity.rs` builds the
//! registry on a `JsRuntime` with no persona at all, and it must keep building the whole union there.
//!
//! # The three things the selector must NOT do
//!
//! * **It must not drop a symbol-keyed row.** The capture's member lists come from
//!   `Object.getOwnPropertyNames`, which returns string keys only — the rig records no
//!   `Symbol.iterator` anywhere in either bundle. A recording that cannot see a key is not evidence
//!   that the key is absent, so a `@@`-spelled row is always installed, after the selected string
//!   rows (where `Reflect.ownKeys` puts every symbol regardless of insertion order).
//! * **It must not speak for an interface the capture does not record.** `members_of` returning
//!   `None` means "this capture has nothing to say", not "this interface has no members" — an
//!   internal helper table, or an interface reached only as somebody's prototype. The table's own
//!   rows stand there.
//! * **It must not become a second definition of a member.** It carries NAMES AND ORDER and nothing
//!   else. Kind, IDL type, nullability, arity, descriptor flags and the body all stay in the one
//!   table, because a selector that carried them would be a second place to look for them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use deno_core::v8;
use deno_core::JsRuntime;
use spec::{RealmSurface, SurfaceRealm};

/// What a realm knows about its own surface. `Declared` before the first lookup, `Resolved` after —
/// resolving reads (and, the first time in the process, parses) the bundle's surface file, so it is
/// done lazily: a realm that installs nothing never pays for it.
enum Slot {
    Declared(SurfaceRealm),
    Resolved(SurfaceRealm, Option<Arc<RealmSurface>>),
}

impl Slot {
    fn realm(&self) -> SurfaceRealm {
        match self {
            Slot::Declared(r) | Slot::Resolved(r, _) => *r,
        }
    }
}

fn slots() -> &'static Mutex<HashMap<u32, Slot>> {
    static S: OnceLock<Mutex<HashMap<u32, Slot>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Declare which realm this v8 context is, before anything creates an interface in it.
pub fn enter(scope: &mut v8::PinScope, realm: SurfaceRealm) {
    let ord = crate::install::installer::realm_ordinal(scope);
    if let Ok(mut s) = slots().lock() {
        s.insert(ord, Slot::Declared(realm));
    }
    forget_rig_entered(ord);
}

/// Per-realm memo for [`rig_entered`], keyed like [`slots`]. An ordinal is reused when a worker
/// thread ends and another starts, so declaring a realm forgets the previous occupant's answer.
fn rig_entered_memo() -> &'static Mutex<HashMap<u32, bool>> {
    static E: OnceLock<Mutex<HashMap<u32, bool>>> = OnceLock::new();
    E.get_or_init(|| Mutex::new(HashMap::new()))
}

fn forget_rig_entered(ord: u32) {
    if let Ok(mut m) = rig_entered_memo().lock() {
        m.remove(&ord);
    }
}

/// **DID THE CAPTURE RIG EVER ENTER THE REALM THIS SCOPE IS IN?**
///
/// It visits exactly TWO realms per device: a window, and a DEDICATED worker. There is one worker
/// capture and all three worker kinds are seeded with it, so in a SHARED or SERVICE worker realm the
/// active surface is a description of a realm this one is not — and its silence about a name is not
/// this device's answer, it is a blind spot.
///
/// That is not a theory. A real service worker's global carries `FetchEvent`, `ExtendableEvent`,
/// `InstallEvent`, `Clients`, `Client`, `WindowClient`, `ServiceWorker` and `ExtendableMessageEvent`;
/// NO bundle's `worker.surface.json` in this tree lists one of them, by construction, and capturing a
/// hundred more devices would not add them. `create_global_scope` already names exactly this blind
/// spot for `ServiceWorkerGlobalScope` and `unrecordable` names it for `@@`-keyed members; this is the
/// same rule at the realm's own scale, so a filter written for the window cannot silently empty a
/// realm nobody recorded.
///
/// MEASURED when it was missing: gating [`publishes_interface`] without this clause took seven
/// service-worker tests red at once — the SW fetch bridge could not find `BroadcastChannel`'s
/// siblings and the realm produced no output at all.
///
/// The kernel answers which kind this is (`op_worker_scope_kind`), the same question
/// `install::live::event_family` asks to decide whether to build that chain in the first place, so
/// there is one source for it and no second list of worker kinds. Memoised per realm because
/// `publishes_interface` runs once per interface and this is a `v8` call into an op.
pub fn rig_entered(scope: &mut v8::PinScope) -> bool {
    let ord = crate::install::installer::realm_ordinal(scope);
    if let Some(v) = rig_entered_memo().lock().ok().and_then(|m| m.get(&ord).copied()) {
        return v;
    }
    let v = match realm(scope) {
        // The window is the realm the rig loads its page in.
        Some(SurfaceRealm::Window) | None => true,
        Some(SurfaceRealm::Worker) => {
            crate::core::workers::worker_global_scope::scope_kind(scope) == "dedicated"
        }
    };
    if let Ok(mut m) = rig_entered_memo().lock() {
        m.insert(ord, v);
    }
    v
}

/// Install a surface for this realm directly, bypassing the persona — the seam a test uses to build
/// the registry on a bare runtime against a NAMED bundle rather than whatever the process wears.
///
/// It is not a back door for engine code: nothing under `src/` calls it, and nothing may. The engine
/// reads the bundle it is wearing; it never chooses one.
pub fn seed(scope: &mut v8::PinScope, realm: SurfaceRealm, surface: Option<Arc<RealmSurface>>) {
    let ord = crate::install::installer::realm_ordinal(scope);
    if let Ok(mut s) = slots().lock() {
        s.insert(ord, Slot::Resolved(realm, surface));
    }
    forget_rig_entered(ord);
}

/// WHICH REALM THIS CONTEXT IS, if it has said — the answer to a question that is a property of the
/// CONTEXT and not of any call site.
///
/// `installer::create_interface` passes `Exposed::Both`, which has always meant "install this table
/// whole, do not filter", because a hand `interface!` table states its exposure once for the whole
/// table. That stopped being the whole truth when rows gained their OWN exposure: a row
/// spelled `window` is making a claim its table does not, and `Exposed::covers` is permissive in both
/// directions, so `Both` silently installed it in a worker. Measured: `worker.rs`'s own
/// `worker_scope_performance_observer` unit test, which asserts `!('eventCounts' in performance)` in a
/// worker with no persona, went red the moment `Performance`'s five `[Exposed=Window]` partials
/// stopped being excluded by a hand-written selector and started saying `window` on the row instead.
pub fn realm(scope: &mut v8::PinScope) -> Option<SurfaceRealm> {
    let ord = crate::install::installer::realm_ordinal(scope);
    slots().lock().ok()?.get(&ord).map(Slot::realm)
}

/// The surface this realm installs against, or `None` when no realm was declared (a bare runtime) or
/// the persona has no bundle behind it.
pub fn active(scope: &mut v8::PinScope) -> Option<Arc<RealmSurface>> {
    let ord = crate::install::installer::realm_ordinal(scope);
    let realm = {
        let s = slots().lock().ok()?;
        match s.get(&ord)? {
            Slot::Resolved(_, v) => return v.clone(),
            Slot::Declared(r) => *r,
        }
    };
    // Resolved OUTSIDE the lock: `Persona::surface` may parse a megabyte of JSON the first time, and
    // holding a process-wide lock across that would serialise every realm in the process behind it.
    let resolved = persona_surface(scope, realm);
    if crate::install::templates::reporting() {
        crate::install::templates::report(&format!("SURFACE resolved for realm {ord}"));
    }
    if let Ok(mut s) = slots().lock() {
        s.insert(ord, Slot::Resolved(realm, resolved.clone()));
    }
    resolved
}

/// The persona this realm wears, asked for its own bundle's surface. The I9 seam, the same one the
/// `Navigator` rows read their values through: the runtime PROVIDES the identity, the Web API READS
/// it. There is no `KORVUS_BUNDLE` here and there must never be — which bundle is being worn is the
/// persona's business, and this asks only what that bundle records.
fn persona_surface(scope: &mut v8::PinScope, realm: SurfaceRealm) -> Option<Arc<RealmSurface>> {
    let op_state = JsRuntime::op_state_from(scope);
    let p = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>()
            .and_then(|c| c.get::<spec::PersonaHandle>())
            .map(|h| h.0.clone())?
    };
    p.surface(realm)
}

/// One interface's selection: the prototype rows and the interface-object rows this realm mounts, by
/// NAME, in the order they must be created in.
pub struct Selection<'a> {
    pub members: Vec<&'a str>,
    pub statics: Vec<&'a str>,
    /// How many of [`Self::members`] the capture puts BEFORE `constructor`.
    ///
    /// Translated out of the capture's `ctorIndex` here rather than carried raw, because `explicit`
    /// may have dropped names from in front of it and an index into a list that no longer exists is
    /// not an answer. `None` is a capture with no recorded position — every worker artifact — and the
    /// installer then keeps the placement its flavour always had.
    pub ctor_after: Option<usize>,
}

/// The names to mount for `exposed_name`, joined from the capture and an optional explicit narrowing.
///
/// `explicit` is a caller that already knows a smaller set than the capture's: the DOM spine's staged
/// build (`Some(&[])` — constructor, tag and chain now, members later) and `Window`'s two constants.
/// It NARROWS; it never widens, and it never re-orders, because the order a page reads is the
/// capture's.
///
/// `None` means the capture does not speak for this name and the table's own rows stand.
pub fn selection<'a>(surface: &'a RealmSurface, exposed_name: &str, explicit: Option<&[&str]>) -> Option<Selection<'a>> {
    let order = surface.members_of(exposed_name)?;
    let kept = |n: &&str| explicit.map(|e| e.contains(n)).unwrap_or(true);
    let keep = |names: &'a [String]| -> Vec<&'a str> {
        names.iter().map(|s| s.as_str()).filter(kept).collect()
    };
    // `constructor`'s seat, moved with the names it sits behind. The capture's index counts the
    // members BEFORE it; `explicit` can remove some of those, so what survives the narrowing is
    // counted rather than the index being reused as if nothing had gone.
    let ctor_after = order.ctor_index.map(|c| order.members.iter().take(c).map(String::as_str).filter(kept).count());
    Some(Selection { members: keep(&order.members), statics: keep(&order.statics), ctor_after })
}

/// May an INTERFACE OBJECT of this name be published on the global — does the ACTIVE capture record
/// the interface at all?
///
/// THE ONE RULE, IN ONE PLACE. It was written inline in `installer::create`, so the
/// OTHER publisher of an interface object -- `install::live::rows::build_newable`, which 135 call
/// sites reach -- published unconditionally. That was invisible for as long as every bundle carried
/// the same interfaces; Opera 135 records no Speech API at all, and the engine put
/// `SpeechSynthesisUtterance` on an Opera window because that one interface happens to be built by
/// the newable path. `persona_surface` reads a name the device does not have as a korvus-only global,
/// which is what a page reads it as too: a `typeof` a real Opera answers `"undefined"` for.
///
/// `None` -- no active capture, a bare `JsRuntime` -- publishes everything, which is the reading the
/// rest of this module gives an absent surface and the behaviour every such realm had before.
///
/// **ITS ONE CALLER IS `core::html::exposure::gate`**, and that is the point of it
/// being `pub(crate)`: this answers only HALF the question. The other half is the name's exposure
/// CONDITION, and a publisher that reached this directly would be publishing a name with no condition
/// stated beside it -- which is exactly how 230 interface objects came to be on every `http://` page.
pub(crate) fn publishes_interface(scope: &mut v8::PinScope, name: &str) -> bool {
    if !rig_entered(scope) {
        return true;
    }
    match active(scope) {
        Some(s) => s.members_of(name).is_some(),
        None => true,
    }
}

/// May a GLOBAL of this name be published — does the ACTIVE capture's own-globals list carry it?
///
/// The companion of [`publishes_interface`] for the own properties of the global that are NOT
/// interface objects: the singletons. `speechSynthesis` is the first one whose presence differs
/// between bundles (Opera 135 has no Speech API), and it was published by a hand `define_property` in
/// `core/speech/speech_synthesis.rs` that no rule reached.
pub fn publishes_global(scope: &mut v8::PinScope, name: &str) -> bool {
    if !rig_entered(scope) {
        return true;
    }
    match active(scope) {
        Some(s) => s.has_global(name),
        None => true,
    }
}

/// Is this row one the capture is structurally unable to record, and so one no selector may drop?
///
/// Exactly the symbol keys. `Object.getOwnPropertyNames` — what the capture rig calls — returns
/// string keys, so neither bundle carries a single `Symbol(...)` member on any interface; filtering
/// against those lists would delete every `@@iterator` in the tree and break `for..of` over a
/// `NodeList`.
pub fn unrecordable(member_name: &str) -> bool {
    member_name.starts_with("@@")
}
