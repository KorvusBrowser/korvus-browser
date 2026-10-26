//! The identifiers this control channel puts on the wire — in the shapes real Chrome uses.
//!
//! # The defect this replaces (goal item 16)
//!
//! Every id the CDP server answered was a hardcoded string naming the engine: `TARGET_ID =
//! "korvus-page-1"`, `SESSION_ID = "korvus-session-1"`, `browserContextId: "korvus-ctx"`, execution
//! contexts `"korvus-ctx-1/-2/-3"`, loaders `"korvus-loader-0/-1"`, injected scripts
//! `"korvus-script-N"`, and a debugger URL ending `/devtools/browser/korvus`. An automation harness
//! attaches over CDP exactly as Playwright does and reads the engine's name off the first message.
//! The page realm reports **0** automation tells; the control channel is the same surface.
//!
//! # The shapes are MEASURED, not guessed
//!
//! `google-chrome --headless=new --remote-debugging-port=0`, two full runs, 2026-09-12,
//! **Chrome 150.0.7871.46**. `/json/version` + `/json/list` read over HTTP, then a session driven
//! (`Target.getTargets` → `attachToTarget` → `Page.enable`/`Runtime.enable` → `getFrameTree` →
//! `navigate` ×2 → `createIsolatedWorld` → `addScriptToEvaluateOnNewDocument` ×2 → `evaluate`):
//!
//! | identifier | measured shape | example | stability |
//! |---|---|---|---|
//! | `targetId` | 32 UPPERCASE hex, no dashes (128 bits) | `6B31D72110BDF38C09533E8A21DF2644` | fresh per run; one per target, stable for the target's life |
//! | `sessionId` | 32 UPPERCASE hex, no dashes | `A20A75892F06461310AFFFBFC293A4B4` | fresh per **attach** — two attaches to the SAME target got two different ids in one run |
//! | `frameId` | 32 UPPERCASE hex, no dashes | `6B31D72110BDF38C09533E8A21DF2644` | **equal to `targetId`**, in both runs |
//! | `browserContextId` | 32 UPPERCASE hex, no dashes | `E770821AB82AB7D7A7DFD6C79AB03371` | fresh per browser; the same id on every target of that browser |
//! | `loaderId` | 32 UPPERCASE hex, no dashes | `6FF4401E6253C2CBF62CF88E35B344DC` | **fresh per navigation**; the same id in that navigation's `frameNavigated` and every `lifecycleEvent` |
//! | execution-context `uniqueId` | two signed 64-bit **decimals** joined by `.` — not hex, not a UUID, either half may be negative | `-6739544306189926202.7689520667313427499` | fresh per context |
//! | browser debugger URL | `/devtools/browser/<lowercase RFC-4122 v4 UUID>` | `f6801a86-f60d-4110-955e-d32275711a74` | fresh per browser process |
//! | page debugger URL | `/devtools/page/<targetId>` | — | follows the target |
//! | `addScriptToEvaluateOnNewDocument` identifier | a plain decimal counter, from `"1"` | `"1"`, `"2"` | per session |
//! | `Runtime` `objectId` | `<i64>.<contextId>.<serial>` | `248396911191720.3.1`, `-4283478743682658141.3.1` | per object |
//!
//! Nothing repeated across the two runs, and nothing but `frameId`/`targetId` repeated within one.
//!
//! **They are deliberately not all the same shape.** A uniform "make everything a UUID" would be a
//! worse tell than the strings it replaced, because a wrong shape that looks random looks
//! *deliberate*. `frameId == targetId` is kept because Chrome does it, not because this engine did.
//!
//! `createIsolatedWorld` with no `worldName` was measured too: Chrome announces the context with
//! `"name": ""`, which is why the fallback here is empty rather than a made-up world name.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::Write as _;

/// `N` bytes from the OS CSPRNG.
///
/// This panics rather than falling back, and that is the point: a zeroed identifier
/// (`00000000000000000000000000000000`) is a far louder tell than any hardcoded string, so an engine
/// that cannot get entropy must not answer at all.
fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("OS entropy for a CDP identifier");
    b
}

fn random_i64() -> i64 {
    i64::from_le_bytes(random_bytes::<8>())
}

/// 32 UPPERCASE hex characters, no dashes — Chrome's `targetId`, `sessionId`, `frameId`,
/// `browserContextId` and `loaderId` are all exactly this, 128 bits wide.
pub fn hex32() -> String {
    let mut s = String::with_capacity(32);
    for x in random_bytes::<16>() {
        let _ = write!(s, "{x:02X}");
    }
    s
}

/// A lowercase RFC-4122 version-4 UUID — used for the `/devtools/browser/<id>` path and **nothing
/// else**, because that is the one place Chrome uses this shape.
pub fn browser_uuid() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC-4122 variant
    let hex = |r: &[u8]| r.iter().fold(String::new(), |mut s, x| { let _ = write!(s, "{x:02x}"); s });
    format!("{}-{}-{}-{}-{}", hex(&b[0..4]), hex(&b[4..6]), hex(&b[6..8]), hex(&b[8..10]), hex(&b[10..16]))
}

/// An execution context's `uniqueId`: `<i64>.<i64>`. V8 builds it from the isolate's id and the
/// context's own, so it is decimal and either half may carry a `-`.
pub fn context_unique_id() -> String {
    format!("{}.{}", random_i64(), random_i64())
}

/// What is fixed for the life of the **server process** — the browser's own identity.
///
/// Chrome keeps these stable across connections: `/json/list` advertises a `targetId` that a client
/// then attaches to, so a per-connection target id would break the discovery→attach handoff that
/// Playwright performs.
#[derive(Clone)]
pub struct BrowserIds {
    /// The UUID in `/devtools/browser/<uuid>`.
    pub debugger_uuid: String,
    pub browser_context_id: String,
    /// This server drives a single page target, so it has exactly one.
    pub target_id: String,
}

impl BrowserIds {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // A page target `korvus browser` started is TOLD its ids -- the router has already given them to its
        // client (`router.rs`). Every other server draws its own.
        let given = crate::router::child_ids().filter(|(t, c)| !t.is_empty() && !c.is_empty());
        BrowserIds {
            debugger_uuid: browser_uuid(),
            browser_context_id: given.map(|(_, c)| c.clone()).unwrap_or_else(hex32),
            target_id: given.map(|(t, _)| t.clone()).unwrap_or_else(hex32),
        }
    }
}

/// What is fresh for each **connection**: the session, the V8 isolate the object ids are scoped to,
/// the current document's loader, and the execution contexts announced so far.
/// One execution context besides the main document's own.
#[derive(Debug, Clone)]
pub struct LiveCtx {
    pub id: i64,
    /// The world's name; `""` for a frame's own realm.
    pub name: String,
    /// The engine's realm index for it (an isolated world's, or a frame realm's -- `webapi::FRAME_REALM_BASE` up).
    pub realm: usize,
    pub origin: String,
    /// The frame it belongs to.
    pub frame: String,
    /// A frame's own realm (`auxData.isDefault`), not a world.
    pub default: bool,
}

/// One frame (an `<iframe>`'s browsing context) a client is told of: Chrome's `frameId` -- kept for the frame's life,
/// across its navigations -- its parent's, and its current document's loader, URL and lifecycle so far.
#[derive(Debug, Clone)]
pub struct FrameRec {
    /// The engine's context id.
    pub bc: u32,
    pub id: String,
    pub parent: String,
    /// The current document's arena root (`u32::MAX` before one is announced).
    pub document: u32,
    pub loader: String,
    pub url: String,
    pub name: String,
    pub dcl_sent: bool,
    pub load_sent: bool,
}

/// A popup this page's process shows (a same-site `window.open`), served to clients as a target of its own: its
/// target id (which is its frame id, as a page's is), its current document's loader, URL and lifecycle so far, and
/// whether the router has been told of it.
#[derive(Debug, Clone)]
pub struct PopupRec {
    pub bc: u32,
    pub target: String,
    pub document: u32,
    pub loader: String,
    pub url: String,
    pub name: String,
    pub dcl_sent: bool,
    pub load_sent: bool,
}

pub struct ConnIds {
    browser: BrowserIds,
    session_id: String,
    /// The first component of every `objectId`; Chrome's is the V8 isolate id.
    isolate_id: i64,
    /// The loader of the document currently in the frame. Replaced on every navigation, exactly as
    /// Chrome replaces it — `Page.navigate`'s result, its `frameNavigated` and its `lifecycleEvent`s
    /// must all carry this one value or a client cannot tell which navigation an event belongs to.
    loader_id: String,
    /// The execution context `Runtime.evaluate` resolves against; the second component of an
    /// `objectId`. 1 for the initial document; each navigation's is the next id (see `next_context`).
    default_context: i64,
    /// The next `executionContextId`. Chrome numbers every context it announces from one counter --
    /// the initial document 1, a world made on it 2, the next document 3 and its worlds 4, 5, a world made then 6,
    /// the document after 7 (three runs alike) -- where this server said 3 for every
    /// navigated document and 2 for every world.
    next_context: i64,
    /// `executionContextId` → its `uniqueId`. Memoised so that re-announcing a context (a second client is
    /// announced the live one) cannot hand out two different unique ids for one context.
    context_unique: HashMap<i64, String>,
    /// The frame's isolated worlds made by `Page.createIsolatedWorld`, in the order they were made: each name and the
    /// origin it was made under, which its context reports in every document after (Chrome keeps a world
    /// made on `about:blank` at `://`).
    created_worlds: Vec<(String, String)>,
    /// Every context a client is told of besides the main document's own and not told destroyed: the main document's
    /// isolated worlds, and each frame's own realm and its worlds ([`LiveCtx`]).
    live: Vec<LiveCtx>,
    /// The page's frames a client is told of ([`FrameRec`]), parents before children.
    pub frames: Vec<FrameRec>,
    /// The popups this process serves as targets of their own ([`PopupRec`]).
    pub popups: Vec<PopupRec>,
    /// A navigation has committed a document in this target: the frame shows more than the initial empty document,
    /// whose lifecycle Chrome does not report again (`Page.setLifecycleEventsEnabled`).
    pub committed: bool,
    /// This session has sent `Page.enable`: a second one is answered with nothing more (Chrome announces the frame to a
    /// session once; Playwright enables the domain again on a page it already set up, and a re-announced
    /// `frameNavigated` reads to it as a navigation that detaches every handle it holds).
    pub page_enabled: bool,
    /// `Runtime.ExceptionDetails.exceptionId`: a plain counter from 1, per session. Measured over
    /// one Chrome session driven through fourteen throwing evaluates — 1, 2, 3, … with no reuse and
    /// no relation to any other id on the wire.
    next_exception_id: i64,
    /// The first component of a subresource's `Network.requestId` (`<prefix>.<n>`), whose second
    /// component is the ledger row's id.
    ///
    /// IT IS BUMPED BY EVERY NAVIGATION, and that is load-bearing rather than tidy. The ledger's own
    /// counter restarts at 1 for each navigation (a generation is one navigation's requests), so a
    /// prefix fixed for the connection would hand the second navigation's first request the same
    /// `requestId` as the first navigation's — and a client that joins its three `Network.*` events
    /// by id would staple two different requests together. Chrome never reuses a requestId within a
    /// renderer; neither does this.
    network_prefix: i64,
}

impl ConnIds {
    pub fn new(browser: &BrowserIds) -> Self {
        ConnIds {
            browser: browser.clone(),
            session_id: hex32(),
            isolate_id: random_i64(),
            loader_id: hex32(),
            default_context: 1,
            next_context: 2,
            context_unique: HashMap::new(),
            created_worlds: Vec::new(),
            live: Vec::new(),
            frames: Vec::new(),
            popups: Vec::new(),
            committed: false,
            page_enabled: false,
            next_exception_id: 1,
            // Chrome's is a renderer pid; the shape (a positive decimal) is what a client parses, so
            // this is a random one in the same range rather than this process's own pid — which
            // would put the host's pid on a wire that is supposed to be a browser's.
            network_prefix: 10_000 + (random_i64().unsigned_abs() % 90_000) as i64,
        }
    }

    /// The current navigation's `Network.requestId` prefix. See [`ConnIds::network_prefix`].
    pub fn network_prefix(&self) -> i64 {
        self.network_prefix
    }

    /// The next `exceptionId`. Chrome's is a per-session counter from 1.
    pub fn next_exception_id(&mut self) -> i64 {
        let id = self.next_exception_id;
        self.next_exception_id += 1;
        id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The execution context a client attaching NOW must be told about — the one the document
    /// currently in this target is running in: `1` before any navigation, then each navigation's own
    /// id (`next_context`); a client is announced this, never a fixed `1`.
    pub fn default_context(&self) -> i64 {
        self.default_context
    }

    /// A SECOND CLIENT ATTACHES TO A TARGET THAT ALREADY EXISTS.
    ///
    /// Measured on Chrome 150.0.7871.46, one page target, a first client that navigates and then
    /// detaches and a second that attaches after it: the second client is announced **the same
    /// execution context** — same `id`, same `uniqueId`, same `origin` — and `Page.getFrameTree`
    /// still answers the navigated url. What is fresh per ATTACH is the `sessionId` (measured:
    /// "two attaches to the SAME target got two different ids in one run", this file's header) and
    /// the per-session `exceptionId` counter. Everything else belongs to the TARGET and survives.
    ///
    /// So this is the whole delta between a first attach and a later one, and everything it does
    /// NOT touch — the loader, the default context, the memoised `uniqueId`s, the isolate id, the
    /// request-id prefix — is the part that must carry over, which is what this method exists to
    /// make visible.
    pub fn reattach(&mut self) {
        self.session_id = hex32();
        self.next_exception_id = 1;
        self.page_enabled = false;
    }
    pub fn target_id(&self) -> &str {
        &self.browser.target_id
    }
    /// Chrome sets a page's main-frame id EQUAL to its target id — measured in both runs, and
    /// Playwright depends on it (`frame(this._targetId)`), so the equality is correct rather than a
    /// leftover.
    pub fn frame_id(&self) -> &str {
        &self.browser.target_id
    }
    pub fn browser_context_id(&self) -> &str {
        &self.browser.browser_context_id
    }
    pub fn loader_id(&self) -> &str {
        &self.loader_id
    }

    /// Start a new document: mint its loader id and the execution context that replaces the old one.
    pub fn begin_navigation(&mut self) -> String {
        self.loader_id = hex32();
        // the new document's default context, and none of the old one's worlds or frames' contexts
        self.default_context = self.mint_context();
        self.live.clear();
        // The request-id prefix moves with the navigation, because the ledger's row ids restart with
        // it (see `network_prefix`).
        self.network_prefix += 1;
        self.loader_id.clone()
    }

    /// This context's `uniqueId`, minted once and then remembered.
    pub fn context_unique(&mut self, context_id: i64) -> String {
        self.context_unique.entry(context_id).or_insert_with(context_unique_id).clone()
    }

    /// A `Runtime.RemoteObject` id of an object of context `context` (the document's default one or an isolated
    /// world's): `<isolate>.<context>.<serial>`, Chrome's shape.
    pub fn object_id_in(&self, context: i64, serial: u32) -> String {
        format!("{}.{}.{}", self.isolate_id, context, serial)
    }

    /// The next execution context's id (see `next_context`).
    pub fn mint_context(&mut self) -> i64 {
        let id = self.next_context;
        self.next_context += 1;
        id
    }

    /// The current document's context of isolated world `name`, if it has one.
    pub fn live_world(&self, name: &str) -> Option<i64> {
        let main = self.frame_id().to_string();
        self.live.iter().find(|c| !c.default && c.name == name && c.frame == main).map(|c| c.id)
    }

    /// A frame's world named `name`, if its current document has one.
    pub fn live_frame_world(&self, frame: &str, name: &str) -> Option<i64> {
        self.live.iter().find(|c| !c.default && c.name == name && c.frame == frame).map(|c| c.id)
    }

    /// The engine realm index of context `id` (an isolated world's, or a frame's own realm).
    pub fn world_index(&self, id: i64) -> Option<usize> {
        self.live.iter().find(|c| c.id == id).map(|c| c.realm)
    }

    /// The context of engine realm index `index`.
    pub fn world_context(&self, index: usize) -> Option<i64> {
        self.live.iter().find(|c| c.realm == index).map(|c| c.id)
    }

    /// The main document's isolated world `name`, context `id`, engine index `index`.
    pub fn add_live_world(&mut self, id: i64, name: &str, index: usize, origin: &str) {
        let frame = self.frame_id().to_string();
        self.live.push(LiveCtx { id, name: name.to_string(), realm: index, origin: origin.to_string(), frame, default: false });
    }

    /// A frame's context.
    pub fn add_live(&mut self, c: LiveCtx) {
        self.live.push(c);
    }

    /// The main document's isolated worlds: context id, name, origin.
    pub fn live_worlds(&self) -> Vec<(i64, String, String)> {
        let main = self.frame_id().to_string();
        self.live.iter().filter(|c| c.frame == main).map(|c| (c.id, c.name.clone(), c.origin.clone())).collect()
    }

    /// Every context of the frames (their own realms and worlds), in the order they were announced.
    pub fn frame_contexts(&self) -> Vec<LiveCtx> {
        let main = self.frame_id().to_string();
        self.live.iter().filter(|c| c.frame != main).cloned().collect()
    }

    /// Frame `frame`'s contexts, taken off the live list (its document went): each id.
    pub fn drop_frame_contexts(&mut self, frame: &str) -> Vec<i64> {
        let gone: Vec<i64> = self.live.iter().filter(|c| c.frame == frame).map(|c| c.id).collect();
        self.live.retain(|c| c.frame != frame);
        gone
    }

    /// The frame id of engine context `bc`.
    pub fn frame_for(&self, bc: u32) -> Option<&FrameRec> {
        self.frames.iter().find(|f| f.bc == bc)
    }

    /// The engine context of frame id `frame`.
    pub fn bc_for(&self, frame: &str) -> Option<u32> {
        self.frames.iter().find(|f| f.id == frame).map(|f| f.bc)
    }

    pub fn remember_created_world(&mut self, name: &str, origin: &str) {
        if !self.created_worlds.iter().any(|(n, _)| n == name) {
            self.created_worlds.push((name.to_string(), origin.to_string()));
        }
    }

    /// The worlds `Page.createIsolatedWorld` made in this frame, with their origins, in the order they were made.
    pub fn created_worlds(&self) -> &[(String, String)] {
        &self.created_worlds
    }

    /// The live world whose `uniqueId` is `unique` (`Runtime.evaluate`'s `uniqueContextId`), or the default context.
    pub fn context_by_unique(&self, unique: &str) -> Option<i64> {
        self.context_unique.iter().find(|(_, u)| u.as_str() == unique).map(|(id, _)| *id)
    }

    /// The `Target.TargetInfo` for this server's one page target.
    pub fn target_info(&self) -> Value {
        json!({
            "targetId": self.target_id(), "type": "page", "title": "", "url": "about:blank",
            "attached": true, "canAccessOpener": false,
            "browserContextId": self.browser_context_id()
        })
    }
}

/// The serial out of a `<isolate>.<context>.<serial>` object id. Shape-checked: three dot-separated
/// components, the last a number. Anything else is not an id this server issued.
pub fn parse_object_id(s: &str) -> Option<u32> {
    let mut parts = s.split('.');
    let (_isolate, _context, serial) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    serial.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex32_is_chromes_shape() {
        for _ in 0..64 {
            let s = hex32();
            assert_eq!(s.len(), 32, "Chrome's targetId/sessionId/loaderId are 32 chars: {s}");
            assert!(
                s.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)),
                "uppercase hex only, no dashes: {s}"
            );
        }
        assert_ne!(hex32(), hex32(), "two ids must not be the same id");
    }

    #[test]
    fn browser_uuid_is_a_lowercase_v4_uuid() {
        let u = browser_uuid();
        let f: Vec<&str> = u.split('-').collect();
        assert_eq!(f.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12], "{u}");
        assert!(u.bytes().all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{u}");
        assert_eq!(&f[2][..1], "4", "version nibble: {u}");
        assert!("89ab".contains(&f[3][..1]), "RFC-4122 variant nibble: {u}");
    }

    #[test]
    fn context_unique_id_is_two_decimal_i64s() {
        for _ in 0..64 {
            let s = context_unique_id();
            let p: Vec<&str> = s.split('.').collect();
            assert_eq!(p.len(), 2, "{s}");
            for half in p {
                half.parse::<i64>().unwrap_or_else(|_| panic!("not an i64: {s}"));
            }
        }
    }

    #[test]
    fn object_ids_round_trip_and_reject_foreign_shapes() {
        let ids = ConnIds::new(&BrowserIds::new());
        for serial in [0u32, 1, 7, u32::MAX] {
            assert_eq!(parse_object_id(&ids.object_id_in(ids.default_context(), serial)), Some(serial));
        }
        // The id this engine used to issue is not accepted back as one it issued.
        assert_eq!(parse_object_id("obj-3"), None);
        assert_eq!(parse_object_id("1.2"), None);
        assert_eq!(parse_object_id("1.2.3.4"), None);
        assert_eq!(parse_object_id("1.2.x"), None);
    }

    #[test]
    fn a_frames_id_is_its_targets_id_and_a_session_is_neither() {
        let b = BrowserIds::new();
        let ids = ConnIds::new(&b);
        assert_eq!(ids.frame_id(), ids.target_id(), "Chrome: frameId == targetId (measured)");
        assert_ne!(ids.session_id(), ids.target_id());
        assert_ne!(ids.browser_context_id(), ids.target_id());
        // Two connections to ONE browser share the target and context, not the session.
        let other = ConnIds::new(&b);
        assert_eq!(other.target_id(), ids.target_id(), "/json/list -> attach must resolve");
        assert_eq!(other.browser_context_id(), ids.browser_context_id());
        assert_ne!(other.session_id(), ids.session_id(), "Chrome mints a session per attach");
    }

    #[test]
    fn a_navigation_replaces_the_loader_and_a_context_id_keeps_its_unique_id() {
        let mut ids = ConnIds::new(&BrowserIds::new());
        let first = ids.loader_id().to_string();
        let second = ids.begin_navigation();
        assert_ne!(first, second, "Chrome mints a loaderId per navigation");
        assert_eq!(ids.loader_id(), second, "and the frame then carries the new one");
        let u = ids.context_unique(3);
        assert_eq!(ids.context_unique(3), u, "one context, one uniqueId");
        assert_ne!(ids.context_unique(2), u);
    }

    // Chrome 150's numbering: one counter for every context -- the
    // initial document 1, a world made on it 2, the next document 3, its worlds 4 and 5, the document after 6.
    #[test]
    fn every_context_takes_the_next_id() {
        let mut ids = ConnIds::new(&BrowserIds::new());
        assert_eq!(ids.default_context(), 1);
        let w2 = ids.mint_context();
        ids.add_live_world(w2, "w2", 0, "://");
        assert_eq!((w2, ids.live_world("w2"), ids.world_index(w2)), (2, Some(2), Some(0)));
        ids.begin_navigation();
        assert_eq!(ids.default_context(), 3);
        assert_eq!(ids.live_world("w2"), None, "a navigation leaves the old document's worlds behind");
        assert_eq!((ids.mint_context(), ids.mint_context()), (4, 5));
        ids.begin_navigation();
        assert_eq!(ids.default_context(), 6);
    }

    #[test]
    fn no_identifier_names_the_engine() {
        let b = BrowserIds::new();
        let mut ids = ConnIds::new(&b);
        let mut wire = vec![
            b.debugger_uuid.clone(),
            ids.target_id().to_string(),
            ids.session_id().to_string(),
            ids.frame_id().to_string(),
            ids.browser_context_id().to_string(),
            ids.loader_id().to_string(),
            ids.object_id_in(ids.default_context(), 1),
            ids.target_info().to_string(),
        ];
        wire.push(ids.context_unique(1));
        for s in wire {
            assert!(!s.to_lowercase().contains("korvus"), "identifier names the engine: {s}");
        }
    }
}
