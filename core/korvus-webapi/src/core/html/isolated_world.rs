//! `core/html/isolated_world.rs` -- ISOLATED WORLDS: a second JavaScript world over the page's
//! document, as Blink's `DOMWrapperWorld` makes one for a DevTools client (`Page.createIsolatedWorld`, or a
//! `Page.addScriptToEvaluateOnNewDocument` naming a `worldName`).
//!
//! THE DEFECT. `Page.createIsolatedWorld` answered context 2 and every evaluation in it ran in the PAGE's context,
//! and a new-document script naming a world ran in the page's too: Playwright's utility world -- where its selector
//! engine lives -- never existed (its selector APIs hung on `korvus serve`), what a
//! driver put in its world was the page's to read, and what the page had done to its own objects was the driver's
//! to trip over (a bot-detection script overrides a challenge iframe's members on the instance; Chrome's isolated world reads the
//! real element, korvus's read the override).
//!
//! WHAT A WORLD IS, measured on stock Chrome 150 over CDP (three runs alike) after a
//! page set `window.pageVar`, `HTMLElement.prototype.pageProto`, `document.body.custom`, an own `document.title`
//! getter and an own `document.querySelector`: in the world `typeof pageVar` is `undefined`, the prototype's and the
//! instance's additions are invisible, `document.title` is the real title and `querySelector` the real method, yet
//! `getElementById('p').textContent` reads the page's text -- the same DOM, other objects. So a world here is:
//!
//! * its own V8 context: its own global and intrinsics, its own interface objects and prototypes -- a realm made
//!   from templates exactly as a frame's is (`browsing_context::new_realm`, `install_lazy_frame`), behind no
//!   browsing context of its own, so every window member answers for the page's;
//! * its own WRAPPERS of the page's nodes (`DOMDataStore` per world): [`wrap`] mints, from the world's own
//!   prototypes, the one object that stands for a node in this world, and `binding::protos::wrap_cached` asks it
//!   whenever the running context is a world; the world's `document` is its own wrapper of the page's document;
//! * an event dispatched from a world runs as the page's ([`dispatch_realm`]): the page's listeners get the page's
//!   objects, as Blink hands every listener its own world's wrappers.
//!
//! WHAT IT IS NOT, said here rather than discovered: a listener a WORLD added gets the page's wrappers (Blink: the
//! world's), an event object a world created reaches the page's listeners as the world's object, `location` is the
//! page's `Location` object, and a node outside the page's own tree -- a frame's, a created document's -- keeps the
//! wrapper of its own realm.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use deno_core::{v8, OpState};

use crate::binding::host_dom::{HostDom, NodeCache};

/// One world: its context, the wrappers it has made of the nodes of its document, and whose document that is.
struct World {
    context: v8::Global<v8::Context>,
    nodes: NodeCache,
    /// The browsing context whose document the world is over: `None` the page's, `Some(id)` a frame's
    /// (`Page.createIsolatedWorld` with a frame's id, or a new-document script naming a world in a frame).
    frame: Option<u32>,
}

/// The frame world `index` is over (`None`: the page, or no such world).
fn world_frame(op_state: &Rc<RefCell<OpState>>, index: usize) -> Option<u32> {
    op_state.borrow().try_borrow::<IsolatedWorlds>()?.list.get(index)?.frame
}

/// The engine's isolated worlds, in the order they were made (a world's index is what the runtime names it by).
#[derive(Default)]
pub(crate) struct IsolatedWorlds {
    list: Vec<World>,
}

thread_local! {
    /// Has a world ever been made on this thread? `wrap_cached` runs on every node a page reaches, and a page that
    /// no DevTools client gave a world must not pay for the question: one thread-local read, then the ordinary path.
    /// (A flag, never a V8 handle: a stale `true` after its engine is gone only costs the lookup below.)
    static ANY: Cell<bool> = const { Cell::new(false) };
}

/// The index of the world whose context is running, if one is.
pub(crate) fn current(scope: &mut v8::PinScope, op_state: &Rc<RefCell<OpState>>) -> Option<usize> {
    if !ANY.with(Cell::get) {
        return None;
    }
    let running = scope.get_current_context();
    let st = op_state.borrow();
    let worlds = st.try_borrow::<IsolatedWorlds>()?;
    worlds.list.iter().position(|w| v8::Local::new(scope, &w.context) == running)
}

/// Is a world running, and over whose document: `Some(None)` the page's, `Some(Some(frame))` a frame's.
pub(crate) fn current_document_owner(scope: &mut v8::PinScope) -> Option<Option<u32>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let world = current(scope, &op_state)?;
    Some(world_frame(&op_state, world))
}

/// World `world`'s object for node `id`: the one it made before, or a new one of its own prototypes. Called with the
/// world's context running.
pub(crate) fn wrap<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &Rc<RefCell<OpState>>, world: usize, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    {
        let st = op_state.borrow();
        let worlds = st.try_borrow::<IsolatedWorlds>()?;
        if let Some(w) = worlds.list.get(world)?.nodes.local(scope, id) {
            return Some(w);
        }
    }
    // A node outside the world's own document keeps its own realm's wrapper (see the module docs): for a page's world,
    // anything of a frame's; for a frame's world, anything not of that frame's realm.
    let frame = world_frame(op_state, world);
    let node_realm = crate::core::html::browsing_context::realm_of_node(scope, id);
    match frame {
        None if node_realm.is_some() => return None,
        Some(f) => {
            let own = crate::core::html::browsing_context::frame_realm(scope, f);
            if node_realm.is_none() || node_realm != own {
                return None;
            }
        }
        None => {}
    }
    let root = match frame {
        Some(f) => crate::core::html::browsing_context::frame_document_root(scope, f)?,
        None => op_state.borrow().borrow::<HostDom>().0.root(),
    };
    let obj = if id == root {
        // the document, as this world's `HTMLDocument` -- with its own `[LegacyUnforgeable]` `location`
        let doc = crate::core::dom::document::JsDocument { root: if frame.is_some() { root } else { crate::core::dom::document::PAGE_DOC_ROOT } };
        let o = crate::binding::protos::make_document_wrapper(scope, doc, "HTMLDocument");
        crate::core::dom::document::install_own_location(scope, o);
        o
    } else {
        let running = scope.get_current_context();
        // The node's interface prototype is the page realm's; the world's own is the same interface made in it. A
        // prototype no interface stamps (a page's custom element class) is passed over, up to the interface it
        // extends: the world has no definition of the page's classes.
        let mut proto = crate::binding::protos::node_proto(scope, op_state, id);
        let mut mine = None;
        for _ in 0..8 {
            let Some(p) = proto else { break };
            let there = crate::binding::protos::proto_in_realm(scope, running, p);
            if there != p {
                mine = Some(there);
                break;
            }
            proto = p.get_prototype(scope).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
        }
        let proto = match mine.or_else(|| crate::binding::protos::shell_proto(scope, "HTMLElement")) {
            Some(p) => p,
            None => return None,
        };
        let o = crate::binding::protos::mint_wrapper_on(scope, proto.into());
        deno_core::cppgc::wrap_object(scope, o, crate::core::dom::element::JsElement { id })
    };
    let g = v8::Global::new(scope, obj);
    let st = op_state.borrow();
    st.try_borrow::<IsolatedWorlds>()?.list.get(world)?.nodes.insert(id, g);
    Some(obj)
}

/// A new isolated world over the page's document; its index. Called with the page's context running.
pub fn create_isolated_world(scope: &mut v8::PinScope) -> usize {
    create_isolated_world_in(scope, None).unwrap_or(0)
}

/// A new isolated world over the document of `frame` (`None`: the page's); its index. `None` when the frame has no
/// document.
pub fn create_isolated_world_in(scope: &mut v8::PinScope, frame: Option<u32>) -> Option<usize> {
    let frame_root = match frame {
        Some(f) => Some(crate::core::html::browsing_context::frame_document_root(scope, f)?),
        None => None,
    };
    let realm = crate::core::html::browsing_context::new_realm(scope, None);
    crate::core::html::browsing_context::install_lazy_frame(scope, realm);
    // THE PAGE'S `SharedArrayBuffer` GATE. V8 gives every context the constructor. Blink asks per context
    // (`SetSharedArrayBufferConstructorEnabledCallback`) of the context's execution context, which for a world is
    // the page's window, so a world of a page that is not cross-origin isolated has none. A frame's realm drops it
    // in `furnish_window`; a world, which is not furnished, kept it (the warm isolated-world sweep's one diff).
    let isolated = crate::core::html::isolation_policy::cross_origin_isolated(scope);
    if !isolated {
        let scope = &mut v8::ContextScope::new(scope, realm);
        let global = realm.global(scope);
        if let Some(k) = v8::String::new(scope, "SharedArrayBuffer") {
            if global.has_own_property(scope, k.into()).unwrap_or(false) {
                global.delete(scope, k.into());
            }
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // the page's `Location` (see the module docs), read while the page's context runs -- or the frame's
    let location = match frame {
        Some(f) => crate::core::html::browsing_context::frame_location(scope, f).map(|l| v8::Global::new(scope, v8::Local::<v8::Value>::from(l))),
        None => crate::core::html::window_globals::window_slot_get(scope, "location").map(|l| v8::Global::new(scope, l)),
    };
    let index = {
        let mut st = op_state.borrow_mut();
        if !st.has::<IsolatedWorlds>() {
            st.put(IsolatedWorlds::default());
        }
        let worlds = st.borrow_mut::<IsolatedWorlds>();
        worlds.list.push(World { context: v8::Global::new(scope, realm), nodes: NodeCache::default(), frame });
        worlds.list.len() - 1
    };
    ANY.with(|a| a.set(true));
    let scope = &mut v8::ContextScope::new(scope, realm);
    // The world's own `console`, built by the install the page's and every frame's is, over the no-op one V8 gives a new
    // context: a world's `console.*` reaches the client as the world's (`Runtime.consoleAPICalled` in its context --
    // Playwright's `setContent` waits for the `console.debug` it writes from its utility world).
    let global = realm.global(scope);
    crate::modules::console_ns::install_console(scope, global, true);
    // The world's `document` and `location` are its window's singletons, as a frame's are.
    if let Some(doc) = {
        let root = frame_root.unwrap_or_else(|| op_state.borrow().borrow::<HostDom>().0.root());
        wrap(scope, &op_state, index, root)
    } {
        crate::core::html::window_globals::window_slot_set(scope, "document", doc.into());
    }
    if let Some(loc) = location {
        let loc = v8::Local::new(scope, &loc);
        crate::core::html::window_globals::window_slot_set(scope, "location", loc);
    }
    Some(index)
}

/// Where the DevTools realm indices of frames start: [`devtools_realm_context`] reads an index below it as an isolated
/// world's and one at or above it as frame `index - FRAME_REALM_BASE`'s own realm.
pub const FRAME_REALM_BASE: usize = 1 << 30;

/// The context a DevTools realm index names: an isolated world's (a page's or a frame's), or a frame's own realm.
pub fn devtools_realm_context<'s>(scope: &mut v8::PinScope<'s, '_>, index: usize) -> Option<v8::Local<'s, v8::Context>> {
    if index >= FRAME_REALM_BASE {
        return crate::core::html::browsing_context::frame_realm(scope, (index - FRAME_REALM_BASE) as u32);
    }
    isolated_world_context(scope, index)
}

/// The DevTools realm index of context `ctx`: an isolated world's index, or [`FRAME_REALM_BASE`] plus the frame's id
/// for a frame's realm; `None` for the page's own realm (and anything else).
pub fn devtools_realm_of(scope: &mut v8::PinScope, ctx: v8::Local<v8::Context>) -> Option<usize> {
    if let Some(w) = isolated_world_of(scope, ctx) {
        return Some(w);
    }
    crate::core::html::browsing_context::frame_of_realm(scope, ctx).map(|id| FRAME_REALM_BASE + id as usize)
}

/// A new document: every world's wrappers were of the last one's nodes, whose ids the new arena hands out again.
pub(crate) fn forget_nodes(state: &OpState) {
    if let Some(w) = state.try_borrow::<IsolatedWorlds>() {
        for world in &w.list {
            world.nodes.clear();
        }
    }
}

/// World `index`'s context.
pub fn isolated_world_context<'s>(scope: &mut v8::PinScope<'s, '_>, index: usize) -> Option<v8::Local<'s, v8::Context>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let w = st.try_borrow::<IsolatedWorlds>()?.list.get(index)?;
    Some(v8::Local::new(scope, &w.context))
}

/// Is `v` an isolated world's global -- its `window`? `Some(frame)`, the browsing context whose window it stands for
/// (`None` the page). A listener a world adds there listens at that window, as
/// Blink keeps one event target per window whatever world registers on it (Playwright's drag detection listens for
/// the page's `mousemove` and `dragstart` from its utility world).
pub(crate) fn world_window(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Option<u32>> {
    if !ANY.with(Cell::get) || !v.is_object() {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let worlds = st.try_borrow::<IsolatedWorlds>()?;
    worlds
        .list
        .iter()
        .find(|w| {
            let g: v8::Local<v8::Value> = v8::Local::new(scope, &w.context).global(scope).into();
            g.strict_equals(v)
        })
        .map(|w| w.frame)
}

/// The world `ctx` is, if it is one.
pub fn isolated_world_of(scope: &mut v8::PinScope, ctx: v8::Local<v8::Context>) -> Option<usize> {
    // no world was ever made on this thread: nothing is one (every listener call asks)
    if !ANY.with(Cell::get) {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let worlds = st.try_borrow::<IsolatedWorlds>()?;
    worlds.list.iter().position(|w| v8::Local::new(scope, &w.context) == ctx)
}

/// The page's own object for the node a world's object `o` stands for -- where the node's internal state lives (a
/// text control's selection, an input's files), which Blink keeps on the element itself, so every world reads and
/// writes the same state. `o` itself when no world is running, or when `o` is no node's.
pub(crate) fn page_object<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'_, v8::Object>) -> v8::Local<'s, v8::Object> {
    let o = v8::Local::new(scope, o);
    if !ANY.with(Cell::get) {
        return o;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if current(scope, &op_state).is_none() {
        return o;
    }
    let Some(id) = crate::binding::protos::unwrap_id(scope, o.into()) else { return o };
    let main = crate::core::html::browsing_context::main_context(scope);
    let scope = &mut v8::ContextScope::new(scope, main);
    crate::binding::protos::wrap_cached(scope, &op_state, id)
}

/// An event a WORLD dispatches runs as the page's (see the module docs): `Some((the page's context, the target as the
/// page's object))` when the running context is a world, for `et_dispatch` to dispatch again under.
pub(crate) fn dispatch_realm<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Value>) -> Option<(v8::Local<'s, v8::Context>, v8::Local<'s, v8::Value>)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let world = current(scope, &op_state)?;
    // a frame's world dispatches as its frame
    let main = match world_frame(&op_state, world) {
        Some(f) => crate::core::html::browsing_context::frame_realm(scope, f)?,
        None => crate::core::html::browsing_context::main_context(scope),
    };
    let world_global: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    let target = if target.strict_equals(world_global) {
        main.global(scope).into()
    } else if crate::core::dom::shared::document_node_id(scope, target) == Some(crate::core::events::shared::ON_DOCUMENT_ID) {
        let scope = &mut v8::ContextScope::new(scope, main);
        crate::binding::protos::document_val(scope, &op_state).unwrap_or(target)
    } else if let Some(id) = crate::binding::protos::unwrap_id(scope, target) {
        let scope = &mut v8::ContextScope::new(scope, main);
        crate::binding::protos::wrap_cached(scope, &op_state, id).into()
    } else {
        target
    };
    Some((main, target))
}
