//! `core/html/popup.rs` -- a popup as a page target of its own (D74 step 3).
//!
//! Design: `docs/superpowers/specs/2026-10-23-korvus-popup-targets.md`. Under the browser endpoint, `window.open` of a
//! URL cross-site to its opener -- or with `noopener` -- does not load the document in the opener's process: the
//! endpoint opens a page target for it, a process of its own in the opener's browser context, as Chrome's site
//! isolation does, and reports it with `openerId`, so a client sees it, attaches to it and drives it like any page
//! (a front end shows it as a window; Playwright's `page.on('popup')` fires). The two windows reach each other as
//! cross-site windows always do -- a cross-origin WindowProxy, `postMessage`, `closed`, `close()`, `location` -- and
//! what crosses goes on the lines the endpoint reads from a page process's stdout and writes to its stdin.
//!
//! TWO SIDES, one file:
//! - the OPENER keeps the popup's auxiliary browsing context -- its name, its WindowProxy, `closed` -- marked remote
//!   (`Ctx::remote`, as a cross-site frame's is), so the page sees the cross-origin view and its `postMessage`,
//!   `close()` and `location` reach the popup ([`open`], [`post`], [`navigate`], [`close`]);
//! - the POPUP's `opener` is a cross-origin WindowProxy onto the opener (`remote_frame::opener_proxy`), its
//!   `postMessage` reaches the opener ([`post_to_opener`]), and its `window.close()` closes it ([`close_self`]).
//!
//! The korvus binary installs the [`PopupBridge`] in a page process the endpoint started; without one (`korvus fetch`,
//! the single-page server) every popup stays in its opener's process, as before.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use deno_core::{op2, OpState};
use tokio::sync::{mpsc, Mutex as TMutex};

/// What carries a popup's lines to and from the browser endpoint: the korvus binary's, over a page process's stdout
/// (and its lifeline thread, which hands the endpoint's answers to [`popup_message`], [`popup_closed`],
/// [`opener_message`] and [`opener_gone`]).
pub trait PopupBridge: Send + Sync {
    /// Open popup `rid` at `url` as a target in this page's browser context, its referrer `referrer`; `name` is the
    /// window's name; `noopener`: no link between the two.
    fn open(&self, rid: u64, url: &str, name: &str, noopener: bool, referrer: &str);
    /// The opener's `postMessage` to popup `rid`: structured-clone bytes and the sender's origin.
    fn post(&self, rid: u64, data: &[u8], origin: &str);
    /// The opener set popup `rid`'s `location`.
    fn navigate(&self, rid: u64, url: &str);
    /// The opener's `close()` on popup `rid`.
    fn close(&self, rid: u64);
    /// The popup's `opener.postMessage`.
    fn post_to_opener(&self, data: &[u8], origin: &str);
    /// The popup's own `window.close()`: a window a script opened may close itself.
    fn close_self(&self);
}

static BRIDGE: OnceLock<Arc<dyn PopupBridge>> = OnceLock::new();

/// Install the bridge: this page process runs under the browser endpoint, which opens popups as targets.
pub fn set_bridge(bridge: Arc<dyn PopupBridge>) {
    let _ = BRIDGE.set(bridge);
}

/// Can a popup open as a target of its own here?
pub(crate) fn available() -> bool {
    BRIDGE.get().is_some()
}

// ── the opener's side ───────────────────────────────────────────────────────────────────────────────

/// What the endpoint tells an opener about its popup.
enum Event {
    Message { data: Vec<u8>, origin: String },
    Closed,
}

struct Link {
    tx: mpsc::UnboundedSender<Event>,
    rx: Arc<TMutex<mpsc::UnboundedReceiver<Event>>>,
}

fn links() -> &'static Mutex<HashMap<u64, Link>> {
    static L: OnceLock<Mutex<HashMap<u64, Link>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Popup ids are the process's, not a document's: a popup outlives the document that opened it.
static NEXT_RID: AtomicU64 = AtomicU64::new(1);

/// Open a popup at `url` as a target of its own: its id, or `None` where there is no endpoint to open one.
pub(crate) fn open(url: &str, name: &str, noopener: bool, referrer: &str) -> Option<u64> {
    let bridge = BRIDGE.get()?;
    let rid = NEXT_RID.fetch_add(1, Ordering::Relaxed);
    if !noopener {
        let (tx, rx) = mpsc::unbounded_channel();
        links().lock().unwrap_or_else(|e| e.into_inner()).insert(rid, Link { tx, rx: Arc::new(TMutex::new(rx)) });
    }
    bridge.open(rid, url, name, noopener, referrer);
    Some(rid)
}

/// The opener's `postMessage` to popup `rid`.
pub(crate) fn post(rid: u64, data: &[u8], origin: &str) {
    if let Some(b) = BRIDGE.get() {
        b.post(rid, data, origin);
    }
}

/// The opener set popup `rid`'s `location` to `url`.
pub(crate) fn navigate(rid: u64, url: &str) {
    if let Some(b) = BRIDGE.get() {
        b.navigate(rid, url);
    }
}

/// The opener's `close()` on popup `rid`. Its `closed` turns true at once (Chrome's is the window's "is closing"
/// flag); the endpoint's `closed` follows when the target is gone.
pub(crate) fn close(rid: u64) {
    if let Some(b) = BRIDGE.get() {
        b.close(rid);
    }
}

/// The endpoint relays popup `rid`'s `opener.postMessage` (the lifeline thread).
pub fn popup_message(rid: u64, data: Vec<u8>, origin: String) {
    if let Some(l) = links().lock().unwrap_or_else(|e| e.into_inner()).get(&rid) {
        let _ = l.tx.send(Event::Message { data, origin });
    }
}

/// The endpoint says popup `rid`'s target is gone (the lifeline thread).
pub fn popup_closed(rid: u64) {
    if let Some(l) = links().lock().unwrap_or_else(|e| e.into_inner()).remove(&rid) {
        let _ = l.tx.send(Event::Closed);
    }
}

/// The next word on popup `rid`: `{kind: "message", data, origin}`, `{kind: "closed"}`, or `null` once nothing more
/// can come. REF'd, as a remote frame's receive is: the opener's loop stays alive while its popup can still talk to it.
#[op2(async(lazy), fast)]
#[serde]
pub(crate) async fn op_popup_recv(_state: std::rc::Rc<std::cell::RefCell<OpState>>, rid: f64) -> Option<serde_json::Value> {
    let rx = links().lock().unwrap_or_else(|e| e.into_inner()).get(&(rid as u64)).map(|l| l.rx.clone())?;
    let ev = rx.lock().await.recv().await?;
    Some(match ev {
        Event::Message { data, origin } => serde_json::json!({"kind": "message", "data": data, "origin": origin}),
        Event::Closed => serde_json::json!({"kind": "closed"}),
    })
}

// ── the popup's side ────────────────────────────────────────────────────────────────────────────────

struct Opened {
    /// The opener's origin; `None` when it opened this window with `noopener` (no link, no `opener`).
    opener_origin: Option<String>,
    tx: mpsc::UnboundedSender<(Vec<u8>, String)>,
    rx: Arc<TMutex<mpsc::UnboundedReceiver<(Vec<u8>, String)>>>,
    opener_gone: AtomicBool,
}

static OPENED: OnceLock<Opened> = OnceLock::new();

/// This page process's window was opened by a script, as a popup: its opener's origin, or `None` under `noopener`.
/// Set once, at the process's start, from what the endpoint started it with.
pub fn set_opened_by_script(opener_origin: Option<String>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let _ = OPENED.set(Opened { opener_origin, tx, rx: Arc::new(TMutex::new(rx)), opener_gone: AtomicBool::new(false) });
}

/// Did a script open this window? Such a window may close itself.
pub fn opened_by_script() -> bool {
    OPENED.get().is_some()
}

/// The opener's origin, when this window has an opener in another process.
pub(crate) fn opener_origin() -> Option<String> {
    OPENED.get().and_then(|o| o.opener_origin.clone())
}

/// Is the opener's window gone?
pub(crate) fn opener_closed() -> bool {
    OPENED.get().is_some_and(|o| o.opener_gone.load(Ordering::Relaxed))
}

/// This window's `opener.postMessage`.
pub(crate) fn post_to_opener(data: &[u8], origin: &str) {
    if opener_closed() {
        return;
    }
    if let Some(b) = BRIDGE.get() {
        b.post_to_opener(data, origin);
    }
}

/// This window's own `window.close()`.
pub(crate) fn close_self() {
    if let Some(b) = BRIDGE.get() {
        b.close_self();
    }
}

/// The endpoint relays the opener's `postMessage` (the lifeline thread).
pub fn opener_message(data: Vec<u8>, origin: String) {
    if let Some(o) = OPENED.get() {
        let _ = o.tx.send((data, origin));
    }
}

/// The endpoint says the opener's target is gone (the lifeline thread): `opener.closed` turns true.
pub fn opener_gone() {
    if let Some(o) = OPENED.get() {
        o.opener_gone.store(true, Ordering::Relaxed);
    }
}

/// The opener's next message: `{data, origin}`. REF'd: a popup's loop stays alive while its opener can still post.
#[op2(async(lazy), fast)]
#[serde]
pub(crate) async fn op_popup_opener_recv(_state: std::rc::Rc<std::cell::RefCell<OpState>>) -> Option<serde_json::Value> {
    let rx = OPENED.get().filter(|o| o.opener_origin.is_some()).map(|o| o.rx.clone())?;
    let (data, origin) = rx.lock().await.recv().await?;
    Some(serde_json::json!({"data": data, "origin": origin}))
}

crate::webapi_ops![op_popup_recv, op_popup_opener_recv];
