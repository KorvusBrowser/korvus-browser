//! `cdp` — the Chrome DevTools Protocol control server.
//!
//! G1.8: an HTTP+WebSocket JSON-RPC server driving a single-page [`engine::Engine`]. Serves
//! `/json/version` discovery, upgrades WebSocket manually, and implements enough of
//! `Browser`/`Target`/`Runtime`/`Page`/`DOM` — including the flatten session model + execution
//! context events — for Playwright `connectOverCDP` to attach, navigate, and evaluate.
//!
//! The Engine holds a V8 isolate (`!Send`), so a connection is served on one thread without spawning.

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use engine::{navtrace, CdpArg, Engine, EvalOutcome, ExceptionInfo, RemoteResult};

/// `korvus serve --humanize` (goal 1020 step 5): a CDP client's mouse move goes along the behaviour model's path, as
/// a plugin's input slot would take it, when no plugin fills that slot.
static HUMANIZE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Humanize this process's CDP input (its context's, or the run's, setting).
pub fn set_humanize(on: bool) {
    HUMANIZE.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub mod access; // Who may reach an endpoint from a web page: `Origin` and `Host` (D88 step 1).
mod viewer; // The viewer channel: what a person's client may do over a ticket (D88 step 3).
mod client_files; // korvus's own client, served at `/client/` (D88 step 4).
mod ids;
use ids::{BrowserIds, ConnIds};
mod network;
mod fetch; // The `Fetch` domain, served beside the page, and the connection's shared write half.
mod page_eval; // The page-eval primitive (plugin ABI 3, B1): a plugin runs a script on this page's thread.
mod screencast; // `Page.startScreencast`: the page's frames to a DevTools client (D74 step 3).
pub mod router; // The browser endpoint (page targets on demand, contexts) over this single-page server.

/// The CDP version this server speaks. Real Chrome answers `1.3` in both `Browser.getVersion` and
/// `/json/version`, and it is a fact about the PROTOCOL, not about the identity.
const PROTOCOL_VERSION: &str = "1.3";

// Event-loop pump budgets (see Engine::pump). A page that settles returns early; these only bound
// pages whose perpetual timers would otherwise hang the drain forever. `navigate` gets room for
// initial scripts + the load lifecycle; `evaluate`/`callFunctionOn` only need to flush ready
// microtasks before reading state. Override via env for tuning.
const NAV_PUMP_MS_DEFAULT: u64 = 1500;
const EVAL_PUMP_MS_DEFAULT: u64 = 100;
// Max time `Runtime.evaluate`/`callFunctionOn` will pump the event loop waiting for a returned
// promise to settle when the caller passed awaitPromise:true (fetch, async fns, ...).
const AWAIT_PROMISE_MS_DEFAULT: u64 = 15000;
fn env_ms(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// `Network.MonotonicTime`: seconds on a monotonic clock whose zero is arbitrary. Chrome's is the
/// process's own base, and a client only ever takes DIFFERENCES of these, so what matters is that the
/// clock never jumps backwards — which is why it is not derived from the wall clock.
fn monotonic_now_s() -> f64 {
    monotonic_s_at(std::time::Instant::now())
}

/// The same clock, read at an instant the engine recorded EARLIER. A lifecycle event
/// is stamped with the moment it fired, not with the moment the navigation that contains it returned;
/// the engine hands those moments over as `Instant`s (`Engine::lifecycle_marks`) and this is the one
/// place that puts them on `Network.MonotonicTime`.
fn monotonic_s_at(at: std::time::Instant) -> f64 {
    static BASE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    at.saturating_duration_since(*BASE.get_or_init(std::time::Instant::now)).as_secs_f64()
}

/// `Network.TimeSinceEpoch`: seconds since the Unix epoch, for `requestWillBeSent.wallTime`.
fn wall_now_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// A JSON-RPC error in the exact envelope Chrome puts on this wire.
///
/// # The defect this replaces (goal item 22)
///
/// This server's dispatch ended `_ => (Ok(json!({})), vec![])`: **every method it did not implement
/// SUCCEEDED with an empty result.** `Foo.bar` answered `{"result":{}}`. One made-up method name was
/// therefore a complete fingerprint — a harness sends a name nobody has ever implemented and learns
/// it is not talking to Chrome, needing no knowledge of this engine at all. That is a cheaper
/// separator than anything in the page realm.
///
/// # Chrome's error envelopes, measured
///
/// `google-chrome --headless=new --remote-debugging-port=0`, 2026-09-12, **Chrome 150.0.7871.46**,
/// probed over both the browser-level WebSocket and a flattened page session:
///
/// | probe | answer |
/// |---|---|
/// | `Foo.bar` (unknown domain) | `{"code":-32601,"message":"'Foo.bar' wasn't found"}` — **no `data`** |
/// | `Nonsense.thing` | `{"code":-32601,"message":"'Nonsense.thing' wasn't found"}` |
/// | `Page.definitelyNotReal` (known domain, unknown method) | `{"code":-32601,"message":"'Page.definitelyNotReal' wasn't found"}` |
/// | `bareMethodNoDot` (no domain at all) | `{"code":-32601,"message":"'bareMethodNoDot' wasn't found"}` |
/// | `page.enable` (wrong case) | `{"code":-32601,"message":"'page.enable' wasn't found"}` |
/// | `Page.js_error_count` | `{"code":-32601,"message":"'Page.js_error_count' wasn't found"}` |
/// | message with no `method` | `{"code":-32600,"message":"Message must have string 'method' property"}` |
/// | `Page.navigate` with `{}` | `{"code":-32602,"message":"Invalid parameters","data":"Failed to deserialize params.url - BINDINGS: mandatory field missing at position 8"}` |
/// | `Page.navigate` with `{"url":42}` | `-32602` / `"Invalid parameters"` / `data: "… string value expected at position 12"` |
/// | `Page.enable` with `{"totallyUnknownParam":1}` | **`{"result":{}}`** — an unknown EXTRA parameter is ignored |
/// | `Input.dispatchMouseEvent` `{"type":"notAType"}` | `{"code":-32602,"message":"Unexpected event type 'notAType'"}` — **no `data`**; the detail is in `message` |
/// | `Runtime.callFunctionOn` with a bogus `objectId` | `{"code":-32000,"message":"Invalid remote object id"}` |
/// | `Page.enable` with an unknown `sessionId` | `{"code":-32001,"message":"Session with given id not found."}` |
///
/// So: **the unknown-method envelope is ONE shape.** An unknown domain and an unknown method on a
/// known domain are byte-identical in Chrome; so is a bare name with no dot at all, and so is a
/// correctly-spelled method in the wrong case. There is nothing to branch on, and inventing a
/// distinction would have been the W9.16 trap ("five different id shapes") inverted.
///
/// # The one measurement that is deliberately NOT copied
///
/// Chrome's method table is **per target type**. On the browser-level connection, `Page.navigate`
/// and `Runtime.evaluate` answer `-32601 "'Page.navigate' wasn't found"` — they exist only inside a
/// page session. This server has ONE flat table, reachable with or without a `sessionId`, because
/// `tests/conformance.rs` and `tests/server.rs` drive `Page.navigate`/`Runtime.evaluate` over the
/// browser connection with no session and that is a supported way to drive this single-page server.
/// Splitting the table by level is a separate change with its own callers to move; the tell being
/// closed here is "an unimplemented method succeeds", not "routing by target type".
#[derive(Debug)]
pub(crate) struct RpcError {
    code: i64,
    message: String,
    /// Present only where Chrome puts it: its `-32602` deserializer failures carry a `data` string,
    /// and `-32601` never does.
    data: Option<String>,
}

impl RpcError {
    /// Chrome's answer to a method it does not have — the exact code and the exact spelling.
    fn method_not_found(method: &str) -> Self {
        RpcError { code: -32601, message: format!("'{method}' wasn't found"), data: None }
    }

    /// Chrome's `-32602` with the detail in `message` and no `data` -- what its input handlers answer for a
    /// parameter they refuse (`"Unexpected event type 'notAType'"`, `"Invalid mouse button"`).
    fn invalid_params(message: String) -> Self {
        RpcError { code: -32602, message, data: None }
    }

    /// Chrome's answer to a message carrying no string `method`.
    fn no_method() -> Self {
        RpcError {
            code: -32600,
            message: "Message must have string 'method' property".to_string(),
            data: None,
        }
    }

    /// Chrome's `-32000`: the method exists and ran, and failed. (`"Invalid remote object id"` is
    /// Chrome's own example, measured.) This is what every engine-side failure becomes, which is
    /// what this server already answered for them.
    fn server(message: String) -> Self {
        RpcError { code: -32000, message, data: None }
    }

    fn to_json(&self) -> Value {
        match &self.data {
            Some(d) => json!({ "code": self.code, "message": self.message, "data": d }),
            None => json!({ "code": self.code, "message": self.message }),
        }
    }
}

impl From<anyhow::Error> for RpcError {
    fn from(e: anyhow::Error) -> Self {
        RpcError::server(e.to_string())
    }
}

/// Methods this server ACCEPTS and does nothing for.
///
/// Every entry was measured twice. **Real Chrome answers each of them with a result** (Chrome
/// 150.0.7871.46, browser connection for `Browser.*`, flattened page session for the rest — all
/// `{}`), so refusing them would be a tell in the opposite direction. And each is on the list
/// because a **real harness sends it while attaching**: driving the built `korvus serve` binary with
/// Playwright 1.60 `chromium.connect_over_cdp` and logging every frame gives exactly this sequence —
///
/// ```text
/// Browser.getVersion · Target.setAutoAttach · Browser.setDownloadBehavior · Page.enable ·
/// Page.getFrameTree · Log.enable · Page.setLifecycleEventsEnabled · Runtime.enable ·
/// Page.addScriptToEvaluateOnNewDocument · Network.enable · Target.setAutoAttach(session) ·
/// Emulation.setFocusEmulationEnabled · Emulation.setEmulatedMedia ·
/// Runtime.runIfWaitingForDebugger · Target.getTargetInfo · Page.createIsolatedWorld ·
/// Runtime.evaluate · Runtime.callFunctionOn · Page.navigate · … · Runtime.releaseObject
/// ```
///
/// — of which `Log.enable`, `Emulation.setFocusEmulationEnabled`, `Emulation.setEmulatedMedia` and
/// `Browser.setDownloadBehavior` had NO arm in `dispatch` and were living off the blanket
/// `_ => Ok({})`. A plain "unknown → -32601" with no list would have broken `connectOverCDP` on its
/// first burst. That is why this list is measured rather than reasoned about.
///
/// **What is deliberately NOT here.** `Fetch.enable` and `Page.setInterceptFileChooserDialog` also
/// answer `{}` in Chrome (measured), and are still refused, because in Chrome they arm behaviour
/// this engine does not have: a client that enables request interception and then waits for
/// `Fetch.requestPaused` waits forever. Answering `{}` there is a fixed value standing in for
/// behaviour that does not exist. `-32601` is the honest answer and the one a client can act on.
const ACCEPTED_NOOPS: &[&str] = &[
    "Browser.setDownloadBehavior",
    "DOM.enable",
    "Emulation.setFocusEmulationEnabled",
    "Log.enable",
    // `Network.enable` WAS HERE, and being here is what made it a lie by omission: it was accepted
    // and then nothing was ever emitted, so a client that armed `page.on('request')` saw a page that
    // fetched zero resources exactly the way it would see a page that fetched none. It has a real arm
    // in `dispatch` now (see `network.rs`). `Network.disable` is NOT added beside it: nothing this
    // server is driven by sends it, and an arm nobody exercises is an untested claim.
    // The persona owns the user agent and the fonts, so a client's override must not change the identity --
    // `Network.setUserAgentOverride`'s reasoning, above. Playwright sends both while it makes a page (captured against
    // Chrome, `~/bench-lean/cdp/needed.txt`), and Chrome answers `{}` (recorded,
    // `~/bench-lean/cdp/probe/chrome150-page-shapes.jsonl`): refusing them fails `newPage` outright.
    "Page.setFontFamilies",
    // What Puppeteer sends while it makes a page, after `Page.enable` (captured from its own traffic against
    // `korvus browser`, 2026-10-12): each arms an event stream -- inspector issues, performance metrics, WebMCP tools,
    // autofill -- that a page with none of those never emits, and stock Chrome 150 answers each `{}` (recorded,
    // `~/bench-lean/cdp/probe/page_init_answers.mjs`). Refusing `Audits.enable` failed `browser.newPage()` outright.
    "Audits.enable",
    "Autofill.enable",
    "Performance.enable",
    "WebMCP.enable",
    // The `Debugger` domain's state-setters a client sends on attach (DevTools and Playwright do). korvus serves
    // scriptParsed/getScriptSource (D106) but halts on nothing, so each of these is accepted and changes nothing:
    // there is no pause to configure. `enable`/`disable`/`getScriptSource`/`setBreakpointByUrl` have real arms above.
    "Debugger.setPauseOnExceptions",
    "Debugger.setAsyncCallStackDepth",
    "Debugger.setBlackboxPatterns",
    "Debugger.setBlackboxExecutionContexts",
    "Debugger.setSkipAllPauses",
    "Debugger.setBreakpointsActive",
];

/// Accept ONE connection on `listener` and service it against `engine`, handing the engine back
/// when the client disconnects. (Test helper.)
///
/// It returns the engine because some of what a CDP client drives is only observable ON THE ENGINE
/// afterwards — the uncaught-error tally is the case that exists today (W9.22 took
/// `Page.js_error_count` off this wire, because Chrome answers `-32601` for it). A test that drives
/// a page over the protocol and then reads engine state needs the engine back; the alternative was
/// to keep an engine-invented method on a channel that is supposed to be Chrome's.
pub async fn serve_connection(listener: TcpListener, engine: Engine) -> anyhow::Result<Engine> {
    serve_connection_counted(listener, engine).await.map(|(engine, _)| engine)
}

/// What one connection's serve loop did on its own, between commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeStats {
    /// Slices for which the page's event loop ran between commands (FIXER C11: it runs continuously, in
    /// slices that end at a command, a navigation the page asked for, or the network flush interval). A
    /// page with nothing scheduled must cost none after it settles -- the efficiency axis measures this
    /// server's CPU while a driver waits.
    pub idle_pumps: u64,
    /// Idle ticks on which the loop found the page settled and did not pump it.
    pub idle_skips: u64,
}

/// [`serve_connection`], also handing back the connection's [`ServeStats`]. (Test helper.)
pub async fn serve_connection_counted(listener: TcpListener, engine: Engine) -> anyhow::Result<(Engine, ServeStats)> {
    let browser = BrowserIds::new();
    let (stream, _) = listener.accept().await?;
    let (engine, _ids, stats) = handle_conn(stream, Supply::Ready(engine), &browser).await?;
    Ok((engine.expect("a connection handed a ready engine hands the same engine back"), stats))
}

/// WHERE A CONNECTION'S ENGINE COMES FROM, and why it is not always an engine.
///
/// A `/json/version` or `/json/list` request never runs a line of JavaScript: it answers a few
/// strings read off the worn identity and closes. Building an `Engine` for one costs a V8 isolate
/// and a full realm install -- measured on this box at ~70 ms of wall and ~25 ms of CPU -- and then
/// throws it away. `tools/parity/driver.py` makes exactly two such requests before it attaches, so
/// a measured korvus row carried FOUR realm installs where the page needed one.
///
/// `Ready` is the test helper's shape (an engine already built, handed straight through).
/// `Deferred` is the server's FIRST connection: the maker is called only when the connection
/// upgrades to a WebSocket, and the discovery answers read the persona directly.
/// `Resumed` is every connection after it — the page target's own realm, and the ids that go with
/// it, handed on from the client that just left (see [`serve`]).
enum Supply<'a> {
    Ready(Engine),
    Deferred { make: &'a dyn Fn() -> Engine, persona: &'a std::sync::Arc<dyn spec::Persona> },
    Resumed { engine: Engine, ids: ConnIds },
}

impl<'a> Supply<'a> {
    /// The engine this connection runs its page in -- not built yet if it was deferred -- and the
    /// target's ids, when this connection is resuming a target that already exists. See [`Lazy`].
    fn lazy(self) -> (Lazy<'a>, Option<ConnIds>) {
        match self {
            Supply::Ready(e) => (Lazy { engine: Some(e), make: None, stacks: false }, None),
            Supply::Deferred { make, .. } => (Lazy { engine: None, make: Some(make), stacks: false }, None),
            Supply::Resumed { engine, ids } => (Lazy { engine: Some(engine), make: None, stacks: false }, Some(ids)),
        }
    }

    /// The identity `/json/version` answers from -- without building anything.
    fn persona(&self) -> std::sync::Arc<dyn spec::Persona> {
        match self {
            Supply::Ready(e) | Supply::Resumed { engine: e, .. } => e.persona(),
            Supply::Deferred { persona, .. } => (*persona).clone(),
        }
    }
}

/// THE CONNECTION'S ENGINE, BUILT ON FIRST USE.
///
/// A page target's realm used to be assembled the moment the WebSocket was accepted, and then
/// thrown away by the first `Page.navigate`: a cross-document navigation loads in a NEW realm
/// (`navigation.rs::Engine::successor`), so a client that connects and navigates -- which is every
/// client -- paid for two realm installs and used one. Measured on this box, replaying the parity
/// corpus: 70 ms of wall and ~25 ms of CPU per install, against a Chrome whose whole nav->load
/// median is 90 ms.
///
/// Building it on first use makes that exact, with no list of which commands need a realm: a
/// command that touches the engine calls [`Lazy::get`] and gets one; one that answers from `ids`
/// alone (`Page.enable`, `Runtime.enable`, `Network.enable`, `Target.*`,
/// `Page.addScriptToEvaluateOnNewDocument`) never does. When `Page.navigate` is the first command
/// to ask, the realm it builds is THE realm the document loads in -- nothing has run in it, so
/// there is no previous document to unload and nothing for a successor to replace. That is what
/// `Committed::first_document` records.
struct Lazy<'a> {
    engine: Option<Engine>,
    make: Option<&'a dyn Fn() -> Engine>,
    /// This client has the `Runtime` domain enabled: every engine the connection wears captures a stack for its
    /// uncaught exceptions ([`Lazy::runtime_stacks`]).
    stacks: bool,
}

impl Lazy<'_> {
    /// The engine, assembled now if this is the first thing to ask for it -- showing the tab's initial `about:blank`, a
    /// real empty document as Chrome's is (`<html><head></head><body></body></html>`): a client that evaluates, or
    /// writes with `page.setContent`, on a tab it has not navigated reads and writes a DOM. It used to be a realm with
    /// no document loaded, whose `document` had no `getElementById`.
    fn get(&mut self) -> &mut Engine {
        self.build(true)
    }

    /// The engine for a navigation that builds the realm it loads into (see [`Committed::first_document`]): no blank
    /// document first, the navigation's own is the realm's first.
    fn get_bare(&mut self) -> &mut Engine {
        self.build(false)
    }

    fn build(&mut self, blank: bool) -> &mut Engine {
        if self.engine.is_none() {
            let make = self.make.expect("a deferred connection carries its engine maker");
            let mut e = navtrace::phase("first_engine", make);
            if self.stacks {
                e.set_runtime_domain_stacks(true);
            }
            if blank {
                e.set_url("about:blank");
                if let Err(err) = e.load_html_injected("<html><head></head><body></body></html>", &[]) {
                    eprintln!("cdp: the initial about:blank did not load: {err}");
                }
            }
            self.engine = Some(e);
        }
        self.engine.as_mut().expect("just built")
    }

    /// The engine a navigation hands back, wearing this client's `Runtime` stacks.
    fn put(&mut self, mut e: Engine) {
        if self.stacks {
            e.set_runtime_domain_stacks(true);
        }
        self.engine = Some(e);
    }

    /// The `Runtime` domain on or off for this client: as Chrome's inspector does for a front-end with it enabled
    /// (`V8Debugger::setMaxCallStackSizeToCapture`), V8 captures a stack for uncaught exceptions while it is on --
    /// a throwing `Runtime.evaluate` carries `exceptionDetails.stackTrace` only between `Runtime.enable` and
    /// `Runtime.disable` (Chrome 152, measured) -- and the page runs as without DevTools otherwise.
    fn runtime_stacks(&mut self, on: bool) {
        self.stacks = on;
        if let Some(e) = self.engine.as_mut() {
            e.set_runtime_domain_stacks(on);
        }
    }

    /// Has this connection built its engine yet?
    fn built(&self) -> bool {
        self.engine.is_some()
    }
}

/// Serve connections forever (sequential), **one page per TARGET**, not one per
/// connection.
///
/// The browser's identity is minted ONCE here, not per connection: `/json/list` advertises a
/// `targetId` that a client then attaches to over a SECOND connection, and Chrome keeps a target's
/// id for the life of the target. Re-minting per connection would break that discovery handoff,
/// which is exactly what Playwright performs.
///
/// # THE PAGE TARGET OUTLIVES THE CONNECTION
///
/// It did not. Every WebSocket connection built an `Engine` of its own — a fresh V8 isolate, a
/// fresh realm, a fresh `about:blank` — while advertising the SAME `targetId` the client that just
/// left had been driving. So one target had as many realms as it had ever had clients, and the
/// second client to a navigated page read this, measured on `example.com` and on a loopback
/// fixture alike:
///
/// ```text
///                                    korvus (before)         chrome 150.0.7871.46
///   document                         {"readyState":"complete"}  the document, serialized
///   Object.getPrototypeOf(document)  Object                     HTMLDocument
///   document.documentElement         null                       HTML
///   location.href                    about:blank                the navigated url
///   the first client's global         absent                    present
/// ```
///
/// Chrome, asked the same question (one page target, a first client that navigates then detaches,
/// a second that attaches after it), answers **every row exactly as the first client did** and
/// announces the second client **the same execution context**: same `id`, same `uniqueId`, same
/// `origin`, and `Page.getFrameTree` still reads the navigated url. A target is a page; a client is
/// a view onto it.
///
/// That is what this loop now does: the realm and its ids are handed on to the next connection,
/// which re-attaches to them ([`ConnIds::reattach`] mints the one thing Chrome does mint per
/// attach — the `sessionId`). What this does NOT fix is the loop's other half: it is sequential, so
/// a second client's WebSocket handshake does not complete until the first has gone (measured: no
/// answer in 10 s). That needs the `!Send` engine served from a `LocalSet` with the connections as
/// tasks over one shared realm, and it is a lane of its own — but a client that waits and then
/// reads the truth is a different thing from one that is answered promptly with another world.
///
/// A connection that ends in an ERROR drops its engine, so the next client starts a fresh page.
/// That is the pre-existing behaviour and it is left: a realm whose connection died mid-command is
/// of unknown shape, and resuming one would be guessing.
pub async fn serve(
    listener: TcpListener,
    make_engine: impl Fn() -> Engine,
    persona: std::sync::Arc<dyn spec::Persona>,
) -> anyhow::Result<()> {
    accept_loop(listener, &make_engine, &persona, None).await
}

/// [`serve`], for exactly `n` connections, then return. (Test helper — it is the SAME loop, so a
/// test of the second client tests the server and not a second definition of it.)
pub async fn serve_connections(
    listener: TcpListener,
    make_engine: impl Fn() -> Engine,
    persona: std::sync::Arc<dyn spec::Persona>,
    n: usize,
) -> anyhow::Result<()> {
    accept_loop(listener, &make_engine, &persona, Some(n)).await
}

async fn accept_loop(
    listener: TcpListener,
    make_engine: &dyn Fn() -> Engine,
    persona: &std::sync::Arc<dyn spec::Persona>,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    let browser = BrowserIds::new();
    // THE TARGET'S PAGE, between clients. `None` until the first client builds one.
    let mut target: Option<(Engine, ConnIds)> = None;
    let mut served = 0usize;
    loop {
        if limit.is_some_and(|n| served >= n) {
            return Ok(());
        }
        let (stream, _) = listener.accept().await?;
        served += 1;
        // THE ENGINE IS NOT BUILT HERE -- see [`Supply`]. A discovery request answers
        // from the persona and builds no realm; a client that attaches to a target this process has
        // already opened gets THAT target's realm.
        let supply = match target.take() {
            Some((engine, ids)) => Supply::Resumed { engine, ids },
            None => Supply::Deferred { make: make_engine, persona },
        };
        match handle_conn(stream, supply, &browser).await {
            Ok((Some(engine), Some(ids), _stats)) => target = Some((engine, ids)),
            Ok(_) => {}
            Err(e) => eprintln!("cdp connection ended: {e}"),
        }
    }
}

/// Parse the HTTP request, then either answer `/json/*` discovery or upgrade to WebSocket.
///
/// Answers with the target's page and its ids where there is one to hand back, so the next client
/// attaches to the page this one was driving rather than to a new one (see [`serve`]).
async fn handle_conn(
    stream: TcpStream,
    supply: Supply<'_>,
    browser: &BrowserIds,
) -> anyhow::Result<(Option<Engine>, Option<ConnIds>, ServeStats)> {
    // TCP_NODELAY, BECAUSE CDP IS A REQUEST/RESPONSE PROTOCOL OVER SMALL FRAMES.
    //
    // Nagle holds a small segment until the previous small segment is ACKed, and Linux's delayed-ACK
    // timer is ~40 ms. Every command that answers with a RESULT AND THEN AN EVENT -- `Page.enable`,
    // `Target.setAutoAttach`, `Page.getFrameTree` -- writes two small frames back to back, and the
    // second one waited for that timer. Measured 2026-09-18 on a replayed row, against stock Chrome
    // on the same wire: `Page.enable` 40.98 ms (Chrome 0.21), `Target.setAutoAttach` 40.62 (0.10),
    // `Page.getFrameTree` 77.91 (0.20) -- and 0.05 ms for every command that emits no event, which is
    // what made it look like a per-command cost rather than a per-write one. Chrome's own DevTools
    // socket sets this; a client cannot.
    //
    // It is an error only if the socket is already gone, which the read below reports properly.
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(stream);
    let mut req_line = String::new();
    reader.read_line(&mut req_line).await?;
    let path = req_line.split_whitespace().nth(1).unwrap_or("/").to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut l = String::new();
        if reader.read_line(&mut l).await? == 0 {
            break;
        }
        let t = l.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            headers.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let is_ws = get("upgrade").map(|u| u.to_lowercase().contains("websocket")).unwrap_or(false);
    // D88 step 1: only the router opens this socket; a site's page that finds the port is turned away
    // (`access::check_page`). The supply goes back untouched, as for a discovery request.
    if let Err(r) = access::check_page(get("host").as_deref(), get("origin").as_deref(), is_ws) {
        eprintln!("cdp: {}", r.log_line());
        reader.get_mut().write_all(r.response().as_bytes()).await?;
        reader.get_mut().flush().await?;
        return Ok(match supply {
            Supply::Ready(e) => (Some(e), None, ServeStats::default()),
            Supply::Resumed { engine, ids } => (Some(engine), Some(ids), ServeStats::default()),
            Supply::Deferred { .. } => (None, None, ServeStats::default()),
        });
    }

    if is_ws {
        let key = get("sec-websocket-key").unwrap_or_default();
        let accept = derive_accept_key(key.as_bytes());
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        reader.get_mut().write_all(resp.as_bytes()).await?;
        reader.get_mut().flush().await?;
        let stream = reader.into_inner();
        let ws = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        // A PAGE socket's session-less commands are the page's own (Chrome treats them as the page
        // session); a browser socket's are the browser's. `Target.setAutoAttach` means different
        // things on the two.
        let page_socket = path.starts_with("/devtools/page/");
        let (lazy, resumed) = supply.lazy();
        return message_loop(ws, lazy, resumed, browser, page_socket).await;
    }

    let host = get("host").unwrap_or_else(|| "127.0.0.1".to_string());
    let body = if path.starts_with("/json/version") {
        VersionInfo::of(&*supply.persona())
            .json_version(format!("ws://{host}/devtools/browser/{}", browser.debugger_uuid))
            .to_string()
    } else if path.starts_with("/json") {
        let target = &browser.target_id;
        json!([{
            "id": target,
            "type": "page",
            "title": "",
            "url": "about:blank",
            "webSocketDebuggerUrl": format!("ws://{host}/devtools/page/{target}"),
        }])
        .to_string()
    } else if path.starts_with("/korvus/har") {
        // The request ledger (`transport::ledger`) as a HAR 1.2 log: THIS page process's own requests --
        // the same rows `Network.*` reports and `GET /korvus/metrics` counts -- in the DevTools / haralyzer
        // interchange format. Served here because the ledger is process-wide and this process is the page's
        // (the module docs' "a process per target"); the browser endpoint's router serves the same path for
        // its own process (`router::har_json`, router.rs).
        router::har_json(&transport::ledger().records(), transport::ledger().dropped()).to_string()
    } else {
        "{}".to_string()
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    reader.get_mut().write_all(resp.as_bytes()).await?;
    reader.get_mut().flush().await?;
    // A DISCOVERY REQUEST MUST NOT COST THE TARGET ITS PAGE. `tools/parity/driver.py`
    // makes two of these before it attaches; a resumed engine handed to one of them and dropped
    // here would put the next client back on a fresh realm by the back door.
    Ok(match supply {
        Supply::Ready(e) => (Some(e), None, ServeStats::default()),
        Supply::Resumed { engine, ids } => (Some(engine), Some(ids), ServeStats::default()),
        Supply::Deferred { .. } => (None, None, ServeStats::default()),
    })
}

async fn message_loop(
    ws: WebSocketStream<TcpStream>,
    mut engine: Lazy<'_>,
    resumed: Option<ConnIds>,
    browser: &BrowserIds,
    page_socket: bool,
) -> anyhow::Result<(Option<Engine>, Option<ConnIds>, ServeStats)> {
    // THE CONNECTION'S TWO HALVES, APART (`fetch.rs`): every `Fetch.*` command is answered beside the
    // page, whatever it is running, and the rest arrive here in order through `read`; the page's loop and the `Fetch`
    // domain share the write half and the network reporting. Dropping `_halves` stops the domain's tasks and lets every
    // paused request go.
    let rep: fetch::Rep = Default::default();
    let (mut write, read, _halves) = fetch::split(ws, &rep);
    // The V8 isolate, document loader and execution contexts of the PAGE this connection drives,
    // plus this connection's own session. A client attaching to a target that already has a page
    // re-attaches to its ids; only the session is fresh (`ConnIds::reattach`).
    let mut ids = match resumed {
        Some(mut ids) => {
            ids.reattach();
            ids
        }
        None => ConnIds::new(browser),
    };
    // Scripts registered via Page.addScriptToEvaluateOnNewDocument, injected before page scripts on
    // each navigation (CDP "evaluate on new document" semantics): `(worldName, source)`, `""` the page's
    // own world (a named world's script runs in that world, never the page's).
    let mut injected_scripts: Vec<(String, String)> = Vec::new();
    // Has this client called `Network.enable`? Chrome emits no `Network.*` event before it does, and
    // a server that emitted them unconditionally would be telling a client about a domain it never
    // armed -- a difference from Chrome that any harness can see.
    //
    // That flag, the rows already reported, and the worker targets announced to this
    // client, live in `network::Reporting` -- which is flushed after every command and on an idle tick,
    // because a request made after the load has to reach the client when it happens (L18 E15).
    // (`rep`, made with the connection's halves above: the `Fetch` domain reports a paused request's events through it.)
    // The session the PAGE's events are stamped with: whatever session its commands arrive on.
    let mut page_session: Option<String> = None;
    // The ledger revision this connection last flushed at (see `transport::RequestLedger::revision`).
    let mut flushed_at = u64::MAX;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(NETWORK_FLUSH_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Has a navigation loaded a document in this TARGET? Until one has, there is no page whose
    // event loop could have anything to run -- and a target resumed from the client that just left
    // already has one, so its page keeps running rather than going quiet under its new client.
    let mut page_loaded = engine.built() && engine.get().current_url() != "about:blank";
    // Did the page's event loop DRAIN on the last idle pump -- no timer, no in-flight fetch, no live
    // worker? Then nothing but a command (or a declarative refresh falling due, whose time is kept in
    // `refresh_due`) can give it work, and the idle ticks leave it alone instead of re-asking.
    let mut page_settled = false;
    let mut refresh_due: Option<std::time::Instant> = None;
    // SESSION 2b-repaint: the fed-frame generation this serve loop last woke the page for. A plugin may feed a
    // camera frame from its own thread AFTER the page settled; the EDGE below (a new generation, not the LEVEL
    // `has_frames()`) un-settles it for exactly one paint per frame, so a fed `<video>` stays live without a
    // `want_frames` opt-in. Seeded from the current generation so a pre-settle frame does not force a wake here.
    let mut last_seen_fed_gen = plugin_host::fed_frame_generation();
    // Which of Blink's two network-quiet points the current document has announced, by its loader id
    // ([`quiet_events`]).
    let mut quiet_sent: (String, bool, bool) = (String::new(), false, false);
    // The tab's title and same-document URL changes, reported as the page makes them (D88 step 2, [`tab_events`]).
    let mut tab = TabState::default();
    // What the document that is being left still owes the navigation replacing it:
    // whether its announcement has already gone out, and its own late `loadEventFired`.
    let mut handover = Handover::default();
    // Awaited evaluations still pending, answered as they settle (`Deferred`).
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut stats = ServeStats::default();
    // PEEKABLE, so that the page's pump can be told a command has arrived WITHOUT taking it off the socket
    // (FIXER C11): `Engine::pump_until` returns at the next turn boundary once the peek completes, and the
    // command is then read below like any other.
    let mut read = read;
    // The page-eval channel (plugin ABI 3, B1): a plugin on another thread sends a script here; the pump below runs
    // it on this, the page's own thread. Installing captures this thread so a re-entrant call from it is refused.
    let page_eval_pump = page_eval::install();
    // The screenshot channel (plugin ABI 3, C6): a plugin on another thread asks for the page as a PNG via
    // `host->capture_png`; the pump below renders it on this, the page's thread (`Engine::render_png`). Same shape
    // and thread-guard as page_eval.
    let screenshot_pump = engine::screenshot::install();
    loop {
        // Answer any page_eval a plugin asked for (cheap when none wait). With a page, scripts run on this thread;
        // with none yet, each waiting call is answered at once rather than left to time out. `built()` avoids
        // forcing a page into being just to service the channel.
        page_eval_pump.drain(if engine.built() { Some(engine.get()) } else { None });
        // Answer any capture_png a plugin asked for (plugin ABI 3, C6): render the page to PNG on this, the page's
        // thread. Same gating as page_eval -- `None` when no page yet, so an early call is answered at once.
        screenshot_pump.drain(if engine.built() { Some(engine.get()) } else { None });
        // Report any metrics this page's plugins recorded (plugin ABI 3, A6) to the router on one stdout line, so
        // they reach `GET /korvus/metrics`. Cheap when none were recorded.
        let metrics = plugin_host::drain_metrics();
        if !metrics.is_empty() {
            let arr: Vec<Value> = metrics.iter().map(|m| json!({ "name": m.name, "value": m.value, "labels": m.labels })).collect();
            println!("{}{}", router::METRIC_LINE, Value::Array(arr));
        }
        // Deliver any input a plugin injected (plugin ABI 3 wave 1, B6: `host->inject_mouse` / `inject_key` /
        // `inject_wheel`), on this -- the page's own -- thread, in the order the plugin queued it, exactly as the
        // page_eval channel is drained above. The queue is drained only once a page exists, so input queued before
        // the first navigation is delivered to the page it was meant for rather than discarded. The runtime does the
        // DOM-event dispatch (`Engine::mouse_event` / `mouse_wheel` / `dispatch_key_event` / `insert_text`, the same
        // entry points an `Input.*` command uses); this maps each queued event onto it.
        if engine.built() {
            let injected = plugin_host::take_injected_input();
            if !injected.is_empty() {
                dispatch_injected_input(engine.get(), injected).await;
            }
        }
        // Carry out any navigation a plugin drove (plugin ABI 3 step 10, B5: `host->navigate` / `host->reload`), on
        // this -- the page's own -- thread, in the order the plugin queued it, exactly as the metrics and injected
        // input are drained above. Each queued command IS the `Page.navigate` the plugin asked for: `begin_navigate`
        // commits the document and `finish_navigate` loads it, the same two steps a client's `Page.navigate` takes
        // below (there is no client command to answer, so the result is dropped). A `reload` is a navigate to the
        // page's current url. The queue is drained unconditionally (cheap and empty on a no-plugin run, so the engine
        // is byte-for-byte unchanged, invariant I9/J2); a navigation builds the engine if nothing has yet, so a plugin
        // can open the first page, and a process with no page never reaches this loop so a command queued there is
        // never acted on (the header's "no-op in a process with no page").
        for cmd in plugin_host::take_navigations() {
            let first_document = !engine.built();
            let (url, reload) = match cmd {
                plugin_host::NavCmd::Navigate { url } => (url, false),
                plugin_host::NavCmd::Reload => (engine.get().current_url(), true),
            };
            let params = json!({ "url": url });
            let (_result, events, committed) =
                begin_navigate(engine.get_bare(), &params, &mut ids, &rep, first_document, &mut write, page_session.as_deref(), reload).await;
            // Emit the navigation's start events, then (mirroring the `Page.navigate` command path below) flush the
            // network rows only when nothing committed -- a committed document's own `Network.*` rows already went
            // out during `begin_navigate`, and its load events follow from `finish_navigate`. There is no client
            // command here, so the navigation's result is dropped.
            for ev in events {
                send_event(&mut write, ev, page_session.as_deref()).await?;
            }
            if committed.is_none() {
                flushed_at = transport::ledger().revision();
                write.send_all(fetch::flushed_messages(&rep, page_session.as_deref())).await?;
            }
            if let Some(committed) = committed {
                let e = engine.engine.take().expect("a committed navigation built the engine");
                let (next, events) =
                    finish_navigate(e, committed, &injected_scripts, &mut ids, &rep, &mut handover, None, &write, page_session.as_deref()).await;
                engine.put(next);
                page_loaded = true;
                page_settled = false;
                flushed_at = transport::ledger().revision();
                for (stamp, ev) in events {
                    send_stamped(&mut write, ev, &stamp, page_session.as_deref()).await?;
                }
            }
        }
        // The frame tap (plugin ABI 3 wave 1, C1, the D29 streaming core) needs nothing here. The runtime hands a
        // frame-wanting plugin each CHANGED picture as the page paints it (`webapi::deliver_frame`, at the page's
        // own frames and at each vsync while the page runs), and a frame-wanted page settles like any other: what
        // changes its picture -- a command, a timer, a fetch -- is what wakes it. It used to be kept from settling
        // so it would keep painting: on a static page that was the same frame ~130 times a second, and a pump
        // that never yielded starved the task that reads this socket, so commands went unanswered (D72).
        // SESSION 2b-repaint: un-settle once per NEW fed camera frame (EDGE on the generation, not the LEVEL
        // `has_frames()` above which would pin a busy loop), so a frame fed after the page settled paints once --
        // the rendering opportunity then runs the cssom Video arm + fires its media events. page_settled returns
        // to true below after the single pump. No fed frame -> the generation is unchanged -> no wake (I9/J2).
        let fed_gen = plugin_host::fed_frame_generation();
        if page_loaded && fed_gen != last_seen_fed_gen {
            last_seen_fed_gen = fed_gen;
            page_settled = false;
        }
        if page_loaded && page_settled && refresh_due.is_some_and(|d| std::time::Instant::now() >= d) {
            page_settled = false;
        }
        // THE PAGE RUNS ON ITS OWN SCHEDULE (FIXER C11). Its event loop used to advance only
        // inside a command (P12d fixed that with an idle tick), and then only 25 ms out of every 50: the tick
        // pumped a fixed slice and left the loop alone until the next one. Measured on a loopback page under
        // `korvus serve`, against Chrome 150 and `korvus fetch` on the same page:
        //
        // ```text
        //                                         chrome 150       korvus fetch     korvus serve (idle tick)
        //   setTimeout(f, 10) delay, min/med/max  10.0/10.1/10.2   10.0/10.0/10.1   10.0/10.1/32.1
        //   requestAnimationFrame interval        16.6/16.7/16.8   16.6/16.7/16.7   3.5/16.7/36.8
        // ```
        //
        // Chrome's renderer runs its timers and its frames on its own clock whether or not anyone is talking
        // to DevTools; the browser process owns the socket and hands a command to the renderer as a task.
        // Here the page now runs continuously, and the socket is the pump's interrupt: a command that arrives
        // is read at the next boundary between two of the page's tasks, answered, and the page runs on. A
        // mouse event is therefore dispatched as the page's next task and answered once it has been handled,
        // which is when Chrome answers `Input.dispatchMouseEvent` (`InputHandler` waits for the renderer's
        // ack) -- not after whatever is left of a 25 ms slice, and not 25 ms of idle tick later.
        //
        // NO BUSY-SPIN: a pump that finds the loop drained marks the page settled, and a settled page is not
        // pumped again until a command arrives or its scheduled refresh falls due.
        if (page_loaded || (!deferred.is_empty() && engine.built())) && !page_settled && !message_waiting(&mut read) {
            stats.idle_pumps += 1;
            let slice = std::time::Duration::from_millis(NETWORK_FLUSH_MS);
            let end = engine.get().pump_until(slice, std::pin::Pin::new(&mut read).peek()).await;
            // `page_loaded` is only true once a navigation has loaded a document, and a navigation builds the
            // engine -- so there is one here by construction.
            let e = engine.engine.take().expect("a loaded page has an engine");
            let (next, navigated) =
                follow_page_navigation(e, &injected_scripts, &mut ids, &rep, &mut write, page_session.as_deref(), &mut handover).await?;
            engine.put(next);
            if !navigated && matches!(end, Ok(engine::PumpEnd::Drained)) && !engine.get().navigation_pending() {
                page_settled = true;
                // A settled page still has its CSS animations' events to send: woken when the next one falls
                // due, as Chrome's frames would send it, rather than at the client's next command.
                refresh_due = [engine.get().navigation_due_at(), engine.get().css_wake_due()].into_iter().flatten().min();
            }
            let rev = transport::ledger().revision();
            if rev != flushed_at {
                flushed_at = rev;
                write.send_all(fetch::flushed_messages(&rep, page_session.as_deref())).await?;
            }
            for ev in quiet_events(engine.get(), &ids, &mut quiet_sent) {
                send_event(&mut write, ev, page_session.as_deref()).await?;
            }
            for ev in tab_events(engine.get(), &ids, &mut tab) {
                send_event(&mut write, ev, page_session.as_deref()).await?;
            }
            for ev in frame_events(engine.get(), &mut ids) {
                send_event(&mut write, ev, page_session.as_deref()).await?;
            }
            for ev in devtools_events(engine.get(), &mut ids) {
                send_event(&mut write, ev, page_session.as_deref()).await?;
            }
            for (t, ev) in popup_events(engine.get(), &mut ids) {
                send_popup_event(&mut write, &t, ev).await?;
            }
            for (url, name) in webapi::devtools_log::take_downloads() {
                let frame = ids.frame_id().to_string();
                download(engine.get(), &frame, &url, name, None).await;
            }
            write.send_all(fetch::worker_console_messages(&rep)).await?;
            settle_deferred(&mut engine, &mut ids, &mut deferred, &mut write).await?;
            say_window_close();
            continue;
        }
        // A settled page's wake: its refresh, or its next CSS event -- on time, not at the next 50 ms tick.
        let wake = refresh_due.filter(|_| page_loaded && page_settled);
        let msg = tokio::select! {
            // A command first: one that is already waiting is never made to wait for a tick.
            biased;
            m = read.next() => match m {
                Some(m) => m,
                None => break,
            },
            _ = async {
                match wake {
                    Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                    None => std::future::pending::<()>().await,
                }
            }, if wake.is_some() => continue,
            _ = tick.tick() => {
                // Only a page with nothing to run gets here on a tick (a running one is pumped above): its
                // scheduled refresh is checked at the top of the loop, and the network rows are flushed.
                if page_loaded {
                    stats.idle_skips += 1;
                    // A settled page is pumped no more, so its quiet points are looked for here: one that drained
                    // before 500 quiet ms had passed would otherwise never announce `networkIdle`.
                    for ev in quiet_events(engine.get(), &ids, &mut quiet_sent) {
                        send_event(&mut write, ev, page_session.as_deref()).await?;
                    }
                    for ev in tab_events(engine.get(), &ids, &mut tab) {
                        send_event(&mut write, ev, page_session.as_deref()).await?;
                    }
                    for ev in frame_events(engine.get(), &mut ids) {
                        send_event(&mut write, ev, page_session.as_deref()).await?;
                    }
                    for ev in devtools_events(engine.get(), &mut ids) {
                        send_event(&mut write, ev, page_session.as_deref()).await?;
                    }
                    for (t, ev) in popup_events(engine.get(), &mut ids) {
                        send_popup_event(&mut write, &t, ev).await?;
                    }
                }
                let rev = transport::ledger().revision();
                if rev != flushed_at {
                    flushed_at = rev;
                    write.send_all(fetch::flushed_messages(&rep, page_session.as_deref())).await?;
                }
                write.send_all(fetch::worker_console_messages(&rep)).await?;
                settle_deferred(&mut engine, &mut ids, &mut deferred, &mut write).await?;
                continue;
            }
        };
        let msg = msg?;
        // Anything the client sends may give the page work.
        page_settled = false;
        match msg {
            Message::Close(_) => break,
            Message::Text(txt) => {
                let req: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
                let id = req.get("id").and_then(Value::as_i64).unwrap_or(0);
                // A message with no string `method` is malformed, and Chrome says so with its OWN
                // code -- -32600, not the -32601 an unknown method gets (measured; see `RpcError`).
                let method = req.get("method").and_then(Value::as_str).map(String::from);
                let params = req.get("params").cloned().unwrap_or(Value::Null);
                let session = req.get("sessionId").and_then(Value::as_str).map(String::from);
                // A command for a popup target this process serves (the router marked it with the target).
                if let Some(target) = req.get("korvusTarget").and_then(Value::as_str).map(String::from) {
                    let m = method.clone().unwrap_or_default();
                    let mut network_on = fetch::rep_lock(&rep).page_network;
                    let (result, events) = dispatch_popup(&mut engine, &target, &m, &params, &mut ids, &mut network_on).await;
                    fetch::rep_lock(&rep).page_network = network_on;
                    for ev in events {
                        send_popup_event(&mut write, &target, ev).await?;
                    }
                    if engine.built() {
                        for (t, ev) in popup_events(engine.get(), &mut ids) {
                            send_popup_event(&mut write, &t, ev).await?;
                        }
                    }
                    let mut resp = match result {
                        Ok(r) => json!({ "id": id, "result": r }),
                        Err(e) => json!({ "id": id, "error": e.to_json() }),
                    };
                    resp["korvusTarget"] = json!(target);
                    write.send(Message::Text(resp.to_string())).await?;
                    page_settled = false;
                    continue;
                }
                // A command on a CHILD target's session (an auto-attached worker).
                if let Some(idx) = session.as_deref().and_then(|s| fetch::rep_lock(&rep).child(s)) {
                    // The worker's realm, announced when its client enables `Runtime`, as Chrome announces a worker's
                    // context: a client (Playwright's `Worker`) waits for it before it calls the worker ready.
                    let ctx = worker_context_id(idx);
                    if method.as_deref() == Some("Runtime.enable") {
                        let ev = json!({ "method": "Runtime.executionContextCreated", "params": { "context": {
                            "id": ctx, "origin": "", "name": "", "uniqueId": format!("worker-{ctx}"),
                            "auxData": { "isDefault": true },
                        } }, "sessionId": session });
                        write.send(Message::Text(ev.to_string())).await?;
                        fetch::rep_lock(&rep).enable_child_runtime(idx);
                    }
                    // a script for the worker runs on its own thread (`engine::worker_devtools`)
                    let result = match method.as_deref() {
                        Some(m @ ("Runtime.evaluate" | "Runtime.callFunctionOn" | "Runtime.getProperties" | "Runtime.releaseObject" | "Runtime.releaseObjectGroup")) => {
                            match fetch::rep_lock(&rep).child_target(idx) {
                                Some(target) => engine::worker_devtools::call(&target, m, params.clone(), ctx).await.map_err(RpcError::server),
                                None => Err(RpcError::server("Cannot find context with specified id".to_string())),
                            }
                        }
                        _ => child_command(&rep, idx, method.as_deref()),
                    };
                    let mut resp = match result {
                        Ok(r) => json!({ "id": id, "result": r }),
                        Err(e) => json!({ "id": id, "error": e.to_json() }),
                    };
                    resp["sessionId"] = json!(session);
                    write.send(Message::Text(resp.to_string())).await?;
                    flushed_at = transport::ledger().revision();
                    write.send_all(fetch::flushed_messages(&rep, page_session.as_deref())).await?;
                    write.send_all(fetch::worker_console_messages(&rep)).await?;
                    continue;
                }
                page_session = session.clone();
                // A plugin-served CDP domain (`Korvus_<name>.*`, ABI 3, A1) relayed to this PAGE process: a domain
                // whose effect lives in the page, not the endpoint -- the virtual-time control (F5) above all, whose
                // clock and timer queue are here. Answered by this process's own plugin copy, so `host->virtual_time_*`
                // flips THIS engine's clock; a browser-level plugin domain keeps being answered endpoint-side (it
                // arrives there with no session and never reaches this relay). Checked before the command match so a
                // plugin cannot be shadowed by a built-in, and only for the reserved `Korvus_` names.
                if let Some(m) = method.as_deref().filter(|m| m.starts_with("Korvus_")) {
                    if let Some(domain) = m.split('.').next() {
                        if plugin_host::cdp_domains().iter().any(|d| d == domain) {
                            let params_json = if params.is_null() { String::new() } else { params.to_string() };
                            let mut resp = match plugin_host::cdp_call(m, &params_json) {
                                Some(r) => json!({ "id": id, "result": serde_json::from_str::<Value>(&r).unwrap_or(Value::Null) }),
                                None => json!({ "id": id, "error": RpcError::server(format!("the plugin did not answer {m}")).to_json() }),
                            };
                            if let Some(s) = session.as_deref() {
                                resp["sessionId"] = json!(s);
                            }
                            write.send(Message::Text(resp.to_string())).await?;
                            write.send_all(fetch::flushed_messages(&rep, page_session.as_deref())).await?;
                            continue;
                        }
                    }
                }
                // `Target.setAutoAttach` on the PAGE arms the announcement of its worker children.
                if method.as_deref() == Some("Target.setAutoAttach") && (session.is_some() || page_socket) {
                    fetch::rep_lock(&rep).set_auto_attach(params.get("autoAttach").and_then(Value::as_bool).unwrap_or(false));
                }
                if std::env::var("KORVUS_CDP_LOG").is_ok() {
                    eprintln!("CDP <- {} (session={})", method.as_deref().unwrap_or("<none>"), session.is_some());
                    if method.as_deref().is_some_and(|m| m.starts_with("Runtime.")) {
                        let p = params.to_string();
                        eprintln!("     params: {}", &p[..p.len().min(400)]);
                    }
                }

                let (result, events, committed) = match method.as_deref() {
                    // `Page.navigate` answers at COMMIT and loads afterwards -- see `begin_navigate`.
                    // The screencast writes its frames on the connection's write half, which `dispatch` does not hold.
                    Some("Page.startScreencast") => {
                        let r = screencast::start(engine.get(), &params, write.clone(), session.clone()).map_err(RpcError::server);
                        (r, vec![], None)
                    }
                    Some("Page.navigate") => {
                        // THE REALM'S AGE, decided before it exists (see [`Lazy`]).
                        let first_document = !engine.built();
                        begin_navigate(engine.get_bare(), &params, &mut ids, &rep, first_document, &mut write, session.as_deref(), false).await
                    }
                    // `Page.reload` (D88 step 2): the current document again, in its entry's place -- answered at commit as a
                    // navigation is, with Chrome's empty result.
                    Some("Page.reload") => {
                        let first_document = !engine.built();
                        let url = if engine.built() { engine.get().current_url() } else { "about:blank".to_string() };
                        let (r, events, committed) =
                            begin_navigate(engine.get_bare(), &json!({ "url": url }), &mut ids, &rep, first_document, &mut write, session.as_deref(), true).await;
                        (r.map(|_| json!({})), events, committed)
                    }
                    Some(m) => {
                        // A PAGE socket's session-less commands are the PAGE session's (Chrome's rule, above): `dispatch`
                        // must not take them for the browser's. Its `Target.setAutoAttach` attached the page to ITSELF
                        // when `korvus browser` relayed a page session here (Playwright saw a second
                        // `attachedToTarget` for the page it was setting up).
                        let mut network_on = fetch::rep_lock(&rep).page_network;
                        let (result, events) = dispatch(
                            &mut engine,
                            m,
                            &params,
                            session.as_deref().or(if page_socket { Some("") } else { None }),
                            &mut injected_scripts,
                            &mut ids,
                            &mut network_on,
                        )
                        .await;
                        fetch::rep_lock(&rep).page_network = network_on;
                        (result, events, None)
                    }
                    None => (Err(RpcError::no_method()), vec![], None),
                };

                // What the command did to the tab -- a `pushState`, a title -- is reported with its own events.
                let mut events = events;
                if engine.built() {
                    events.extend(tab_events(engine.get(), &ids, &mut tab));
                    events.extend(frame_events(engine.get(), &mut ids));
                    events.extend(devtools_events(engine.get(), &mut ids));
                    for (t, ev) in popup_events(engine.get(), &mut ids) {
                        send_popup_event(&mut write, &t, ev).await?;
                    }
                }
                // Emit any events first (Playwright waits on some before continuing).
                for ev in events {
                    send_event(&mut write, ev, session.as_deref()).await?;
                }
                // a download link the command followed
                if engine.built() {
                    for (url, name) in webapi::devtools_log::take_downloads() {
                        let frame = ids.frame_id().to_string();
                        download(engine.get(), &frame, &url, name, None).await;
                    }
                }
                // Whatever the command caused on the network (a `Runtime.evaluate` that fetched, a
                // child's `Network.enable`), reported before its answer, as Chrome's arrive before it.
                if committed.is_none() {
                    flushed_at = transport::ledger().revision();
                    write.send_all(fetch::flushed_messages(&rep, page_session.as_deref())).await?;
                }

                if let Some(d) = Deferred::of(&result, id, session.clone()) {
                    // answered when its promise settles; the page runs on meanwhile
                    deferred.push(d);
                    page_settled = false;
                } else {
                    let mut resp = match result {
                        Ok(r) => json!({ "id": id, "result": r }),
                        Err(e) => json!({ "id": id, "error": e.to_json() }),
                    };
                    if let (Some(obj), Some(s)) = (resp.as_object_mut(), session.as_deref()) {
                        obj.insert("sessionId".into(), json!(s));
                    }
                    if std::env::var("KORVUS_CDP_LOG").is_ok() {
                        let s = resp.to_string();
                        eprintln!("CDP => {}", &s[..s.len().min(200)]);
                    }
                    write.send(Message::Text(resp.to_string())).await?;
                }
                say_window_close();

                // The committed document loads NOW, after its command has been answered -- which is
                // where Chrome's renderer runs a page's scripts too. The events it produces follow.
                if let Some(committed) = committed {
                    let e = engine.engine.take().expect("a committed navigation built the engine");
                    let (next, events) =
                        finish_navigate(e, committed, &injected_scripts, &mut ids, &rep, &mut handover, None, &write, page_session.as_deref()).await;
                    engine.put(next);
                    page_loaded = true;
                    flushed_at = transport::ledger().revision();
                    for (stamp, ev) in events {
                        send_stamped(&mut write, ev, &stamp, page_session.as_deref()).await?;
                    }
                    // what the document logged while it loaded
                    for ev in devtools_events(engine.get(), &mut ids) {
                        send_event(&mut write, ev, page_session.as_deref()).await?;
                    }
                }
                // A navigation the page asked for -- during its load, or in the command just answered
                // (`Runtime.evaluate("location.href = …")`) -- is performed now, after the answer, which
                // is where Chrome's follows too.
                if page_loaded {
                    let e = engine.engine.take().expect("a loaded page has an engine");
                    engine.put(
                        follow_page_navigation(e, &injected_scripts, &mut ids, &rep, &mut write, page_session.as_deref(), &mut handover)
                            .await?
                            .0,
                    );
                    for ev in quiet_events(engine.get(), &ids, &mut quiet_sent) {
                        send_event(&mut write, ev, page_session.as_deref()).await?;
                    }
                }
                settle_deferred(&mut engine, &mut ids, &mut deferred, &mut write).await?;
            }
            _ => {}
        }
    }
    // The page and the ids that describe it go back to the accept loop, for the next client -- with no stack
    // capture, the next client not having enabled `Runtime` yet.
    // A connection that leaves by an `?` above hands back nothing, and the next
    // client gets a fresh page -- see [`serve`].
    engine.runtime_stacks(false);
    Ok((engine.engine, Some(ids), stats))
}

/// Deliver the input a plugin injected (plugin ABI 3 wave 1, B6) to the page, in the order it was queued.
///
/// Each [`plugin_host::InjectedInput`] is dispatched through the same engine entry points a CDP `Input.*` command
/// uses, so a synthetic event is the one platform event Blink makes of it -- a press is a press, a release a release
/// -- never a humanised trajectory. `inject_mouse` names the button (0 left / 1 middle / 2 right; a move carries
/// none), `inject_key` is a `keyDown` / `keyUp` of a DOM key name (its other CDP fields take their defaults) or a
/// `char` typed as text, and `inject_wheel` scrolls by `dy` at the point. Step 12 (Frontier S, B6) adds two:
/// `inject_touch` is one trusted finger ("start"/"move"/"end" at the point), and `inject_ime` commits a composed
/// string as one edit at the focus. The result of each is dropped: a plugin's injected event has no caller to
/// answer, and one that lands on nothing is a no-op as it is in Chrome.
async fn dispatch_injected_input(engine: &mut Engine, events: Vec<plugin_host::InjectedInput>) {
    use plugin_host::InjectedInput;
    for ev in events {
        match ev {
            InjectedInput::Mouse { kind, x, y, button } => {
                let mk = match kind.as_str() {
                    "move" => webapi::input::MouseKind::Move,
                    "down" => webapi::input::MouseKind::Down,
                    "up" => webapi::input::MouseKind::Up,
                    _ => continue,
                };
                // A move names no button; a press/release carries the one named, and a press holds it in `buttons`
                // (a release leaves none held, as its only button comes up). `click_count` 1 for a press/release.
                let b = if matches!(mk, webapi::input::MouseKind::Move) { webapi::input::NO_BUTTON } else { button };
                let buttons = match mk {
                    webapi::input::MouseKind::Down => webapi::input::button_bit(b),
                    _ => 0,
                };
                let click_count = if matches!(mk, webapi::input::MouseKind::Move) { 0 } else { 1 };
                let _ = engine.mouse_event(mk, x, y, b, buttons, click_count, webapi::input::Modifiers::default());
            }
            InjectedInput::Wheel { x, y, dy } => {
                let _ = engine.mouse_wheel(x, y, 0.0, dy, 0, webapi::input::Modifiers::default());
            }
            InjectedInput::Key { kind, key } => match kind.as_str() {
                // A typed character: `Input.insertText`'s path (`beforeinput` -> value change -> `input`, the whole
                // string as `data`, no key event), which is what a `char` is.
                "char" => {
                    let _ = engine.insert_text(&key).await;
                }
                // A key down/up of the named DOM key; every other CDP field is its default (`KeyInput::from_cdp`).
                "down" | "up" => {
                    let ty = if kind == "down" { "keyDown" } else { "keyUp" };
                    let input = webapi::input::KeyInput::from_cdp(&key, "", "", None, None, false, false, None);
                    let _ = engine.dispatch_key_event(ty, input).await;
                }
                _ => {}
            },
            // A single trusted finger (step 12, B6): the one platform `TouchEvent` Blink makes of it at the element
            // under (x, y), the raw path (J5) `Input.dispatchTouchEvent` takes -- never the humanised `tap`/`swipe`
            // graph (the mobile persona's behaviour plan). `kind` is the host call's "start"|"move"|"end"; force and
            // the 1×1 elliptical contact are CDP's touch-point defaults for an injected finger.
            InjectedInput::Touch { kind, x, y } => {
                let ty = match kind.as_str() {
                    "start" => "touchstart",
                    "move" => "touchmove",
                    "end" => "touchend",
                    _ => continue,
                };
                let _ = engine.dispatch_trusted_touch(ty, x, y, 1.0, 1.0, 1.0);
            }
            // A committed IME string (step 12, B6): the text an input method delivers as ONE edit, which is
            // `Input.insertText`'s path (`beforeinput` -> value change -> `input`, the whole string as `data`, no
            // key events) at the focus -- the same engine entry point a `Key { kind: "char" }` takes.
            // TODO(1021-verify): this commits the composed text but fires no compositionstart/update/end and sets no
            // `isComposing`; a full composition-event sequence would need a webapi seam (an `Emit::Composition` /
            // CompositionEvent path) outside these two files -- GAP if a plugin needs the composition events too.
            InjectedInput::Ime { text } => {
                let _ = engine.insert_text(&text).await;
            }
        }
    }
}

/// The current document's network-quiet points, each announced once as Chrome's `Page.lifecycleEvent`
/// (`networkAlmostIdle` before `networkIdle`, Blink's `WillProcessTask` order), stamped with the moment its quiet
/// period began -- what Blink's `NetworkBecameAlmostIdle` / `NetworkBecameIdle` pass on. Puppeteer's
/// `waitUntil: 'networkidle2' | 'networkidle0'` waits for exactly these. A new navigation's loader starts both again.
fn quiet_events(engine: &mut Engine, ids: &ConnIds, sent: &mut (String, bool, bool)) -> Vec<Value> {
    if sent.0 != ids.loader_id() {
        *sent = (ids.loader_id().to_string(), false, false);
    }
    let marks = engine.network_idle();
    let ev = |name: &str, at: std::time::Instant| {
        json!({ "method": "Page.lifecycleEvent", "params": {
            "frameId": ids.frame_id(), "loaderId": ids.loader_id(), "name": name, "timestamp": monotonic_s_at(at) } })
    };
    let mut out = Vec::new();
    if let (false, Some(at)) = (sent.1, marks.almost_idle) {
        sent.1 = true;
        out.push(ev("networkAlmostIdle", at));
    }
    if let (false, Some(at)) = (sent.2, marks.idle) {
        sent.2 = true;
        out.push(ev("networkIdle", at));
    }
    out
}

/// Is a message (or the end of the stream) waiting on the socket, ready to be read without waiting?
fn message_waiting(read: &mut SocketRead) -> bool {
    futures_util::FutureExt::now_or_never(std::pin::Pin::new(read).peek()).is_some()
}

/// What the page's loop reads: every message the `Fetch` domain did not answer itself, peekable (see `message_loop`).
type SocketRead = futures_util::stream::Peekable<fetch::Inbox>;

/// Perform ONE navigation the page asks for now, if it asks for one (`true` when it did), and send its
/// events; announce a
/// declarative refresh that has just been scheduled. One per call, so a document that refreshes itself
/// with no delay (Chrome reloads such a page hundreds of times a second, measured) still leaves this
/// connection reading its commands between hops.
async fn follow_page_navigation(
    mut engine: Engine,
    injected: &[(String, String)],
    ids: &mut ConnIds,
    rep: &fetch::Rep,
    write: &mut fetch::Out,
    page_session: Option<&str>,
    handover: &mut Handover,
) -> anyhow::Result<(Engine, bool)> {
    if let Some(nav) = engine.take_scheduled_navigation() {
        send_event(write, scheduled_navigation_event(ids.frame_id(), &nav), page_session).await?;
    }
    let Some(nav) = engine.take_due_navigation() else { return Ok((engine, false)) };
    let (engine, events) = page_navigation(engine, nav, injected, ids, rep, handover, write, page_session).await;
    for (stamp, ev) in events {
        send_stamped(write, ev, &stamp, page_session).await?;
    }
    Ok((engine, true))
}

/// How often an idle connection looks for ledger rows it has not reported. A client waiting for network
/// quiet (the parity driver settles on 500 ms without a new request) sees a request within this much of
/// the transport recording it.
const NETWORK_FLUSH_MS: u64 = 50;

/// `load`, with the ledger rows it adds reported every [`NETWORK_FLUSH_MS`] while it runs: a request a loading document
/// makes reaches the client when it is made, as Chrome's do, and not in one batch once the load is over.
///
/// In the batch, a client that waits for network quiet (Playwright's `networkidle`, the benchmark's 500 ms) saw a whole
/// load as silence. costco.com loads in two steps of about 1.9 s each (a bot-detection endpoint reloads the document); the ledger's own
/// times for korvus's requests there had no gap over 400 ms, as Chrome's had none, while their events arrived with
/// gaps of 1.9-2.2 s -- the benchmark called the page settled during the second load and read it before a bot-detection endpoint had
/// let its content through, where it read Chrome's whole (2026-10-22, korvus 'thin' 2 of 2, Chrome 'loaded').
///
/// The rows go out where the load yields (a fetch, a timer, a parse waiting for bytes); a running script is not
/// interrupted, and what it requested is reported when it returns. A client that has gone is noticed by the next write
/// after the load.
async fn reporting_network<T>(
    mut load: impl std::future::Future<Output = T> + Unpin,
    rep: &fetch::Rep,
    write: &fetch::Out,
    page_session: Option<&str>,
) -> T {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(NETWORK_FLUSH_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut flushed_at = u64::MAX;
    loop {
        tokio::select! {
            biased;
            done = &mut load => return done,
            _ = tick.tick() => {
                let rev = transport::ledger().revision();
                if rev != flushed_at {
                    flushed_at = rev;
                    let _ = write.send_all(fetch::flushed_messages(rep, page_session)).await;
                }
            }
        }
    }
}

/// The network events this connection has not sent yet (see `network::Reporting::flush`).
fn flush_network(rep: &fetch::Rep) -> Vec<(network::Stamp, Value)> {
    let rows = transport::ledger().records();
    let targets = transport::ledger().targets();
    fetch::rep_lock(rep).flush(&rows, &targets, ids::hex32)
}

/// The execution context id a worker child's realm is announced with (its index among the page's children).
pub(crate) fn worker_context_id(idx: usize) -> i64 {
    1_000_000 + idx as i64
}

/// A command on an auto-attached worker's session. A worker target has no page, so this answers the
/// commands a client sends while instrumenting it (L18's driver: `Network.enable`,
/// `Target.setAutoAttach`, `Runtime.runIfWaitingForDebugger`) and refuses the rest with Chrome's
/// unknown-method envelope rather than pretending to run them.
fn child_command(rep: &fetch::Rep, idx: usize, method: Option<&str>) -> Result<Value, RpcError> {
    match method {
        None => Err(RpcError::no_method()),
        Some("Network.enable") => {
            fetch::rep_lock(rep).enable_child_network(idx);
            Ok(json!({}))
        }
        Some("Target.setAutoAttach" | "Runtime.runIfWaitingForDebugger" | "Runtime.enable" | "Log.enable" | "Network.setCacheDisabled" | "Runtime.addBinding") => Ok(json!({})),
        Some(m) => Err(RpcError::method_not_found(m)),
    }
}

/// An event from a flush: the page's go out on the page's session, a child's on its own.
async fn send_stamped(
    write: &mut fetch::Out,
    ev: Value,
    stamp: &network::Stamp,
    page_session: Option<&str>,
) -> anyhow::Result<()> {
    match stamp {
        network::Stamp::Page => send_event(write, ev, page_session).await,
        network::Stamp::Session(s) => send_event(write, ev, Some(s)).await,
    }
}

/// The page's own `window.close()` (a popup's), asked and not yet said to the router: said once the command that ran it
/// has been answered, as Chrome closes the window after the task that asked -- said at once, the router ended the
/// process before its answer went out, and the client waited for ever.
static WINDOW_CLOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The page asked to close its window ([`WINDOW_CLOSE`]).
pub fn request_window_close() {
    WINDOW_CLOSE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Say a window close the page asked for, now that nothing it ran is waiting for an answer.
fn say_window_close() {
    if WINDOW_CLOSE.swap(false, std::sync::atomic::Ordering::Relaxed) {
        println!("{}{{}}", router::WINDOW_CLOSE_LINE);
    }
}

/// What the page's tab shows besides its picture (D88 step 2): its title as last reported, and when it was read.
#[derive(Default)]
struct TabState {
    title: Option<String>,
    revision: u64,
    checked: Option<std::time::Instant>,
    loader: String,
}

/// The page's same-document navigations as `Page.navigatedWithinDocument` (Chrome's `fragment` / `historyApi`), and --
/// to a router, whose `Target.getTargetInfo` answers it -- the title when it changed ([`router::TITLE_EVENT`], never
/// relayed to a client), read again only when the DOM moved, at most every 200 ms, or after a navigation.
fn tab_events(engine: &mut Engine, ids: &ConnIds, tab: &mut TabState) -> Vec<Value> {
    let mut out = Vec::new();
    let navs = engine.take_same_document_navigations();
    if router::child_ids().is_some() {
        if tab.loader != ids.loader_id() {
            // a new document: its title is announced afresh, whatever the last one's was
            *tab = TabState { loader: ids.loader_id().to_string(), revision: u64::MAX, ..TabState::default() };
        }
        let rev = engine.page_dom_revision();
        let due = !navs.is_empty() || (rev != tab.revision && tab.checked.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(200)));
        if due {
            let (title, rev) = engine.page_title();
            tab.revision = rev;
            tab.checked = Some(std::time::Instant::now());
            if tab.title.as_deref() != Some(title.as_str()) {
                out.push(json!({ "method": router::TITLE_EVENT, "params": { "title": title } }));
                tab.title = Some(title);
            }
        }
    }
    for (url, kind) in navs {
        out.push(json!({ "method": "Page.navigatedWithinDocument", "params": { "frameId": ids.frame_id(), "url": url, "navigationType": kind } }));
    }
    out
}

/// Does this page process's history start at the `about:blank` its tab was created at? A tab a client or the endpoint
/// opened does (Chrome 150: `Target.createTarget` at `about:blank` then two navigations reads three entries); a popup
/// a script opened, and the single-page server, start at their first document.
fn seeds_blank_entry() -> bool {
    router::child_ids().is_some() && !webapi::opened_by_script()
}

/// One event onto the wire, stamped with the session the command that produced it arrived on.
async fn send_event(
    write: &mut fetch::Out,
    mut ev: Value,
    session: Option<&str>,
) -> anyhow::Result<()> {
    if let (Some(obj), Some(s)) = (ev.as_object_mut(), session) {
        obj.insert("sessionId".into(), json!(s));
    }
    if std::env::var("KORVUS_CDP_LOG").is_ok() {
        eprintln!("CDP -> {}", ev);
    }
    write.send(Message::Text(ev.to_string())).await?;
    Ok(())
}

/// A navigation whose document has arrived and been answered for, and has not been loaded yet.
struct Committed {
    /// The document's URL: the RESPONSE's, after every redirect.
    url: String,
    frame_id: String,
    loader: String,
    /// The new document's default execution context, and its `uniqueId`.
    ctx_id: i64,
    ctx_unique: String,
    /// The response, for an http(s) navigation: its bytes and its header block. `None` is the empty
    /// document a non-http url gets.
    document: Option<(Vec<u8>, Vec<(String, String)>)>,
    /// THIS NAVIGATION BUILT THE REALM IT COMMITS INTO (see [`Lazy`]). There is then no
    /// document being left: nothing has run in the realm, it holds no listener, no timer and no
    /// storage of its own, and a successor assembled from the same persona would be
    /// indistinguishable from it. So `finish_navigate` unloads nothing and replaces nothing, and
    /// the page loads in the realm this navigation already paid for instead of a second one.
    first_document: bool,
    /// How the document was reached, handed to the realm that loads it.
    arrival: engine::Arrival,
}

/// The document request of a navigation, shared by `Page.navigate` and a navigation the page asked for:
/// `beforeunload` on the document being left, the request, and the events Chrome sends up to the
/// response -- `frameStartedNavigating`, `frameStartedLoading`, the previous document's detached children
/// and the new document's own `Network.*` rows.
///
/// # The URL is the response's
///
/// This used to `engine.set_url(url)` with the REQUESTED url before the fetch and never look at the
/// response's. Measured, Chrome 150 over loopback: `GET /r301` answered `301 Location:
/// http://127.0.0.1:<other port>/target.html` commits `target.html` -- `Page.frameNavigated.url`,
/// `location.href`, `document.URL` and `document.baseURI` all name it, and a relative link on the page
/// resolves against it. Korvus reported `/r301` for all four and resolved the link against it.
async fn request_document(
    engine: &mut Engine,
    nav: &engine::PageNavigation,
    ids: &mut ConnIds,
    rep: &fetch::Rep,
    first_document: bool,
    opening: Opening<'_>,
) -> anyhow::Result<(Vec<Value>, Committed)> {
    let url = &nav.url;
    // Leaving the current document starts with its `beforeunload`, before the next one is requested.
    // A realm this navigation just built is leaving nothing: there is no document to
    // ask, and dispatching at a window nobody has listened on is work with no observer.
    if !first_document {
        engine.fire_beforeunload();
    }
    // Whatever the document being left requested and has not reported yet belongs to ITS navigation,
    // so it is reported before the ledger starts a new generation.
    let mut events: Vec<Value> = flush_network(rep).into_iter().map(|(_, ev)| ev).collect();
    // A navigation is a NEW document, so it gets a new loader id and a new default
    // execution context -- Chrome mints a fresh `loaderId` per navigation (measured) and
    // carries that ONE value in the result, in `frameNavigated` and in every
    // `lifecycleEvent` of the navigation. That is how a client tells which navigation an
    // event belongs to, so they are all read from `ids` here rather than re-derived.
    let loader = ids.begin_navigation();
    let frame_id = ids.frame_id().to_string();
    let ctx_id = ids.default_context();
    let ctx_unique = ids.context_unique(ctx_id);
    events.push(json!({ "method": "Page.frameStartedNavigating", "params": {
        "frameId": frame_id, "url": url, "loaderId": loader, "navigationType": "differentDocument" } }));
    events.push(json!({ "method": "Page.frameStartedLoading", "params": { "frameId": frame_id } }));
    // A NEW NAVIGATION IS A NEW LEDGER GENERATION, started before the document request so
    // that the document's own row is in it. The `Network.*` events are then exactly
    // this navigation's requests, never the previous page's as well.
    transport::ledger_reset();
    let epoch_s = monotonic_now_s();
    let wall_s = wall_now_s();
    let network_prefix = ids.network_prefix();
    // The previous document's children detach, and THE NEW DOCUMENT'S CONTEXT IS SET BEFORE ITS REQUEST IS SENT:
    // a DevTools pause of that request is reported with it -- its `networkId` is this loader id, which is
    // how Playwright and Puppeteer know a navigation request -- and with the URL asked for, until the redirects are
    // known. These go on the navigating command's session, which is the page's.
    events.extend(
        fetch::rep_lock(rep)
            .begin_navigation(network::NavContext {
                frame_id: frame_id.clone(),
                loader_id: loader.clone(),
                document_url: url.split('#').next().unwrap_or(url).to_string(),
                prefix: network_prefix,
                epoch_s,
                wall_s,
            })
            .into_iter()
            .map(|(_, ev)| ev),
    );
    // Everything so far is on the wire before the request is sent, as Chrome's is: the caller's events, the leaving
    // document's last requests, `frameStartedNavigating`, `frameStartedLoading`. A paused document request is then
    // reported after them.
    let Opening { write, session, before } = opening;
    write.send_all(before.into_iter().chain(events).map(|ev| fetch::stamped(ev, session)).collect()).await?;
    let t_doc = std::time::Instant::now();
    let (final_url, document) = fetch_document(engine, nav).await?;
    navtrace::mark("doc_fetch", t_doc.elapsed().as_secs_f64() * 1000.0);
    // Step 7 (plugin ABI 3 wave 1): a navigation whose response is an attachment is a download. When a plugin takes
    // downloads (`on_download` set), hand it the bytes it came as -- the URL, the name the response proposed, and the
    // body -- in load order. The one place korvus has a download's bytes is the navigation response here.
    // TODO(1021-verify): download detection is `Content-Disposition: attachment` only -- a non-displayable content
    // type is a download in Chrome too; and a true download navigation does NOT also commit a document, whereas this
    // still loads the response below. Suppressing that load is a fuller download pipeline than this delivery seam.
    if plugin_host::wants_downloads() {
        if let Some((body, headers)) = &document {
            if let Some(name) = download_name(headers, &final_url) {
                plugin_host::on_download(&final_url, &name, body);
            }
        }
    }
    // An attachment, while a DevTools client set a download behaviour, is a download and not a document: saved for the
    // client, and the navigation ends there (Chrome's `net::ERR_ABORTED`), the page staying where it was.
    if spec::devtools::download_behavior().is_some() {
        if let Some(name) = document.as_ref().and_then(|(_, headers)| download_name(headers, &final_url)) {
            if let Some((body, headers)) = document {
                let frame = frame_id.clone();
                download(engine, &frame, &final_url, Some(name), Some((body, headers))).await;
                // the document request ends as Chrome's does for a download: aborted, which is how a client learns
                // the navigation it was waiting on will not commit
                if let Some(row) = transport::ledger().records().into_iter().rev().find(|r| r.kind == "document") {
                    transport::ledger().mark_failed(row.id, "net::ERR_ABORTED");
                }
                anyhow::bail!("net::ERR_ABORTED");
            }
        }
    }
    fetch::rep_lock(rep).set_document_url(&final_url);
    // The document's own row is reported before the answer.
    let events: Vec<Value> = flush_network(rep).into_iter().map(|(_, ev)| ev).collect();
    let arrival = engine::arrival_for(nav, &final_url);
    Ok((events, Committed { url: final_url, frame_id, loader, ctx_id, ctx_unique, document, first_document, arrival }))
}

/// Where a navigation's opening events go: onto `write`, stamped with `session`, after `before` --
/// whatever the caller has collected ahead of them -- and before the document request is sent.
struct Opening<'a> {
    write: &'a mut fetch::Out,
    session: Option<&'a str>,
    before: Vec<Value>,
}

/// `Page.navigate`, up to COMMIT: the document request is made and answered, and the command is
/// answered with it. Loading -- parsing, running the page's scripts, the load lifecycle -- is
/// [`finish_navigate`], which runs after the answer is on the wire.
///
/// # Why the answer does not wait for the page
///
/// This used to answer after the WHOLE load, scripts and `load` event included, so a page whose
/// scripts ran long left `Page.navigate` unanswered for as long as they ran. Chrome does not.
/// Measured 2026-09-17, stock `google-chrome --headless=new` 150.0.7871.46, a loopback page whose
/// inline script is `while (Date.now() < t + 40000) {}`, over one page-target WebSocket, times
/// from sending `Page.navigate`:
///
/// ```text
///                                           chrome 150         korvus before
///   Page.navigate answered                     0.05 s              40.09 s
///   Page.frameNavigated                        0.05 s              40.09 s
///   Runtime.evaluate(document.title) sent      0.05 s              40.09 s
///   Runtime.evaluate answered                 40.06 s ("done")     40.09 s ("done")
///   Page.loadEventFired                       40.07 s              40.09 s
/// ```
///
/// So Chrome answers the navigation when the document COMMITS, and the page's scripts run after
/// that; a command sent meanwhile waits for the renderer's long task, exactly as one sent here
/// waits for the load. That fourth row is not a gap: Chrome does not preempt a long task to answer
/// `Runtime.evaluate` either, so neither does this. The parity driver's replay sweep hit the first
/// row (`Page.navigate did not answer within 30.0s`) on seven sites whose oracle answered all of
/// them.
///
/// The document's own `Network.*` events precede the answer, as in Chrome: its request is sent
/// before the navigation commits. `frameNavigated` and the lifecycle still come after the load --
/// this server builds the document and runs its scripts in one step, so there is no earlier moment
/// at which the new document exists to announce.
async fn begin_navigate(
    engine: &mut Engine,
    params: &Value,
    ids: &mut ConnIds,
    rep: &fetch::Rep,
    first_document: bool,
    write: &mut fetch::Out,
    session: Option<&str>,
    reload: bool,
) -> (Result<Value, RpcError>, Vec<Value>, Option<Committed>) {
    let url = params.get("url").and_then(Value::as_str).unwrap_or("").to_string();
    let valid_url = url::Url::parse(&url).is_ok();
    // `Page.navigate` is a GET the protocol asked for, not one the page asked for: its reason is
    // never reported (no `frameRequestedNavigation` precedes it), and `PageNavigation` is only the
    // shape `request_document` reads the URL and the method out of. `Page.reload` reloads in place; the entry a
    // navigation makes is `typed` unless the command named its `transitionType` (D88 step 2, Chrome 150).
    let mut nav = if reload {
        engine::PageNavigation::reload(url, engine::NavigationReason::Other)
    } else {
        engine::PageNavigation::get(url, engine::NavigationReason::Other)
    };
    if !reload {
        nav.transition = Some(params.get("transitionType").and_then(Value::as_str).unwrap_or("typed").to_string());
    }
    let opening = Opening { write, session, before: Vec::new() };
    match request_document(engine, &nav, ids, rep, first_document, opening).await {
        Ok((events, committed)) => {
            let result = json!({ "frameId": committed.frame_id, "loaderId": committed.loader });
            (Ok(result), events, Some(committed))
        }
        // The navigation's start has gone out (`Opening`), so its frame stops loading before the answer -- which is
        // Chrome's: a result naming the frame and the loader, and the network error as `errorText`
        // (`net::ERR_CERT_AUTHORITY_INVALID`, `net::ERR_CONNECTION_REFUSED`, ...), which a client turns into its
        // navigation error.
        Err(e) => {
            let stopped = json!({ "method": "Page.frameStoppedLoading", "params": { "frameId": ids.frame_id() } });
            // a URL that is no URL is refused, as Chrome refuses it
            if !valid_url {
                return (Err(RpcError::server("Cannot navigate to invalid URL".to_string())), vec![stopped], None);
            }
            let result = json!({ "frameId": ids.frame_id(), "loaderId": ids.loader_id(), "errorText": transport::net_error_name(&e) });
            (Ok(result), vec![stopped], None)
        }
    }
}

/// The origin a context is announced with: `scheme://host[:port]` for a network URL, as Chrome's
/// `Runtime.ExecutionContextDescription.origin` reads (measured `http://127.0.0.1:36343`, never the
/// document's path), and the empty string for anything without one.
fn origin_of(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return String::new() };
    if !matches!(scheme, "http" | "https" | "ws" | "wss") {
        return String::new();
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    format!("{scheme}://{host}")
}

/// The origin an execution context is announced with: [`origin_of`], and Chrome's `://` for a document with none
/// (measured: the initial `about:blank`'s context, and a world made on it).
fn context_origin(url: &str) -> String {
    match origin_of(url) {
        o if o.is_empty() => "://".to_string(),
        o => o,
    }
}

/// THE ISOLATED WORLDS A NEW DOCUMENT GETS, in Chrome's order: every world a new-document script names,
/// in the order of the scripts (`InspectorPageAgent::DidCreateMainWorldContext` injects them, making each world it
/// names), then every other world `Page.createIsolatedWorld` made in the frame, in the order it made them (their
/// window proxies re-initialised for the new document) -- each with the scripts that name it, and the origin a
/// made world keeps reporting (`None`: the document's). Measured on Chrome 150 (three
/// runs alike): a script naming `util`, `w2` made on `about:blank`, `w3` made on the first document -- the second
/// document's contexts are the default, `util`, `w2` (origin `://`), `w3`.
fn document_worlds(injected: &[(String, String)], created: &[(String, String)]) -> Vec<(String, Vec<String>, Option<String>)> {
    let mut out: Vec<(String, Vec<String>, Option<String>)> = Vec::new();
    for (world, source) in injected.iter().filter(|(w, _)| !w.is_empty()) {
        match out.iter_mut().find(|(n, _, _)| n == world) {
            Some((_, scripts, _)) => scripts.push(source.clone()),
            None => out.push((world.clone(), vec![source.clone()], None)),
        }
    }
    for (name, origin) in created {
        if !out.iter().any(|(n, _, _)| n == name) {
            out.push((name.clone(), Vec::new(), Some(origin.clone())));
        }
    }
    out
}

/// THE PAGE'S FRAMES AS CHROME REPORTS THEM, kept in step with the engine's (`Engine::devtools_frames`): a frame
/// that is new is `Page.frameAttached`; a new document in one is `Page.frameNavigated` with its own loader, the old
/// document's contexts destroyed (`Runtime.executionContextDestroyed`) and the new one's created -- its realm's, then
/// each isolated world's -- and its lifecycle as it goes (`init`, `DOMContentLoaded`, `load`, `frameStoppedLoading`);
/// a frame that went is `Page.frameDetached`. Run after every command and slice of the page, and inside a navigation
/// before the main document's own `DOMContentLoaded` (its frames load first).
fn frame_events(engine: &mut Engine, ids: &mut ConnIds) -> Vec<Value> {
    let snapshot = engine.devtools_frames();
    if snapshot.is_empty() && ids.frames.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // the frames that went, children first
    let gone: Vec<u32> = ids.frames.iter().rev().filter(|f| !snapshot.iter().any(|s| s.id == f.bc)).map(|f| f.bc).collect();
    for bc in gone {
        let Some(pos) = ids.frames.iter().position(|f| f.bc == bc) else { continue };
        let rec = ids.frames.remove(pos);
        for ctx in ids.drop_frame_contexts(&rec.id) {
            out.push(context_destroyed(ids, ctx));
        }
        out.push(json!({ "method": "Page.frameDetached", "params": { "frameId": rec.id, "reason": "remove" } }));
    }
    let main = ids.frame_id().to_string();
    for f in snapshot {
        let parent = match f.parent {
            None => main.clone(),
            Some(p) => match ids.frame_for(p) {
                Some(r) => r.id.clone(),
                None => continue,
            },
        };
        if ids.frame_for(f.id).is_none() {
            let id = ids::hex32();
            out.push(json!({ "method": "Page.frameAttached", "params": { "frameId": id, "parentFrameId": parent } }));
            ids.frames.push(ids::FrameRec {
                bc: f.id,
                id,
                parent: parent.clone(),
                document: u32::MAX,
                loader: String::new(),
                url: String::new(),
                name: String::new(),
                dcl_sent: false,
                load_sent: false,
            });
        }
        let fid = ids.frame_for(f.id).map(|r| r.id.clone()).unwrap_or_default();
        let origin = context_origin(&f.url);
        // the initial `about:blank` shown while the frame's first navigation is on its way is not its document yet
        if f.provisional && ids.frame_for(f.id).is_some_and(|r| r.document == u32::MAX) {
            continue;
        }
        if ids.frame_for(f.id).is_some_and(|r| r.document != f.document) {
            // a new document: the old one's contexts go, the new one is navigated to and its contexts made
            for ctx in ids.drop_frame_contexts(&fid) {
                out.push(context_destroyed(ids, ctx));
            }
            let loader = ids::hex32();
            if let Some(r) = ids.frames.iter_mut().find(|r| r.bc == f.id) {
                r.document = f.document;
                r.loader = loader.clone();
                r.url = f.url.clone();
                r.name = f.name.clone();
                r.dcl_sent = false;
                r.load_sent = false;
            }
            let rec = ids.frame_for(f.id).cloned().expect("just recorded");
            out.push(json!({ "method": "Page.frameNavigated", "params": { "frame": child_frame_json(&rec), "type": "Navigation" } }));
            let id = ids.mint_context();
            ids.add_live(ids::LiveCtx { id, name: String::new(), realm: Engine::frame_realm_index(f.id), origin: origin.clone(), frame: fid.clone(), default: true });
            out.push(frame_context_event(ids, id, "", &origin, &fid, true));
            out.push(json!({ "method": "Page.lifecycleEvent", "params": { "frameId": fid, "loaderId": loader, "name": "init", "timestamp": monotonic_now_s() } }));
        }
        // the worlds this document has that the client has not been told of (a new-document script's, made as it started)
        for (name, index) in &f.worlds {
            if ids.world_context(*index).is_none() {
                let id = ids.mint_context();
                ids.add_live(ids::LiveCtx { id, name: name.clone(), realm: *index, origin: origin.clone(), frame: fid.clone(), default: false });
                out.push(frame_context_event(ids, id, name, &origin, &fid, false));
            }
        }
        let Some(rec) = ids.frames.iter_mut().find(|r| r.bc == f.id) else { continue };
        let lifecycle = |name: &str, rec: &ids::FrameRec| {
            json!({ "method": "Page.lifecycleEvent", "params": { "frameId": rec.id, "loaderId": rec.loader, "name": name, "timestamp": monotonic_now_s() } })
        };
        if f.dom_content_loaded && !rec.dcl_sent {
            rec.dcl_sent = true;
            out.push(lifecycle("DOMContentLoaded", rec));
        }
        if f.loaded && !rec.load_sent {
            rec.load_sent = true;
            out.push(lifecycle("load", rec));
            out.push(json!({ "method": "Page.frameStoppedLoading", "params": { "frameId": rec.id } }));
        }
    }
    out
}

/// SAME-SITE POPUPS AS TARGETS OF THEIR OWN (decisions.md D89, decided D103). A popup a script opens on its own site
/// stays in its opener's process -- the opener's script reaches into it, as Chrome keeps such a popup in its opener's
/// renderer -- and is a page target all the same: the router is told of it (`router::POPUP_HOSTED_LINE`) and announces
/// it in the context with its `openerId`, relaying its sessions' commands here marked with its target id. This keeps
/// the router told and returns the popup's own events -- its document as `Page.frameNavigated` (its frame id is its
/// target id, as a page's is), its realm's and worlds' contexts, its lifecycle -- each marked for the popup's target.
/// The initial `about:blank` a popup shows while its first navigation is on its way is not announced as its document.
fn popup_events(engine: &mut Engine, ids: &mut ConnIds) -> Vec<(String, Value)> {
    if router::child_ids().is_none() {
        return Vec::new();
    }
    let snapshot = engine.devtools_popups();
    if snapshot.is_empty() && ids.popups.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(String, Value)> = Vec::new();
    // gone or closed: the router is told, and its contexts are no more
    let gone: Vec<u32> = ids.popups.iter().filter(|r| !snapshot.iter().any(|p| p.id == r.bc && !p.closed)).map(|r| r.bc).collect();
    for bc in gone {
        let Some(pos) = ids.popups.iter().position(|r| r.bc == bc) else { continue };
        let rec = ids.popups.remove(pos);
        ids.drop_frame_contexts(&rec.target);
        println!("{}{}", router::POPUP_HOSTED_CLOSE_LINE, json!({ "targetId": rec.target }));
    }
    for p in snapshot.into_iter().filter(|p| !p.closed) {
        if !ids.popups.iter().any(|r| r.bc == p.id) {
            let target = ids::hex32();
            println!("{}{}", router::POPUP_HOSTED_LINE, json!({ "targetId": target, "url": p.url, "name": p.name }));
            ids.popups.push(ids::PopupRec {
                bc: p.id,
                target,
                document: u32::MAX,
                loader: String::new(),
                url: p.url.clone(),
                name: p.name.clone(),
                dcl_sent: false,
                load_sent: false,
            });
        }
        let target = ids.popups.iter().find(|r| r.bc == p.id).map(|r| r.target.clone()).unwrap_or_default();
        let origin = context_origin(&p.url);
        if !p.provisional && ids.popups.iter().any(|r| r.bc == p.id && r.document != p.document) {
            for ctx in ids.drop_frame_contexts(&target) {
                out.push((target.clone(), context_destroyed(ids, ctx)));
            }
            let loader = ids::hex32();
            if let Some(r) = ids.popups.iter_mut().find(|r| r.bc == p.id) {
                r.document = p.document;
                r.loader = loader.clone();
                r.url = p.url.clone();
                r.name = p.name.clone();
                r.dcl_sent = false;
                r.load_sent = false;
            }
            let rec = ids.popups.iter().find(|r| r.bc == p.id).cloned().expect("just recorded");
            out.push((target.clone(), json!({ "method": "Runtime.executionContextsCleared", "params": {} })));
            out.push((target.clone(), json!({ "method": "Page.frameNavigated", "params": { "frame": popup_frame_json(&rec), "type": "Navigation" } })));
            let id = ids.mint_context();
            ids.add_live(ids::LiveCtx { id, name: String::new(), realm: Engine::frame_realm_index(p.id), origin: origin.clone(), frame: target.clone(), default: true });
            out.push((target.clone(), frame_context_event(ids, id, "", &origin, &target, true)));
            out.push((target.clone(), json!({ "method": "Page.lifecycleEvent", "params": { "frameId": target, "loaderId": loader, "name": "init", "timestamp": monotonic_now_s() } })));
        }
        if ids.popups.iter().any(|r| r.bc == p.id && r.document == u32::MAX) {
            continue;
        }
        for (name, index) in &p.worlds {
            if ids.world_context(*index).is_none() {
                let id = ids.mint_context();
                ids.add_live(ids::LiveCtx { id, name: name.clone(), realm: *index, origin: origin.clone(), frame: target.clone(), default: false });
                out.push((target.clone(), frame_context_event(ids, id, name, &origin, &target, false)));
            }
        }
        let Some(rec) = ids.popups.iter_mut().find(|r| r.bc == p.id) else { continue };
        let now = monotonic_now_s();
        if p.dom_content_loaded && !rec.dcl_sent {
            rec.dcl_sent = true;
            out.push((target.clone(), json!({ "method": "Page.lifecycleEvent", "params": { "frameId": target, "loaderId": rec.loader, "name": "DOMContentLoaded", "timestamp": now } })));
            out.push((target.clone(), json!({ "method": "Page.domContentEventFired", "params": { "timestamp": now } })));
        }
        if p.loaded && !rec.load_sent {
            rec.load_sent = true;
            out.push((target.clone(), json!({ "method": "Page.lifecycleEvent", "params": { "frameId": target, "loaderId": rec.loader, "name": "load", "timestamp": now } })));
            out.push((target.clone(), json!({ "method": "Page.loadEventFired", "params": { "timestamp": now } })));
            out.push((target.clone(), json!({ "method": "Page.frameStoppedLoading", "params": { "frameId": target } })));
        }
    }
    out
}

/// A popup's main frame as `Page.Frame` (its id is its target's).
fn popup_frame_json(rec: &ids::PopupRec) -> Value {
    let (bare, fragment) = match rec.url.split_once('#') {
        Some((u, f)) => (u.to_string(), Some(format!("#{f}"))),
        None => (rec.url.clone(), None),
    };
    let mut frame = json!({ "id": rec.target, "loaderId": rec.loader, "url": bare, "domainAndRegistry": "",
        "securityOrigin": context_origin(&rec.url), "mimeType": "text/html" });
    if !rec.name.is_empty() {
        frame["name"] = json!(rec.name);
    }
    if let Some(f) = fragment {
        frame["urlFragment"] = json!(f);
    }
    frame
}

/// An event of popup target `target`'s, onto the wire marked for the router to hand to that target's sessions.
async fn send_popup_event(write: &mut fetch::Out, target: &str, mut ev: Value) -> anyhow::Result<()> {
    ev["korvusTarget"] = json!(target);
    if std::env::var("KORVUS_CDP_LOG").is_ok() {
        eprintln!("CDP -> {ev}");
    }
    write.send(Message::Text(ev.to_string())).await
}

/// A command a client sent to popup target `target` (the router relays it marked so): the page's own handling for
/// what is the popup's -- its frame, its contexts, its navigation, its window's picture and mouse -- and the page's
/// otherwise, the domains whose state is the process's (emulation, network, cookies) or keyed by node and context.
async fn dispatch_popup(
    engine: &mut Lazy<'_>,
    target: &str,
    method: &str,
    params: &Value,
    ids: &mut ConnIds,
    network_on: &mut bool,
) -> (Result<Value, RpcError>, Vec<Value>) {
    let Some(rec) = ids.popups.iter().find(|r| r.target == target).cloned() else {
        return (Err(RpcError::server("No target with given id found".to_string())), vec![]);
    };
    let announced = rec.document != u32::MAX;
    match method {
        "Page.enable" => {
            let mut events = Vec::new();
            if announced {
                events.push(json!({ "method": "Page.frameNavigated", "params": { "frame": popup_frame_json(&rec) } }));
                let now = monotonic_now_s();
                let ev = |name: &str| json!({ "method": "Page.lifecycleEvent", "params": { "frameId": rec.target, "loaderId": rec.loader, "name": name, "timestamp": now } });
                events.push(ev("init"));
                if rec.dcl_sent {
                    events.push(ev("DOMContentLoaded"));
                }
                if rec.load_sent {
                    events.push(ev("load"));
                    events.push(json!({ "method": "Page.loadEventFired", "params": { "timestamp": now } }));
                }
            }
            (Ok(json!({})), events)
        }
        "Page.disable" | "Page.bringToFront" => (Ok(json!({})), vec![]),
        // Chrome sends the frame's lifecycle so far when a client turns lifecycle events on (Playwright listens for
        // them only after `getFrameTree`, so this is how it learns a popup that loaded early has loaded)
        "Page.setLifecycleEventsEnabled" => {
            let mut events = Vec::new();
            if announced && params.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
                let now = monotonic_now_s();
                let ev = |name: &str| json!({ "method": "Page.lifecycleEvent", "params": { "frameId": rec.target, "loaderId": rec.loader, "name": name, "timestamp": now } });
                events.push(ev("init"));
                if rec.dcl_sent {
                    events.push(ev("DOMContentLoaded"));
                }
                if rec.load_sent {
                    events.push(ev("load"));
                    events.push(ev("networkAlmostIdle"));
                    events.push(ev("networkIdle"));
                }
            }
            (Ok(json!({})), events)
        }
        "Page.getFrameTree" => {
            let frame = if announced { popup_frame_json(&rec) } else { json!({ "id": rec.target, "loaderId": rec.loader, "url": rec.url, "domainAndRegistry": "", "securityOrigin": "://", "mimeType": "text/html" }) };
            (Ok(json!({ "frameTree": { "frame": frame, "childFrames": [] } })), vec![])
        }
        // The popup's live contexts, as Chrome announces them to a session that enables `Runtime`. Playwright sends this
        // after `Page.getFrameTree`, so it is listening by then: contexts made earlier -- a popup whose document
        // committed while its client was still setting it up -- reach it here, and later ones as they are made.
        "Runtime.enable" => {
            webapi::devtools_log::set_capture(true);
            let events = ids
                .frame_contexts()
                .into_iter()
                .filter(|c| c.frame == rec.target)
                .collect::<Vec<_>>()
                .into_iter()
                .map(|c| frame_context_event(ids, c.id, &c.name, &c.origin, &c.frame, c.default))
                .collect();
            (Ok(json!({})), events)
        }
        "Runtime.runIfWaitingForDebugger" => (Ok(json!({})), vec![]),
        "Page.createIsolatedWorld" => {
            let world = params.get("worldName").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(id) = ids.live_frame_world(&rec.target, &world) {
                return (Ok(json!({ "executionContextId": id })), vec![]);
            }
            let Some(index) = engine.get().create_frame_world(rec.bc, &world) else {
                return (Err(RpcError::server("Cannot create isolated world".to_string())), vec![]);
            };
            let origin = context_origin(&rec.url);
            let id = ids.mint_context();
            ids.add_live(ids::LiveCtx { id, name: world.clone(), realm: index, origin: origin.clone(), frame: rec.target.clone(), default: false });
            let event = frame_context_event(ids, id, &world, &origin, &rec.target, false);
            (Ok(json!({ "executionContextId": id })), vec![event])
        }
        // every new document of every frame runs the client's scripts already (`devtools_log::set_new_document_scripts`)
        "Page.addScriptToEvaluateOnNewDocument" => (Ok(json!({ "identifier": "1" })), vec![]),
        "Page.navigate" => {
            let url = params.get("url").and_then(Value::as_str).unwrap_or("").to_string();
            if url::Url::parse(&url).is_err() {
                return (Err(RpcError::server("Cannot navigate to invalid URL".to_string())), vec![]);
            }
            engine.get().navigate_popup(rec.bc, &url);
            (Ok(json!({ "frameId": rec.target, "loaderId": ids::hex32() })), vec![])
        }
        "Page.close" | "Korvus.closePopup" => {
            engine.get().close_popup(rec.bc);
            if method == "Korvus.closePopup" {
                // the router closed it: nothing to tell it back
                ids.popups.retain(|r| r.target != target);
                ids.drop_frame_contexts(target);
            }
            (Ok(json!({})), vec![])
        }
        "Page.captureScreenshot" => {
            let format = match params.get("format").and_then(Value::as_str).unwrap_or("png") {
                "jpeg" => spec::ImageFormat::Jpeg,
                "webp" => spec::ImageFormat::Webp,
                _ => spec::ImageFormat::Png,
            };
            let quality = params.get("quality").and_then(Value::as_f64).filter(|_| format != spec::ImageFormat::Png);
            match engine.get().popup_shot(rec.bc, format, quality) {
                Some(bytes) => {
                    use base64::Engine as _;
                    (Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(bytes) })), vec![])
                }
                None => (Err(RpcError::server("Unable to capture screenshot".to_string())), vec![]),
            }
        }
        "Input.dispatchMouseEvent" => {
            let ty = params.get("type").and_then(Value::as_str).unwrap_or("");
            let n = |k: &str| params.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let m = params.get("modifiers").and_then(Value::as_i64).unwrap_or(0);
            let modifiers = webapi::input::Modifiers { alt: m & 1 != 0, ctrl: m & 2 != 0, meta: m & 4 != 0, shift: m & 8 != 0 };
            let button = match params.get("button").and_then(Value::as_str).unwrap_or("") {
                "left" => 0,
                "middle" => 1,
                "right" => 2,
                "back" => 3,
                "forward" => 4,
                _ => webapi::input::NO_BUTTON,
            };
            let held = params.get("buttons").and_then(Value::as_i64).unwrap_or(0) as u16 & 31;
            let click_count = params.get("clickCount").and_then(Value::as_i64).unwrap_or(0) as i32;
            let kind = match ty {
                "mouseMoved" => webapi::input::MouseKind::Move,
                "mousePressed" => webapi::input::MouseKind::Down,
                "mouseReleased" => webapi::input::MouseKind::Up,
                // a wheel and anything else go the page's way
                _ => return dispatch(engine, method, params, Some(""), &mut Vec::new(), ids, network_on).await,
            };
            let buttons = match kind {
                webapi::input::MouseKind::Up => held & !webapi::input::button_bit(button),
                _ => held | webapi::input::button_bit(button),
            };
            engine.get().popup_mouse_event(rec.bc, kind, n("x"), n("y"), button, buttons, click_count, modifiers);
            (Ok(json!({})), vec![])
        }
        // an evaluation that names no context is the popup's
        "Runtime.evaluate" if params.get("contextId").is_none() && params.get("uniqueContextId").is_none() => {
            let mut p = params.clone();
            if let Some(ctx) = ids.frame_contexts().into_iter().find(|c| c.frame == rec.target && c.default).map(|c| c.id) {
                p["contextId"] = json!(ctx);
            }
            dispatch(engine, method, &p, Some(""), &mut Vec::new(), ids, network_on).await
        }
        // the page's for the rest: a session-level `Target.setAutoAttach` announces nothing; emulation, network and
        // cookies are the process's; DOM and object commands are keyed by node and context
        _ => dispatch(engine, method, params, Some(""), &mut Vec::new(), ids, network_on).await,
    }
}

/// A child frame as `Page.Frame`.
fn child_frame_json(rec: &ids::FrameRec) -> Value {
    let (bare, fragment) = match rec.url.split_once('#') {
        Some((u, f)) => (u.to_string(), Some(format!("#{f}"))),
        None => (rec.url.clone(), None),
    };
    let mut frame = json!({ "id": rec.id, "parentId": rec.parent, "loaderId": rec.loader, "name": rec.name, "url": bare,
        "domainAndRegistry": "", "securityOrigin": context_origin(&rec.url), "mimeType": "text/html",
        "secureContextType": "Secure", "crossOriginIsolatedContextType": "NotIsolated", "gatedAPIFeatures": [] });
    if let Some(f) = fragment {
        frame["urlFragment"] = json!(f);
    }
    frame
}

/// The main frame and its frames as `Page.FrameTree`.
fn frame_tree(ids: &ConnIds, main: Value) -> Value {
    fn children(ids: &ConnIds, parent: &str) -> Vec<Value> {
        ids.frames
            .iter()
            .filter(|f| f.parent == parent && f.document != u32::MAX)
            .map(|f| json!({ "frame": child_frame_json(f), "childFrames": children(ids, &f.id) }))
            .collect()
    }
    json!({ "frame": main, "childFrames": children(ids, ids.frame_id()) })
}

/// `Runtime.executionContextCreated` for a frame's context: its own realm's, or an isolated world's.
fn frame_context_event(ids: &mut ConnIds, id: i64, name: &str, origin: &str, frame_id: &str, default: bool) -> Value {
    let aux = if default {
        json!({ "isDefault": true, "type": "default", "frameId": frame_id })
    } else {
        json!({ "isDefault": false, "type": "isolated", "frameId": frame_id })
    };
    json!({ "method": "Runtime.executionContextCreated", "params": { "context": {
        "id": id, "origin": origin, "name": name, "uniqueId": ids.context_unique(id), "auxData": aux,
    }}})
}

/// `Runtime.executionContextDestroyed` for context `id`.
fn context_destroyed(ids: &mut ConnIds, id: i64) -> Value {
    json!({ "method": "Runtime.executionContextDestroyed", "params": { "executionContextId": id, "executionContextUniqueId": ids.context_unique(id) } })
}

/// The frames a client attaching late is told of, as Chrome announces a page's frames to a session: each attached and
/// navigated to its document, parents first.
fn announce_frames(ids: &ConnIds) -> Vec<Value> {
    let mut out = Vec::new();
    for f in ids.frames.iter().filter(|f| f.document != u32::MAX) {
        out.push(json!({ "method": "Page.frameAttached", "params": { "frameId": f.id, "parentFrameId": f.parent } }));
        out.push(json!({ "method": "Page.frameNavigated", "params": { "frame": child_frame_json(f), "type": "Navigation" } }));
        for name in ["init", "DOMContentLoaded", "load"] {
            let sent = match name {
                "DOMContentLoaded" => f.dcl_sent,
                "load" => f.load_sent,
                _ => true,
            };
            if sent {
                out.push(json!({ "method": "Page.lifecycleEvent", "params": { "frameId": f.id, "loaderId": f.loader, "name": name, "timestamp": monotonic_now_s() } }));
            }
        }
    }
    out
}

/// `Runtime.executionContextCreated` for isolated world `name`'s context `id`.
fn world_context_event(ids: &mut ConnIds, id: i64, name: &str, origin: &str, frame_id: &str) -> Value {
    json!({ "method": "Runtime.executionContextCreated", "params": { "context": {
        "id": id, "origin": origin, "name": name, "uniqueId": ids.context_unique(id),
        "auxData": { "isDefault": false, "type": "isolated", "frameId": frame_id }
    }}})
}

/// The engine world a `Runtime.*` command names: `Ok(None)` for the page's own context -- no context
/// named, or the current document's default -- `Ok(Some(index))` for one of its isolated worlds, and Chrome's
/// refusal for any other id (a context a navigation destroyed among them).
fn named_world(ids: &ConnIds, params: &Value, key: &str) -> Result<Option<usize>, RpcError> {
    let id = match (params.get(key).and_then(Value::as_i64), params.get("uniqueContextId").and_then(Value::as_str)) {
        (Some(id), _) => id,
        (None, Some(unique)) => match ids.context_by_unique(unique) {
            Some(id) => id,
            None => return Err(RpcError::server("Cannot find context with specified id".to_string())),
        },
        (None, None) => return Ok(None),
    };
    if id == ids.default_context() {
        return Ok(None);
    }
    match ids.world_index(id) {
        Some(w) => Ok(Some(w)),
        None => Err(RpcError::server("Cannot find context with specified id".to_string())),
    }
}

/// The rest of a navigation: the document being left is unloaded, the committed one loads in a NEW
/// realm, and the events that load produced follow. Takes the engine by value because the new document
/// gets a new one (see `Engine::successor`).
async fn finish_navigate(
    engine: Engine,
    c: Committed,
    injected: &[(String, String)],
    ids: &mut ConnIds,
    rep: &fetch::Rep,
    handover: &mut Handover,
    left_by: Option<engine::NavigationReason>,
    write: &fetch::Out,
    page_session: Option<&str>,
) -> (Engine, Vec<(network::Stamp, Value)>) {
    let Committed { url, frame_id, loader, ctx_id, ctx_unique, document, first_document, arrival } = c;
    ids.committed = true;
    // HTML "unload a document": `pagehide`, `visibilitychange`, `unload` -- after the response, which is
    // when Chrome runs them (measured order `beforeunload pagehide vis:hidden unload`).
    let mut engine = engine;
    // Chrome sends the SECOND `frameStartedLoading` exactly when it never reported the leaving
    // document's `Page.loadEventFired` (measured over all six shapes): a parse a script
    // abandoned, and a `location.*` navigation from a `load` handler. A form submission and a
    // `window.open(url, "_self")` from a `load` handler both have their `loadEventFired` reported,
    // and neither is followed by the second `frameStartedLoading`.
    let left_loading = engine.document_abandoned()
        || (engine.navigated_during_load() && !matches!(left_by, Some(r) if r != engine::NavigationReason::ScriptInitiated));
    // A REALM THIS NAVIGATION BUILT HAS NOTHING TO UNLOAD AND NOTHING TO REPLACE:
    // see `Committed::first_document`. Otherwise the document being left is unloaded and the next
    // one loads in a new realm, which is what a cross-document navigation IS.
    let commit_s;
    let mut engine = if first_document {
        commit_s = monotonic_now_s();
        engine
    } else {
        navtrace::phase("unload", || engine.fire_unload());
        // The COMMIT's own moment, for the `init` lifecycle event below. Read here
        // rather than after the load, because that is when the frame committed.
        commit_s = monotonic_now_s();
        navtrace::phase("successor", || engine.successor())
    };
    // How the document was reached: a realm this navigation built was reached the same way.
    engine.arrive(arrival);
    // A tab's first document: the `about:blank` the tab was created at is an entry of its own in Chrome, which only the
    // tab's own first URL (`auto_toplevel`) replaces; a popup's initial document is replaced by its first.
    if first_document && seeds_blank_entry() {
        engine.seed_blank_history();
    }
    // The frame's isolated worlds, made for the new document as it starts (see `document_worlds`).
    let plan = document_worlds(injected, ids.created_worlds());
    engine.set_new_document_worlds(plan.iter().map(|(name, scripts, _)| (name.clone(), scripts.clone())).collect());
    // The answer is already on the wire, so a load that fails has no command left to fail; Chrome's
    // navigation has committed by this point too. Said where an operator can see it.
    // Boxed: the load's future is large, and wrapped by value it left a page's `load` handler short of V8 stack -- a
    // form's `submit()` from `onload` threw `RangeError: Maximum call stack size exceeded` (`tests/navigation.rs`,
    // `a_form_submits_to_the_url_and_the_context_chrome_chooses`, red by value and green boxed).
    if let Err(e) = reporting_network(Box::pin(load_document(&mut engine, &url, document, injected)), rep, write, page_session).await {
        eprintln!("cdp: loading {url} failed after commit: {e}");
    }
    // Each world the document got is a context of its own, numbered after the document's (Chrome 150: the default 3,
    // `util` 4, `w2` 5). A world a new-document script names reports the document's origin; one
    // `Page.createIsolatedWorld` made reports the origin it was made under.
    let mut world_events = Vec::new();
    for (name, index) in engine.new_document_worlds() {
        let origin = match plan.iter().find(|(n, _, _)| *n == name).and_then(|(_, _, o)| o.clone()) {
            Some(o) => o,
            None => context_origin(&url),
        };
        let id = ids.mint_context();
        ids.add_live_world(id, &name, index, &origin);
        world_events.push(world_context_event(ids, id, &name, &origin, &frame_id));
    }
    // The ledger, as the protocol: every row of this navigation the load did not report as it ran -- see
    // `network.rs` for what is and is not claimed. Rows that finish later are reported by later flushes.
    let net_events = flush_network(rep);
    // Playwright's page.goto waits for these lifecycle/load events.
    //
    // THE TIMESTAMP WAS THE LITERAL `0.0` ON ALL FOUR OF THEM, and that is a fixed value
    // standing in for a measurement: a client that subtracts a navigation's `load` timestamp
    // from its first request's gets a negative number, and `tools/parity/driver.py` computes
    // §6's `load_ms` from exactly that subtraction. They are read from the same monotonic
    // clock the `Network.*` events use, so the two domains are on ONE timeline -- which is
    // the only way `load_ms` and a request's `ms` can be compared at all.
    //
    // AND THEN ALL FOUR WERE ONE `now()` TAKEN HERE, which is a different fixed
    // value standing in for four measurements. `load_document` above returns only after
    // `pump_until_loaded` has noticed the `load` on a 25 ms slice boundary and settled for a
    // further 50 ms, so a timestamp read at this line says the page loaded ~75 ms later than it
    // did -- on a corpus whose oracle's median nav->load is under 100 ms. The engine records the
    // instant each lifecycle event actually fired; `dcl_s`/`load_s` are those instants on this
    // clock, and `done_s` remains the fallback for a document that never reached them.
    let done_s = monotonic_now_s();
    let (dcl_at, loaded_at) = engine.lifecycle_marks();
    let dcl_s = dcl_at.map(monotonic_s_at).unwrap_or(done_s);
    let load_s = loaded_at.map(monotonic_s_at).unwrap_or(done_s);
    // `frame.url` CARRIES NO FRAGMENT: Chrome splits it into `url` and `urlFragment`, and a client
    // that compares frame URLs across a `#`-navigation compares the document, not the anchor
    // (measured, a form whose `action` carries one).
    let (bare, fragment) = match url.split_once('#') {
        Some((u, f)) => (u.to_string(), Some(format!("#{f}"))),
        None => (url.clone(), None),
    };
    let mut frame = json!({ "id": frame_id, "loaderId": loader, "url": bare,
        "domainAndRegistry": "", "securityOrigin": "", "mimeType": "text/html" });
    if let (Some(o), Some(f)) = (frame.as_object_mut(), fragment) {
        o.insert("urlFragment".into(), json!(f));
    }
    let ev = |name: &str, at: f64| json!({ "method": "Page.lifecycleEvent",
        "params": { "frameId": frame_id, "loaderId": loader, "name": name, "timestamp": at } });
    // THE SUBRESOURCES' NETWORK EVENTS COME FIRST: they were requested while the document loaded,
    // before its `load`. The main frame is already known to a client by this point (`Page.enable`
    // announced it), so they resolve against a frame it has registered.
    let mut events = net_events;
    // CHROME'S ORDER (measured on every navigation of the P12d table): the old contexts
    // are cleared, the frame commits, and only then is the new document's context created.
    //
    // `executionContextsCleared` is sent TWICE per commit by Chrome 150, on every navigation measured
    // (Page.navigate and page-initiated alike, with no isolated world created); when the document being
    // left had not finished loading, a second `frameStartedLoading` falls between the two.
    // the document being left takes its frames with it
    let mut page_events: Vec<Value> = ids.frames.drain(..).rev().map(|f| json!({ "method": "Page.frameDetached", "params": { "frameId": f.id, "reason": "remove" } })).collect();
    page_events.push(json!({ "method": "Runtime.executionContextsCleared", "params": {} }));
    if left_loading {
        page_events.push(json!({ "method": "Page.frameStartedLoading", "params": { "frameId": frame_id } }));
    }
    page_events.extend([
        json!({ "method": "Runtime.executionContextsCleared", "params": {} }),
        json!({ "method": "Page.frameNavigated", "params": { "frame": frame, "type": "Navigation" } }),
        json!({ "method": "Runtime.executionContextCreated", "params": { "context": {
            "id": ctx_id, "origin": origin_of(&url), "name": "", "uniqueId": ctx_unique,
            "auxData": { "isDefault": true, "type": "default", "frameId": frame_id }
        }}}),
    ]);
    // the document's isolated worlds right after its own context, as Chrome announces them
    page_events.extend(world_events);
    page_events.push(ev("init", commit_s));
    // its frames, which loaded while it did: before its own `DOMContentLoaded`
    page_events.extend(frame_events(&mut engine, ids));
    // A document a script navigated away from while it was being parsed never reached
    // `DOMContentLoaded`, and one whose `load` handler navigated reached `load` without Chrome reporting
    // `Page.loadEventFired` or `frameStoppedLoading` for it (measured, `navigation.rs`'s P12d table).
    //
    // WHICH OF THE THREE HAPPENS DEPENDS ON THE NAVIGATION. A `location.*` navigation
    // from a `load` handler suppresses `loadEventFired` AND `frameStoppedLoading`; a FORM SUBMISSION
    // from one suppresses neither (it is queued as a task, so the load event finishes first); and
    // `window.open(url, "_self")` from one reports `loadEventFired` LATE -- after the next
    // navigation's request is on the wire -- and never `frameStoppedLoading`. Measured on stock
    // Chrome 150 over loopback; the sequences are in `tests/navigation.rs`.
    let pending = engine.peek_due_navigation();
    let during_load = engine.navigated_during_load();
    if !engine.document_abandoned() {
        page_events.push(ev("DOMContentLoaded", dcl_s));
        page_events.push(json!({ "method": "Page.domContentEventFired", "params": { "timestamp": dcl_s } }));
        // Chrome announces a navigation WHERE THE SCRIPT RAN, which for one started in the `load`
        // handler is between `domContentEventFired` and `loadEventFired`.
        if during_load {
            if let Some(nav) = &pending {
                page_events.extend(announce_navigation(&frame_id, nav));
                handover.announced = true;
            }
        }
        let load_events = |page_events: &mut Vec<Value>| {
            page_events.push(ev("load", load_s));
            page_events.push(json!({ "method": "Page.loadEventFired", "params": { "timestamp": load_s } }));
        };
        let stopped = json!({ "method": "Page.frameStoppedLoading", "params": { "frameId": frame_id } });
        match (during_load, pending.as_ref().map(|n| n.reason)) {
            // Nothing navigated from the load handler: the ordinary lifecycle.
            (false, _) | (true, None) => {
                load_events(&mut page_events);
                // A declarative refresh is scheduled once the document has loaded, and Chrome announces it
                // between `loadEventFired` and `frameStoppedLoading`.
                if let Some(nav) = engine.take_scheduled_navigation() {
                    page_events.push(scheduled_navigation_event(&frame_id, &nav));
                }
                page_events.push(stopped);
            }
            // A form submission: the task runs after the load event, so both still go out -- and the
            // submission's own `frameScheduledNavigation` follows `frameStoppedLoading` rather than
            // preceding the `frameRequestedNavigation`.
            (true, Some(r)) if r.is_form_submission() => {
                load_events(&mut page_events);
                page_events.push(stopped);
            }
            // `window.open(url, "_self")`: the navigation starts inside the handler, so this
            // document's `loadEventFired` lands after the next one's request and its
            // `frameStoppedLoading` never comes -- the frame did not stop, it left.
            (true, Some(engine::NavigationReason::Other)) => {
                let mut late = Vec::new();
                load_events(&mut late);
                handover.late_load = late;
            }
            // `location.*`: Chrome reports neither.
            (true, Some(_)) => {}
        }
    } else if let Some(nav) = &pending {
        // The parse was abandoned, so there is no lifecycle to interleave with: the announcement is
        // all this document has left to say.
        page_events.extend(announce_navigation(&frame_id, nav));
        handover.announced = true;
    }
    events.extend(page_events.into_iter().map(|ev| (network::Stamp::Page, ev)));
    navtrace::flush(&url);
    (engine, events)
}

/// What a finished document still owes the navigation that replaces it.
#[derive(Default)]
struct Handover {
    /// Chrome already reported this navigation's `frameScheduledNavigation` /
    /// `frameRequestedNavigation` where the script ran, so performing it must not report them again.
    announced: bool,
    /// The leaving document's own `load` lifecycle event and `Page.loadEventFired`, which Chrome
    /// reports AFTER the next navigation's request when `window.open(url, "_self")` started it from
    /// inside the load handler.
    late_load: Vec<Value>,
}

/// Chrome's REQUEST-TIME announcement of a navigation a script has just asked for, emitted where the
/// script ran. `frameRequestedNavigation` always; `frameScheduledNavigation` in front of it only for
/// a `location.*` navigation -- a form submission's follows the old document's `frameStoppedLoading`
/// and `window.open(url, "_self")` gets none at all (measured, all three).
fn announce_navigation(frame_id: &str, nav: &engine::PageNavigation) -> Vec<Value> {
    let mut out = Vec::new();
    if nav.reason == engine::NavigationReason::ScriptInitiated {
        out.push(scheduled_navigation_event(frame_id, nav));
    }
    out.push(requested_navigation_event(frame_id, nav));
    out
}

/// `Page.frameRequestedNavigation` for `nav`.
fn requested_navigation_event(frame_id: &str, nav: &engine::PageNavigation) -> Value {
    json!({ "method": "Page.frameRequestedNavigation", "params": {
        "frameId": frame_id, "reason": nav.reason.as_cdp(), "url": nav.url, "disposition": "currentTab" } })
}

/// `Page.frameScheduledNavigation` for `nav` -- deprecated in the protocol and still sent by Chrome 150
/// for every navigation of the P12d table.
fn scheduled_navigation_event(frame_id: &str, nav: &engine::PageNavigation) -> Value {
    json!({ "method": "Page.frameScheduledNavigation", "params": {
        "frameId": frame_id, "delay": seconds(nav.delay_s), "reason": nav.reason.as_cdp(), "url": nav.url } })
}

/// A whole number of seconds as the integer Chrome writes (`"delay":0`), anything else as itself.
fn seconds(s: f64) -> Value {
    if s.fract() == 0.0 && s.abs() < 9.0e15 {
        json!(s as i64)
    } else {
        json!(s)
    }
}

/// A navigation the PAGE asked for: a script's `location` navigation, or a
/// declarative refresh that fell due. Performed exactly as `Page.navigate` performs one, with the events
/// Chrome sends for a navigation it did not get from the protocol in front:
///
/// ```text
/// scriptInitiated   frameScheduledNavigation(delay 0) · frameRequestedNavigation · frameStartedNavigating ·
///                   frameStartedLoading · <request> · frameClearedScheduledNavigation · … frameNavigated …
/// metaTagRefresh    (frameScheduledNavigation already sent after the load) · frameRequestedNavigation · …
/// ```
///
/// A navigation whose document cannot be fetched leaves the current document where it is and says so;
/// Chrome would commit its own error page there, which this server does not have.
async fn page_navigation(
    engine: Engine,
    nav: engine::PageNavigation,
    injected: &[(String, String)],
    ids: &mut ConnIds,
    rep: &fetch::Rep,
    handover: &mut Handover,
    write: &mut fetch::Out,
    page_session: Option<&str>,
) -> (Engine, Vec<(network::Stamp, Value)>) {
    let mut engine = engine;
    let frame_id = ids.frame_id().to_string();
    let mut events: Vec<(network::Stamp, Value)> = Vec::new();
    // The announcement, unless the document being left already made it: Chrome reports
    // `frameRequestedNavigation` where the SCRIPT ran, which for a navigation started during a parse
    // or a `load` handler is before this navigation is performed at all.
    let owed = std::mem::take(handover);
    if !owed.announced {
        events.extend(announce_navigation(&frame_id, &nav).into_iter().map(|ev| (network::Stamp::Page, ev)));
    }
    // A form submission is queued as a task, and Chrome's `frameScheduledNavigation` for it comes
    // when the task RUNS -- after the old document's `frameStoppedLoading`, not with the `ASK`.
    if nav.reason.is_form_submission() {
        events.push((network::Stamp::Page, scheduled_navigation_event(&frame_id, &nav)));
    }
    let cleared = json!({ "method": "Page.frameClearedScheduledNavigation", "params": { "frameId": frame_id } });
    // what is collected so far goes out with the navigation's opening events, ahead of its request (`Opening`)
    let opening = Opening { write, session: page_session, before: events.drain(..).map(|(_, ev)| ev).collect() };
    match request_document(&mut engine, &nav, ids, rep, false, opening).await {
        Ok((head, committed)) => {
            events.extend(head.into_iter().map(|ev| (network::Stamp::Page, ev)));
            // The leaving document's own `loadEventFired`, when Chrome reports it here (see `Handover`).
            events.extend(owed.late_load.into_iter().map(|ev| (network::Stamp::Page, ev)));
            // Only a navigation Chrome ANNOUNCED as scheduled is cleared; `window.open(url, "_self")`
            // is never scheduled and never cleared.
            if nav.reason.is_scheduled() {
                events.push((network::Stamp::Page, cleared));
            }
            // What this navigation has said so far goes out before its document loads: the load reports its requests
            // as they are made (`reporting_network`), and they follow these. A client that has gone is noticed by the
            // caller's next write.
            for (stamp, ev) in events.drain(..) {
                let _ = send_stamped(write, ev, &stamp, page_session).await;
            }
            let (engine, tail) =
                finish_navigate(engine, committed, injected, ids, rep, handover, Some(nav.reason), write, page_session).await;
            events.extend(tail);
            (engine, events)
        }
        Err(e) => {
            eprintln!("cdp: the page's navigation to {} failed: {e}", nav.url);
            events.extend(owed.late_load.into_iter().map(|ev| (network::Stamp::Page, ev)));
            if nav.reason.is_scheduled() {
                events.push((network::Stamp::Page, cleared));
            }
            events.push((network::Stamp::Page, json!({ "method": "Page.frameStoppedLoading",
                "params": { "frameId": frame_id } })));
            (engine, events)
        }
    }
}

/// Returns `(result, events_to_emit)`.
async fn dispatch(
    engine: &mut Lazy<'_>,
    method: &str,
    params: &Value,
    session: Option<&str>,
    injected_scripts: &mut Vec<(String, String)>,
    ids: &mut ConnIds,
    network_enabled: &mut bool,
) -> (Result<Value, RpcError>, Vec<Value>) {
    match method {
        "Browser.getVersion" => (Ok(VersionInfo::of(&*engine.get().persona()).cdp()), vec![]),

        // Arm the Network domain. Chrome answers `{}` and emits nothing until a request happens;
        // this server does the same, and `begin_navigate` and `finish_navigate` are where the requests happen.
        "Network.enable" => {
            *network_enabled = true;
            // each response's body is kept for `Network.getResponseBody` while the domain is on, and the page's
            // WebSockets are reported (`Network.webSocket*`)
            transport::keep_bodies(true);
            transport::websocket::capture_devtools(true);
            (Ok(json!({})), vec![])
        }

        // A response's body, as the network delivered it (content coding removed): text as itself, anything else in
        // base64. The id is a `Network.*` event's (`network::request_id`): the loader's for the document, else
        // `<prefix>.<row>`. Chrome's refusal for one it no longer has: "No resource with given identifier found".
        "Network.getResponseBody" => {
            let rid = params.get("requestId").and_then(Value::as_str).unwrap_or("");
            let row = if rid == ids.loader_id() {
                transport::ledger().records().into_iter().find(|r| r.kind == "document").map(|r| r.id)
            } else {
                rid.rsplit_once('.').and_then(|(_, n)| n.parse::<u64>().ok())
            };
            let found = row.and_then(|r| transport::response_body(r).map(|b| (r, b)));
            match found {
                Some((r, body)) => {
                    let headers = transport::ledger().record(r).map(|rec| rec.headers).unwrap_or_default();
                    let mime = transport::mime::mime_type_and_charset(&headers).mime.to_ascii_lowercase();
                    let texty = mime.starts_with("text/") || mime.contains("json") || mime.contains("javascript") || mime.contains("xml") || mime.is_empty();
                    match (texty, std::str::from_utf8(&body)) {
                        (true, Ok(text)) => (Ok(json!({ "body": text, "base64Encoded": false })), vec![]),
                        _ => {
                            use base64::Engine as _;
                            (Ok(json!({ "body": base64::engine::general_purpose::STANDARD.encode(&*body), "base64Encoded": true })), vec![])
                        }
                    }
                }
                None => (Err(RpcError::server("No resource with given identifier found".to_string())), vec![]),
            }
        }

        // Headers every request of the page carries from now on, over its own of the same name (`Network.Headers`, an
        // object of name -> value).
        "Network.setExtraHTTPHeaders" => {
            let headers: Vec<(String, String)> = params
                .get("headers")
                .and_then(Value::as_object)
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect())
                .unwrap_or_default();
            spec::devtools::set_extra_headers(headers);
            (Ok(json!({})), vec![])
        }

        // Offline (`context.setOffline`): every request fails `net::ERR_INTERNET_DISCONNECTED` and `navigator.onLine`
        // reads false. The throughput and latency fields are not emulated.
        "Network.emulateNetworkConditions" => {
            spec::devtools::set_offline(params.get("offline").and_then(Value::as_bool).unwrap_or(false));
            (Ok(json!({})), vec![])
        }

        // The user agent a client asks for, over the persona's (decisions.md D100): `User-Agent` and
        // `navigator.userAgent`, `navigator.platform`, the client hints from `userAgentMetadata` (none without it, as
        // Chrome sends none), and `acceptLanguage` as `Accept-Language` and `navigator.languages`. An empty
        // `userAgent` keeps the persona's (Playwright sends one with a locale and no user agent). The engine under the
        // strings stays what it is, as Chrome's does under its override.
        "Network.setUserAgentOverride" | "Emulation.setUserAgentOverride" => {
            let st = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let metadata = params.get("userAgentMetadata").filter(|m| m.is_object()).map(ua_metadata);
            spec::devtools::set_user_agent(Some(spec::devtools::UaOverride {
                user_agent: st("userAgent"),
                accept_language: st("acceptLanguage"),
                platform: st("platform"),
                metadata,
            }));
            (Ok(json!({})), vec![])
        }

        "Security.setIgnoreCertificateErrors" => {
            spec::devtools::set_ignore_cert_errors(params.get("ignore").and_then(Value::as_bool).unwrap_or(false));
            (Ok(json!({})), vec![])
        }

        // A zone for the page (`newContext({timezoneId})`): `TZ`, and V8 and ICU told to read it again. Empty clears
        // it back to the context's own. Before the page's realm exists it is simply what the realm starts with.
        "Emulation.setTimezoneOverride" => {
            let tz = params.get("timezoneId").and_then(Value::as_str).unwrap_or("");
            engine::set_timezone_override((!tz.is_empty()).then_some(tz));
            if engine.built() {
                engine.get().time_zone_changed();
            }
            (Ok(json!({})), vec![])
        }
        // The page's locale (`newContext({locale})`): ICU's default -- what a bare `Intl` formatter and
        // `toLocaleString()` resolve to. Before the realm exists (a client sets it on a new page) the realm starts
        // in it; a realm already running gets it from its next document.
        "Emulation.setLocaleOverride" => {
            let locale = params.get("locale").and_then(Value::as_str).unwrap_or("");
            engine::set_locale_override((!locale.is_empty()).then_some(locale));
            (Ok(json!({})), vec![])
        }

        // The viewport a client emulates (`newContext({viewport})`, `page.setViewportSize`): the layout viewport,
        // `innerWidth`/`innerHeight`, `@media` and the painted frame follow, with `devicePixelRatio` and the screen
        // when it names them; the page gets its `resize`.
        "Emulation.setDeviceMetricsOverride" => {
            let n = |k: &str| params.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            spec::devtools::set_device_metrics(Some(spec::devtools::DeviceMetrics {
                width: n("width"),
                height: n("height"),
                device_scale_factor: n("deviceScaleFactor"),
                mobile: params.get("mobile").and_then(Value::as_bool).unwrap_or(false),
                screen_width: n("screenWidth"),
                screen_height: n("screenHeight"),
            }));
            if engine.built() {
                engine.get().viewport_changed();
            }
            (Ok(json!({})), vec![])
        }
        "Emulation.clearDeviceMetricsOverride" => {
            spec::devtools::set_device_metrics(None);
            if engine.built() {
                engine.get().viewport_changed();
            }
            (Ok(json!({})), vec![])
        }
        // `page.emulateMedia`, `newContext({colorScheme, reducedMotion})`: the media type and the preferences named.
        "Emulation.setEmulatedMedia" => {
            let features: Vec<(String, String)> = params
                .get("features")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|f| Some((f.get("name")?.as_str()?.to_string(), f.get("value").and_then(Value::as_str).unwrap_or("").to_string())))
                        .collect()
                })
                .unwrap_or_default();
            spec::devtools::set_emulated_media(Some(spec::devtools::EmulatedMedia {
                media: params.get("media").and_then(Value::as_str).unwrap_or("").to_string(),
                features,
            }));
            (Ok(json!({})), vec![])
        }
        "Emulation.setScriptExecutionDisabled" => {
            spec::devtools::set_scripts_disabled(params.get("value").and_then(Value::as_bool).unwrap_or(false));
            (Ok(json!({})), vec![])
        }
        // A position for `navigator.geolocation` (`context.setGeolocation`); no coordinates at all is "position
        // unavailable".
        "Emulation.setGeolocationOverride" => {
            let n = |k: &str| params.get(k).and_then(Value::as_f64);
            let g = match (n("latitude"), n("longitude")) {
                (Some(latitude), Some(longitude)) => Some(spec::devtools::Geolocation { latitude, longitude, accuracy: n("accuracy").unwrap_or(1.0) }),
                _ => None,
            };
            spec::devtools::set_geolocation(Some(g));
            (Ok(json!({})), vec![])
        }
        "Emulation.clearGeolocationOverride" => {
            spec::devtools::set_geolocation(None);
            (Ok(json!({})), vec![])
        }
        // The colour under the page in its pictures (`screenshot({omitBackground})` sends one with no alpha).
        "Emulation.setDefaultBackgroundColorOverride" => {
            let c = params.get("color").filter(|c| c.is_object()).map(|c| {
                let ch = |k: &str| c.get(k).and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 255.0) as u8;
                let a = c.get("a").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.0, 1.0);
                [ch("r"), ch("g"), ch("b"), (a * 255.0).round() as u8]
            });
            spec::devtools::set_background(c);
            (Ok(json!({})), vec![])
        }
        // No Content Security Policy enforced (`newContext({bypassCSP})`).
        "Page.setBypassCSP" => {
            spec::devtools::set_bypass_csp(params.get("enabled").and_then(Value::as_bool).unwrap_or(false));
            (Ok(json!({})), vec![])
        }
        // One page per window: it is already in front.
        "Page.bringToFront" => (Ok(json!({})), vec![]),
        // A file chooser the page opens is the client's (`page.on('filechooser')`): `Page.fileChooserOpened`, and the
        // client sets the control's files.
        "Page.setInterceptFileChooserDialog" => {
            webapi::devtools_log::set_intercept_file_chooser(params.get("enabled").and_then(Value::as_bool).unwrap_or(false));
            (Ok(json!({})), vec![])
        }
        // The router's: how this page's context downloads (`Browser.setDownloadBehavior`).
        "Korvus.setDownloadBehavior" => {
            let behavior = params.get("behavior").and_then(Value::as_str).unwrap_or("default").to_string();
            let path = params.get("downloadPath").and_then(Value::as_str).unwrap_or("").to_string();
            spec::devtools::set_download_behavior(Some((behavior, path)));
            (Ok(json!({})), vec![])
        }
        // The router's: the permissions a client granted this page's context (`Browser.grantPermissions`).
        "Korvus.setPermissions" => {
            let list = params.get("permissions").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect());
            spec::devtools::set_permissions(list);
            (Ok(json!({})), vec![])
        }

        // A global function the page calls to reach the client (`page.exposeFunction`, `exposeBinding`): each call is a
        // `Runtime.bindingCalled` with its string argument, in the document now and in every later one.
        "Runtime.addBinding" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                return (Err(RpcError::invalid_params("Invalid parameters".to_string())), vec![]);
            }
            if engine.built() {
                engine.get().add_devtools_binding(name);
            } else {
                webapi::devtools_log::add_binding(name);
            }
            (Ok(json!({})), vec![])
        }
        "Runtime.removeBinding" => {
            webapi::devtools_log::remove_binding(params.get("name").and_then(Value::as_str).unwrap_or(""));
            (Ok(json!({})), vec![])
        }

        // THE COOKIE STORE AS DEVTOOLS SEES IT -- every cookie, HttpOnly ones included -- read and
        // written as stock Chrome 150 does (`chrome150-a1s3.json`, `chrome150-a1s3c.json`). Under `korvus browser`
        // each page target is a process of its own, so this is that page's store.
        "Network.getCookies" => {
            let urls: Vec<String> = match params.get("urls").and_then(Value::as_array) {
                Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                // Chrome's default: the page's URL and its frames' (`ComputeCookieURLs`); the page's here
                None => vec![if engine.built() { engine.get().current_url() } else { "about:blank".to_string() }],
            };
            let cookies: Vec<Value> = transport::cookies::devtools_cookies(Some(&urls)).iter().map(network::cookie_json).collect();
            (Ok(json!({ "cookies": cookies })), vec![])
        }
        "Network.getAllCookies" | "Storage.getCookies" => {
            let cookies: Vec<Value> = transport::cookies::devtools_cookies(None).iter().map(network::cookie_json).collect();
            (Ok(json!({ "cookies": cookies })), vec![])
        }
        "Network.setCookie" => match transport::cookies::devtools_set(&network::cookie_init(params)) {
            Ok(success) => (Ok(json!({ "success": success })), vec![]),
            Err(m) => (Err(RpcError::invalid_params(m.to_string())), vec![]),
        },
        "Network.setCookies" | "Storage.setCookies" => {
            let list: Vec<transport::cookies::DevtoolsCookieInit> =
                params.get("cookies").and_then(Value::as_array).map(|l| l.iter().map(network::cookie_init).collect()).unwrap_or_default();
            if transport::cookies::devtools_set_all(&list) {
                (Ok(json!({})), vec![])
            } else {
                (Err(RpcError::invalid_params("Invalid cookie fields".to_string())), vec![])
            }
        }
        "Network.deleteCookies" => {
            let s = |k: &str| params.get(k).and_then(Value::as_str);
            let partition = params.get("partitionKey").and_then(|k| {
                let site = k.get("topLevelSite").and_then(Value::as_str).filter(|s| !s.is_empty())?;
                Some((site.to_string(), k.get("hasCrossSiteAncestor").and_then(Value::as_bool).unwrap_or(false)))
            });
            match transport::cookies::devtools_delete(s("name").unwrap_or(""), s("url"), s("domain"), s("path"), partition) {
                Ok(()) => (Ok(json!({})), vec![]),
                Err(m) => (Err(RpcError::invalid_params(m.to_string())), vec![]),
            }
        }
        "Network.clearBrowserCookies" | "Storage.clearCookies" => {
            transport::cookies::clear();
            (Ok(json!({})), vec![])
        }
        // DevTools' "Disable cache" (`transport::set_cache_disabled`): nothing is read from the HTTP cache while it is
        // set, and every request says so on the wire, as Chrome 150's did. Puppeteer sets it with request
        // interception.
        "Network.setCacheDisabled" => {
            transport::set_cache_disabled(params.get("cacheDisabled").and_then(Value::as_bool).unwrap_or(false));
            (Ok(json!({})), vec![])
        }

        // Attach the (single) page target in flatten mode — ONLY for the browser-level call
        // (no session). The page-level setAutoAttach (with session) is for child frames/workers,
        // of which we have none, so it must NOT re-attach the page (Playwright: "Duplicate target").
        "Target.setAutoAttach" => {
            if session.is_none() {
                (
                    Ok(json!({})),
                    vec![json!({
                        "method": "Target.attachedToTarget",
                        "params": {
                            "sessionId": ids.session_id(),
                            "targetInfo": ids.target_info(),
                            "waitingForDebugger": false
                        }
                    })],
                )
            } else {
                (Ok(json!({})), vec![])
            }
        }
        // The page target is already advertised via attachedToTarget; don't re-announce it here.
        "Target.setDiscoverTargets" => (Ok(json!({})), vec![]),
        "Target.getTargets" => (Ok(json!({ "targetInfos": [ids.target_info()] })), vec![]),
        "Target.getTargetInfo" => (Ok(json!({ "targetInfo": ids.target_info() })), vec![]),
        // The page is already attached via auto-attach; just hand back its session (no re-attach
        // event, or Playwright errors "Duplicate target").
        "Target.attachToTarget" => (Ok(json!({ "sessionId": ids.session_id() })), vec![]),

        // The default execution context is advertised on runIfWaitingForDebugger (below), which
        // Playwright sends LATE — after it has processed Page.getFrameTree and registered the main
        // frame. Emitting it on Runtime.enable races that registration: Playwright's
        // _onExecutionContextCreated drops the context if frameManager.frame(frameId) isn't known yet.
        "Runtime.enable" => {
            engine.runtime_stacks(true);
            // the page's console calls and unhandled errors are reported from now on
            webapi::devtools_log::set_capture(true);
            (Ok(json!({})), vec![])
        }
        "Runtime.disable" => {
            engine.runtime_stacks(false);
            webapi::devtools_log::set_capture(false);
            (Ok(json!({})), vec![])
        }

        // THE CONTEXT A CLIENT IS ANNOUNCED IS THE ONE THE DOCUMENT IS RUNNING IN.
        // This said `1` and `""` unconditionally, which was true of the only client a target ever
        // had; a client attaching to a target that has already navigated was told about a context
        // that no longer exists, while its evaluates ran in the one that does. Chrome announces a
        // late-attaching client the LIVE context -- same id, same uniqueId, same origin as the
        // client before it was given (measured, Chrome 150.0.7871.46).
        "Runtime.runIfWaitingForDebugger" => {
            let id = ids.default_context();
            // Chrome's `://` for a document with no origin, the initial `about:blank`'s (measured).
            let origin = context_origin(&if engine.built() { engine.get().current_url() } else { "about:blank".to_string() });
            let mut events = vec![json!({
                "method": "Runtime.executionContextCreated",
                "params": { "context": {
                    "id": id, "origin": origin, "name": "",
                    "uniqueId": ids.context_unique(id),
                    "auxData": { "isDefault": true, "type": "default", "frameId": ids.frame_id() }
                }}
            })];
            // and the document's isolated worlds, which a client attaching late is told about as Chrome tells it
            // every live context
            let frame_id = ids.frame_id().to_string();
            for (wid, name, worigin) in ids.live_worlds() {
                events.push(world_context_event(ids, wid, &name, &worigin, &frame_id));
            }
            // and every frame's
            for c in ids.frame_contexts() {
                events.push(frame_context_event(ids, c.id, &c.name, &c.origin, &c.frame, c.default));
            }
            (Ok(json!({})), events)
        }

        "Runtime.evaluate" => {
            let expr = params.get("expression").and_then(Value::as_str).unwrap_or("").to_string();
            let rbv = params.get("returnByValue").and_then(Value::as_bool).unwrap_or(false);
            let await_promise = params.get("awaitPromise").and_then(Value::as_bool).unwrap_or(false);
            // The context it runs in -- the page's, or one of its isolated worlds -- and that context's
            // id in the object ids it hands out.
            let world = match named_world(ids, params, "contextId") {
                Ok(w) => w,
                Err(e) => return (Err(e), vec![]),
            };
            let ctx = world.and_then(|w| ids.world_context(w)).unwrap_or(ids.default_context());
            // Flush ready microtasks/resolved async before reading state -- and NOTHING ELSE. This
            // was `pump`, whose budget is a duration to wait rather than a ceiling, so a page with a
            // heartbeat timer (which is most pages) paid the full EVAL_PUMP_MS on EVERY evaluate:
            // measured over the replay tier on 2026-09-18, a `Runtime.evaluate` of `1` cost 127.7 ms
            // against Chrome's 0.7 and the §6 probe cost 133 ms to do 3 ms of work. See
            // `Engine::flush_ready`; the budget is still the ceiling it always was.
            let _ = engine.get().flush_ready(std::time::Duration::from_millis(env_ms("KORVUS_EVAL_PUMP_MS", EVAL_PUMP_MS_DEFAULT))).await;
            // A THROW IS A RESULT, NOT AN ERROR. This used to answer
            // `-32000 "execute_script failed: Error: boom\n    at cdp:Runtime.evaluate:1:7"`, which
            // was a tell twice over and unusable to every client that reads `exceptionDetails`.
            // See `engine::EvalOutcome` for Chrome's measured shape.
            let r = if await_promise {
                // CDP awaitPromise: a promise still pending is answered when it settles, from the connection's loop
                // (`Deferred`), which goes on answering the client's other commands meanwhile -- as Chrome does. A
                // page that awaits something only the client can give it (a binding's answer) needs that.
                match engine.get().eval_remote_begin_in(world, expr, rbv) {
                    engine::Awaited::Ready(outcome) => Ok(evaluate_result(engine.get(), outcome, ids, ctx)),
                    engine::Awaited::Pending(h) => Ok(Deferred::marker(h, ctx, rbv)),
                }
            } else {
                let outcome = engine.get().eval_remote_details_in(world, expr, rbv);
                Ok(evaluate_result(engine.get(), outcome, ids, ctx))
            };
            (r, vec![])
        }

        "Runtime.callFunctionOn" => {
            let fn_decl =
                params.get("functionDeclaration").and_then(Value::as_str).unwrap_or("").to_string();
            let rbv = params.get("returnByValue").and_then(Value::as_bool).unwrap_or(false);
            let this_id = params.get("objectId").and_then(Value::as_str).and_then(ids::parse_object_id);
            // The context named, else the object's -- an `objectId` carries its context's id
            let ctx_of_object = params.get("objectId").and_then(Value::as_str).and_then(|o| o.split('.').nth(1)).and_then(|c| c.parse::<i64>().ok());
            let world = match named_world(ids, params, "executionContextId") {
                Ok(w) => w,
                Err(e) => return (Err(e), vec![]),
            };
            let ctx = match world.and_then(|w| ids.world_context(w)) {
                Some(c) => c,
                None => ctx_of_object.filter(|c| ids.world_index(*c).is_some()).unwrap_or(ids.default_context()),
            };
            let args = parse_args(params.get("arguments"));
            let _ = engine.get().flush_ready(std::time::Duration::from_millis(env_ms("KORVUS_EVAL_PUMP_MS", EVAL_PUMP_MS_DEFAULT))).await;
            // Same rule as Runtime.evaluate, and this is the path that matters most: Playwright's
            // `page.evaluate` is a callFunctionOn, so a thrown page error reached it as
            // `"function call returned nothing / threw"` and nothing else.
            let in_world = world.or_else(|| ids.world_index(ctx));
            // `awaitPromise` as `Runtime.evaluate` honours it: an `async` function's promise is awaited, bounded
            // the same way. Playwright runs every action (`fill`, `click`, its actionability checks) as one.
            if params.get("awaitPromise").and_then(Value::as_bool).unwrap_or(false) {
                // answered when it settles, as `Runtime.evaluate`'s awaited promise is (`Deferred`)
                let r = match engine.get().call_function_on_begin_in(in_world, &fn_decl, this_id, args, rbv) {
                    Ok(engine::Awaited::Ready(outcome)) => Ok(evaluate_result(engine.get(), outcome, ids, ctx)),
                    Ok(engine::Awaited::Pending(h)) => Ok(Deferred::marker(h, ctx, rbv)),
                    Err(e) => Err(RpcError::from(e)),
                };
                return (r, vec![]);
            }
            let called = engine.get().call_function_on_in(in_world, &fn_decl, this_id, args, rbv);
            let r = called.map(|outcome| evaluate_result(engine.get(), outcome, ids, ctx)).map_err(RpcError::from);
            (r, vec![])
        }

        // The handle is dropped, so the value (a DOM node's wrapper, an array of element handles) stops being pinned
        // until the next navigation. It was a no-op, and a client that disposes every handle it takes -- Puppeteer's
        // `JSHandle.dispose`, Playwright's -- held all of them for the document's life. V8's inspector answers: a
        // malformed id "Invalid remote object id", an unknown one "Could not find object with given id"
        // (`V8RuntimeAgentImpl::releaseObject` -> `InjectedScript::ObjectScope::findInjectedScript`).
        "Runtime.releaseObject" => {
            let oid = params.get("objectId").and_then(Value::as_str).unwrap_or("");
            let Some(id) = ids::parse_object_id(oid) else {
                return (Err(RpcError::server("Invalid remote object id".to_string())), vec![]);
            };
            if engine.get().release_object(id) {
                (Ok(json!({})), vec![])
            } else {
                (Err(RpcError::server("Could not find object with given id".to_string())), vec![])
            }
        }
        // No result here carries an `objectGroup` (no evaluation records one), so there is no group to drop.
        "Runtime.releaseObjectGroup" => (Ok(json!({})), vec![]),

        // An object's own properties, each value a RemoteObject (an object stored and named, as an evaluation's
        // result is). Puppeteer's `$$` / `$$eval` / `JSHandle.getProperties` read an array of element handles this way
        // (Chrome 150, `chrome150-compat-traffic.log`). Read by the engine through V8's property API: no getter and no
        // page-replaceable `Object.*` runs. `ownProperties: false` (the prototype chain's accessors too) answers as
        // `true` does -- what Chrome adds there is not listed here.
        "Runtime.getProperties" => {
            let oid = params.get("objectId").and_then(Value::as_str).unwrap_or("");
            let Some(id) = ids::parse_object_id(oid) else {
                return (Err(RpcError::server("Invalid remote object id".to_string())), vec![]);
            };
            let ctx = oid.split('.').nth(1).and_then(|c| c.parse::<i64>().ok()).unwrap_or(ids.default_context());
            let Some((rows, proto)) = engine.get().own_properties(id) else {
                return (Err(RpcError::server("Invalid remote object id".to_string())), vec![]);
            };
            let accessors_only = params.get("accessorPropertiesOnly").and_then(Value::as_bool).unwrap_or(false);
            let non_indexed_only = params.get("nonIndexedPropertiesOnly").and_then(Value::as_bool).unwrap_or(false);
            let mut result = Vec::new();
            for r in rows {
                if (accessors_only && r.get.is_none() && r.set.is_none()) || (non_indexed_only && !r.symbol && r.name.parse::<u32>().is_ok()) {
                    continue;
                }
                let mut d = json!({ "name": r.name });
                if let Some(v) = r.value {
                    d["value"] = prop_value(engine.get(), v, ids, ctx);
                }
                if let Some(w) = r.writable {
                    d["writable"] = json!(w);
                }
                if let Some(g) = r.get {
                    d["get"] = prop_value(engine.get(), g, ids, ctx);
                }
                if let Some(s) = r.set {
                    d["set"] = prop_value(engine.get(), s, ids, ctx);
                }
                d["configurable"] = json!(r.configurable);
                d["enumerable"] = json!(r.enumerable);
                d["isOwn"] = json!(true);
                result.push(d);
            }
            let mut answer = json!({ "result": result });
            if let Some(p) = proto {
                answer["internalProperties"] = json!([{ "name": "[[Prototype]]", "value": prop_value(engine.get(), p, ids, ctx) }]);
            }
            (Ok(answer), vec![])
        }

        // Touch emulation OFF is this persona as it is (no touch points: a desktop), so it is answered as Chrome does;
        // ON would change `maxTouchPoints`, the touch events and the pointer media features, which this engine does not
        // emulate -- it says so rather than accepting it. Puppeteer sends `{enabled: false}` for every new page.
        "Emulation.setTouchEmulationEnabled" => {
            // a client emulating a touch device (`newContext({hasTouch})`): `maxTouchPoints`, and `(pointer: coarse)`
            // with no hover; off is the persona as it is
            let on = params.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let points = params.get("maxTouchPoints").and_then(Value::as_i64).unwrap_or(1) as i32;
            spec::devtools::set_touch_points(on.then_some(points.max(1)));
            if on && engine.built() {
                engine.get().touch_emulation_changed();
            }
            (Ok(json!({})), vec![])
        }

        // `Page.navigate` is not dispatched here: it answers at commit and loads after its answer is
        // on the wire, which a `(result, events)` return cannot express. See `begin_navigate`.

        // An ISOLATED WORLD: a context of its own over the frame's document -- Playwright's utility world
        // among them. It answered context 2 and every evaluation in it ran in the page's context. Chrome
        // (`InspectorPageAgent::createIsolatedWorld`): the world named `worldName` (one per name -- a name the
        // document already has is the same context, and nothing is announced), its context announced with the
        // document's origin, and the world the frame's from then on: every later document gets it too
        // (`document_worlds`).
        "Page.createIsolatedWorld" => {
            // No worldName means no name: Chrome announces such a context with `"name": ""`
            // (measured), so there is nothing here to fall back to.
            let world = params.get("worldName").and_then(Value::as_str).unwrap_or("").to_string();
            let frame_id = ids.frame_id().to_string();
            // a frame's: a world over its current document
            if let Some(child) = params.get("frameId").and_then(Value::as_str).filter(|f| *f != frame_id) {
                let Some(bc) = ids.bc_for(child) else {
                    return (Err(RpcError::invalid_params("No frame for given id found".to_string())), vec![]);
                };
                let child = child.to_string();
                if let Some(id) = ids.live_frame_world(&child, &world) {
                    return (Ok(json!({ "executionContextId": id })), vec![]);
                }
                let origin = context_origin(&ids.frame_for(bc).map(|f| f.url.clone()).unwrap_or_default());
                let Some(index) = engine.get().create_frame_world(bc, &world) else {
                    return (Err(RpcError::server("Cannot create isolated world".to_string())), vec![]);
                };
                let id = ids.mint_context();
                ids.add_live(ids::LiveCtx { id, name: world.clone(), realm: index, origin: origin.clone(), frame: child.clone(), default: false });
                let event = frame_context_event(ids, id, &world, &origin, &child, false);
                return (Ok(json!({ "executionContextId": id })), vec![event]);
            }
            if let Some(id) = ids.live_world(&world) {
                return (Ok(json!({ "executionContextId": id })), vec![]);
            }
            let origin = context_origin(&if engine.built() { engine.get().current_url() } else { "about:blank".to_string() });
            let index = engine.get().create_isolated_world();
            let id = ids.mint_context();
            ids.add_live_world(id, &world, index, &origin);
            ids.remember_created_world(&world, &origin);
            let event = world_context_event(ids, id, &world, &origin, &frame_id);
            (Ok(json!({ "executionContextId": id })), vec![event])
        }

        // The CURRENT document's loader, not a constant: before any navigation that is the one
        // `Page.enable` announced, and after one it is that navigation's.
        // D88 step 2: the tab's session history, as Chrome's browser keeps it (`Engine::navigation_history`).
        "Page.getNavigationHistory" => {
            let (index, entries) = if engine.built() {
                engine.get().navigation_history()
            } else {
                let blank = engine::NavEntry { serial: 0, url: "about:blank".into(), title: String::new(), transition: "auto_toplevel".into() };
                (0, if seeds_blank_entry() { vec![blank] } else { Vec::new() })
            };
            let entries: Vec<Value> = entries
                .into_iter()
                .map(|e| json!({ "id": e.serial, "url": e.url, "userTypedURL": e.url, "title": e.title, "transitionType": e.transition }))
                .collect();
            (Ok(json!({ "currentIndex": index, "entries": entries })), vec![])
        }
        "Page.navigateToHistoryEntry" => {
            let id = params.get("entryId").and_then(Value::as_u64);
            if id.is_some_and(|id| engine.built() && engine.get().traverse_to_entry(id)) {
                (Ok(json!({})), vec![])
            } else {
                (Err(RpcError::invalid_params("No entry with passed id".into())), vec![])
            }
        }

        "Page.getFrameTree" => {
            // the frames as they are now: any the client has not been told of are announced first
            let events = frame_events(engine.get(), ids);
            let main = json!({ "id": ids.frame_id(), "loaderId": ids.loader_id(), "url": engine.get().current_url(), "domainAndRegistry": "", "securityOrigin": "", "mimeType": "text/html" });
            (Ok(json!({ "frameTree": frame_tree(ids, main) })), events)
        }

        // On Page.enable, mark the frame as loaded so Playwright's frame is "ready" and its main
        // execution context (from Runtime.enable) is usable.
        //
        // WHICH DOCUMENT IT ANNOUNCES IS THE ONE THAT IS THERE. The url was the
        // literal `about:blank`, which was true of the only client a target ever had; a client
        // attaching to a target that has already navigated was told its frame sat at `about:blank`
        // while `Page.getFrameTree`, two commands later, answered the real url.
        "Page.disable" => {
            ids.page_enabled = false;
            (Ok(json!({})), vec![])
        }
        "Page.enable" => {
            // announced once per session (see `ConnIds::page_enabled`)
            if std::mem::replace(&mut ids.page_enabled, true) {
                return (Ok(json!({})), vec![]);
            }
            let frame_id = ids.frame_id().to_string();
            let loader = ids.loader_id().to_string();
            // Same clock as `Page.navigate`'s and as the `Network.*` events'. See there for why a
            // literal 0.0 was not merely untidy.
            let now_s = monotonic_now_s();
            let url = if engine.built() { engine.get().current_url() } else { "about:blank".to_string() };
            // Chrome's `securityOrigin` for the initial `about:blank` is the literal `://`
            // (measured); for a document with an origin it is that origin.
            let security_origin = match origin_of(&url) {
                o if o.is_empty() => "://".to_string(),
                o => o,
            };
            let frame = json!({ "id": frame_id, "loaderId": loader, "url": url,
                "domainAndRegistry": "", "securityOrigin": security_origin, "mimeType": "text/html" });
            let ev = |name: &str| json!({ "method": "Page.lifecycleEvent",
                "params": { "frameId": frame_id, "loaderId": loader, "name": name, "timestamp": now_s } });
            let mut events = vec![
                json!({ "method": "Page.frameNavigated", "params": { "frame": frame } }),
                ev("init"),
                ev("DOMContentLoaded"),
                ev("load"),
                json!({ "method": "Page.loadEventFired", "params": { "timestamp": now_s } }),
            ];
            // and its frames, as Chrome announces them to a session that enables `Page`
            events.extend(announce_frames(ids));
            (Ok(json!({})), events)
        }

        // Chrome's: the lifecycle each frame has reached so far, when a client turns lifecycle events on.
        "Page.setLifecycleEventsEnabled" => {
            let mut events = Vec::new();
            if params.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
                let now = monotonic_now_s();
                // a tab's initial empty document has no navigation timing, and Chrome says nothing of it
                if ids.committed {
                    let loaded = engine.get().load_fired();
                    let main = |name: &str| json!({ "method": "Page.lifecycleEvent", "params": { "frameId": ids.frame_id(), "loaderId": ids.loader_id(), "name": name, "timestamp": now } });
                    events.push(main("init"));
                    if loaded {
                        events.push(main("DOMContentLoaded"));
                        events.push(main("load"));
                    }
                }
                for f in ids.frames.iter().filter(|f| f.document != u32::MAX) {
                    let ev = |name: &str| json!({ "method": "Page.lifecycleEvent", "params": { "frameId": f.id, "loaderId": f.loader, "name": name, "timestamp": now } });
                    events.push(ev("init"));
                    if f.dcl_sent {
                        events.push(ev("DOMContentLoaded"));
                    }
                    if f.load_sent {
                        events.push(ev("load"));
                    }
                }
            }
            (Ok(json!({})), events)
        }
        "Page.addScriptToEvaluateOnNewDocument" => {
            let source = params.get("source").and_then(Value::as_str).unwrap_or("").to_string();
            // In the world it names -- it was run in the page's, so Playwright's utility world never existed
            // and what a driver put in one was the page's to read
            let world = params.get("worldName").and_then(Value::as_str).unwrap_or("").to_string();
            injected_scripts.push((world, source));
            // every frame's new documents run them too, as Chrome runs them in every frame
            webapi::devtools_log::set_new_document_scripts(injected_scripts.clone());
            // Chrome answers a plain decimal counter from "1" (measured), not a named handle.
            (Ok(json!({ "identifier": injected_scripts.len().to_string() })), vec![])
        }

        // Input domain, the MOUSE half (FIXER C6, 2026-10-11): each `Input.dispatchMouseEvent` is EXACTLY the
        // platform event the client sent, as Chrome 150 makes it (`content/browser/devtools/protocol/
        // input_handler.cc` `CreateWebMouseEvent`) and Blink dispatches it -- at the element under the point,
        // into a frame when the point is on one. This arm used to turn a `mouseMoved` into a humanised trajectory
        // from wherever the cursor was (696 events where Chrome dispatched 18 for the same CDP moves), a
        // `mousePressed` into a WHOLE click released ~65 ms later, and ignore `mouseReleased` -- so a client's
        // press-and-hold (a bot-detection script) was over before it began, and a right press was a left click. Humanised
        // input still exists, where a caller asks for a person: `Engine::move_mouse` / `click_at`.
        //
        // Chrome's rules, from the source: `button` names the button (`none` / `left` / `middle` / `right` /
        // `back` / `forward`, absent = none; anything else "Invalid mouse button"); `buttons` is the bitfield held,
        // to which the named button is added; `clickCount` defaults to 0 -- a press without it is a `mousedown`
        // with `detail` 0 and no `click`, as in Chrome; `modifiers` is Alt=1 Ctrl=2 Meta=4 Shift=8; a press or
        // release with no button does nothing. A `mouseWheel` needs `deltaX` and `deltaY`.
        "Input.dispatchMouseEvent" => {
            let ty = params.get("type").and_then(Value::as_str).unwrap_or("");
            let x = params.get("x").and_then(Value::as_f64).unwrap_or(0.0);
            let y = params.get("y").and_then(Value::as_f64).unwrap_or(0.0);
            let modifiers = params.get("modifiers").and_then(Value::as_i64).unwrap_or(0);
            let modifiers = webapi::input::Modifiers {
                alt: modifiers & 1 != 0,
                ctrl: modifiers & 2 != 0,
                meta: modifiers & 4 != 0,
                shift: modifiers & 8 != 0,
            };
            let button = match params.get("button").and_then(Value::as_str).unwrap_or("") {
                "" | "none" => Some(webapi::input::NO_BUTTON),
                "left" => Some(0),
                "middle" => Some(1),
                "right" => Some(2),
                "back" => Some(3),
                "forward" => Some(4),
                _ => None,
            };
            let held = params.get("buttons").and_then(Value::as_i64).unwrap_or(0) as u16 & 31;
            let click_count = params.get("clickCount").and_then(Value::as_i64).unwrap_or(0) as i32;
            let kind = match ty {
                "mouseMoved" => Some(webapi::input::MouseKind::Move),
                "mousePressed" => Some(webapi::input::MouseKind::Down),
                "mouseReleased" => Some(webapi::input::MouseKind::Up),
                _ => None,
            };
            let r = match (ty, kind, button) {
                (_, _, None) => Err(RpcError::invalid_params("Invalid mouse button".to_string())),
                ("mouseWheel", _, Some(b)) => {
                    match (params.get("deltaX").and_then(Value::as_f64), params.get("deltaY").and_then(Value::as_f64)) {
                        (Some(dx), Some(dy)) => {
                            let buttons = held | webapi::input::button_bit(b);
                            engine.get().mouse_wheel(x, y, dx, dy, buttons, modifiers).map(|_| json!({})).map_err(RpcError::from)
                        }
                        _ => Err(RpcError::invalid_params("'deltaX' and 'deltaY' are expected for mouseWheel event".to_string())),
                    }
                }
                (_, Some(kind), Some(b)) => {
                    // The named button is in the bitfield of a press (Chrome adds its modifier); the client's
                    // `buttons` is the state AFTER the event, so a release of the only button held reads 0.
                    let buttons = match kind {
                        webapi::input::MouseKind::Up => held & !webapi::input::button_bit(b),
                        _ => held | webapi::input::button_bit(b),
                    };
                    // The `input` plugin slot (goal 1020 step 2): with a plugin that plans mouse paths loaded, a move
                    // goes from where the pointer is along the plugin's path, each sample at its time; with none and
                    // humanize on, along the behaviour model's (the slot's built-in answer); else it is the one event
                    // the client sent, as above.
                    let path = if matches!(kind, webapi::input::MouseKind::Move) && plugin_host::has_input() {
                        let (fx, fy) = engine.get().cursor();
                        plugin_host::plan_move(plugin_host::KorvusPoint { x: fx, y: fy }, plugin_host::KorvusPoint { x, y }, 0.0)
                    } else if matches!(kind, webapi::input::MouseKind::Move) && HUMANIZE.load(std::sync::atomic::Ordering::Relaxed) {
                        let (fx, fy) = engine.get().cursor();
                        // CDP names no target: a button's width, so a long move takes Fitts's ~1 s, not a 1 px target's 2
                        let samples = engine.get().plan_move(spec::Point::new(fx, fy), spec::Point::new(x, y), 24.0);
                        (!samples.is_empty()).then(|| samples.into_iter().map(|s| plugin_host::KorvusMotionSample { x: s.x, y: s.y, t_us: s.t_us }).collect())
                    } else {
                        None
                    };
                    match path {
                        Some(samples) => {
                            let start = std::time::Instant::now();
                            let mut r = Ok(json!({}));
                            for s in samples {
                                let due = start + std::time::Duration::from_micros(s.t_us.max(0.0) as u64);
                                tokio::time::sleep_until(due.into()).await;
                                if let Err(e) = engine.get().mouse_event(kind, s.x as f64, s.y as f64, b, buttons, click_count, modifiers) {
                                    r = Err(RpcError::from(e));
                                    break;
                                }
                            }
                            r
                        }
                        None => engine.get().mouse_event(kind, x, y, b, buttons, click_count, modifiers).map(|_| json!({})).map_err(RpcError::from),
                    }
                }
                (other, None, Some(_)) => Err(RpcError::invalid_params(format!("Unexpected event type '{other}'"))),
            };
            (r, vec![])
        }
        // HTML drag and drop driven by the client (Playwright's `dragTo`, Puppeteer's `drag`): with interception on, a
        // drag the page starts is handed over as `Input.dragIntercepted`, and `Input.dispatchDragEvent` moves and drops
        // it (or drags data in from outside the page).
        "Input.setInterceptDrags" => {
            webapi::input::set_intercept_drags(params.get("enabled").and_then(Value::as_bool).unwrap_or(false));
            (Ok(json!({})), vec![])
        }
        "Input.dispatchDragEvent" => {
            let ty = params.get("type").and_then(Value::as_str).unwrap_or("");
            if !matches!(ty, "dragEnter" | "dragOver" | "drop" | "dragCancel") {
                return (Err(RpcError::invalid_params(format!("Unexpected event type '{ty}'"))), vec![]);
            }
            let x = params.get("x").and_then(Value::as_f64).unwrap_or(0.0);
            let y = params.get("y").and_then(Value::as_f64).unwrap_or(0.0);
            let data = params.get("data").cloned().unwrap_or(Value::Null);
            let items: Vec<(String, String)> = data
                .get("items")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|i| Some((i.get("mimeType")?.as_str()?.to_string(), i.get("data").and_then(Value::as_str).unwrap_or("").to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let mask = data.get("dragOperationsMask").and_then(Value::as_u64).unwrap_or(19) as u32;
            let m = params.get("modifiers").and_then(Value::as_i64).unwrap_or(0);
            let modifiers = webapi::input::Modifiers { alt: m & 1 != 0, ctrl: m & 2 != 0, meta: m & 4 != 0, shift: m & 8 != 0 };
            engine.get().dispatch_drag(ty, x, y, items, mask, modifiers);
            (Ok(json!({})), vec![])
        }
        // A finger (`locator.tap()` on a touch context): the touch events at the point, and on its release the click
        // Chrome's gesture recognizer makes of a tap (`mousedown`/`mouseup`/`click` at the same point). A touch end names
        // no point; it is where the touch was.
        "Input.dispatchTouchEvent" => {
            static LAST_TOUCH: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);
            let ty = params.get("type").and_then(Value::as_str).unwrap_or("");
            let point = params.get("touchPoints").and_then(Value::as_array).and_then(|a| a.first()).map(|p| {
                (p.get("x").and_then(Value::as_f64).unwrap_or(0.0), p.get("y").and_then(Value::as_f64).unwrap_or(0.0))
            });
            let mut last = LAST_TOUCH.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = point {
                *last = Some(p);
            }
            let (x, y) = last.unwrap_or((0.0, 0.0));
            let r = match ty {
                "touchStart" => engine.get().dispatch_trusted_touch("touchstart", x, y, 1.0, 1.0, 1.0).map(|_| json!({})),
                "touchMove" => engine.get().dispatch_trusted_touch("touchmove", x, y, 1.0, 1.0, 1.0).map(|_| json!({})),
                "touchCancel" => engine.get().dispatch_trusted_touch("touchcancel", x, y, 1.0, 1.0, 1.0).map(|_| json!({})),
                "touchEnd" => {
                    let none = webapi::input::Modifiers { alt: false, ctrl: false, meta: false, shift: false };
                    engine
                        .get()
                        .dispatch_trusted_touch("touchend", x, y, 1.0, 1.0, 1.0)
                        .and_then(|_| engine.get().mouse_event(webapi::input::MouseKind::Down, x, y, 0, 1, 1, none))
                        .and_then(|_| engine.get().mouse_event(webapi::input::MouseKind::Up, x, y, 0, 0, 1, none))
                        .map(|_| json!({}))
                }
                other => return (Err(RpcError::invalid_params(format!("Unexpected event type '{other}'"))), vec![]),
            };
            *last = if ty == "touchEnd" || ty == "touchCancel" { None } else { *last };
            (r.map_err(RpcError::from), vec![])
        }
        // BOTH OF THESE USED TO CALL `Engine::type_text`, which dispatched the behaviour
        // model's humanised `keydown`/`keyup` stream ON `document`. Neither is what the method
        // means, and the interactivity sweep measured the cost: the `input` capability scored 0 of
        // its 169 floor rows with `counts.keydown` non-zero on every one — the keystrokes landed,
        // and landed nowhere. They carry no selector because they target whatever has focus, so the
        // engine resolves the focus itself now (`core/events/typing.rs`).
        "Input.insertText" => {
            let text = params.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            // NOT a keystroke stream. Chrome fires `beforeinput` → the value change → `input`, all
            // three with the WHOLE string as `data`, and no key event of any kind: the method's own
            // contract is "text that doesn't come from a key press". Humanising it here would make
            // this engine the only browser that types when asked to paste.
            (engine.get().insert_text(&text).await.map(|_| json!({})).map_err(RpcError::from), vec![])
        }
        // D88 step 2: a person's IME (Korean, Japanese, Chinese) composing at the focus; `insertText` commits it.
        "Input.imeSetComposition" => {
            let text = params.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            (engine.get().ime_set_composition(&text).await.map(|_| json!({})).map_err(RpcError::from), vec![])
        }
        "Input.dispatchKeyEvent" => {
            // ONE key event, the one the caller named — where this arm used to run the whole
            // behaviour model over `text`, which for a single character could inject a neighbour
            // typo and a Backspace and deliver a key the caller never sent.
            //
            // And with EVERY field the caller named. `modifiers`, `windowsVirtualKeyCode`,
            // `autoRepeat`, `isKeypad` and `location` used to be dropped here, so a Shift-held key read
            // `shiftKey: false` and the engine guessed `keyCode` from the key name (Shift came out 83).
            // `InputHandler::DispatchKeyEvent` (`input_handler.cc:1181-1270`) is the model: every absent
            // field is its default, not something derived from another field.
            let ty = params.get("type").and_then(Value::as_str).unwrap_or("");
            let s = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or("");
            let i = |k: &str| params.get(k).and_then(Value::as_i64);
            let b = |k: &str| params.get(k).and_then(Value::as_bool).unwrap_or(false);
            // `SetKeyboardEventText`: the text fits `WebKeyboardEvent::kTextLengthCap` (4) with its NUL.
            if s("text").encode_utf16().count() >= 4 {
                return (Err(RpcError { code: -32602, message: "Invalid 'text' parameter".to_string(), data: None }), vec![]);
            }
            if s("unmodifiedText").encode_utf16().count() >= 4 {
                return (Err(RpcError { code: -32602, message: "Invalid 'unmodifiedText' parameter".to_string(), data: None }), vec![]);
            }
            let input = webapi::input::KeyInput::from_cdp(
                s("key"),
                s("code"),
                s("text"),
                i("windowsVirtualKeyCode"),
                i("modifiers"),
                b("autoRepeat"),
                b("isKeypad"),
                i("location"),
            );
            match engine.get().dispatch_key_event(ty, input).await {
                Ok(Some(_)) => (Ok(json!({})), vec![]),
                // Chrome's refusal of a type it has no event for (measured for the mouse twin:
                // `-32602`, the detail in `message`, no `data`).
                Ok(None) => (Err(RpcError { code: -32602, message: format!("Unexpected event type '{ty}'"), data: None }), vec![]),
                Err(e) => (Err(RpcError::from(e)), vec![]),
            }
        }

        // The DOM a client asks about, answered from the engine's arena and layout (`Engine::node_info`,
        // `node_quad`, `object_for_node`) -- never through the page's members (`querySelector`, `getBoundingClientRect`
        // and the like are the page's to replace). Chrome 150's answers on the same page are pinned in
        // `tests/dom_handles.rs`. A node's `backendNodeId` is its arena id + 1 (Chrome never hands out 0); its `nodeId`,
        // which no client relies on across sessions, is the same number.
        "DOM.describeNode" => match node_param(engine.get(), params).and_then(|n| engine.get().node_info(n).map(|i| (n, i))) {
            Some((n_id, info)) => {
                let url = engine.get().current_url();
                let mut node = node_json(n_id, &info, &url);
                // The document, and its document element, carry the frame they are the document of, as Chrome's do:
                // Playwright finds an element's frame by describing its `documentElement` (`ElementHandle.ownerFrame`,
                // which `setInputFiles` asks first) -- the page's, or a frame's.
                if info.node_type == 9 || (info.node_type == 1 && info.local_name == "html") {
                    let owner = match engine.get().frame_of_node(n_id) {
                        Some(Some(bc)) => ids.frame_for(bc).map(|f| f.id.clone()),
                        _ => None,
                    };
                    node["frameId"] = json!(owner.unwrap_or_else(|| ids.frame_id().to_string()));
                }
                // A frame element carries the frame it holds and that frame's document (`contentFrame()` reads it).
                if info.node_type == 1 && matches!(info.local_name.as_str(), "iframe" | "frame") {
                    if let Some(child) = engine.get().content_frame_of(n_id).and_then(|bc| ids.frame_for(bc).cloned()) {
                        node["frameId"] = json!(child.id);
                        if child.document != u32::MAX {
                            node["contentDocument"] = json!({
                                "nodeId": child.document + 1, "backendNodeId": child.document + 1, "nodeType": 9, "nodeName": "#document",
                                "localName": "", "nodeValue": "", "documentURL": child.url, "baseURL": child.url, "xmlVersion": "",
                                "compatibilityMode": "NoQuirksMode",
                            });
                        }
                    }
                }
                (Ok(json!({ "node": node })), vec![])
            }
            None => (Err(RpcError::server("Could not find node with given id".to_string())), vec![]),
        },
        // The element that holds a frame (`frame.frameElement()`, and Playwright's hit-target check of a click in one).
        "DOM.getFrameOwner" => {
            let fid = params.get("frameId").and_then(Value::as_str).unwrap_or("");
            match ids.bc_for(fid).and_then(|bc| engine.get().devtools_frames().into_iter().find(|f| f.id == bc)) {
                Some(f) => (Ok(json!({ "backendNodeId": f.element + 1, "nodeId": f.element + 1 })), vec![]),
                None => (Err(RpcError::server("Frame with the given id was not found.".to_string())), vec![]),
            }
        }
        "DOM.resolveNode" => {
            let Some(node) = node_param(engine.get(), params) else {
                return (Err(RpcError::server("No node with given id found".to_string())), vec![]);
            };
            let world = match named_world(ids, params, "executionContextId") {
                Ok(w) => w,
                Err(e) => return (Err(e), vec![]),
            };
            let ctx = world.and_then(|w| ids.world_context(w)).unwrap_or(ids.default_context());
            match engine.get().object_for_node(world, node) {
                Some(id) => (Ok(json!({ "object": remote_from(engine.get(), RemoteResult::Object(id), ids, ctx) })), vec![]),
                None => (Err(RpcError::server("No node with given id found".to_string())), vec![]),
            }
        }
        "DOM.getContentQuads" => match node_param(engine.get(), params).and_then(|n| engine.get().node_quad(n)) {
            Some(q) => (Ok(json!({ "quads": [q] })), vec![]),
            None => (Err(RpcError::server("Could not compute content quads.".to_string())), vec![]),
        },
        // The border quad for all four boxes until the engine reports padding, border and margin widths: Puppeteer's
        // `boundingBox()` reads `border`, which is exact; `content` / `padding` / `margin` are its approximation.
        "DOM.getBoxModel" => match node_param(engine.get(), params).and_then(|n| engine.get().node_quad(n)) {
            Some(q) => (
                Ok(json!({ "model": { "content": q, "padding": q, "border": q, "margin": q, "width": q[2] - q[0], "height": q[5] - q[1] } })),
                vec![],
            ),
            None => (Err(RpcError::server("Could not compute box model.".to_string())), vec![]),
        },
        // `{}`, as Chrome's, after the scroll: the node (or `rect`, in its border box) centred on each axis where it was
        // not wholly visible. Playwright's click scrolls its target this way, then checks it is inside the viewport.
        "DOM.scrollIntoViewIfNeeded" => match node_param(engine.get(), params) {
            Some(n) => {
                let rect = params.get("rect").and_then(|r| {
                    Some((r.get("x")?.as_f64()?, r.get("y")?.as_f64()?, r.get("width")?.as_f64()?, r.get("height")?.as_f64()?))
                });
                engine.get().scroll_into_view_if_needed(n, rect);
                (Ok(json!({})), vec![])
            }
            None => (Err(RpcError::server("Node with given id does not belong to the document".to_string())), vec![]),
        },
        "DOM.getDocument" => match engine.get().document_node() {
            Some((root, children)) => {
                let url = engine.get().current_url();
                (
                    Ok(json!({ "root": {
                        "nodeId": root + 1, "backendNodeId": root + 1, "nodeType": 9, "nodeName": "#document", "localName": "",
                        "nodeValue": "", "childNodeCount": children, "documentURL": url, "baseURL": url, "xmlVersion": "",
                        "compatibilityMode": "NoQuirksMode",
                    } })),
                    vec![],
                )
            }
            None => (Err(RpcError::server("Document is not available".to_string())), vec![]),
        },

        // The document as the compositor paints it (what `korvus fetch --screenshot` writes), as
        // Chrome's `{data}` -- base64 PNG. PNG is what this engine paints: a client asking for JPEG or WebP is told so,
        // not handed PNG bytes under the wrong type (Puppeteer and Playwright ask for PNG by default).
        "Page.stopScreencast" => {
            screencast::stop();
            (Ok(json!({})), vec![])
        }
        "Page.screencastFrameAck" => {
            screencast::ack();
            (Ok(json!({})), vec![])
        }

        // PNG, JPEG or WebP (`quality` 0-100 for the lossy two), of the viewport or of `clip` (document CSS px, and its
        // `scale`); with `captureBeyondViewport` a clip outside the viewport is painted too -- a full-page screenshot.
        "Page.captureScreenshot" => {
            let format = match params.get("format").and_then(Value::as_str).unwrap_or("png") {
                "png" => spec::ImageFormat::Png,
                "jpeg" => spec::ImageFormat::Jpeg,
                "webp" => spec::ImageFormat::Webp,
                other => return (Err(RpcError::server(format!("Unsupported screenshot format: {other}"))), vec![]),
            };
            let quality = params.get("quality").and_then(Value::as_f64).filter(|_| format != spec::ImageFormat::Png);
            let clip = params.get("clip").filter(|c| c.is_object()).map(|c| {
                let n = |k: &str| c.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                [n("x"), n("y"), n("width"), n("height"), c.get("scale").and_then(Value::as_f64).unwrap_or(1.0)]
            });
            let beyond = params.get("captureBeyondViewport").and_then(Value::as_bool).unwrap_or(false);
            match engine.get().render_shot(format, quality, clip, beyond) {
                Some(bytes) => {
                    use base64::Engine as _;
                    (Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(bytes) })), vec![])
                }
                None => (Err(RpcError::server("Unable to capture screenshot".to_string())), vec![]),
            }
        }

        // The document printed, as Chrome prints it for `page.pdf()`: `print` media, Letter paper with Chrome's 0.4 in
        // margins unless the client names its own, the scale, landscape and page ranges asked for; the PDF in base64.
        "Page.printToPDF" => {
            let n = |k: &str, d: f64| params.get(k).and_then(Value::as_f64).unwrap_or(d);
            let opts = webapi::PrintOptions {
                paper_width_in: n("paperWidth", 8.5),
                paper_height_in: n("paperHeight", 11.0),
                margins_in: [n("marginTop", 0.4), n("marginRight", 0.4), n("marginBottom", 0.4), n("marginLeft", 0.4)],
                scale: n("scale", 1.0),
                landscape: params.get("landscape").and_then(Value::as_bool).unwrap_or(false),
                page_ranges: params.get("pageRanges").and_then(Value::as_str).unwrap_or("").to_string(),
            };
            if !(0.1..=2.0).contains(&opts.scale) {
                return (Err(RpcError::invalid_params("scale is outside [0.1 - 2] range".to_string())), vec![]);
            }
            match engine.get().print_pdf(&opts) {
                // `ReturnAsStream` (Playwright's): the PDF behind a stream handle the client reads with `IO.read`
                Ok(bytes) if params.get("transferMode").and_then(Value::as_str) == Some("ReturnAsStream") => {
                    (Ok(json!({ "data": "", "stream": io_streams::open(bytes) })), vec![])
                }
                Ok(bytes) => {
                    use base64::Engine as _;
                    (Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(bytes) })), vec![])
                }
                Err(m) => (Err(RpcError::server(m)), vec![]),
            }
        }
        // A stream a command handed out (`Page.printToPDF`'s): read in chunks, base64, until `eof`; then closed.
        "IO.read" => {
            let handle = params.get("handle").and_then(Value::as_str).unwrap_or("");
            let offset = params.get("offset").and_then(Value::as_u64).map(|o| o as usize);
            let size = params.get("size").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(1 << 20);
            match io_streams::read(handle, offset, size) {
                Some((chunk, eof)) => {
                    use base64::Engine as _;
                    (Ok(json!({ "base64Encoded": true, "data": base64::engine::general_purpose::STANDARD.encode(chunk), "eof": eof })), vec![])
                }
                None => (Err(RpcError::server("Invalid stream handle".to_string())), vec![]),
            }
        }
        "IO.close" => {
            let handle = params.get("handle").and_then(Value::as_str).unwrap_or("");
            if io_streams::close(handle) {
                (Ok(json!({})), vec![])
            } else {
                (Err(RpcError::server("Invalid stream handle".to_string())), vec![])
            }
        }

        // The viewport is the persona's window, in CSS px (Chrome 150's shape, recorded). The content size is
        // reported as the viewport until the engine reports a document's scroll size (`A1-step2-engine-api.md`),
        // which is what a page no taller than its window has.
        "Page.getLayoutMetrics" => {
            // the layout viewport as `documentElement.clientWidth`/`clientHeight` read it, where it is scrolled to, and
            // the document's scroll size -- after a front end's resize (D75) and a client's emulated viewport too
            let [w, h, sx, sy, cw, ch] = engine.get().layout_metrics();
            let layout = json!({ "pageX": sx, "pageY": sy, "clientWidth": w, "clientHeight": h });
            let visual = json!({ "offsetX": 0, "offsetY": 0, "pageX": sx, "pageY": sy, "clientWidth": w, "clientHeight": h, "scale": 1, "zoom": 1 });
            let content = json!({ "x": 0, "y": 0, "width": cw, "height": ch });
            (
                Ok(json!({
                    "layoutViewport": layout, "visualViewport": visual, "contentSize": content,
                    "cssLayoutViewport": layout, "cssVisualViewport": visual, "cssContentSize": content,
                })),
                vec![],
            )
        }

        // The router's own command (it never relays a client's `Korvus.*`): `Browser.setWindowBounds` on this page's
        // window (D75). The bounds are `Browser.Bounds`, as the router keeps them for the window.
        "Korvus.setWindowBounds" => {
            let b = params.get("bounds").cloned().unwrap_or(Value::Null);
            let num = |k: &str| b.get(k).and_then(Value::as_f64);
            let state = match b.get("windowState").and_then(Value::as_str) {
                Some("minimized") => webapi::WindowState::Minimized,
                Some("maximized") => webapi::WindowState::Maximized,
                Some("fullscreen") => webapi::WindowState::Fullscreen,
                _ => webapi::WindowState::Normal,
            };
            engine.get().set_window_bounds(webapi::WindowBounds { left: num("left"), top: num("top"), width: num("width"), height: num("height"), state });
            (Ok(json!({})), vec![])
        }

        // THE `Debugger` DOMAIN (D106): the Sources side of DevTools against korvus's own engine -- which scripts
        // parsed and their text. `enable` turns recording on; from then on every classic script the page compiles is
        // sent as `Debugger.scriptParsed` (by the loop, from `devtools_log::take_parsed_scripts`) and its text is read
        // back with `getScriptSource`. Live pause/step/resume is the V8 inspector's and is NOT offered: a breakpoint
        // is accepted and answered, but execution is not halted on it. A client that enables `Debugger` before it
        // navigates (DevTools and Playwright do) sees the page's scripts as they load.
        "Debugger.enable" => {
            webapi::devtools_log::set_debugger_capture(true);
            (Ok(json!({ "debuggerId": "korvus-debugger" })), vec![])
        }
        "Debugger.disable" => {
            webapi::devtools_log::set_debugger_capture(false);
            (Ok(json!({})), vec![])
        }
        "Debugger.getScriptSource" => {
            let id = params.get("scriptId").and_then(Value::as_str).and_then(|s| s.parse::<u32>().ok());
            match id.and_then(webapi::devtools_log::script_source) {
                Some(src) => (Ok(json!({ "scriptSource": src })), vec![]),
                None => (Err(RpcError::server("No script for given id found".to_string())), vec![]),
            }
        }
        // A breakpoint is recorded and answered, but nothing halts on it (see above): the client gets a
        // `breakpointId` and -- because this server resolves no concrete location without the inspector -- an empty
        // `locations` list, which is a valid answer (Chrome returns it for a URL no script matches yet).
        "Debugger.setBreakpointByUrl" => {
            let line = params.get("lineNumber").and_then(Value::as_i64).unwrap_or(0);
            let url = params.get("url").and_then(Value::as_str).unwrap_or("");
            (Ok(json!({ "breakpointId": format!("korvus:{url}:{line}"), "locations": [] })), vec![])
        }

        m if ACCEPTED_NOOPS.contains(&m) => (Ok(json!({})), vec![]),

        // JAVASCRIPT DIALOGS (plugin ABI 3 wave 1, step 7): the plugin dialog hook (`plugin_host::on_dialog`) is
        // reached from `window.alert` / `confirm` / `prompt` in `korvus-webapi` (window.rs), which decides to accept
        // or dismiss a dialog inline on the page's thread -- there is no asynchronous CDP dialog flow here to carry
        // it. So this server emits no `Page.javascriptDialogOpening` and has no `Page.handleJavaScriptDialog` arm:
        // a client sending one lands in the method-not-found arm below, which is honest for an engine with no such
        // flow. TODO(1021-verify): no CDP-level dialog plumbing to wire until the engine surfaces dialog events.

        // Everything else is refused the way Chrome refuses it. THIS ARM WAS `Ok(json!({}))`, and
        // that is goal item 22: an engine that succeeds at `Foo.bar` has told a harness what it is.
        // Note that `Page.js_error_count` -- this engine's own uncaught-error tally, which used to be
        // answered here -- now lands in this arm, which is exactly what Chrome answers for it. Its
        // capability did not move: it is `Engine::uncaught_error_count`, where its real callers
        // (`korvus fetch --errors`, `tools/corpus/run.py`) already read it.
        other => (Err(RpcError::method_not_found(other)), vec![]),
    }
}

/// A download, performed for a DevTools client's download behaviour (`Browser.setDownloadBehavior`): its bytes -- the
/// navigation response that turned out to be an attachment, or fetched now for an `<a download>` -- written into the
/// client's folder (under the guid for `allowAndName`, Playwright's, else the suggested name), and the router told
/// (`router::DOWNLOAD_LINE`), which sends the clients `Browser.downloadWillBegin` and `Browser.downloadProgress` as
/// Chrome's browser does. No behaviour set: nothing happens.
async fn download(engine: &mut Engine, frame_id: &str, url: &str, name: Option<String>, fetched: Option<(Vec<u8>, Vec<(String, String)>)>) {
    let Some((behavior, dir)) = spec::devtools::download_behavior() else { return };
    let guid = ids::hex32();
    let (bytes, headers) = match fetched {
        Some(f) => f,
        None => {
            let page = engine.current_url();
            let req = transport::FetchRequest { initiator: Some(page), ..transport::FetchRequest::navigation(url.to_string()).worn_by(engine.persona().as_ref()) };
            match transport::fetch(req).await {
                Ok(r) => (r.bytes, r.headers),
                Err(_) => (Vec::new(), Vec::new()),
            }
        }
    };
    let suggested = name
        .or_else(|| download_name(&headers, url))
        .unwrap_or_else(|| url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("download").to_string());
    let line = |v: Value| println!("{}{}", router::DOWNLOAD_LINE, v);
    line(json!({ "frameId": frame_id, "guid": guid, "url": url, "suggestedFilename": suggested }));
    if behavior == "deny" || dir.is_empty() {
        line(json!({ "guid": guid, "totalBytes": 0, "receivedBytes": 0, "state": "canceled" }));
        return;
    }
    let file = std::path::Path::new(&dir).join(if behavior == "allowAndName" { guid.as_str() } else { suggested.as_str() });
    let saved = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, &bytes));
    let n = bytes.len();
    line(json!({ "guid": guid, "totalBytes": n, "receivedBytes": n, "state": if saved.is_ok() { "completed" } else { "canceled" } }));
}

/// The download file name a navigation response proposes (step 7), or `None` when the response is not a download.
///
/// A response is a download when its `Content-Disposition` is `attachment`; its `filename` is the proposed name,
/// else the URL's last path segment (and `"download"` when even that is empty). An `inline` or absent disposition
/// is a document, not a download, and gives `None`.
fn download_name(headers: &[(String, String)], url: &str) -> Option<String> {
    let cd = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-disposition")).map(|(_, v)| v.as_str())?;
    // The disposition type is the part before the first `;`.
    if !cd.split(';').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("attachment")) {
        return None;
    }
    // `filename="..."` or `filename=...`; a `filename*` (RFC 5987 extended value) is left to the integration pass.
    let named = cd.split(';').find_map(|p| Some(p.trim().strip_prefix("filename=")?.trim().trim_matches('"').to_string()));
    let name = named.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        let path = url.split('#').next().unwrap_or(url).split('?').next().unwrap_or(url);
        path.rsplit('/').next().unwrap_or("").to_string()
    });
    Some(if name.is_empty() { "download".to_string() } else { name })
}

/// The navigation's document request: `(the URL the document came from, its bytes and WHOLE header
/// block)`, or no document for a non-http url, which gets an empty one instead.
async fn fetch_document(
    engine: &mut Engine,
    nav: &engine::PageNavigation,
) -> anyhow::Result<(String, Option<(Vec<u8>, Vec<(String, String)>)>)> {
    let url = nav.url.as_str();
    // The response's Content-Type comes along with its body: it is what a browser picks its PARSER
    // from, and an `application/xhtml+xml` / `image/svg+xml` document run through the HTML parser
    // keeps its CDATA wrappers inside every inline script. Without the header the URL extension is
    // all `load_page_typed` has to go on, which is a guess where this is a fact.
    // The WHOLE header block travels now, not just Content-Type: `Cross-Origin-Opener-Policy` and
    // `Cross-Origin-Embedder-Policy` decide what the browsing context IS, and a caller that kept only
    // the body could not have told the engine.
    //
    // THE BYTES, NOT `body`: `body` is a UTF-8-lossy view, so an EUC-KR or
    // windows-1252 page was mojibake before the engine saw it. The engine decodes the bytes by the HTML
    // encoding sniff, from the header, the markup and the persona's locale.
    if url.starts_with("http://") || url.starts_with("https://") {
        // Worn by the engine this navigation is FOR, so the document request carries the same
        // identity -- UA and ClientHello alike -- that the page's own `fetch()` will.
        // A `method=post` form submission navigates with its serialised body and the `Content-Type`
        // the form's `enctype` chose; everything else is the plain `GET` navigation.
        // THE REQUEST URL HAS NO FRAGMENT. A fragment is never sent on the wire, and Chrome's
        // `Network.requestWillBeSent` does not carry one either; the DOCUMENT keeps it.
        // The document that asked is the request's initiator; `Page.navigate` has none.
        let requested = url.split_once('#').map(|(u, _)| u).unwrap_or(url);
        let req = engine::navigation_request(nav, engine.persona().as_ref());
        let resp = transport::fetch(req).await?;
        // The DOCUMENT keeps the fragment the request dropped, so long as nothing redirected: a
        // response that arrived from where it was asked leaves the requested URL intact.
        let final_url =
            if resp.url.is_empty() || resp.url == requested { url.to_string() } else { resp.url.clone() };
        Ok((final_url, Some((resp.bytes, resp.headers))))
    } else {
        Ok((url.to_string(), None))
    }
}

/// Load a committed document: parse it, run its scripts, and pump to its `load`.
async fn load_document(
    engine: &mut Engine,
    url: &str,
    document: Option<(Vec<u8>, Vec<(String, String)>)>,
    injected: &[(String, String)],
) -> anyhow::Result<()> {
    // The page world's new-document scripts; a named world's run in that world (`Engine::set_new_document_worlds`).
    let injected: Vec<String> = injected.iter().filter(|(w, _)| w.is_empty()).map(|(_, s)| s.clone()).collect();
    let injected = injected.as_slice();
    // load_page fetches + executes external <script src> (subresource pipeline), so a navigated real
    // site's JS actually runs; falls back to the injected-only path for non-http (about:blank) loads.
    let t = std::time::Instant::now();
    match document {
        Some((bytes, headers)) => engine.load_page_response(&bytes, url, injected, &headers).await?,
        None => {
            engine.set_url(url);
            engine.load_html_injected("<!doctype html><html><head></head><body></body></html>", injected)?
        }
    }
    navtrace::mark("parse_scripts", t.elapsed().as_secs_f64() * 1000.0);
    let t = std::time::Instant::now();
    // Let the page's scripts initialise + fire the load lifecycle, then return -- bounded, because
    // modern pages keep perpetual timers running so an unconditional drain never returns (see
    // Engine::pump). `pump_until_loaded` returns as soon as the `load` event fires instead of always
    // burning the full budget, so fast pages navigate fast. A document a script already navigated away
    // from has no load to wait for.
    if !engine.document_abandoned() {
        pump_to_load(engine, std::time::Duration::from_millis(env_ms("KORVUS_NAV_PUMP_MS", NAV_PUMP_MS_DEFAULT))).await;
    }
    let pump = t.elapsed().as_secs_f64() * 1000.0;
    // The pump's two halves: up to the `load` event, and the tail it spends after it (a 25 ms slice
    // boundary plus the 50 ms post-load settle). The tail is real engine time but NOT time the page
    // spent loading, and until P14a it was inside every `Page.loadEventFired` timestamp.
    let to_load = engine
        .lifecycle_marks()
        .1
        .map(|at| at.saturating_duration_since(t).as_secs_f64() * 1000.0)
        .unwrap_or(pump);
    navtrace::mark("pump_to_load", to_load);
    navtrace::mark("pump_tail", pump - to_load);
    Ok(())
}

/// `Engine::pump_until_loaded`, except that a navigation the page asks for ENDS the wait (FIXER C11).
///
/// The load pump used to run on to `load` or to its budget (`NAV_PUMP_MS_DEFAULT`, 1.5 s) whatever the page
/// did meanwhile, and the navigation was followed only after it. Measured on a loopback page that calls
/// `location.reload()` 300 ms after `DOMContentLoaded` while an image holds its `load` for 3 s, from the
/// page's ask to the reload's request reaching the server: Chrome 150 1.7-2.0 ms, `korvus serve` 1212 ms
/// (a bot sensor investigation saw 2.2-3.4 s on a challenge page). Chrome's browser process starts a
/// renderer-initiated navigation when it is asked; the document being left does not get to finish loading
/// first. So the slices here stop at the ask (`Engine::pump_until`), and the caller follows it at once.
async fn pump_to_load(engine: &mut Engine, budget: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + budget;
    let slice = std::time::Duration::from_millis(25);
    loop {
        let end = engine.pump_until(slice, std::future::pending::<()>()).await;
        if engine.navigation_pending() {
            return;
        }
        if engine.load_fired() {
            // The short post-load settle `pump_until_loaded` gives load-handler microtasks and immediate work.
            let _ = engine.pump_until(std::time::Duration::from_millis(50), std::future::pending::<()>()).await;
            return;
        }
        // A drained loop cannot fire `load` without new input; the budget is a ceiling, not a quota.
        if matches!(end, Ok(engine::PumpEnd::Drained)) || tokio::time::Instant::now() >= deadline {
            return;
        }
    }
}

/// What this browser says it IS, derived from the worn identity — the one place the answer is
/// computed (W9.5).
///
/// THE DEFECT THIS REPLACES. `/json/version` answered `"Browser": "Korvus/0.0.0"`,
/// `"User-Agent": "Korvus"`, and `Browser.getVersion` answered `"product": "Korvus/0.0.0"`,
/// `"userAgent": "Korvus"` — hardcoded, and contradicting `navigator.userAgent` in the very same
/// process. Anything driving Korvus over CDP (which is what an automation-detection harness does, and
/// what Playwright does on `connectOverCDP`) could read the engine's real name off the control channel
/// while the page claimed to be Chrome 151. That is a first-tier automation tell, not untidiness.
///
/// THE SHAPE IS CHROME'S, measured rather than guessed — `google-chrome --headless=new
/// --remote-debugging-port`, 2026-09-11, Chrome 150.0.7871.46:
///
/// ```text
/// Browser.getVersion -> {"protocolVersion":"1.3", "product":"Chrome/150.0.7871.46",
///                        "revision":"@5b586c06e0d27582900f17e2d59c5370d8d6e0bb",
///                        "userAgent":"Mozilla/5.0 (X11; Linux x86_64) … HeadlessChrome/150.0.0.0 …",
///                        "jsVersion":"15.0.245.13"}
/// /json/version      -> {"Browser":"Chrome/150.0.7871.46", "Protocol-Version":"1.3",
///                        "User-Agent":"<the same UA>", "V8-Version":"15.0.245.13",
///                        "WebKit-Version":"537.36 (@5b586c06…)"}
/// ```
///
/// Note what that shows: `product` is NOT the UA's browser token. Chrome's UA is version-reduced
/// (`150.0.0.0`) and headless renames the token (`HeadlessChrome`), while `product` carries the real
/// full version and always says `Chrome`. So `product` comes from the identity's full version
/// (`navigator.userAgentData.getHighEntropyValues(["uaFullVersion"])`) and `userAgent` from the UA —
/// the two disagree in real Chrome, and reading both from one string would be its own tell.
///
/// `revision` is the one field no capture carries: it is a Chromium git sha, which the persona bundle
/// does not record and which cannot be derived from anything it does record. Inventing one would be a
/// recorded value standing in for a captured one, so it stays empty and is listed here rather than
/// hidden. Closing it needs the capture rig to record `Browser.getVersion` at capture time.
struct VersionInfo {
    product: String,
    user_agent: String,
    js_version: &'static str,
    webkit_version: String,
}

impl VersionInfo {
    fn of(p: &dyn spec::Persona) -> Self {
        let user_agent = p.navigator().user_agent;
        let full = p.client_hints().ua_full_version;
        // The captured full version if there is one; else the UA's own `Chrome/<v>` token, which is
        // the same number reduced. A UA with neither leaves the version off rather than making one up.
        let version = if !full.is_empty() {
            full
        } else {
            ua_token(&user_agent, "Chrome").unwrap_or_default()
        };
        let product = if version.is_empty() { String::new() } else { format!("Chrome/{version}") };
        // Chrome reports `537.36 (@<sha>)`; the sha half is the same uncaptured revision, so this is
        // the number the UA itself declares and nothing more.
        let webkit_version = ua_token(&user_agent, "AppleWebKit").unwrap_or_default();
        VersionInfo { product, user_agent, js_version: Engine::js_version(), webkit_version }
    }

    /// The `Browser.getVersion` result.
    fn cdp(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "product": self.product,
            "revision": "",
            "userAgent": self.user_agent,
            "jsVersion": self.js_version,
        })
    }

    /// The `/json/version` body. Same four facts, HTTP field names — one computation, so the control
    /// channel cannot answer one thing over HTTP and another over the WebSocket.
    fn json_version(&self, ws_url: String) -> Value {
        json!({
            "Browser": self.product,
            "Protocol-Version": PROTOCOL_VERSION,
            "User-Agent": self.user_agent,
            "V8-Version": self.js_version,
            "WebKit-Version": self.webkit_version,
            "webSocketDebuggerUrl": ws_url,
        })
    }
}

/// The version of a `Name/Version` token in a User-Agent string (`ua_token(ua, "Chrome")` →
/// `"151.0.0.0"`). `None` when the UA carries no such token.
fn ua_token(ua: &str, name: &str) -> Option<String> {
    ua.split_whitespace().find_map(|t| t.strip_prefix(name)?.strip_prefix('/').map(str::to_string))
}

/// Parse a CDP `arguments` array into engine args (by-value literal or object ref).
fn parse_args(v: Option<&Value>) -> Vec<CdpArg> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|a| {
                    if let Some(id) =
                        a.get("objectId").and_then(Value::as_str).and_then(ids::parse_object_id)
                    {
                        CdpArg::Object(id)
                    } else if let Some(v) = a.get("value") {
                        CdpArg::Value(v.clone())
                    } else {
                        // `unserializableValue`, or no value at all: `undefined`
                        CdpArg::Unserializable(a.get("unserializableValue").and_then(Value::as_str).map(str::to_string))
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `Runtime.evaluate` result for one [`EvalOutcome`] — `{result}`, or, when the expression threw
/// or its promise rejected, `{result, exceptionDetails}`.
///
/// Chrome answers a RESULT in both cases. The thrown value appears TWICE, with two different
/// `objectId`s, once in `result` and once in `exceptionDetails.exception` — measured, and reproduced
/// here because a client that releases one must not lose the other.
fn evaluate_result(engine: &mut Engine, outcome: EvalOutcome, ids: &mut ConnIds, ctx: i64) -> Value {
    let EvalOutcome { result, exception } = outcome;
    let Some(mut ex) = exception else {
        return json!({ "result": remote_from(engine, result, ids, ctx) });
    };
    // The second handle on the thrown value comes out of `ex` first; everything else in `ex`
    // describes BOTH copies, and `result` gets the same treatment below.
    let ex_value = std::mem::replace(&mut ex.value, RemoteResult::Value(Value::Null));
    let mut details = json!({
        "exceptionId": ids.next_exception_id(),
        "text": ex.text,
        "lineNumber": ex.line_number,
        "columnNumber": ex.column_number,
        "exception": remote_thrown(ex_value, &ex, ids, ctx),
    });
    let obj = details.as_object_mut().expect("a json object");
    if let Some(sid) = &ex.script_id {
        obj.insert("scriptId".into(), json!(sid));
    }
    // A SyntaxError has no stack, and Chrome then omits the whole `stackTrace` key rather than
    // sending an empty `callFrames` (measured).
    if let Some(frames) = &ex.stack {
        let frames: Vec<Value> = frames
            .iter()
            .map(|f| {
                json!({
                    "functionName": f.function_name,
                    "scriptId": f.script_id,
                    "url": f.url,
                    "lineNumber": f.line_number,
                    "columnNumber": f.column_number,
                })
            })
            .collect();
        obj.insert("stackTrace".into(), json!({ "callFrames": frames }));
    }
    json!({ "result": remote_thrown(result, &ex, ids, ctx), "exceptionDetails": details })
}

/// A thrown value as a CDP `RemoteObject`.
///
/// Chrome's is `{"type":"object","subtype":"error","className":"Error","description":"<the Error's
/// own stack>","objectId":…}` — the `description` is the stack text, which is where `<anonymous>`
/// lives; nothing else carries it. A thrown non-object (`throw 'plainstring'`) is a plain
/// `{"type":"string","value":"plainstring"}` with no id at all, which is the ordinary by-value path.
fn remote_thrown(res: RemoteResult, ex: &ExceptionInfo, ids: &ConnIds, ctx: i64) -> Value {
    match res {
        RemoteResult::Value(v) => remote_object(&v),
        RemoteResult::Object(id) => {
            let mut o = json!({ "type": "object", "objectId": ids.object_id_in(ctx, id) });
            let obj = o.as_object_mut().expect("a json object");
            if ex.is_error {
                obj.insert("subtype".into(), json!("error"));
            }
            if let Some(c) = &ex.class_name {
                obj.insert("className".into(), json!(c));
            }
            if let Some(d) = &ex.description {
                obj.insert("description".into(), json!(d));
            }
            o
        }
    }
}

/// Build a CDP `RemoteObject` from an engine [`RemoteResult`].
///
/// The arena node a DOM command names -- by `objectId` (a handle this server gave out), else by
/// `backendNodeId` / `nodeId` (both the arena id + 1).
fn node_param(engine: &mut Engine, params: &Value) -> Option<u32> {
    if let Some(oid) = params.get("objectId").and_then(Value::as_str) {
        return engine.node_of_object(ids::parse_object_id(oid)?);
    }
    let n = params.get("backendNodeId").or_else(|| params.get("nodeId")).and_then(Value::as_u64)?;
    u32::try_from(n).ok()?.checked_sub(1)
}

/// An element as CDP's `DOM.Node`, with Chrome 150's keys (recorded on the compat form fixture).
fn node_json(node: u32, n: &engine::NodeInfo, document_url: &str) -> Value {
    // a document, as Chrome 150 describes `document` (`chrome150-compat-traffic.log`): Puppeteer moves its document
    // handle into its utility world by this `backendNodeId`
    if n.node_type == 9 {
        return json!({
            "nodeId": node + 1, "backendNodeId": node + 1, "nodeType": 9, "nodeName": "#document", "localName": "",
            "nodeValue": "", "childNodeCount": n.child_count, "documentURL": document_url, "baseURL": document_url,
            "xmlVersion": "", "compatibilityMode": "NoQuirksMode",
        });
    }
    let attributes: Vec<&str> = n.attributes.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]).collect();
    json!({
        "nodeId": node + 1, "backendNodeId": node + 1, "nodeType": 1, "nodeName": n.node_name, "localName": n.local_name,
        "nodeValue": "", "childNodeCount": n.child_count, "attributes": attributes,
    })
}

/// The `objectId` is Chrome-shaped (`<isolate>.<context>.<serial>`, measured), and the serial is the
/// engine's own handle — so an id this server hands out is the id it parses back on the next
/// `callFunctionOn`.
///
/// An object says what it is, as Chrome's do -- `type`, `subtype`, `className`, `description`
/// (`Engine::object_meta`, read from V8's own constructor name and the engine's arena, never through the page's
/// members). Puppeteer and Playwright make an ELEMENT handle only of `subtype: "node"`; every object here was
/// `{"type": "object", "className": "Object"}`, so no handle could click, type or fill.
fn remote_from(engine: &mut Engine, res: RemoteResult, ids: &ConnIds, ctx: i64) -> Value {
    match res {
        RemoteResult::Value(v) => remote_object(&v),
        RemoteResult::Object(id) => {
            let object_id = ids.object_id_in(ctx, id);
            match engine.object_meta(id) {
                Some(m) => match m.subtype {
                    Some(sub) => json!({ "type": m.kind, "subtype": sub, "className": m.class_name, "description": m.description, "objectId": object_id }),
                    None => json!({ "type": m.kind, "className": m.class_name, "description": m.description, "objectId": object_id }),
                },
                None => json!({ "type": "object", "objectId": object_id, "className": "Object" }),
            }
        }
    }
}

/// A property's value from [`Engine::own_properties`] as a `Runtime.RemoteObject`.
fn prop_value(engine: &mut Engine, v: engine::PropValue, ids: &ConnIds, ctx: i64) -> Value {
    match v {
        engine::PropValue::Object(id) => remote_from(engine, RemoteResult::Object(id), ids, ctx),
        engine::PropValue::Primitive(json) => json,
    }
}

/// `Emulation.UserAgentMetadata` as the override keeps it.
fn ua_metadata(m: &Value) -> spec::devtools::UaMetadata {
    let st = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let brands = |k: &str| -> Vec<spec::Brand> {
        m.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|b| Some(spec::Brand { brand: b.get("brand")?.as_str()?.to_string(), version: b.get("version")?.as_str()?.to_string() }))
                    .collect()
            })
            .unwrap_or_default()
    };
    spec::devtools::UaMetadata {
        brands: brands("brands"),
        full_version_list: brands("fullVersionList"),
        full_version: st("fullVersion"),
        platform: st("platform"),
        platform_version: st("platformVersion"),
        architecture: st("architecture"),
        model: st("model"),
        mobile: m.get("mobile").and_then(Value::as_bool).unwrap_or(false),
        bitness: st("bitness"),
        wow64: m.get("wow64").and_then(Value::as_bool).unwrap_or(false),
        form_factors: m.get("formFactors").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default(),
    }
}

/// An awaited evaluation still pending: its command's id and session, the promise's handle (`Engine::settled`), the
/// context it ran in, `returnByValue`, and how long it may wait. Chrome waits forever; this connection waits the
/// `awaitPromise` budget (`KORVUS_AWAIT_PROMISE_MS`) and then says so with a `-32000`.
struct Deferred {
    id: i64,
    session: Option<String>,
    handle: u32,
    ctx: i64,
    rbv: bool,
    until: std::time::Instant,
}

impl Deferred {
    /// The key `dispatch` answers a pending awaited evaluation with, for the loop to park instead of sending.
    const KEY: &'static str = "__korvus_deferred_await";

    fn marker(handle: u32, ctx: i64, rbv: bool) -> Value {
        json!({ Self::KEY: handle, "ctx": ctx, "rbv": rbv })
    }

    /// A pending evaluation, when `result` is `dispatch`'s marker for one.
    fn of(result: &Result<Value, RpcError>, id: i64, session: Option<String>) -> Option<Deferred> {
        let v = result.as_ref().ok()?;
        let handle = v.get(Self::KEY)?.as_u64()? as u32;
        let budget = std::time::Duration::from_millis(env_ms("KORVUS_AWAIT_PROMISE_MS", AWAIT_PROMISE_MS_DEFAULT));
        Some(Deferred {
            id,
            session,
            handle,
            ctx: v.get("ctx").and_then(Value::as_i64).unwrap_or(0),
            rbv: v.get("rbv").and_then(Value::as_bool).unwrap_or(false),
            until: std::time::Instant::now() + budget,
        })
    }
}

/// Answer every pending awaited evaluation that has settled -- or whose context went with its document (Chrome's
/// "Execution context was destroyed."), or whose wait ran out -- and keep the rest.
async fn settle_deferred(engine: &mut Lazy<'_>, ids: &mut ConnIds, pending: &mut Vec<Deferred>, write: &mut fetch::Out) -> anyhow::Result<()> {
    if pending.is_empty() || !engine.built() {
        return Ok(());
    }
    let now = std::time::Instant::now();
    let mut keep = Vec::with_capacity(pending.len());
    for d in std::mem::take(pending) {
        let live = d.ctx == ids.default_context() || ids.world_index(d.ctx).is_some();
        let mut resp = if !live {
            let _ = engine.get().release_object(d.handle);
            json!({ "id": d.id, "error": { "code": -32000, "message": "Execution context was destroyed." } })
        } else if let Some(outcome) = engine.get().settled(d.handle, d.rbv) {
            json!({ "id": d.id, "result": evaluate_result(engine.get(), outcome, ids, d.ctx) })
        } else if now >= d.until {
            let _ = engine.get().release_object(d.handle);
            json!({ "id": d.id, "error": RpcError::server("awaitPromise timed out".to_string()).to_json() })
        } else {
            keep.push(d);
            continue;
        };
        if let Some(s) = &d.session {
            resp["sessionId"] = json!(s);
        }
        write.send(Message::Text(resp.to_string())).await?;
    }
    *pending = keep;
    Ok(())
}

/// What the page logged and the client's bindings were called with since the last call, as the events Chrome sends:
/// `Runtime.bindingCalled` (always -- the client added the binding) and, with `Runtime` enabled,
/// `Runtime.consoleAPICalled` and `Runtime.exceptionThrown`, each in the context it happened in.
fn devtools_events(engine: &mut Engine, ids: &mut ConnIds) -> Vec<Value> {
    let mut out = devtools_log_events(engine, ids);
    // the page document's lifecycle after a `document.open()` / `close()` (Playwright's `setContent` waits for its
    // `load`): after what the page logged, which a `close()` follows -- Playwright clears the lifecycle at the
    // `console.debug` it writes between `open()` and `close()`
    for (name, at) in engine.take_reopened_lifecycle() {
        out.push(json!({ "method": "Page.lifecycleEvent", "params": {
            "frameId": ids.frame_id(), "loaderId": ids.loader_id(), "name": name, "timestamp": monotonic_s_at(at) } }));
    }
    out
}

/// `Debugger.scriptParsed` for one classic script the page compiled (D106), in context `ctx`. The end line is the
/// start plus the source's newline count; `hash` is a cheap FNV-1a of the source as a hex string (DevTools uses it
/// only to tell two scripts apart, not to verify them).
fn script_parsed_event(s: &webapi::devtools_log::ParsedScript, ctx: i64) -> Value {
    let hash = s.source.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
    let end_line = s.start_line + s.source.matches('\n').count() as i32;
    json!({ "method": "Debugger.scriptParsed", "params": {
        "scriptId": s.id.to_string(),
        "url": s.url,
        "startLine": s.start_line, "startColumn": 0,
        "endLine": end_line, "endColumn": 0,
        "executionContextId": ctx,
        "hash": format!("{hash:016x}"),
        "length": s.source.len(),
        "isModule": false,
    } })
}

/// [`devtools_events`]'s first part: bindings called, file choosers and drags opened, console and errors.
fn devtools_log_events(engine: &mut Engine, ids: &mut ConnIds) -> Vec<Value> {
    let mut out = Vec::new();
    for (name, payload, world) in webapi::devtools_log::take_binding_calls() {
        let ctx = world.and_then(|w| ids.world_context(w)).unwrap_or(ids.default_context());
        out.push(json!({ "method": "Runtime.bindingCalled", "params": { "name": name, "payload": payload, "executionContextId": ctx } }));
    }
    // a drag the page started while the client intercepts drags (`Input.setInterceptDrags`)
    if let Some((items, mask)) = webapi::input::take_drag_intercepted() {
        let items: Vec<Value> = items.into_iter().map(|(t, d)| json!({ "mimeType": t, "data": d })).collect();
        out.push(json!({ "method": "Input.dragIntercepted", "params": { "data": { "items": items, "dragOperationsMask": mask } } }));
    }
    for (node, multiple) in webapi::devtools_log::take_file_choosers() {
        out.push(json!({ "method": "Page.fileChooserOpened", "params": {
            "frameId": ids.frame_id(), "mode": if multiple { "selectMultiple" } else { "selectSingle" }, "backendNodeId": node + 1,
        } }));
    }
    // D106: the classic scripts the page compiled since the last drain, as `Debugger.scriptParsed`, in the page's
    // default context. Gated on the `Debugger` domain's own switch, independent of the `Runtime` console capture
    // below, so a client debugging without `Runtime.enable` still gets its scripts.
    if webapi::devtools_log::debugger_capturing() {
        let ctx = ids.default_context();
        for s in webapi::devtools_log::take_parsed_scripts() {
            out.push(script_parsed_event(&s, ctx));
        }
    }
    if !webapi::devtools_log::capturing() {
        return out;
    }
    for e in engine.take_devtools_log() {
        match e {
            engine::DevtoolsEntry::Console { kind, args, world, timestamp } => {
                let ctx = world.and_then(|w| ids.world_context(w)).unwrap_or(ids.default_context());
                let args: Vec<Value> = args.into_iter().map(|a| remote_from(engine, a, ids, ctx)).collect();
                out.push(json!({ "method": "Runtime.consoleAPICalled", "params": {
                    "type": kind, "args": args, "executionContextId": ctx, "timestamp": timestamp,
                    "stackTrace": { "callFrames": [] },
                } }));
            }
            engine::DevtoolsEntry::Error { text, url, line, column, value, class_name, description, is_error, timestamp } => {
                let ctx = ids.default_context();
                let exception = match value {
                    Some(RemoteResult::Object(id)) => {
                        let mut o = json!({ "type": "object", "objectId": ids.object_id_in(ctx, id) });
                        let obj = o.as_object_mut().expect("a json object");
                        if is_error {
                            obj.insert("subtype".into(), json!("error"));
                        }
                        if let Some(c) = &class_name {
                            obj.insert("className".into(), json!(c));
                        }
                        obj.insert("description".into(), json!(description.clone().unwrap_or_else(|| text.trim_start_matches("Uncaught ").to_string())));
                        o
                    }
                    Some(RemoteResult::Value(v)) => remote_object(&v),
                    None => json!({ "type": "undefined" }),
                };
                out.push(json!({ "method": "Runtime.exceptionThrown", "params": {
                    "timestamp": timestamp,
                    "exceptionDetails": {
                        "exceptionId": ids.next_exception_id(), "text": text, "url": url,
                        "lineNumber": line.saturating_sub(1), "columnNumber": column.saturating_sub(1),
                        "executionContextId": ctx, "exception": exception,
                    },
                } }));
            }
        }
    }
    out
}

fn remote_object(v: &Value) -> Value {
    match v {
        Value::Number(n) => json!({ "type": "number", "value": n }),
        Value::String(s) => json!({ "type": "string", "value": s }),
        Value::Bool(b) => json!({ "type": "boolean", "value": b }),
        Value::Null => json!({ "type": "object", "subtype": "null", "value": null }),
        other => json!({ "type": "object", "value": other }),
    }
}

/// The streams commands hand out (`IO.StreamHandle`): their bytes and how far a client has read.
mod io_streams {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn streams() -> &'static Mutex<(u64, HashMap<String, (Vec<u8>, usize)>)> {
        static S: OnceLock<Mutex<(u64, HashMap<String, (Vec<u8>, usize)>)>> = OnceLock::new();
        S.get_or_init(Default::default)
    }

    /// A new stream over `bytes`; its handle.
    pub(super) fn open(bytes: Vec<u8>) -> String {
        let mut s = streams().lock().unwrap_or_else(|e| e.into_inner());
        s.0 += 1;
        let handle = s.0.to_string();
        s.1.insert(handle.clone(), (bytes, 0));
        handle
    }

    /// Up to `size` bytes from `offset` (else from where the last read ended), and whether that was the end.
    pub(super) fn read(handle: &str, offset: Option<usize>, size: usize) -> Option<(Vec<u8>, bool)> {
        let mut s = streams().lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, pos) = s.1.get_mut(handle)?;
        let from = offset.unwrap_or(*pos).min(bytes.len());
        let to = (from + size.max(1)).min(bytes.len());
        *pos = to;
        Some((bytes[from..to].to_vec(), to >= bytes.len()))
    }

    pub(super) fn close(handle: &str) -> bool {
        streams().lock().unwrap_or_else(|e| e.into_inner()).1.remove(handle).is_some()
    }
}

/// Banner string used by the binary.
pub fn cdp_name() -> &'static str {
    "korvus-cdp (HTTP+WebSocket, G1.8)"
}

/// The CDP server as a host-level [`spec::ServingPlugin`] (D5: serving is a plugin). It holds the engine
/// factory; `serve` binds a listener and runs the existing accept loop. The binary loads this to run as
/// a daemon; not loading it (e.g. the `fetch` command drives an `Engine` directly) is library mode.
pub struct CdpServing<F: Fn() -> engine::Engine> {
    make_engine: F,
    /// The identity the discovery endpoints answer from, so `/json/version` costs no realm
    /// (see [`Supply`]).
    persona: std::sync::Arc<dyn spec::Persona>,
}
impl<F: Fn() -> engine::Engine> CdpServing<F> {
    pub fn new(make_engine: F, persona: std::sync::Arc<dyn spec::Persona>) -> Self {
        Self { make_engine, persona }
    }
}
impl<F: Fn() -> engine::Engine + 'static> spec::ServingPlugin for CdpServing<F> {
    fn name(&self) -> &str {
        cdp_name()
    }
    fn serve(
        self: Box<Self>,
        addr: std::net::SocketAddr,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>>>> {
        Box::pin(async move {
            let CdpServing { make_engine, persona } = *self;
            let listener = TcpListener::bind(addr).await?;
            serve(listener, make_engine, persona).await.map_err(|e| format!("cdp serve: {e}"))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod io_stream_tests {
    #[test]
    fn a_stream_is_read_in_chunks_to_its_end_and_closed() {
        let h = super::io_streams::open(b"%PDF-1.4 body".to_vec());
        assert_eq!(super::io_streams::read(&h, None, 5), Some((b"%PDF-".to_vec(), false)));
        assert_eq!(super::io_streams::read(&h, None, 100), Some((b"1.4 body".to_vec(), true)));
        assert_eq!(super::io_streams::read(&h, Some(0), 4), Some((b"%PDF".to_vec(), false)), "an offset reads from there");
        assert!(super::io_streams::close(&h));
        assert!(super::io_streams::read(&h, None, 1).is_none(), "a closed stream is gone");
    }
}
