//! `core/html/session_history.rs` — the session history of this browsing context: ONE list with two
//! readers, `History` (`korvus-runtime`'s `singletons.rs`, which also makes every write) and the
//! Navigation API (`navigation_api.rs`).
//!
//! It lived in `singletons.rs` until the Navigation API needed the same entries: a
//! second list would have let `history.length` and `navigation.entries().length` disagree after the
//! first `pushState`.

use deno_core::v8;

/// One session-history entry.
///
/// `generation` is the DOCUMENT the entry belongs to: a traversal to an entry of the current document
/// is same-document (the URL moves, `popstate` and maybe `hashchange` fire); a traversal to an entry of
/// an earlier document is a navigation the driver loads.
///
/// `key` names the entry's SLOT and `id` the entry itself: a replacement keeps the key and gets a new
/// id, which is what `NavigationHistoryEntry`'s `key` and `id` report. `nav_state` is the Navigation
/// API's state (`updateCurrentEntry` / `getState()`), a different thing from `state`, the classic
/// `history.state`.
///
/// `serial`, `title` and `transition` are what the browser side of Chrome keeps for each entry and reports
/// through `Page.getNavigationHistory` (D88 step 2): its id, the title its document had, and how it was reached
/// (`typed`, `link`, `reload`, `auto_toplevel`, ...). `seeded` marks the `about:blank` a new tab is created at,
/// which is an entry of its own in Chrome (measured: `Target.createTarget` at `about:blank` then two navigations
/// reads three entries), unlike the initial empty document a popup's first navigation replaces.
pub struct HistoryEntry {
    pub url: String,
    pub state: Option<v8::Global<v8::Value>>,
    pub generation: u64,
    pub key: String,
    pub id: String,
    pub nav_state: Option<v8::Global<v8::Value>>,
    pub serial: u64,
    pub title: String,
    pub transition: String,
    pub seeded: bool,
}

/// The next entry's `serial`: unique in this process, as Chrome's entry ids are unique in its browser.
fn next_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl HistoryEntry {
    /// An entry in a new slot: a fresh key and id, and no Navigation API state.
    pub fn new(url: String, state: Option<v8::Global<v8::Value>>, generation: u64) -> Self {
        let uuid = crate::binding::instance_state::uuid;
        HistoryEntry {
            url,
            state,
            generation,
            key: uuid(),
            id: uuid(),
            nav_state: None,
            serial: next_serial(),
            title: String::new(),
            transition: "link".to_string(),
            seeded: false,
        }
    }

    /// The entry that takes this one's slot in a replacement: the same key, a new id.
    pub fn replacement(&self, url: String, state: Option<v8::Global<v8::Value>>, generation: u64) -> Self {
        HistoryEntry { key: self.key.clone(), ..HistoryEntry::new(url, state, generation) }
    }
}

/// How a committed document enters the session history (Chrome's `NavigationController`): after the current
/// entry (`Push`), in its place (`Replace`, `Reload`), or at the entry a traversal went to (`Traverse`, by its
/// `serial`). The driver hands it to the new realm with the document's [`crate::page::Arrival`].
#[derive(Debug, Clone, Default, PartialEq)]
pub enum HistoryCommit {
    #[default]
    Push,
    Replace,
    Reload,
    Traverse(u64),
}

/// A session history on its way from one realm to the next (`Engine::successor`): the states structured-serialized,
/// since a `v8::Global` dies with its isolate.
pub struct CarriedHistory {
    entries: Vec<(HistoryEntry, Option<Vec<u8>>, Option<Vec<u8>>)>,
    index: usize,
    generation: u64,
    scroll_restoration: String,
}

/// Take this realm's session history, its entries' states serialized, and the leaving document's title kept in its
/// entry; `None` for a realm that never had one.
pub fn carry_history(scope: &mut v8::PinScope, title: String) -> Option<CarriedHistory> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let h = op_state.borrow_mut().try_take::<HistoryState>()?;
    let mut entries = Vec::with_capacity(h.entries.len());
    for (i, mut e) in h.entries.into_iter().enumerate() {
        if i == h.index && e.generation == h.generation {
            e.title = title.clone();
        }
        let ser = |scope: &mut v8::PinScope, g: Option<v8::Global<v8::Value>>| {
            g.map(|g| {
                let v = v8::Local::new(scope, &g);
                crate::modules::clone_state::wire_serialize(scope, v, &[])
            })
        };
        let state = ser(scope, e.state.take());
        let nav_state = ser(scope, e.nav_state.take());
        entries.push((e, state, nav_state));
    }
    Some(CarriedHistory { entries, index: h.index, generation: h.generation, scroll_restoration: h.scroll_restoration })
}

/// Hand a successor realm the session history its predecessor carried ([`carry_history`]).
pub fn restore_history(scope: &mut v8::PinScope, carried: CarriedHistory) {
    let mut entries = Vec::with_capacity(carried.entries.len());
    for (mut e, state, nav_state) in carried.entries {
        let de = |scope: &mut v8::PinScope, b: Option<Vec<u8>>| {
            b.map(|b| {
                let v = crate::modules::clone_state::wire_deserialize(scope, &b, &[]);
                v8::Global::new(scope, v)
            })
        };
        e.state = de(scope, state);
        e.nav_state = de(scope, nav_state);
        entries.push(e);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(HistoryState {
        entries,
        index: carried.index,
        generation: carried.generation,
        scroll_restoration: carried.scroll_restoration,
        activation: None,
    });
}

/// A new tab's session history before its first navigation: the `about:blank` it was created at, an entry of its
/// own (see [`HistoryEntry::seeded`]).
pub fn seed_blank_history(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) {
    let mut e = HistoryEntry::new("about:blank".to_string(), None, 0);
    e.transition = "auto_toplevel".to_string();
    e.seeded = true;
    op_state.borrow_mut().put(HistoryState { entries: vec![e], ..HistoryState::default() });
}

/// The session history a page can see and move through. One per isolate, in `OpState`.
///
/// `length` starts at 1 and `state` at `null`, which is what the bundle's oracle records for a fresh
/// document (`oracle/History.json`: `length` 1, `scrollRestoration` "auto", `state` null).
pub struct HistoryState {
    pub entries: Vec<HistoryEntry>,
    pub index: usize,
    pub generation: u64,
    pub scroll_restoration: String,
    /// `navigation.activation.navigationType` of the current document, as its commit entered the history: `push`,
    /// `replace` (a replacement, or the first document over the initial one), `reload`, `traverse`.
    pub activation: Option<&'static str>,
}

impl Default for HistoryState {
    fn default() -> Self {
        HistoryState { entries: Vec::new(), index: 0, generation: 0, scroll_restoration: "auto".to_string(), activation: None }
    }
}

/// Run `f` over this isolate's session history. A history nobody has touched yet gets its first entry,
/// `href` (the document's URL), here.
pub fn with_history<R>(scope: &mut v8::PinScope, href: String, f: impl FnOnce(&mut HistoryState) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<HistoryState>().is_none() {
        st.put(HistoryState::default());
    }
    let h = st.borrow_mut::<HistoryState>();
    if h.entries.is_empty() {
        let generation = h.generation;
        h.entries.push(HistoryEntry::new(href, None, generation));
        h.index = 0;
    }
    f(h)
}

/// The page's same-document navigations since the driver last asked: each URL and what `Page.navigatedWithinDocument`
/// calls it (`fragment`, `historyApi`, `other`; D88 step 2).
#[derive(Default)]
struct SameDocumentLog(Vec<(String, &'static str)>);

/// Note a same-document navigation of the page to `url`, for the driver to report.
pub fn note_same_document(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, url: &str, kind: &'static str) {
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<SameDocumentLog>().is_none() {
        st.put(SameDocumentLog::default());
    }
    st.borrow_mut::<SameDocumentLog>().0.push((url.to_string(), kind));
}

/// The same-document navigations noted since the last call, in order.
pub fn take_same_document(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) -> Vec<(String, &'static str)> {
    op_state.borrow_mut().try_borrow_mut::<SameDocumentLog>().map(|l| std::mem::take(&mut l.0)).unwrap_or_default()
}
