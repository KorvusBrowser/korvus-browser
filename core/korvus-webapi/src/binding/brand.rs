//! `binding/brand.rs` -- the brand of a platform object: WHICH interface made it, in any realm
//! (the Signature family).
//!
//! WebIDL's receiver check ("is this a platform object that implements the interface?") and its interface-type
//! argument conversion ask one question, and Chrome answers it from the object itself: another realm's node is
//! a `Node` (a frame's document passed to the page's `Node.prototype.contains`), an `Object.create(Node.prototype)`
//! is not, and neither answer depends on the object's prototype chain. The chain walk this engine asked
//! instead answered both wrongly without help -- the calling realm's prototype is not in another realm's
//! chain (hence the stamp fallback), and a fake has the prototype in its chain (2,218 rows of the
//! lock answer it).
//!
//! THE BRAND IS CARRIED BY THE TEMPLATE. Every interface's instance template holds its interface's id under a
//! private ([`brand_key`]), so every object made from it -- `protos::mint_on`, `mint_wrapper_on`, and `new X()`,
//! whose receiver V8 makes from the same template -- carries it from birth, in every realm; the prototype
//! template holds the same id ([`proto_id_key`]), which is how a check that holds a prototype names its
//! interface. The ids are the process's (one [`Registry`] for every isolate: blueprints are process-wide),
//! and "is a" follows the blueprints' parent names. Not V8's `FunctionTemplate::HasInstance`, which follows
//! `Inherit`: 214 interfaces' templates are built before their parent's in every isolate -- installs record
//! a child before its parent -- and `Inherit` must precede a template's first instantiation.

use deno_core::v8;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Bumped whenever an id is allocated or a parent recorded, so [`is_a`]'s per-thread ancestor table knows
/// to rebuild: every receiver check asks `is_a`, and it must not take the registry's lock to answer.
static VERSION: AtomicU64 = AtomicU64::new(0);

/// The process's interface ids: a name's id is allocated the first time it is named (as an interface or as
/// another's parent), and its parent is recorded when the interface itself registers.
#[derive(Default)]
struct Registry {
    ids: HashMap<String, u32>,
    names: Vec<String>,
    parent: Vec<Option<u32>>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

impl Registry {
    fn id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as u32;
        self.ids.insert(name.to_string(), id);
        self.names.push(name.to_string());
        self.parent.push(None);
        VERSION.fetch_add(1, Ordering::Release);
        id
    }
}

/// Interface `name`'s id, with `parent` (its WebIDL parent's name) recorded the first time it is given.
pub(crate) fn iface_id(name: &str, parent: Option<&str>) -> u32 {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let id = r.id(name);
    if let Some(p) = parent {
        let pid = r.id(p);
        if r.parent[id as usize].is_none() && pid != id {
            r.parent[id as usize] = Some(pid);
            VERSION.fetch_add(1, Ordering::Release);
        }
    }
    id
}

/// Record `parent` as interface `id`'s parent unless one is known (the blueprints' parent names come first).
/// The context snapshot (slice 3): the registry in id order -- each interface's name and its parent's id. A
/// snapshot's templates and objects carry these ids (privates), so a process made from it must number interfaces
/// as the one that made it did.
#[cfg(not(feature = "analysis"))]
pub(crate) fn registry_snapshot() -> (Vec<String>, Vec<Option<u32>>) {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    (r.names.clone(), r.parent.clone())
}

/// [`registry_snapshot`]'s reader, before this process registers anything of its own -- or after, when what it
/// registered is the same list so far (a second page of the process made from the same snapshot). An error says the
/// ids disagree: the snapshot cannot be used in this process.
#[cfg(not(feature = "analysis"))]
pub(crate) fn registry_compatible(names: &[String]) -> bool {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let shared = r.names.len().min(names.len());
    r.names[..shared] == names[..shared]
}

#[cfg(not(feature = "analysis"))]
pub(crate) fn registry_restore(names: Vec<String>, parents: Vec<Option<u32>>) -> Result<(), String> {
    if names.len() != parents.len() || parents.iter().flatten().any(|p| *p as usize >= names.len()) {
        return Err("the snapshot's interface registry does not decode".to_string());
    }
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let shared = r.names.len().min(names.len());
    if r.names[..shared] != names[..shared] {
        return Err("this process numbered its interfaces differently from the snapshot".to_string());
    }
    // a parent this process had not learnt yet; one it had is the same interface's (same name, same id)
    for i in 0..shared {
        if r.parent[i].is_none() {
            r.parent[i] = parents[i];
        }
    }
    for i in shared..names.len() {
        r.ids.insert(names[i].clone(), i as u32);
        r.names.push(names[i].clone());
        r.parent.push(parents[i]);
    }
    VERSION.fetch_add(1, Ordering::Release);
    Ok(())
}

pub(crate) fn learn_parent(id: u32, parent: u32) {
    if id == parent {
        return;
    }
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = r.parent.get_mut(id as usize) {
        if slot.is_none() {
            *slot = Some(parent);
            VERSION.fetch_add(1, Ordering::Release);
        }
    }
}

/// Learn each interface's parent from the realm's own prototype chains, which an install finishes: the
/// nearest prototype above an interface's that belongs to an interface. A blueprint recorded without its
/// parent's name (`HTMLImageElement` is created with its chain spliced in later) gets it here.
pub(crate) fn learn_parents_from_chains(scope: &mut v8::PinScope, protos: &[v8::Local<v8::Object>]) {
    let k = proto_id_key(scope);
    for &p in protos {
        let Some(id) = id_under(scope, p.into(), k) else { continue };
        let mut cur = p.get_prototype(scope);
        for _ in 0..16 {
            let Some(c) = cur.and_then(|c| v8::Local::<v8::Object>::try_from(c).ok()) else { break };
            if let Some(pid) = id_under(scope, c.into(), k) {
                learn_parent(id, pid);
                break;
            }
            cur = c.get_prototype(scope);
        }
    }
}

/// Interface `id`'s name (for a report).
pub(crate) fn name_of(id: u32) -> Option<String> {
    registry().lock().unwrap_or_else(|e| e.into_inner()).names.get(id as usize).cloned()
}

/// Interface `name`'s id, if any template has named it.
pub(crate) fn id_of(name: &str) -> Option<u32> {
    registry().lock().unwrap_or_else(|e| e.into_inner()).ids.get(name).copied()
}

/// Whether interface `id` is `target` or inherits from it.
///
/// Every receiver check asks, so this takes no lock: each thread keeps every id's ancestors, rebuilt from
/// the registry when [`VERSION`] says it changed -- which it stops doing once the installs are over.
pub(crate) fn is_a(id: u32, target: u32) -> bool {
    if id == target {
        return true;
    }
    thread_local! {
        static ANCESTORS: std::cell::RefCell<(u64, Vec<Vec<u32>>)> = const { std::cell::RefCell::new((u64::MAX, Vec::new())) };
    }
    let v = VERSION.load(Ordering::Acquire);
    ANCESTORS.with(|a| {
        let mut a = a.borrow_mut();
        if a.0 != v {
            let r = registry().lock().unwrap_or_else(|e| e.into_inner());
            let table = (0..r.parent.len())
                .map(|i| {
                    let mut up = Vec::new();
                    let mut cur = i;
                    for _ in 0..32 {
                        match r.parent.get(cur).copied().flatten() {
                            Some(p) => {
                                up.push(p);
                                cur = p as usize;
                            }
                            None => break,
                        }
                    }
                    up
                })
                .collect();
            *a = (v, table);
        }
        a.1.get(id as usize).is_some_and(|up| up.contains(&target))
    })
}

/// The two privates, once per isolate (an isolate slot: a check reads them on every call).
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct Keys {
    brand: v8::Global<v8::Private>,
    proto: v8::Global<v8::Private>,
}

fn keys<'s>(scope: &mut v8::PinScope<'s, '_>) -> (v8::Local<'s, v8::Private>, v8::Local<'s, v8::Private>) {
    if let Some(k) = scope.get_slot::<Keys>() {
        return (v8::Local::new(scope, &k.brand), v8::Local::new(scope, &k.proto));
    }
    let b = v8::String::new(scope, "korvus.brand").unwrap();
    let b = v8::Private::for_api(scope, Some(b));
    let p = v8::String::new(scope, "korvus.ifaceId").unwrap();
    let p = v8::Private::for_api(scope, Some(p));
    let k = Keys { brand: v8::Global::new(scope, b), proto: v8::Global::new(scope, p) };
    scope.set_slot(k);
    (b, p)
}

/// The private an instance carries its interface's id under (set by its template). Read straight out of
/// the isolate slot: a receiver check asks for it on every call.
pub(crate) fn brand_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    if let Some(k) = scope.get_slot::<Keys>() {
        return v8::Local::new(scope, &k.brand);
    }
    keys(scope).0
}

/// The private an interface prototype carries its interface's id under (set by its template).
pub(crate) fn proto_id_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    if let Some(k) = scope.get_slot::<Keys>() {
        return v8::Local::new(scope, &k.proto);
    }
    keys(scope).1
}

fn id_under(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, key: v8::Local<v8::Private>) -> Option<u32> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let x = o.get_private(scope, key)?;
    if !x.is_int32() {
        return None;
    }
    x.int32_value(scope).map(|n| n as u32)
}

/// THE BRAND FIELD: internal field 0 of an object that has one holds its brand once a check has
/// read it, as the Smi `(id + 1) << 4 | 0xB`. Reading the private is a full V8 property lookup, ~420
/// instructions of every receiver check (`firstChild` was 1,780 in all, measured 2026-09-30); reading the
/// field is four API calls.
///
/// A template cannot give a field a value, so the field is filled lazily, by the first check that reads the
/// private -- the private stays the brand, and the field is only its cache. That is exact because a brand
/// never changes after birth except through [`set_brand`], which writes both. Every branded instance
/// template reserves the field (`install::templates`); the other objects with internal fields leave field 0
/// as V8 made it (deno's global template reserves two and never writes them, promises and array buffers
/// hold Smi 0), and neither `undefined` nor Smi 0 decodes as a brand. The only other internal fields the engine
/// writes are a list's fields 1..=3 (`core::dom::collections::LIST_FIELDS`, reserved by list templates alone),
/// never field 0.
const BRAND_FIELD: usize = 0;

fn field_brand(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<u32> {
    let d = o.get_internal_field(scope, BRAND_FIELD)?;
    let n = v8::Local::<v8::Int32>::try_from(v8::Local::<v8::Value>::try_from(d).ok()?).ok()?.value();
    (n & 0xF == 0xB && n > 0xF).then(|| ((n >> 4) - 1) as u32)
}

fn set_field_brand(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, id: u32) {
    // The context snapshot (slice 3): never in the snapshot maker. V8 hands each internal field holding a Smi
    // of an object it serialises to the embedder's callback, and rusty_v8's reads the field as a pointer to copy from
    // (`SerializeInternalFields`, binding.cc): a cached brand there faults the maker. The private stays the brand, so
    // an object made from the snapshot fills its field on its first check, as an installed one does.
    if o.internal_field_count() > BRAND_FIELD && !crate::binding::refs::recording() {
        let v = v8::Integer::new(scope, ((id as i32 + 1) << 4) | 0xB);
        o.set_internal_field(BRAND_FIELD, v.into());
    }
}

/// The interface `v` was made by, if it is a branded platform object.
pub(crate) fn brand_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    if let Some(id) = field_brand(scope, o) {
        return Some(id);
    }
    let k = brand_key(scope);
    let id = id_under(scope, v, k)?;
    set_field_brand(scope, o, id);
    Some(id)
}

/// The interface prototype `proto` belongs to (its template's id).
pub(crate) fn proto_iface(scope: &mut v8::PinScope, proto: v8::Local<v8::Value>) -> Option<u32> {
    let k = proto_id_key(scope);
    id_under(scope, proto, k)
}

/// Brand an object an interface template did not make (an object with interceptors of its own, a realm's
/// global): the same private the template would have set.
pub(crate) fn set_brand(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, id: u32) {
    let k = brand_key(scope);
    let v = v8::Integer::new_from_unsigned(scope, id);
    obj.set_private(scope, k, v.into());
    set_field_brand(scope, obj, id);
}

/// Whether `v` is a platform object implementing interface `target` -- in any realm.
pub(crate) fn implements_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, target: u32) -> bool {
    brand_of(scope, v).is_some_and(|b| is_a(b, target))
}

/// Give `to` the brand `from` carries, if any (a structured clone of a platform object is the same kind of
/// object; a clone of a fake stays a fake).
pub(crate) fn copy_brand(scope: &mut v8::PinScope, from: v8::Local<v8::Value>, to: v8::Local<v8::Object>) {
    if let Some(id) = brand_of(scope, from) {
        set_brand(scope, to, id);
    }
}
