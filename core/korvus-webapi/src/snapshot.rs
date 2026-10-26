//! THE PAGE REALM IN A SNAPSHOT (the context snapshot, slice 3).
//!
//! A V8 snapshot carries the realm's HEAP: its objects, its templates, the privates on them. It does not carry what
//! the install left in RUST:
//! - the registries that number interfaces (brand ids sit in the heap as privates);
//! - the process statics only an install fills (the window singletons' mints, the realm's surface);
//! - the `OpState` holders of the realm's own objects (the DOM prototypes, the device oracle); the isolation gate's
//!   `SharedArrayBuffer` is not carried: V8 puts it back on a deserialised global, and the restore re-holds it;
//! - V8's late names (`Temporal`, `WebAssembly`, ...: what V8 adds to a context only outside a snapshot maker): their
//!   list, so the restore makes the engine's parts of them and seats every global in the capture's order;
//! - the isolate's hooks.
//!
//! A process made from the snapshot has none of these. The two halves:
//! - [`prepare_page_snapshot`] and [`snapshot_page`] run where the snapshot is made, after the install and before V8
//!   serialises;
//! - [`restore_page`] runs in a runtime made from it, in place of the install.
//!
//! What is listed here, and why each entry is, is the rewire table in the snapshot design note (`SNAPSHOT.md`,
//! "Slice 3 audit"). Every entry is either carried here, re-made on load, or empty at the snapshot point
//! ([`prepare_page_snapshot`] asserts the DOM's).
use deno_core::snapshot::{EmbedderSnapshotData, SnapshotLoadDataStore, SnapshotStoreDataStore};
use deno_core::v8;

use crate::binding::host_dom::{
    DocumentGlobal, ElementProto, MathmlElementProto, MathmlTagProtos, NodeCache, NodeProtos, OwnerMap, ShadowRootProto, SvgElementProto, SvgTagProtos, TagProtos,
    UnknownElementProto,
};
use crate::core::html::window_globals::{Mint, MintRow};

/// The page section's tag, and its format's version.
const MAGIC: &[u8; 8] = b"KVSNPGE\x02";

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.0.push(v as u8);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.str(s)
            }
        }
    }
    fn opt_u32(&mut self, v: Option<u32>) {
        match v {
            None => self.u8(0),
            Some(v) => {
                self.u8(1);
                self.u32(v)
            }
        }
    }
}

struct R<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn bool(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn opt_str(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.str()?)),
            _ => None,
        }
    }
    fn opt_u32(&mut self) -> Option<Option<u32>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.u32()?)),
            _ => None,
        }
    }
}

/// The snapshot creator's last step before V8 serialises the page realm: nothing that cannot travel may be in it.
/// - No DOM wrapper may exist yet: the arena is Rust's, so a wrapper in the snapshot would come back unmapped, and
///   the next wrap would mint a second object for the same node (`document !== document`).
/// - Every window singleton minted from Rust state leaves its slot, and is minted on its first read after a load
///   (`window_globals::unseat_minted`): the cppgc ones (performance, storage, IndexedDB, ...) cannot travel, and the
///   rest are per-run.
pub fn prepare_page_snapshot(scope: &mut v8::PinScope) -> Result<(), String> {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let wrapped = !st.borrow::<NodeCache>().is_empty() || !st.borrow::<OwnerMap>().0.borrow().is_empty() || st.borrow::<DocumentGlobal>().0.borrow().is_some();
        if wrapped {
            return Err("a DOM wrapper exists where the snapshot is made".to_string());
        }
    }
    // window.performance's Rust-held sub-objects (STATE, minted again after a load): released while its slot still holds it
    crate::core::timing::performance::release_for_snapshot(scope);
    crate::core::html::window_globals::unseat_minted(scope);
    // deno reads `Deno.core`'s callbacks off the global on every `JsRuntime::new`, one made from this snapshot
    // included: the snapshot carries `Deno`, and the restore hides it again ([`PageStatics::apply`])
    crate::binding::hidden::reseat_deno(scope);
    Ok(())
}

/// What the page realm left in Rust, into the snapshot being made: V8 data in `store`, the rest appended to `out`.
/// [`restore_page`] reads it back in the same order.
pub fn snapshot_page(scope: &mut v8::PinScope, store: &mut SnapshotStoreDataStore, out: &mut Vec<u8>) -> Result<(), String> {
    // the capture's global order, for the restore's seat of V8's late names (section 8): read here, where the install
    // already parsed the surface, so a restore never parses it for this
    let order = match crate::install::installer::v8_late_names().is_empty() {
        true => Vec::new(),
        false => crate::install::surface::active(scope).map(|s| s.globals.clone()).unwrap_or_default(),
    };
    // the page realm's surface, already parsed (section 9): a restored process seeds the persona's cache with
    // it and parses no surface JSON
    let page_surface = crate::install::surface::active(scope);
    let mut w = W::default();
    w.0.extend_from_slice(MAGIC);
    // 1. the brand registry, in id order
    let (names, parents) = crate::binding::brand::registry_snapshot();
    w.u32(names.len() as u32);
    for (n, p) in names.iter().zip(&parents) {
        w.str(n);
        w.opt_u32(*p);
    }
    // 2. the window singletons' mints
    let mints = crate::core::html::window_globals::mints_snapshot()?;
    w.u32(mints.len() as u32);
    for (name, row, iface) in &mints {
        w.str(name);
        match row {
            MintRow::Plain => w.u8(0),
            MintRow::Shell(s) => {
                w.u8(1);
                w.str(s)
            }
            MintRow::Alias(s) => {
                w.u8(2);
                w.str(s)
            }
            MintRow::With(off) => {
                w.u8(3);
                w.i64(*off)
            }
        }
        w.opt_str(iface.as_deref());
    }
    // 3. the template cache and the function records (slice 2a)
    crate::install::templates::snapshot_templates(scope, store, &mut w.0)?;
    // 4. the page's lazy names
    let (lazy, tags) = crate::install::templates::lazy_page_snapshot(scope);
    w.u32(lazy.len() as u32);
    for (k, i, e, wr) in &lazy {
        w.str(k);
        w.str(i);
        w.bool(*e);
        w.bool(*wr);
    }
    w.u32(tags.len() as u32);
    for t in &tags {
        w.str(t);
    }
    // 5. the DOM's prototypes (`fill_dom_protos`), 6. the device oracle (7., V8's late names, after them)
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let mut node: Vec<(u32, v8::Global<v8::Object>)> = st.borrow::<NodeProtos>().0.borrow().iter().map(|(k, g)| (*k, g.clone())).collect();
    node.sort_by_key(|(k, _)| *k);
    w.u32(node.len() as u32);
    for (k, g) in node {
        w.u32(k);
        w.u32(store.register(g));
    }
    for map in [&st.borrow::<TagProtos>().0, &st.borrow::<SvgTagProtos>().0, &st.borrow::<MathmlTagProtos>().0] {
        let mut v: Vec<(String, v8::Global<v8::Object>)> = map.borrow().iter().map(|(k, g)| (k.clone(), g.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        w.u32(v.len() as u32);
        for (k, g) in v {
            w.str(&k);
            w.u32(store.register(g));
        }
    }
    for one in [
        &st.borrow::<ShadowRootProto>().0,
        &st.borrow::<UnknownElementProto>().0,
        &st.borrow::<SvgElementProto>().0,
        &st.borrow::<ElementProto>().0,
        &st.borrow::<MathmlElementProto>().0,
    ] {
        let g = one.borrow().clone();
        w.opt_u32(g.map(|g| store.register(g)));
    }
    match st.try_borrow::<crate::core::html::webgl::WebGLOracle>() {
        None => w.u8(0),
        Some(o) => {
            w.u8(1);
            w.u32(store.register(o.0.clone()));
            w.bool(o.1);
        }
    }
    // 7. V8's late names, which the restore seats, and re-makes the engine's parts of (`installer::set_v8_late`)
    let late = crate::install::installer::v8_late_names();
    w.u32(late.len() as u32);
    for n in late {
        w.str(n);
    }
    // 8. the capture's global order they are seated by
    w.u32(order.len() as u32);
    for n in &order {
        w.str(n);
    }
    // 9. the page realm's surface (the Window realm's: what `surface::active` resolved for it)
    put_surface(&mut w, page_surface.as_deref());
    out.extend_from_slice(&w.0);
    Ok(())
}

/// [`snapshot_page`]'s reader, in a runtime made from the snapshot, in place of the page realm's install. It works in
/// TWO halves (MAIN, 10-13):
/// - this one decodes and checks everything, and writes only what belongs to the runtime being made (its isolate's
///   templates, slots and `OpState`), which dies with it if the restore fails;
/// - [`PageStatics::apply`], called when nothing that can fail is left, writes the PROCESS's statics: the brand
///   registry, the singleton mints, the realm's ordinal and surface. So a restore that fails leaves this process as it
///   was, and the engine it falls back to installs on clean statics.
///
/// It reads `data.bytes` from `*at` on and moves `*at` past its section. An error says the snapshot cannot be used in
/// this process; the caller builds the realm the ordinary way instead.
pub fn restore_page(scope: &mut v8::PinScope, data: &mut EmbedderSnapshotData, at: &mut usize) -> Result<PageStatics, String> {
    let bad = || "the snapshot's page section does not decode".to_string();
    let bytes = std::mem::take(&mut data.bytes);
    let res = restore_inner(scope, &bytes, at, &mut data.store).and_then(|r| r.ok_or_else(bad));
    data.bytes = bytes;
    res
}

/// The second half of a restore ([`restore_page`]): the process's statics.
pub struct PageStatics {
    names: Vec<String>,
    parents: Vec<Option<u32>>,
    mints: Vec<(String, Mint, Option<String>)>,
    late: Vec<String>,
    order: Vec<String>,
    surface: Option<std::sync::Arc<spec::RealmSurface>>,
}

impl PageStatics {
    /// Write the process's statics, then re-make what reads the finished realm (the frame surface) and the isolate's
    /// hook. The brand registry is checked again under its own lock, and an error there writes nothing.
    pub fn apply(self, scope: &mut v8::PinScope) -> Result<(), String> {
        crate::binding::brand::registry_restore(self.names, self.parents)?;
        crate::core::html::window_globals::mints_install(self.mints);
        // the realm's own ordinal in this process, and its surface (`surface::active`, which every template key reads)
        crate::install::installer::fresh_ordinal(scope);
        crate::install::surface::enter(scope, spec::SurfaceRealm::Window);
        // the persona's surface, as the snapshot carried it parsed: seeded before anything asks for it, so this
        // process -- this realm, its frames, `createEvent` -- never parses the bundle's JSON
        if let Some(s) = self.surface.clone() {
            let persona = {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::PersonaHandle>()).map(|h| h.0.clone())
            };
            if let Some(p) = persona {
                if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
                    eprintln!("korvus: snapshot: the page realm's surface seeded ({} globals, {} interfaces)", s.globals.len(), s.interfaces.len());
                }
                p.seed_surface(spec::SurfaceRealm::Window, s);
            }
        }
        // `Deno`, which the snapshot carries for deno's own load, off the global again -- before anything reads the
        // global's names
        crate::hide_deno(scope);
        // the `SharedArrayBuffer` gate, made again as a fresh realm's bootstrap makes it, by the same function: V8 puts
        // the constructor back on every global it deserialises (bootstrapper.cc, `InitializeExperimentalGlobal` ->
        // `InitializeGlobal_sharedarraybuffer`), and the gate holds this realm's own; the navigation's policy decides
        crate::core::html::isolation_policy::isolation_gate(scope);
        // V8's late names (`installer::set_v8_late`): the maker's realm had none of them, and V8 put each on this global
        // after the snapshot's own names. What the engine adds to them is made as a fresh install makes it --
        // WebAssembly's streaming rows, the sealed intrinsics -- then every global is seated in the capture's order, and
        // the creators' locks applied, by the install's own last pass
        if self.late.iter().any(|n| n == "WebAssembly") {
            crate::modules::wasm_streaming::install_wasm_streaming_rows(scope);
        }
        crate::binding::protos::reseal_intrinsics(scope, &self.late);
        let stuck = crate::install::installer::seat_globals_in_order(scope, Some(&self.order), &self.late);
        if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
            let stuck = if stuck.is_empty() { "none".to_string() } else { stuck.join(", ") };
            eprintln!("korvus: snapshot: V8's late names, seated after the load: {} (stuck: {stuck})", self.late.join(", "));
        }
        // what reads the finished realm: the surface a frame's window is built from, and the main context's handle
        crate::seal_frame_surface(scope);
        // the isolate's own hook (a snapshot carries none): CSP / Trusted Types code generation
        crate::install_codegen_hook(scope);
        // what a lazy interface's first make reads -- the realm's surface identity, so the persona's surface parsed, and
        // the surface's blueprint file loaded -- read NOW, before any page script, as an install has it: the first lazy
        // make of a restored realm then costs what a fresh realm's does (until D6 R6 keys a restored realm's makes from
        // the snapshot and reads one blueprint, which takes this out of the restore)
        let _ = crate::install::templates::realm_identity(scope);
        if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
            eprintln!("korvus: snapshot: restore re-makes the released isolate slots: {SLOTS_REMADE}");
        }
        Ok(())
    }
}

/// The maker's last step, AFTER its sections are in the store: V8 refuses to serialise while a `v8::Global` it was not
/// handed is alive (`CheckGlobalAndEternalHandles`), and deno's cleanup clears only `OpState`. The isolate slots this
/// crate keeps Globals in -- key caches, template caches, the lazy page -- go here: caches, made again on demand, in an
/// isolate that is serialised and never used again. (loaf's monitor slot holds none, and its hooks stay installed.)
pub fn release_isolate_slots(scope: &mut v8::PinScope) {
    let released = [
        ("DtlKeys", scope.remove_slot::<crate::core::dom::dom_token_list::DtlKeys>().is_some()),
        ("EvtKeys", scope.remove_slot::<crate::core::events::event::EvtKeys>().is_some()),
        ("SlotKeys", scope.remove_slot::<crate::core::events::event::SlotKeys>().is_some()),
        ("WindowSlotKeys", scope.remove_slot::<crate::core::html::window_globals::WindowSlotKeys>().is_some()),
        ("brand::Keys", scope.remove_slot::<crate::binding::brand::Keys>().is_some()),
        ("LazyPage", scope.remove_slot::<crate::install::templates::LazyPage>().is_some()),
        ("MintTemplates", scope.remove_slot::<crate::binding::protos::MintTemplates>().is_some()),
        ("PrivateKeys", scope.remove_slot::<crate::binding::protos::PrivateKeys>().is_some()),
        ("IfaceNameKey", scope.remove_slot::<crate::binding::protos::IfaceNameKey>().is_some()),
    ];
    // loaf's monitor: its WEAK context handles count too; a fresh empty one stays in its slot for the hooks
    crate::core::timing::loaf::reset_for_snapshot(scope);
    if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
        let names: Vec<&str> = released.iter().filter(|(_, r)| *r).map(|(n, _)| *n).collect();
        eprintln!("korvus: snapshot: released isolate slots (Globals V8 was not handed): {}; loaf monitor reset (weak context handles)", names.join(", "));
    }
}

/// What the restore says about the isolate slots the maker released ([`release_isolate_slots`]): the lazy page is
/// STATE, re-made from the snapshot's section; the rest are CACHES keyed by a name (V8's API private registry,
/// restored with the isolate, gives back the very private stamped on the snapshot's objects) or by a per-isolate
/// record ID (restored identically with the function table), filled on first use exactly as in a fresh realm.
const SLOTS_REMADE: &str = "LazyPage by restore_page (eagerly, from the snapshot); window.performance with its [SameObject] \
    timing / navigation / eventCounts minted fresh on first read (released by the maker); loaf's monitor installed fresh \
    at engine start (install_loaf_monitor, before the restore); the SharedArrayBuffer gate re-held from the restored \
    global by isolation_policy::isolation_gate (V8 re-adds the constructor to a deserialised global); the page context in deno's uv \
    loop.data re-taken from THIS runtime's context by deno's register_uv_loop at JsRuntime::new; on first use, by name, \
    as in a fresh realm: \
    DtlKeys (dom_token_list keys), EvtKeys (event::evt_keys), SlotKeys (event::slot_key), WindowSlotKeys \
    (window_globals::slot), brand::Keys (brand::brand_key / proto_id_key), MintTemplates (protos::mint_template), \
    PrivateKeys (protos::private_key), IfaceNameKey (protos::iface_name_private)";

/// The switches of this crate's recorders that observe the INSTALL -- the layer each row is mounted at
/// (`KORVUS_LAYERS_OUT`, `status.json`'s layer), the coverage and touch recorders (`KORVUS_COVERAGE_OUT`,
/// `KORVUS_TOUCH_LOG`: armed before the first member is mounted), the stub log (`KORVUS_STUB_LOG`), and the template
/// reporter (`KORVUS_TPL_REPORT`: its surface walks count what the install made, by identity -- in a restored realm every
/// native would read as made by no template). A restored realm ran no install, so a run with one of them set installs
/// (the runtime asks before it loads a snapshot). Snapshot diagnostics have their own switch,
/// `KORVUS_SNAPSHOT_REPORT`. The lock's snapshot stage strips these (`snapshot_stage.py`, `OBSERVERS`: keep both lists
/// alike).
const OBSERVES_INSTALL: [&str; 5] = ["KORVUS_LAYERS_OUT", "KORVUS_COVERAGE_OUT", "KORVUS_TOUCH_LOG", "KORVUS_STUB_LOG", "KORVUS_TPL_REPORT"];

/// How a restore's error begins when the snapshot is sound but cannot be used THIS run: the surface's
/// blueprint file, which its lazy interfaces are made from, is missing, damaged or not trusted. The runtime installs the
/// ordinary way and keeps the snapshot (no refusal): that install writes the file again, and the next run restores.
pub const NOT_NOW: &str = "not this run: ";

/// Whether this run observes the install ([`OBSERVES_INSTALL`]): it must install, not restore.
pub fn observes_install() -> bool {
    OBSERVES_INSTALL.iter().any(|k| std::env::var_os(k).is_some())
}

/// Keep the restore's data store for the templates decoded on first lookup (the runtime hands it over once
/// its last section has read from it, before the statics are applied).
pub fn keep_store(scope: &mut v8::PinScope, store: deno_core::snapshot::SnapshotLoadDataStore) {
    crate::install::templates::keep_snapshot_store(scope, store);
}

/// The running binary's build id (the blueprint files'): a snapshot file names the build that made it.
pub fn build_id() -> Option<&'static [u8]> {
    crate::install::templates::snapshot_disk::build_id()
}

/// The blueprint files' word-wise checksum, over a snapshot file's payload.
pub fn checksum(b: &[u8]) -> u64 {
    crate::install::templates::snapshot_disk::checksum(b)
}

/// Whether a cache file may be read as this binary's own (a snapshot is code): the file and its directory belong to
/// this user, and nobody else may write either.
pub fn trusted(path: &std::path::Path) -> bool {
    crate::install::templates::snapshot_disk::trusted(path)
}

/// `dir`, created if missing, made this user's alone.
pub fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    crate::install::templates::snapshot_disk::private_dir(dir)
}

/// `bytes` into a new file `path` only this user can read or write.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::install::templates::snapshot_disk::write_private(path, bytes)
}

fn restore_inner(scope: &mut v8::PinScope, b: &[u8], at: &mut usize, store: &mut SnapshotLoadDataStore) -> Result<Option<PageStatics>, String> {
    let mut r = R { b, i: *at };
    if r.take(8) != Some(&MAGIC[..]) {
        return Ok(None);
    }
    // 1. the brand registry: checked here, written by `apply`
    let Some(n) = r.u32() else { return Ok(None) };
    let (mut names, mut parents) = (Vec::with_capacity(n as usize), Vec::with_capacity(n as usize));
    for _ in 0..n {
        let (Some(name), Some(p)) = (r.str(), r.opt_u32()) else { return Ok(None) };
        names.push(name);
        parents.push(p);
    }
    if !crate::binding::brand::registry_compatible(&names) {
        return Err("this process numbered its interfaces differently from the snapshot".to_string());
    }
    // 2. the window singletons' mints: decoded and checked here, written by `apply`
    let Some(n) = r.u32() else { return Ok(None) };
    let mut rows = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let Some(name) = r.str() else { return Ok(None) };
        let row = match r.u8() {
            Some(0) => MintRow::Plain,
            Some(1) => MintRow::Shell(match r.str() {
                Some(s) => s,
                None => return Ok(None),
            }),
            Some(2) => MintRow::Alias(match r.str() {
                Some(s) => s,
                None => return Ok(None),
            }),
            Some(3) => MintRow::With(match r.i64() {
                Some(o) => o,
                None => return Ok(None),
            }),
            _ => return Ok(None),
        };
        let Some(iface) = r.opt_str() else { return Ok(None) };
        rows.push((name, row, iface));
    }
    let mints = crate::core::html::window_globals::mints_decode(rows)?;
    // 3. the template cache and the function records (slice 2a)
    let mut t_at = r.i;
    crate::install::templates::restore_templates(scope, b, &mut t_at, store)?;
    r.i = t_at;
    // 4. the page's lazy names
    let Some(n) = r.u32() else { return Ok(None) };
    let mut lazy = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (Some(k), Some(i), Some(e), Some(wr)) = (r.str(), r.str(), r.bool(), r.bool()) else { return Ok(None) };
        lazy.push((k, i, e, wr));
    }
    let Some(n) = r.u32() else { return Ok(None) };
    let mut tags = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let Some(t) = r.str() else { return Ok(None) };
        tags.push(t);
    }
    // every interface the page put lazily can be made in this process -- before anything here writes a static:
    // a realm whose blueprint file is not here this run installs the ordinary way, and keeps the snapshot
    crate::install::templates::restored_lazy_sources(scope, lazy.iter().map(|(_, i, _, _)| i.as_str())).map_err(|why| format!("{NOT_NOW}{why}"))?;
    crate::install::templates::lazy_page_restore(scope, lazy, tags);
    // 5. the DOM's prototypes, 6. the device oracle
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let Some(n) = r.u32() else { return Ok(None) };
        let mut node = std::collections::HashMap::with_capacity(n as usize);
        for _ in 0..n {
            let (Some(k), Some(id)) = (r.u32(), r.u32()) else { return Ok(None) };
            node.insert(k, store.get::<v8::Object>(scope, id));
        }
        let mut maps: Vec<std::collections::HashMap<String, v8::Global<v8::Object>>> = Vec::with_capacity(3);
        for _ in 0..3 {
            let Some(n) = r.u32() else { return Ok(None) };
            let mut m = std::collections::HashMap::with_capacity(n as usize);
            for _ in 0..n {
                let (Some(k), Some(id)) = (r.str(), r.u32()) else { return Ok(None) };
                m.insert(k, store.get::<v8::Object>(scope, id));
            }
            maps.push(m);
        }
        let mut ones: Vec<Option<v8::Global<v8::Object>>> = Vec::with_capacity(5);
        for _ in 0..5 {
            let Some(id) = r.opt_u32() else { return Ok(None) };
            ones.push(id.map(|id| store.get::<v8::Object>(scope, id)));
        }
        let oracle = match r.u8() {
            Some(0) => None,
            Some(1) => {
                let (Some(id), Some(flag)) = (r.u32(), r.bool()) else { return Ok(None) };
                Some((store.get::<v8::Value>(scope, id), flag))
            }
            _ => return Ok(None),
        };
        let mut st = op_state.borrow_mut();
        *st.borrow::<NodeProtos>().0.borrow_mut() = node;
        let mut maps = maps.into_iter();
        *st.borrow::<TagProtos>().0.borrow_mut() = maps.next().unwrap_or_default();
        *st.borrow::<SvgTagProtos>().0.borrow_mut() = maps.next().unwrap_or_default();
        *st.borrow::<MathmlTagProtos>().0.borrow_mut() = maps.next().unwrap_or_default();
        let mut ones = ones.into_iter();
        *st.borrow::<ShadowRootProto>().0.borrow_mut() = ones.next().flatten();
        *st.borrow::<UnknownElementProto>().0.borrow_mut() = ones.next().flatten();
        *st.borrow::<SvgElementProto>().0.borrow_mut() = ones.next().flatten();
        *st.borrow::<ElementProto>().0.borrow_mut() = ones.next().flatten();
        *st.borrow::<MathmlElementProto>().0.borrow_mut() = ones.next().flatten();
        if let Some((g, flag)) = oracle {
            st.put(crate::core::html::webgl::WebGLOracle(g, flag));
        }
    }
    // 7. V8's late names
    let Some(n) = r.u32() else { return Ok(None) };
    let mut late = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let Some(s) = r.str() else { return Ok(None) };
        late.push(s);
    }
    // 8. the capture's global order
    let Some(n) = r.u32() else { return Ok(None) };
    let mut order = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let Some(s) = r.str() else { return Ok(None) };
        order.push(s);
    }
    // 9. the page realm's surface
    let Some(surface) = get_surface(&mut r) else { return Ok(None) };
    *at = r.i;
    Ok(Some(PageStatics { names, parents, mints, late, order, surface: surface.map(std::sync::Arc::new) }))
}

/// Section 9: a realm's surface (`spec::RealmSurface`), interfaces in name order.
fn put_surface(w: &mut W, s: Option<&spec::RealmSurface>) {
    let Some(s) = s else {
        w.u8(0);
        return;
    };
    w.u8(1);
    w.str(&s.capture_id);
    w.u32(s.globals.len() as u32);
    for g in &s.globals {
        w.str(g);
    }
    let mut names: Vec<&String> = s.interfaces.keys().collect();
    names.sort();
    w.u32(names.len() as u32);
    for n in names {
        let m = &s.interfaces[n];
        w.str(n);
        w.u32(m.members.len() as u32);
        for x in &m.members {
            w.str(x);
        }
        w.u32(m.statics.len() as u32);
        for x in &m.statics {
            w.str(x);
        }
        w.opt_u32(m.ctor_index.map(|i| i as u32));
    }
}

/// [`put_surface`]'s reader: `None` when it does not decode, `Some(None)` for a realm that had none.
fn get_surface(r: &mut R) -> Option<Option<spec::RealmSurface>> {
    match r.u8()? {
        0 => return Some(None),
        1 => {}
        _ => return None,
    }
    let capture_id = r.str()?;
    let n = r.u32()? as usize;
    let mut globals = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        globals.push(r.str()?);
    }
    let n = r.u32()? as usize;
    let mut interfaces = std::collections::HashMap::with_capacity(n.min(1 << 14));
    for _ in 0..n {
        let name = r.str()?;
        let k = r.u32()? as usize;
        let mut members = Vec::with_capacity(k.min(1 << 12));
        for _ in 0..k {
            members.push(r.str()?);
        }
        let k = r.u32()? as usize;
        let mut statics = Vec::with_capacity(k.min(1 << 12));
        for _ in 0..k {
            statics.push(r.str()?);
        }
        let ctor_index = r.opt_u32()?.map(|i| i as usize);
        interfaces.insert(name, spec::MemberOrder { members, statics, ctor_index });
    }
    Some(Some(spec::RealmSurface { capture_id, globals, interfaces }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::Persona;

    /// What a restored process seeds is what it would have parsed -- every bundle's Window surface written
    /// as section 9 and read back is the parsed one, field by field, and the reader stops where the writer did.
    #[test]
    fn section_9_carries_every_bundles_window_surface_exactly() {
        let bundles = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
        let mut seen = 0;
        for entry in std::fs::read_dir(&bundles).expect("the persona bundles") {
            let dir = entry.expect("a bundle entry").path();
            if !dir.join("manifest.toml").exists() {
                continue;
            }
            let id = dir.file_name().expect("a bundle id").to_string_lossy().to_string();
            let p = persona::CapturedPersona::select(Some(id.as_str())).unwrap_or_else(|e| panic!("{id}: {e}"));
            let parsed = p.surface(spec::SurfaceRealm::Window).unwrap_or_else(|| panic!("{id}: no Window surface"));
            let mut w = W::default();
            put_surface(&mut w, Some(&parsed));
            w.u8(0x5a);
            let mut r = R { b: &w.0, i: 0 };
            let read = get_surface(&mut r).flatten().unwrap_or_else(|| panic!("{id}: section 9 does not read back"));
            assert_eq!(r.u8(), Some(0x5a), "{id}: the reader stopped elsewhere than the writer");
            assert_eq!(read.capture_id, parsed.capture_id, "{id}: capture id");
            assert_eq!(read.globals, parsed.globals, "{id}: globals, in order");
            let mut names: Vec<&String> = parsed.interfaces.keys().collect();
            names.sort();
            assert_eq!(read.interfaces.len(), names.len(), "{id}: interface count");
            for n in names {
                let (a, b) = (&parsed.interfaces[n], read.interfaces.get(n).unwrap_or_else(|| panic!("{id}: {n} missing")));
                assert_eq!((&a.members, &a.statics, a.ctor_index), (&b.members, &b.statics, b.ctor_index), "{id}: {n}");
            }
            seen += 1;
        }
        assert!(seen >= 1, "no persona bundle in the tree");
        // and a realm with no surface reads back as none
        let mut w = W::default();
        put_surface(&mut w, None);
        assert!(matches!(get_surface(&mut R { b: &w.0, i: 0 }), Some(None)));
    }
}
