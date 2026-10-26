//! The BROWSER endpoint Puppeteer and Playwright connect to -- page targets created on demand,
//! browser contexts -- built over the single-page server this crate already is.
//!
//! `korvus browser <port>` serves this. Every page target is a `korvus serve` process of its own (the
//! child), started with the target's ids, which serves exactly the single page its id names; this router
//! holds one WebSocket to each child's PAGE socket and relays the clients' sessions to it. Why a process
//! per target and not a thread: the transport's request ledger, the CDP Fetch interceptor and the page's
//! timezone are process-wide (`transport::ledger()`, `transport::intercept`, `korvus/src/main.rs`), so two
//! pages in one process would report each other's requests. A process is the isolation Chrome's own
//! renderers have.
//!
//! **A session is a browser context (D8).** What a context's pages must share -- cookies, each origin's
//! `localStorage`, Cache API and IndexedDB -- lives in the router's context store
//! (`transport::context_store`), as Chrome keeps a profile's storage in its browser process: every page
//! process of a context reaches its context's slots there, and no other context's (I2). What a context is
//! made with -- its persona name, identity, seed, device, proxy, timezone, locale and plugins -- is fixed
//! when it is made (I1) and written once to its **spec** (`<runtime dir>/contexts/<id>.json`), which each of
//! its page processes reads at start (`--context <path>`).
//!
//! **Many clients at once (D9).** Each connection is a task of its own; one actor owns the browser's state
//! and every message -- a client's command, a child's answer or event, a started target -- reaches it as an
//! [`Ev`]. A connection sees its HOME context (the browser's default context, or for `/persona/<name>` that
//! persona's context) and the contexts it created, and no other connection's: two clients sharing one
//! endpoint never see, attach to or close each other's sessions.
//!
//! What a client sees follows Chrome 150, recorded raw on its browser socket
//! (`~/bench-lean/cdp/probe/chrome150-target-shapes.jsonl`, 2026-10-12):
//! * `Target.createTarget`: `targetCreated` (attached false, title "") -> `targetInfoChanged` (attached) ->
//!   `attachedToTarget` (`waitingForDebugger` as the auto-attach asked) -> the answer -> `targetInfoChanged`
//!   (title "about:blank").
//! * `Target.setAutoAttach` on the browser: each existing page `targetInfoChanged` (attached) +
//!   `attachedToTarget` (`waitingForDebugger: false`), then the answer.
//! * `Target.attachToTarget`: `attachedToTarget`, then `{sessionId}`.
//! * `Target.detachFromTarget`: `detachedFromTarget`, then `{}`.
//! * `Target.closeTarget`: `{success: true}` FIRST, then per session `Inspector.detached` ("Render process
//!   gone."), `targetInfoChanged` (attached false), `detachedFromTarget`, and `targetDestroyed`.
//! * `Target.getBrowserContexts`: the created contexts plus `defaultBrowserContextId`.
//! * `Target.getTargetInfo` with no `targetId`: the browser target itself.
//! * An unknown method: `-32601 'X' wasn't found`; an unknown session: `-32001 Session with given id not found.`
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::ids::{browser_uuid, hex32};
use crate::{RpcError, VersionInfo};

/// How a page target's process is started: `exe args... 0`, with the target's ids as its last arguments
/// (`serve 0 --cdp-child <target id> <context id>`; ARGUMENTS, not environment: every `KORVUS_*` variable is part of
/// the template blueprints' cache key, so a per-target id there made each new page record its templates afresh and
/// write a 3.8 MB blueprint of its own). The child binds an ephemeral
/// loopback port, writes `KORVUS-TARGET-PORT <port>` as its only line on stdout, and ends when its stdin
/// closes, so a router that dies takes its targets with it.
pub struct Launcher {
    pub exe: std::path::PathBuf,
    pub args: Vec<String>,
    /// After the target's ids: what every page process of this browser shares -- `--gpu-socket <path>`, the one GPU
    /// service the browser started (Chrome's one GPU process per browser).
    pub tail: Vec<String>,
}

/// The argument that makes `korvus serve-page` a page target's process: `serve-page <port> --cdp-child <target id>
/// <context id>`.
pub const CHILD_FLAG: &str = "--cdp-child";

/// The argument that hands a page process its context's spec: `--context <path>` ([`ContextSpec`]).
pub const CONTEXT_FLAG: &str = "--context";

/// The ids a page target's process was started with (its target and its browser context), set once by the binary
/// before it serves (`set_child_ids`), read by [`crate::ids::BrowserIds::new`].
static CHILD_IDS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();

/// This process is page target `target` in browser context `context` (`korvus serve … --cdp-child`).
pub fn set_child_ids(target: String, context: String) {
    let _ = CHILD_IDS.set((target, context));
}

/// The ids this process was started with as a page target, if it was.
pub(crate) fn child_ids() -> Option<&'static (String, String)> {
    CHILD_IDS.get()
}

/// The line a child announces its port with.
pub const PORT_LINE: &str = "KORVUS-TARGET-PORT ";

/// A page process reports the metrics its plugins recorded (plugin ABI 3, A6) on one stdout line, so they reach the
/// router without a channel of their own: this prefix then a JSON array of `{name, value, labels}`.
pub const METRIC_LINE: &str = "KORVUS-METRIC ";
/// A page process's download (a client's `Browser.setDownloadBehavior`): `{frameId, guid, url, suggestedFilename}` as it
/// begins, `{guid, totalBytes, receivedBytes, state}` as it ends -- the router's `Browser.downloadWillBegin` and
/// `Browser.downloadProgress` to the clients that see the page's context.
pub const DOWNLOAD_LINE: &str = "KORVUS-DOWNLOAD ";

/// The durable chrome.storage.local bridge (A4): a page process forwards a plugin's KV host calls to the router's
/// KV actor over its stdout. `KV_SET_LINE` carries `{ctx,key,value}` (fire-and-forget); `KV_GET_LINE` carries
/// `{id,ctx,key}` and the router answers with [`KV_REPLY_LINE`] written to the child's stdin.
pub const KV_SET_LINE: &str = "KORVUS-KV-SET ";
pub const KV_GET_LINE: &str = "KORVUS-KV-GET ";
/// The router's reply to a `KORVUS-KV-GET`, written to the child's stdin: this prefix then `{id,value}` (`value`
/// null = the key is absent). The child's korvus-lifeline thread routes it to the waiting `kv_get` by `id`.
pub const KV_REPLY_LINE: &str = "KORVUS-KV-REPLY ";

/// A popup a page's script opened, as a target of its own (D74 step 3, `webapi`'s `core/html/popup.rs`). A page process
/// says what crosses between it and its popups on stdout, one line each, and hears the other side on stdin.
/// From the OPENER's process: open popup `{rid, url, name, noopener, referrer}`; post `{rid, data, origin}` (`data`
/// structured-clone bytes, base64); navigate `{rid, url}`; close `{rid}`.
pub const POPUP_OPEN_LINE: &str = "KORVUS-POPUP-OPEN ";
pub const POPUP_POST_LINE: &str = "KORVUS-POPUP-POST ";
pub const POPUP_NAV_LINE: &str = "KORVUS-POPUP-NAV ";
pub const POPUP_CLOSE_LINE: &str = "KORVUS-POPUP-CLOSE ";
/// From the POPUP's process: `opener.postMessage` `{data, origin}`; its own `window.close()`.
pub const OPENER_POST_LINE: &str = "KORVUS-OPENER-POST ";
pub const WINDOW_CLOSE_LINE: &str = "KORVUS-WINDOW-CLOSE ";
/// To the opener's process: `{rid, kind: "message", data, origin}` or `{rid, kind: "closed"}`.
pub const POPUP_EV_LINE: &str = "KORVUS-POPUP-EV ";
/// To the popup's process: the opener's `postMessage` `{data, origin}`; the opener's target is gone.
pub const OPENER_MSG_LINE: &str = "KORVUS-OPENER-MSG ";
pub const OPENER_GONE_LINE: &str = "KORVUS-OPENER-GONE ";
/// The flag that starts a page process as a popup: its opener's origin, `-` when it was opened `noopener`.
pub const POPUP_FLAG: &str = "--popup";
/// A page process shows a same-site popup its script opened and serves it as a target of its own (`{targetId, url,
/// name}`): the router announces it in the opener's context and relays its sessions' commands to that process,
/// marked with the target (`korvusTarget`), as the process marks the popup's answers and events.
pub const POPUP_HOSTED_LINE: &str = "KORVUS-POPUP-HOSTED ";
/// That popup closed (`{targetId}`): its target goes.
pub const POPUP_HOSTED_CLOSE_LINE: &str = "KORVUS-POPUP-HOSTED-CLOSE ";
/// A page process's own event to its router: the page's `document.title` changed (D88 step 2). The router keeps it
/// for `Target.getTargetInfo` and the next `targetInfoChanged`, as Chrome's browser keeps a tab's title, and relays it
/// to no client.
pub const TITLE_EVENT: &str = "Korvus.targetTitle";
/// A page's `alert` / `confirm` / `prompt` (D88 step 2, `webapi`'s `core/html/dialog.rs`): from its process
/// `{type, message, defaultPrompt}` while its thread waits; to it the answer `{accept, promptText}`, or
/// `{unhandled: true}` at once when no session that enabled `Page` is there to answer.
pub const DIALOG_OPEN_LINE: &str = "KORVUS-DIALOG-OPEN ";
pub const DIALOG_CLOSE_LINE: &str = "KORVUS-DIALOG-CLOSE ";

/// A context's proxy, as `Target.createBrowserContext` names it (`proxyServer`, `proxyBypassList`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxySpec {
    pub server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass: Option<String>,
}

/// What a browser context is made with, fixed when it is made (invariant I1) and read by each of its page processes
/// at start. `None` / empty: the endpoint's own default for that setting.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextSpec {
    /// The context's id, as CDP names it.
    pub context: String,
    /// The persona's name: `/persona/<name>`'s, or `korvus.name`.
    #[serde(default)]
    pub name: Option<String>,
    /// `korvus.identity`: `stealth` or `korvus` (the identity slot, goal 1020 step 4).
    #[serde(default)]
    pub identity: Option<String>,
    /// `korvus.seed`: the seed the context's persona is drawn and perturbed with.
    #[serde(default)]
    pub seed: Option<String>,
    /// `korvus.device`: `pool`, `host` or a persona document's path.
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub proxy: Option<ProxySpec>,
    /// `korvus.timezone`: an IANA zone.
    #[serde(default)]
    pub timezone: Option<String>,
    /// `korvus.locale`: a BCP 47 tag.
    #[serde(default)]
    pub locale: Option<String>,
    /// `korvus.rotation`: `rotate`, `named` or `off` -- how the identity's slots choose the context's seed.
    #[serde(default)]
    pub rotation: Option<String>,
    /// `korvus.humanize`: CDP mouse moves along the behaviour model's paths.
    #[serde(default)]
    pub humanize: Option<bool>,
    /// `korvus.plugins`: plugin library paths, in load order.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Where the context's shared state is (`transport::context_store`).
    #[serde(default)]
    pub store: Option<transport::context_store::Endpoint>,
    /// What the loaded plugins' `rotation`, `device` and `identity` slots answered when the context was made (goal 1020
    /// step 2): the context's seed and stable name, its device facts and its persona document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawn: Option<Drawn>,
    /// `korvus.fresh`: a named persona's context that starts with no cookies or storage and keeps none -- its profile
    /// on disk is neither read nor written (a clean, reproducible run of a persona that otherwise remembers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh: Option<bool>,
    /// The permissions a DevTools client granted the context (`Browser.grantPermissions`); its later pages start with
    /// them, as its pages already open are told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// How a DevTools client said the context downloads (`Browser.setDownloadBehavior`): `(behavior, downloadPath)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<(String, String)>,
}

/// The answer slots' answers for one context.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Drawn {
    pub seed: Option<u64>,
    pub name: Option<String>,
    pub device: Option<Value>,
    pub identity: Option<Value>,
}

impl ContextSpec {
    /// The spec at `path`, as the router wrote it.
    pub fn load(path: &Path) -> anyhow::Result<ContextSpec> {
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read the context spec {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("bad context spec {}: {e}", path.display()))
    }

    /// The settings `Target.createBrowserContext` carries: Chrome's `proxyServer` / `proxyBypassList`, and korvus's
    /// own, as a `korvus` object or as `korvus.<setting>` keys.
    fn from_params(context: &str, params: &Value) -> ContextSpec {
        let mut spec = ContextSpec { context: context.to_string(), ..ContextSpec::default() };
        if let Some(server) = params.get("proxyServer").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let bypass = params.get("proxyBypassList").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
            spec.proxy = Some(ProxySpec { server: server.to_string(), bypass });
        }
        let korvus = |key: &str| -> Option<Value> {
            params.get("korvus").and_then(|k| k.get(key)).cloned().or_else(|| params.get(format!("korvus.{key}")).cloned())
        };
        let text = |key: &str| korvus(key).and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        spec.name = text("name");
        spec.identity = text("identity");
        spec.seed = text("seed");
        spec.device = text("device");
        spec.timezone = text("timezone");
        spec.locale = text("locale");
        spec.rotation = text("rotation");
        spec.humanize = korvus("humanize").and_then(|v| v.as_bool());
        spec.fresh = korvus("fresh").and_then(|v| v.as_bool());
        // a proxy as korvus names it, with its login (`korvus.proxy`), when Chrome's `proxyServer` names none
        if spec.proxy.is_none() {
            spec.proxy = text("proxy").map(|server| ProxySpec { server, bypass: None });
        }
        if let Some(Value::Array(a)) = korvus("plugins") {
            spec.plugins = a.iter().filter_map(Value::as_str).map(String::from).collect();
        }
        spec
    }
}

/// The settings every context of this endpoint takes unless it states its own (`korvus serve`'s flags, the
/// environment, config.toml: goal 1020 step 5), and where named personas live.
#[derive(Clone, Debug, Default)]
pub struct Defaults {
    pub seed: Option<String>,
    pub device: Option<String>,
    pub proxy: Option<String>,
    pub timezone: Option<String>,
    pub locale: Option<String>,
    pub humanize: Option<bool>,
    /// `<korvus home>/personas`: a context named `<name>` whose `<name>/persona.json` is there wears it and keeps
    /// its profile in `<name>/profile/`.
    pub personas: Option<PathBuf>,
    /// `korvus serve --fresh`: every named persona's context starts clean and keeps nothing (see `ContextSpec::fresh`).
    pub fresh: bool,
}

static DEFAULTS: std::sync::OnceLock<Defaults> = std::sync::OnceLock::new();

/// Set this endpoint's [`Defaults`], once, before it serves.
pub fn set_defaults(d: Defaults) {
    let _ = DEFAULTS.set(d);
}

/// A [`plugin_host::ContextInfo`] for `spec`, as the answer and storage slots read a context: its id, persona name,
/// host OS, proxy, identity and settings. The answer slots (`drawn`) pass the `exit_ip` they looked up; the storage
/// slots key off the context and pass "".
fn context_info(spec: &ContextSpec, exit_ip: String) -> plugin_host::ContextInfo {
    let settings = json!({
        "name": spec.name, "identity": spec.identity, "seed": spec.seed, "device": spec.device,
        "timezone": spec.timezone, "locale": spec.locale, "rotation": spec.rotation,
    });
    plugin_host::ContextInfo {
        context_id: spec.context.clone(),
        name: spec.name.clone().unwrap_or_default(),
        host_os: match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            _ => "linux",
        }
        .to_string(),
        proxy: spec.proxy.as_ref().map(|p| p.server.clone()).unwrap_or_default(),
        identity: spec.identity.clone().unwrap_or_default(),
        config_json: settings.to_string(),
        exit_ip,
    }
}

/// Ask the loaded plugins' answer slots about a new context: rotation (its seed and stable name), then device (the
/// machine it presents, for that seed), then identity (its whole persona document, given that device). `None` when no
/// plugin answers any of them. Also how a process that is its own one context (`korvus fetch`, `serve`) finds the
/// identity it wears.
pub fn drawn(spec: &ContextSpec) -> Option<Drawn> {
    if !plugin_host::any() {
        return None;
    }
    let info = context_info(spec, exit_ip(spec));
    let rotation = plugin_host::rotation(&info);
    let seed = rotation.as_ref().map(|r| r.0).or_else(|| spec.seed.as_deref().and_then(|s| s.parse().ok()));
    let device = plugin_host::device(&info, seed.unwrap_or(0));
    let identity = plugin_host::identity(&info, device.as_deref().unwrap_or("{}"));
    if rotation.is_none() && device.is_none() && identity.is_none() {
        return None;
    }
    let parse = |s: Option<String>| s.and_then(|j| serde_json::from_str::<Value>(&j).ok());
    Some(Drawn { seed, name: rotation.map(|r| r.1).filter(|n| !n.is_empty()), device: parse(device), identity: parse(identity) })
}

/// Where a proxied context's proxy exits, for the identity's slots (a persona's zone and locale follow it): asked of an
/// IP echo through the proxy (`$KORVUS_IP_ECHO`, else `https://api.ipify.org`) when the context asks for no identity
/// of korvus's own and states no zone or locale of its own. "" when it has no proxy, or the echo does not answer.
fn exit_ip(spec: &ContextSpec) -> String {
    let Some(p) = spec.proxy.as_ref() else { return String::new() };
    if spec.identity.as_deref() == Some("korvus") || (spec.timezone.is_some() && spec.locale.is_some()) {
        return String::new();
    }
    let server = if p.server.contains("://") { p.server.clone() } else { format!("http://{}", p.server) };
    let echo = std::env::var("KORVUS_IP_ECHO").ok().filter(|e| !e.is_empty()).unwrap_or_else(|| "https://api.ipify.org".into());
    match transport::exit_ip(&server, &echo, std::time::Duration::from_secs(10)) {
        Ok(ip) => ip.to_string(),
        Err(e) => {
            eprintln!("[korvus] context {}: where its proxy exits: {e}", spec.context);
            String::new()
        }
    }
}

/// What a child's socket said, or that it closed.
enum FromChild {
    Text(String),
    Closed,
}

struct Target {
    id: String,
    context: String,
    url: String,
    title: String,
    window_id: u32,
    bounds: Value,
    to_child: mpsc::UnboundedSender<String>,
    /// Lines to the page process's stdin (the KV bridge's replies, the popup lines).
    to_stdin: mpsc::UnboundedSender<String>,
    /// The target whose script opened this one as a popup (D74 step 3), and whether this one may reach it (`false`
    /// under `noopener`).
    opener: Option<(String, bool)>,
    /// A client made it (`Target.createTarget`): its context is a session, which the license's cap counts. The
    /// endpoint's own first page is not, until a client makes one beside it.
    counted: bool,
    /// Owned so the process ends with the target (`kill_on_drop`); its stdin is the router's lifeline. `None` for a
    /// target served by another target's process ([`Target::host`]).
    _child: Option<tokio::process::Child>,
    /// The target whose process serves this one: a same-site popup that stays in its opener's process
    /// (`POPUP_HOSTED_LINE`). Its commands go there marked with this target's id (`korvusTarget`).
    host: Option<String>,
    /// The first navigation of a new target an auto-attached client paused (`waitForDebuggerOnStart`), held until it
    /// sends `Runtime.runIfWaitingForDebugger`.
    pending_nav: Option<String>,
    /// The client (and session) whose `Page.startScreencast` is running in this page: its frames go there alone, as
    /// Chrome's `PageHandler` sends them to the session that asked.
    screencast: Option<(u64, Option<String>)>,
    /// The JavaScript dialog the page is waiting on (D88 step 2): its `javascriptDialogOpening` params.
    dialog: Option<Value>,
}

impl Target {
    fn info(&self, attached: bool) -> Value {
        let mut info = json!({
            "targetId": self.id,
            "type": "page",
            "title": self.title,
            "url": self.url,
            "attached": attached,
            "canAccessOpener": self.opener.as_ref().is_some_and(|(_, linked)| *linked),
            "browserContextId": self.context,
        });
        if let Some((opener, _)) = &self.opener {
            info["openerId"] = json!(opener);
        }
        info
    }
}

/// A session a client holds: on a page target (relayed with its `sessionId` removed -- the child's page
/// socket takes session-less commands as the page's), or on a child's own child (a worker the child
/// auto-attached, relayed with its `sessionId` as the child named it).
#[derive(Clone)]
enum Session {
    Page(String),
    Child(String),
    /// `Target.attachToBrowserTarget`'s: the browser's own target, as a session -- its commands are the browser's,
    /// and what they answer and announce carries the session's id (Playwright's `newCDPSession` attaches its page
    /// sessions through one).
    Browser,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The browser's own: every root connection's home, with Chrome's first blank page.
    Default,
    /// `/persona/<name>`'s: the home of every connection to that name.
    Persona,
    /// `Target.createBrowserContext`'s: seen by the connection that made it.
    Created,
}

struct Ctx {
    id: String,
    kind: Kind,
    /// The connection that made it (`Kind::Created`).
    owner: Option<u64>,
    /// `disposeOnDetach`: it goes when its owner does.
    dispose_on_detach: bool,
    spec: ContextSpec,
    spec_path: PathBuf,
    /// A named persona's profile (`<persona>/profile/store.bin`): its store is saved there.
    profile: Option<PathBuf>,
}

/// One client connection's view: its sessions and what it asked to hear.
struct Client {
    out: mpsc::UnboundedSender<String>,
    /// The context this connection's session-less `createTarget` and `defaultBrowserContextId` mean.
    home: String,
    discover: bool,
    /// `Target.setAutoAttach` on the browser: `Some(waitForDebuggerOnStart)` once armed.
    auto_attach: Option<bool>,
    sessions: HashMap<String, Session>,
    /// A `/devtools/page/<id>` connection: its session-less commands are that page's.
    page_socket: Option<String>,
    /// The tenant this connection belongs to (plugin ABI 3, G1): the id `on_connect` returned, or -1 when no
    /// tenancy plugin gates connections. Read for `context_allow` (G2) and `queue_order` (G4).
    tenant: i64,
    /// The sessions (`""`: a page socket's own) that sent `Page.enable`: they hear a page's dialogs and may answer
    /// them, as Chrome's `PageHandler` does (D88 step 2).
    page_on: std::collections::HashSet<String>,
    /// The `Host` it came in by: where a ticket it mints says to connect.
    host: String,
    /// A viewer connection's grant (D88 step 3); `None` on the automation channel.
    viewer: Option<Grant>,
}

impl Client {
    fn send(&self, v: Value) {
        let _ = self.out.send(v.to_string());
    }
}

/// What a command does once its answer is out: Chrome answers `closeTarget` and `disposeBrowserContext` before the
/// closed pages' events.
enum Then {
    CloseTarget(String),
    RemoveContext(String),
}

/// A `createTarget` waiting for its process to start.
struct Job {
    client: u64,
    id: Value,
    context: String,
    url: String,
    /// A popup a page's script opened (no client asked; nothing is answered).
    popup: Option<PopupJob>,
}

/// A popup's opening: the target and `rid` of its opener, `noopener`, and the referrer its navigation carries.
#[derive(Clone)]
struct PopupJob {
    opener: String,
    rid: u64,
    noopener: bool,
    opener_origin: String,
}

/// Everything the browser's actor hears.
enum Ev {
    /// An HTTP discovery request: the answer's body.
    Http { path: String, host: String, reply: oneshot::Sender<String> },
    /// A WebSocket connection opened on `path`; `auth` is its bearer token and `peer` its address (plugin ABI 3,
    /// G1); `ok` says whether the router took it.
    Open { client: u64, path: String, auth: String, peer: String, out: mpsc::UnboundedSender<String>, ok: oneshot::Sender<bool>, door: Door },
    Text { client: u64, text: String },
    Gone { client: u64 },
    Child { target: String, ev: FromChild },
    Spawned { job: Job, result: Result<Target, String> },
    /// A page process's plugins recorded metrics (plugin ABI 3, A6): a JSON array of `{name, value, labels}`.
    Metric { target: String, json: String },
    /// A page process's popup line (D74 step 3): its prefix and its JSON.
    Popup { target: String, line: &'static str, json: String },
    /// A page process's download line (`DOWNLOAD_LINE`).
    Download { target: String, json: String },
    /// A plugin emitted a CDP event on a domain it serves (plugin ABI 3, A1): broadcast to the clients.
    PluginEvent { method: String, params: String },
    /// Time to write the named personas' profiles.
    Flush,
    /// The license's cap changed (`set_cap`).
    Cap,
    /// Time to sample the page processes' CPU and memory into the per-context metrics (plugin ABI 3, G3).
    Sample,
    /// A tenancy plugin asked to idle / wake a context (plugin ABI 3, G5): freeze or thaw its page processes.
    Hibernate(String),
    Resume(String),
    /// A tenancy plugin read / wrote the shared per-context KV (plugin ABI 3, A4).
    KvGet { context: String, key: String, reply: oneshot::Sender<Option<String>> },
    KvSet { context: String, key: String, value: String },
}

/// How a WebSocket came in (D88): the `Host` it named (for the URLs a ticket is handed out with), the page origin it
/// carried, and the viewer ticket it presented (`?ticket=`), if any.
struct Door {
    host: String,
    origin: Option<String>,
    ticket: Option<String>,
}

/// A viewer connection's grant (D88 step 3): the ticket it came in with, and what that ticket allows.
#[derive(Clone)]
struct Grant {
    ticket: String,
    interactive: bool,
}

/// A metric total the router keeps, summed per (context, name, labels). Served at `GET /korvus/metrics`.
struct MetricRow {
    context: String,
    name: String,
    labels: String,
    value: f64,
}

// ----- GET /korvus/har: the request ledger as a HAR 1.2 log -----------------------------------------
//
// The transport's [request ledger](transport::ledger) holds every request a process performed since its last
// navigation reset -- the same rows `Network.*` reports (`network.rs`) and `GET /korvus/metrics` counts.
// `GET /korvus/har` serves them in HAR 1.2, the interchange format DevTools' "Save all as HAR" and
// `haralyzer` read, so a run's whole request / response / timing firehose is one GET. Request HEADERS are not
// in the ledger (it records what the transport did, not the per-hop request block it rebuilt --
// `RequestRecord::headers` is the RESPONSE's), so `request.headers` is empty and the response side carries
// what was measured; a redirect chain is one ledger row rendered as its hops then its final response, the way
// Chrome exports them. The serializer is shared: a page process serves its OWN ledger here (`handle_conn`,
// lib.rs), and the browser endpoint's router serves the same path for the router process (see `http`).

/// The ledger `records` as a HAR 1.2 `{"log":{…}}` value. `dropped` is the rows lost to the ledger cap; a
/// non-zero count is reported as `log._dropped` so a truncated firehose says so rather than read short.
pub fn har_json(records: &[transport::RequestRecord], dropped: u64) -> Value {
    // The ledger stores only times RELATIVE to its generation's epoch (an `Instant`), never wall time, so
    // absolute `startedDateTime`s are reconstructed against the serialize-time clock: the latest request is
    // placed at ~now and the earlier ones by their `started_ms` offset. The relative spacing is exact; the
    // absolute instant is off by at most this call's own age.
    // TODO(1021-verify): if HAR timestamps must be absolute to the millisecond, expose the ledger
    // generation's wall epoch from `transport` and offset from it instead.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    let latest = records.iter().map(|r| r.started_ms).fold(0.0_f64, f64::max);
    let mut entries = Vec::new();
    for r in records {
        har_entries(r, now_ms - (latest - r.started_ms), &mut entries);
    }
    let mut log = json!({
        "version": "1.2",
        "creator": { "name": "korvus", "version": env!("CARGO_PKG_VERSION") },
        "pages": [],
        "entries": entries,
    });
    if dropped > 0 {
        log["_dropped"] = json!(dropped);
    }
    json!({ "log": log })
}

/// The fields of one HAR entry drawn from a ledger row (see [`har_entries`]).
struct HarEntry<'a> {
    start_ms: f64,
    time_ms: f64,
    method: &'a str,
    url: &'a str,
    status: i64,
    status_text: &'a str,
    resp_headers: &'a [(String, String)],
    body_size: i64,
    redirect_url: &'a str,
    kind: &'a str,
    id: u64,
    from_cache: bool,
    error: Option<&'a str>,
}

/// One ledger row as HAR `entries[]`: an entry per redirect hop it followed (each the request that asked and
/// the redirect response that answered, with its `redirectURL`), then the final request / response, or the
/// failure (`status` 0, the block reason or transport error as `_error`). `start_ms` is the row's start as
/// Unix milliseconds. The whole chain's measured wire time is charged to the final entry's `wait`.
fn har_entries(r: &transport::RequestRecord, start_ms: f64, out: &mut Vec<Value>) {
    let (mut url, mut method) = (r.url.as_str(), r.method.as_str());
    for h in &r.hops {
        out.push(har_entry(HarEntry {
            start_ms,
            time_ms: 0.0,
            method,
            url,
            status: h.status as i64,
            status_text: h.status_text.as_str(),
            resp_headers: &h.headers,
            body_size: 0,
            redirect_url: h.next_url.as_str(),
            kind: r.kind.as_str(),
            id: r.id,
            from_cache: false,
            error: None,
        }));
        url = h.next_url.as_str();
        method = h.next_method.as_str();
    }
    let (status, status_text) = match r.status {
        Some(s) => (s as i64, r.status_text.as_str()),
        None => (0, r.blocked_reason.as_deref().or(r.error.as_deref()).unwrap_or("")),
    };
    out.push(har_entry(HarEntry {
        start_ms,
        time_ms: r.ms,
        method,
        url,
        status,
        status_text,
        resp_headers: &r.headers,
        body_size: r.bytes as i64,
        redirect_url: "",
        kind: r.kind.as_str(),
        id: r.id,
        from_cache: r.from_cache,
        error: r.error.as_deref().filter(|e| !e.is_empty()),
    }));
}

/// Render one [`HarEntry`] as a HAR 1.2 `entries[]` object. The `_`-prefixed fields are HAR's sanctioned
/// place for extra data (Chrome's own export uses `_resourceType`): the ledger row id, Fetch destination,
/// cache hit and, for a failure, the error — the observability a firehose is read for.
fn har_entry(e: HarEntry) -> Value {
    let headers: Vec<Value> = e.resp_headers.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect();
    let mime = if e.status == 0 && e.error.is_some() {
        "x-unknown".to_string()
    } else {
        transport::mime::mime_type_and_charset(e.resp_headers).mime
    };
    let mut entry = json!({
        "startedDateTime": iso8601_utc(e.start_ms),
        "time": e.time_ms,
        "request": {
            "method": e.method,
            "url": e.url,
            // The ledger has no request headers or body (see the module note above); `httpVersion` is not
            // recorded either, so it is "" rather than a fabricated version. `-1` is HAR's "unknown size".
            "httpVersion": "",
            "cookies": [],
            "headers": [],
            "queryString": query_string(e.url),
            "headersSize": -1,
            "bodySize": -1,
        },
        "response": {
            "status": e.status,
            "statusText": e.status_text,
            "httpVersion": "",
            "cookies": [],
            "headers": headers,
            "content": { "size": e.body_size.max(0), "mimeType": mime },
            "redirectURL": e.redirect_url,
            "headersSize": -1,
            "bodySize": e.body_size,
        },
        "cache": {},
        "timings": { "blocked": -1, "dns": -1, "connect": -1, "send": 0, "wait": e.time_ms, "receive": 0, "ssl": -1 },
        "_requestId": e.id,
        "_resourceType": e.kind,
        "_fromCache": e.from_cache,
    });
    if let Some(err) = e.error {
        entry["_error"] = json!(err);
    }
    entry
}

/// A url's query as HAR `queryString` name / value pairs, raw (as `RequestRecord` stores the url).
fn query_string(url: &str) -> Value {
    let q = url.split_once('?').map(|(_, q)| q.split('#').next().unwrap_or("")).unwrap_or("");
    let pairs: Vec<Value> = q
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => json!({ "name": k, "value": v }),
            None => json!({ "name": p, "value": "" }),
        })
        .collect();
    Value::Array(pairs)
}

/// A Unix-millisecond instant as a HAR `startedDateTime`: ISO 8601 UTC with milliseconds. Days are turned
/// into a civil date with Howard Hinnant's algorithm, so no date dependency is pulled in for one field.
fn iso8601_utc(unix_ms: f64) -> String {
    let ms = unix_ms.max(0.0) as i64;
    let (secs, millis) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, tod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// The browser: its contexts and targets, which outlive a client connection, as Chrome's do.
struct Browser {
    persona: Arc<dyn spec::Persona>,
    launch: Arc<Launcher>,
    uuid: String,
    default_context: String,
    /// Every context, the default first, then in creation order.
    contexts: Vec<Ctx>,
    /// Page targets, newest first (`Target.getTargets` lists Chrome's newest first).
    targets: Vec<Target>,
    clients: HashMap<u64, Client>,
    /// A command relayed to a child, by the id the router gave it: the client, its own id and its session.
    pending: HashMap<u64, (u64, Value, Option<String>)>,
    next_child_id: u64,
    ev: mpsc::UnboundedSender<Ev>,
    /// `<temp>/korvus-browser-<pid>`: the contexts' specs.
    dir: PathBuf,
    store: Arc<transport::context_store::Server>,
    /// The current command's [`Then`]s.
    then: Vec<Then>,
    /// Metric totals from the page processes and the router's own slots (plugin ABI 3, A6), summed per
    /// (context, name, labels).
    metrics: Vec<MetricRow>,
    /// `createTarget`s waiting for a session under the license's cap (goal 1020 step 6), first come first served.
    queue: std::collections::VecDeque<Job>,
    /// Contexts a `createTarget` is starting a page in: sessions already, for the count.
    starting: HashMap<String, usize>,
    /// The shared per-context key/value store a tenancy plugin keeps across a context's hibernate/resume (plugin
    /// ABI 3, A4): `context -> key -> value`, cleared when the context goes.
    kv: HashMap<String, HashMap<String, String>>,
    /// Each popup's target by (its opener's target, the opener's id for it) (D74 step 3).
    popup_rids: HashMap<(String, u64), String>,
    /// The viewer tickets minted and not yet expired or revoked (D88 step 3).
    tickets: HashMap<String, crate::viewer::Ticket>,
}

/// The sessions this endpoint may run at once (the license's allocation, goal 1020 step 6): none counted until the
/// endpoint sets it. A lower cap never stops a running session (invariant I3); it only makes a new one wait.
static CAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
/// The sessions running now and waiting now, as the actor last counted them.
static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static QUEUED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The actor, for [`set_cap`].
static ACTOR: std::sync::OnceLock<mpsc::UnboundedSender<Ev>> = std::sync::OnceLock::new();

/// Set the license's cap on sessions; a raised one lets waiting sessions in.
pub fn set_cap(n: usize) {
    CAP.store(n, std::sync::atomic::Ordering::Relaxed);
    if let Some(tx) = ACTOR.get() {
        let _ = tx.send(Ev::Cap);
    }
}

/// The sessions running now (what a heartbeat reports).
pub fn active() -> usize {
    ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The sessions waiting for one to end, or for a larger cap.
pub fn queued() -> usize {
    QUEUED.load(std::sync::atomic::Ordering::Relaxed)
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.flush_profiles();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A named persona's directory, when the context is one: `korvus.name` / `/persona/<name>` naming a persona made
/// with `korvus persona new` (its `persona.json` there).
fn persona_dir(spec: &ContextSpec) -> Option<PathBuf> {
    let name = spec.name.as_deref().filter(|n| !n.is_empty() && !n.contains(['/', '\\']) && *n != "." && *n != "..")?;
    let dir = DEFAULTS.get()?.personas.as_ref()?.join(name);
    dir.join("persona.json").is_file().then_some(dir)
}

/// A tab's title while its document has none: its URL without the `http://` or `https://` (Chrome 150 titles
/// `http://127.0.0.1:8000/b` `127.0.0.1:8000/b`, measured).
fn untitled(url: &str) -> String {
    url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")).unwrap_or(url).to_string()
}

/// `path`'s persona name and what follows it: `/persona/shop/json/version` -> (`shop`, `/json/version`).
fn split_route(path: &str) -> (Option<String>, String) {
    if let Some(rest) = path.strip_prefix("/persona/") {
        let (name, tail) = rest.split_once('/').map(|(n, t)| (n, format!("/{t}"))).unwrap_or((rest, String::new()));
        let name = name.split(['?', '#']).next().unwrap_or("");
        if !name.is_empty() {
            return (Some(name.to_string()), tail);
        }
    }
    (None, path.to_string())
}

/// Serve the browser endpoint on `listener` until a client sends `Browser.close`.
pub async fn serve_browser(listener: TcpListener, persona: Arc<dyn spec::Persona>, launch: Launcher) -> anyhow::Result<()> {
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    // A plugin's `host->emit_cdp_event` reaches the actor here and is broadcast to the clients (plugin ABI 3, A1).
    plugin_host::set_cdp_event_sink(Arc::new({
        let tx = ev_tx.clone();
        move |method: &str, params: &str| {
            let _ = tx.send(Ev::PluginEvent { method: method.to_string(), params: params.to_string() });
        }
    }));
    // A tenancy plugin's hibernate/resume and KV host calls reach the actor from here (plugin ABI 3, G5/A4).
    plugin_host::set_tenancy(Arc::new(TenancyHandle { ev: ev_tx.clone() }));
    let dir = std::env::temp_dir().join(format!("korvus-browser-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("contexts"))?;
    let store = Arc::new(transport::context_store::Server::start()?);
    let mut browser = Browser {
        persona,
        launch: Arc::new(launch),
        uuid: browser_uuid(),
        default_context: hex32(),
        contexts: Vec::new(),
        targets: Vec::new(),
        clients: HashMap::new(),
        pending: HashMap::new(),
        next_child_id: 1,
        ev: ev_tx.clone(),
        dir,
        store,
        then: Vec::new(),
        metrics: Vec::new(),
        queue: Default::default(),
        starting: HashMap::new(),
        kv: HashMap::new(),
        popup_rids: HashMap::new(),
        tickets: HashMap::new(),
    };
    let _ = ACTOR.set(ev_tx.clone());
    // D88 step 3: the endpoint's own viewer ticket, for its own client -- any route, interactive, for the endpoint's
    // life -- said where the person who started it reads it. Not when a tenancy plugin gates connections: its tenants'
    // own tickets are their servers' to mint.
    if !plugin_host::has_connect_auth() {
        let key = crate::viewer::mint();
        browser.tickets.insert(key.clone(), crate::viewer::Ticket { context: None, interactive: true, expires: None, origins: Vec::new(), tenant: -1 });
        if let Ok(addr) = listener.local_addr() {
            println!("korvus client: http://{addr}/client/#ticket={key}");
        }
    }
    let first = browser.default_context.clone();
    browser.add_context(first.clone(), Kind::Default, None, false, ContextSpec::default())?;
    // Chrome starts with one blank page in its default context, and clients expect to find it
    // (Puppeteer's `browser.pages()`, Playwright's default context).
    let t = spawn_target(browser.launch.clone(), browser.ev.clone(), browser.persona.clone(), first.clone(), browser.spec_path(&first), Vec::new()).await?;
    browser.targets.insert(0, t);
    let accept_tx = ev_tx.clone();
    tokio::spawn(async move {
        let mut next = 1u64;
        while let Ok((stream, _)) = listener.accept().await {
            let (tx, id) = (accept_tx.clone(), next);
            next += 1;
            tokio::spawn(async move {
                if let Err(e) = connection(stream, id, tx).await {
                    eprintln!("korvus browser: connection ended: {e}");
                }
            });
        }
    });
    // A named persona's profile is written as its context ends, and every few seconds while it runs, so a run that
    // is killed loses at most that much of what its pages stored.
    let flush_tx = ev_tx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            if flush_tx.send(Ev::Flush).is_err() {
                return;
            }
        }
    });
    // The page processes' CPU and memory are sampled on a short tick and folded into the per-context metrics (G3).
    let sample_tx = ev_tx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            if sample_tx.send(Ev::Sample).is_err() {
                return;
            }
        }
    });
    while let Some(ev) = ev_rx.recv().await {
        if browser.on_event(ev) {
            return Ok(()); // `Browser.close`
        }
    }
    Ok(())
}

/// One TCP connection: an HTTP discovery request, answered and closed, or a WebSocket, relayed to the actor until
/// either side ends it.
async fn connection(stream: TcpStream, id: u64, ev: mpsc::UnboundedSender<Ev>) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    // G1: the client's address, for a tenancy plugin's connect gate (read before the stream is wrapped).
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
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
    let websocket = get("upgrade").is_some_and(|u| u.to_lowercase().contains("websocket"));
    // D88 step 3: a viewer ticket (`?ticket=`) is its own key -- the actor checks it, and the origins it allows.
    let ticket = path.split_once('?').and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("ticket=").map(str::to_string)));
    // D88 step 1: a site's page reaches this port too; a foreign `Origin` or a rebound `Host` is turned away here.
    let origin = if ticket.is_some() { None } else { get("origin") };
    if let Err(r) = crate::access::check_browser(get("host").as_deref(), origin.as_deref(), websocket) {
        eprintln!("korvus browser: {}", r.log_line());
        reader.get_mut().write_all(r.response().as_bytes()).await?;
        reader.get_mut().flush().await?;
        return Ok(());
    }
    let host = get("host").unwrap_or_else(|| "127.0.0.1".to_string());
    // D88 step 4: korvus's own client, from this port
    if let Some(resp) = (!websocket).then(|| crate::client_files::response(&path)).flatten() {
        reader.get_mut().write_all(resp.as_bytes()).await?;
        reader.get_mut().flush().await?;
        return Ok(());
    }
    if !websocket {
        let (tx, rx) = oneshot::channel();
        ev.send(Ev::Http { path, host, reply: tx }).map_err(|_| anyhow::anyhow!("the browser is gone"))?;
        let body = rx.await.unwrap_or_else(|_| "{}".into());
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        reader.get_mut().write_all(resp.as_bytes()).await?;
        reader.get_mut().flush().await?;
        return Ok(());
    }
    // G1: the client's bearer token -- the Authorization header, else a `?token=` query param, "" when it sent
    // none -- for the tenancy plugin's connect gate (the gate itself runs on the actor, in `open`).
    let auth = get("authorization")
        .and_then(|h| h.strip_prefix("Bearer ").map(str::to_string))
        .or_else(|| path.split_once('?').and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("token=").map(str::to_string))))
        .unwrap_or_default();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (ok_tx, ok_rx) = oneshot::channel();
    let door = Door { host: host.clone(), origin: get("origin"), ticket };
    ev.send(Ev::Open { client: id, path, auth, peer, out: out_tx, ok: ok_tx, door }).map_err(|_| anyhow::anyhow!("the browser is gone"))?;
    if !ok_rx.await.unwrap_or(false) {
        return Ok(());
    }
    let accept = derive_accept_key(get("sec-websocket-key").unwrap_or_default().as_bytes());
    let resp = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
    reader.get_mut().write_all(resp.as_bytes()).await?;
    reader.get_mut().flush().await?;
    let ws = WebSocketStream::from_raw_socket(reader.into_inner(), Role::Server, None).await;
    let (mut write, mut read) = ws.split();
    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if write.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
        let _ = write.close().await;
    });
    while let Some(m) = read.next().await {
        match m {
            Ok(Message::Text(t)) => {
                if ev.send(Ev::Text { client: id, text: t }).is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let _ = ev.send(Ev::Gone { client: id });
    writer.abort();
    Ok(())
}

/// Start a page target's process in `context` and open its page socket.
async fn spawn_target(launch: Arc<Launcher>, ev: mpsc::UnboundedSender<Ev>, persona: Arc<dyn spec::Persona>, context: String, spec_path: PathBuf, extra: Vec<String>) -> anyhow::Result<Target> {
    let id = hex32();
    let mut child = tokio::process::Command::new(&launch.exe)
        .args(&launch.args)
        .arg("0")
        .arg(CHILD_FLAG)
        .arg(&id)
        .arg(&context)
        .args(&launch.tail)
        .args(&extra)
        .arg(CONTEXT_FLAG)
        .arg(&spec_path)
        // a page process loads the endpoint's plugins again: the endpoint already said which, once
        .env("KORVUS_PAGE_PROCESS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("the target process has no stdout"))?;
    // A4: the router writes KORVUS-KV-REPLY to the child's stdin (chrome.storage.local get replies). A no-KV child
    // is never written to, so its lifeline sees only EOF -- unchanged. (stdin was already piped at spawn.)
    let child_stdin = child.stdin.take();
    let mut lines = BufReader::new(stdout).lines();
    let port: u16 = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while let Some(line) = lines.next_line().await? {
            if let Some(p) = line.strip_prefix(PORT_LINE) {
                return Ok::<u16, anyhow::Error>(p.trim().parse()?);
            }
        }
        Err(anyhow::anyhow!("the target process ended before it listened"))
    })
    .await
    .map_err(|_| anyhow::anyhow!("the target process did not listen within 15 s"))??;
    // The child's stdin: one writer, fed by the router (the KV replies, the popup lines). A child nobody writes to sees
    // only the lifeline's EOF, as before.
    let (to_stdin, mut stdin_lines) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let Some(mut w) = child_stdin else { return };
        while let Some(line) = stdin_lines.recv().await {
            if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
                return;
            }
        }
    });
    // The child's later stdout lines: a metric report reaches the router here (A6/A3), and the KV and popup lines;
    // anything else is dropped. A closed pipe ends the task.
    let line_ev = ev.clone();
    let line_tid = id.clone();
    let stdin = to_stdin.clone();
    tokio::spawn(async move {
        const POPUP_LINES: [&str; 9] = [
            POPUP_OPEN_LINE,
            POPUP_POST_LINE,
            POPUP_NAV_LINE,
            POPUP_CLOSE_LINE,
            OPENER_POST_LINE,
            WINDOW_CLOSE_LINE,
            DIALOG_OPEN_LINE,
            POPUP_HOSTED_CLOSE_LINE,
            POPUP_HOSTED_LINE,
        ];
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(&prefix) = POPUP_LINES.iter().find(|p| line.starts_with(**p)) {
                let json = line[prefix.len()..].to_string();
                if line_ev.send(Ev::Popup { target: line_tid.clone(), line: prefix, json }).is_err() {
                    return;
                }
            } else if let Some(j) = line.strip_prefix(METRIC_LINE) {
                if line_ev.send(Ev::Metric { target: line_tid.clone(), json: j.to_string() }).is_err() {
                    return;
                }
            } else if let Some(j) = line.strip_prefix(DOWNLOAD_LINE) {
                if line_ev.send(Ev::Download { target: line_tid.clone(), json: j.to_string() }).is_err() {
                    return;
                }
            } else if let Some(j) = line.strip_prefix(KV_SET_LINE) {
                // chrome.storage.local write (A4): forward {ctx,key,value} to the KV actor, fire-and-forget.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
                    let _ = line_ev.send(Ev::KvSet {
                        context: v.get("ctx").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        key: v.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        value: v.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    });
                }
            } else if let Some(j) = line.strip_prefix(KV_GET_LINE) {
                // chrome.storage.local read (A4): ask the KV actor and reply on the child's stdin. AWAIT the oneshot
                // (NOT blocking_recv -- the endpoint is a current_thread runtime, so blocking the reader task would
                // deadlock the actor that must answer); the child's page thread is parked on this reply.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
                    let qid = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                    let (reply, answer) = oneshot::channel();
                    let sent = line_ev.send(Ev::KvGet {
                        context: v.get("ctx").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        key: v.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        reply,
                    });
                    if sent.is_ok() {
                        let val = answer.await.ok().flatten();
                        let _ = stdin.send(format!("{}{}\n", KV_REPLY_LINE, serde_json::json!({ "id": qid, "value": val })));
                    }
                }
            }
        }
    });
    let stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let _ = stream.set_nodelay(true);
    let (ws, _) = tokio_tungstenite::client_async(format!("ws://127.0.0.1:{port}/devtools/page/{id}"), stream).await?;
    let (mut to_child, mut from_child) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if to_child.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });
    let tid = id.clone();
    tokio::spawn(async move {
        while let Some(m) = from_child.next().await {
            match m {
                Ok(Message::Text(t)) => {
                    if ev.send(Ev::Child { target: tid.clone(), ev: FromChild::Text(t) }).is_err() {
                        return;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        let _ = ev.send(Ev::Child { target: tid, ev: FromChild::Closed });
    });
    let screen = persona.screen();
    let bounds = json!({
        "left": screen.avail_left,
        "top": screen.avail_top,
        "width": screen.window_outer_width,
        "height": screen.window_outer_height,
        "windowState": "normal",
    });
    // Chrome's window ids are large positive integers (599027229 recorded).
    let mut entropy = [0u8; 4];
    getrandom::getrandom(&mut entropy).expect("OS entropy for a window id");
    let window_id = u32::from_be_bytes(entropy) % 0x7fff_0000 + 0x1000;
    Ok(Target {
        id,
        context,
        url: "about:blank".into(),
        title: String::new(),
        window_id,
        bounds,
        to_child: tx,
        to_stdin,
        opener: None,
        counted: false,
        _child: Some(child),
        host: None,
        screencast: None,
        dialog: None,
        pending_nav: None,
    })
}

type Answer = (Result<Value, RpcError>, Vec<Value>, Vec<Value>, bool);

/// A permission as the Permissions API names it, from CDP's `PermissionType` (`geolocation`, `notifications`, ...;
/// `videoCapture` is `camera`, `audioCapture` `microphone`, `clipboardReadWrite` `clipboard-read`).
fn permission_name(p: &str) -> String {
    match p {
        "videoCapture" => "camera".into(),
        "audioCapture" => "microphone".into(),
        "clipboardReadWrite" => "clipboard-read".into(),
        "clipboardSanitizedWrite" => "clipboard-write".into(),
        "midiSysex" | "midi" => "midi".into(),
        "durableStorage" => "persistent-storage".into(),
        "backgroundSync" => "background-sync".into(),
        "displayCapture" => "display-capture".into(),
        "localFonts" => "local-fonts".into(),
        "windowManagement" => "window-management".into(),
        "idleDetection" => "idle-detection".into(),
        "storageAccess" => "storage-access".into(),
        "paymentHandler" => "payment-handler".into(),
        "periodicBackgroundSync" => "periodic-background-sync".into(),
        other => other.to_string(),
    }
}

fn fail(message: &str) -> Answer {
    (Err(RpcError::server(message.into())), Vec::new(), Vec::new(), false)
}

/// The router's side of the tenancy host calls (plugin ABI 3, G5/A4). A tenancy plugin reaches these from the
/// endpoint process; each hops to the actor as an [`Ev`], because the actor owns the page processes (to signal) and
/// the KV store. Installed once with `plugin_host::set_tenancy` as the endpoint starts serving.
struct TenancyHandle {
    ev: mpsc::UnboundedSender<Ev>,
}

impl plugin_host::Tenancy for TenancyHandle {
    fn hibernate(&self, ctx: &str) {
        let _ = self.ev.send(Ev::Hibernate(ctx.to_string()));
    }
    fn resume(&self, ctx: &str) {
        let _ = self.ev.send(Ev::Resume(ctx.to_string()));
    }
    fn kv_get(&self, ctx: &str, key: &str) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        if self.ev.send(Ev::KvGet { context: ctx.to_string(), key: key.to_string(), reply: tx }).is_err() {
            return None;
        }
        // TODO(1021-verify): this blocks the calling thread for the actor's reply. It is safe from a tenancy
        // plugin's own thread (the intended caller), but a plugin that calls kv_get synchronously from on_connect /
        // context_allow / queue_order / on_cdp_call runs ON the actor and would deadlock -- confirm tenancy plugins
        // touch the KV only off-actor, or move the store behind a shared lock reached without the actor.
        rx.blocking_recv().ok().flatten()
    }
    fn kv_set(&self, ctx: &str, key: &str, val: &str) {
        let _ = self.ev.send(Ev::KvSet { context: ctx.to_string(), key: key.to_string(), value: val.to_string() });
    }
}

/// G3: a page process's cumulative CPU (utime + stime, in clock ticks) and current resident memory (VmRSS, in
/// bytes), read from `/proc`. Linux only; `None` when the process is gone or `/proc` cannot be read.
#[cfg(target_os = "linux")]
fn proc_sample(pid: u32) -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (field 2) is parenthesized and may hold spaces, so the numeric fields start after the last ')': [0] is
    // state (field 3), so utime (field 14) is [11] and stime (field 15) is [12].
    let after = stat.rsplit_once(')').map(|(_, r)| r)?;
    let f: Vec<&str> = after.split_whitespace().collect();
    let utime: u64 = f.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
    let stime: u64 = f.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:").and_then(|v| v.split_whitespace().next()).and_then(|n| n.parse().ok()))
        .unwrap_or(0);
    Some((utime + stime, rss_kb * 1024))
}
#[cfg(not(target_os = "linux"))]
fn proc_sample(_pid: u32) -> Option<(u64, u64)> {
    // TODO(1021-verify): CPU/RSS sampling reads /proc, so it is Linux-only; other OSes omit these gauges.
    None
}

/// G6: was a page process OOM-killed? The Linux OOM killer sends SIGKILL, so a child that died on SIGKILL is
/// reported as `page.oom` rather than a plain crash. An unknown status (not reaped yet, or a non-Unix host) reads as
/// a crash.
fn oom_killed(status: Option<std::process::ExitStatus>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        return status.and_then(|s| s.signal()) == Some(9);
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

/// A child's exit status as JSON (`{code, signal}`) for the G6 event: its exit code when it exited on its own, the
/// signal that killed it (Unix), or nulls when it had not been reaped when the crash was seen.
fn status_json(status: Option<std::process::ExitStatus>) -> Value {
    match status {
        #[cfg(unix)]
        Some(s) => {
            use std::os::unix::process::ExitStatusExt;
            json!({ "code": s.code(), "signal": s.signal() })
        }
        #[cfg(not(unix))]
        Some(s) => json!({ "code": s.code() }),
        None => json!({ "code": Value::Null, "signal": Value::Null }),
    }
}

// POSIX `kill(2)`, bound directly so hibernate/resume (G5) add no dependency.
#[cfg(target_os = "linux")]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

impl Browser {
    fn spec_path(&self, context: &str) -> PathBuf {
        self.dir.join("contexts").join(format!("{context}.json"))
    }

    /// A context begins: its spec is written for its page processes and its slots open in the store.
    fn add_context(&mut self, id: String, kind: Kind, owner: Option<u64>, dispose_on_detach: bool, mut spec: ContextSpec) -> anyhow::Result<()> {
        spec.context = id.clone();
        spec.store = Some(self.store.endpoint(&id));
        // The endpoint's own plugins load in every page process, ahead of the context's.
        let mut plugins: Vec<String> = plugin_host::loaded().iter().map(|p| format!("{}:{}", p.path.display(), p.config)).collect();
        plugins.append(&mut spec.plugins);
        spec.plugins = plugins;
        // the context's own settings first, then a named persona's (its document: its device and its session), then
        // the endpoint's
        let persona = persona_dir(&spec);
        // A persona whose document says `"persistent": false`, a context asked for `korvus.fresh`, or an endpoint run
        // `--fresh`: no profile is read or written for it, so every run of it starts as the first did.
        let mut keeps_profile = !spec.fresh.unwrap_or(false) && !DEFAULTS.get().is_some_and(|d| d.fresh);
        if let Some(dir) = &persona {
            let doc = dir.join("persona.json");
            let session = std::fs::read_to_string(&doc).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|d| d["session"].clone());
            if session.as_ref().and_then(|s| s.get("persistent")).and_then(Value::as_bool) == Some(false) {
                keeps_profile = false;
            }
            let said = |k: &str| session.as_ref().and_then(|s| s.get(k)).and_then(Value::as_str).filter(|v| !v.is_empty()).map(String::from);
            spec.device = spec.device.take().or_else(|| Some(doc.display().to_string()));
            spec.timezone = spec.timezone.take().or_else(|| said("timezone"));
            spec.locale = spec.locale.take().or_else(|| said("locale"));
            if spec.proxy.is_none() {
                spec.proxy = said("proxy").map(|server| ProxySpec { server, bypass: None });
            }
        }
        if let Some(d) = DEFAULTS.get() {
            spec.seed = spec.seed.take().or_else(|| d.seed.clone());
            spec.device = spec.device.take().or_else(|| d.device.clone());
            spec.timezone = spec.timezone.take().or_else(|| d.timezone.clone());
            spec.locale = spec.locale.take().or_else(|| d.locale.clone());
            spec.humanize = spec.humanize.or(d.humanize);
            if spec.proxy.is_none() {
                spec.proxy = d.proxy.clone().map(|server| ProxySpec { server, bypass: None });
            }
        }
        // D2 (goal 1021 step 10, 4.2): a plugin may CHOOSE this context's proxy as it is made. When one answers,
        // that proxy becomes the context's own -- written to the spec, so every page process of the context routes
        // ALL of the context's traffic through it (the transport AND, with it, WebRTC/ICE: a page process's
        // `apply_context` sets the proxy environment, which both the transport's clients and `ice::proxied` read),
        // not per-request. This is the context-level proxy that closes the 1020 "4.5 per-request-proxy leak": a
        // per-request `req_set_proxy` never reaches ICE, but a context's proxy does. The context's own configured
        // bypass list carries over to the chosen proxy. Off entirely with no choose_proxy plugin (the slot is then
        // never asked and the context keeps its configured proxy -- invariants I2/J2). Chosen before `drawn` below,
        // so the identity is drawn against the proxy the context actually exits through.
        // TODO(1021-verify): two judgment calls here -- (1) choose_proxy runs BEFORE `drawn`, so the exit-IP lookup
        // and persona are drawn against the CHOSEN proxy (the proxy the context uses), not its configured one;
        // (2) the chosen proxy inherits the configured proxy's bypass list. Confirm the owner wants both.
        if plugin_host::has_choose_proxy() {
            let info = context_info(&spec, String::new());
            if let Some(server) = plugin_host::choose_proxy(&info) {
                let bypass = spec.proxy.as_ref().and_then(|p| p.bypass.clone());
                spec.proxy = Some(ProxySpec { server, bypass });
            }
        }
        spec.drawn = drawn(&spec);
        self.store.open(&id);
        let profile = persona.filter(|_| keeps_profile).map(|d| d.join("profile").join("store.bin"));
        if let Some(p) = &profile {
            if let Err(e) = self.store.load(&id, p) {
                eprintln!("[korvus] persona {}: its profile {}: {e}", spec.name.as_deref().unwrap_or(""), p.display());
            }
        }
        // A storage plugin's kept state for this context (goal 1021 step 5, E1): its blob seeds the store before the
        // context's first page, so a reused login (cookies, storage) is already there. Off entirely with no such plugin.
        if plugin_host::has_storage() {
            let info = context_info(&spec, String::new());
            if let Some(blob) = plugin_host::storage_load(&info) {
                self.store.import(&id, &blob);
            }
        }
        let spec_path = self.spec_path(&id);
        std::fs::write(&spec_path, serde_json::to_string_pretty(&spec)?)?;
        plugin_host::event("context.created", &json!({ "context": id, "name": spec.name }).to_string());
        self.contexts.push(Ctx { id, kind, owner, dispose_on_detach, spec, spec_path, profile });
        Ok(())
    }

    /// A named persona's profile, written from its context's store (what its pages stored so far).
    fn save_profile(&self, c: &Ctx) {
        if let Some(p) = &c.profile {
            if let Err(e) = self.store.save(&c.id, p) {
                eprintln!("[korvus] persona {}: its profile {}: {e}", c.spec.name.as_deref().unwrap_or(""), p.display());
            }
        }
    }

    /// Hand a context's state to the storage plugins (goal 1021 step 5, E1): its slots -- cookies and storage --
    /// serialized to a blob and given to every `storage_save` plugin, so a login can be reused next run. Off entirely
    /// when no plugin takes storage, and silent when the context has stored nothing.
    fn save_storage(&self, c: &Ctx) {
        if !plugin_host::has_storage() {
            return;
        }
        if let Some(blob) = self.store.export(&c.id) {
            let info = context_info(&c.spec, String::new());
            plugin_host::storage_save(&info, &blob);
        }
    }

    /// Every named persona's profile, written now: on a timer, and as the endpoint ends. The storage plugins are
    /// handed each context's state on the same beats.
    fn flush_profiles(&self) {
        for c in &self.contexts {
            self.save_profile(c);
            self.save_storage(c);
        }
    }

    /// A context ends: its pages close, its state goes (cookies, storage) and its spec with it.
    fn remove_context(&mut self, id: &str) {
        let doomed: Vec<String> = self.targets.iter().filter(|t| t.context == id).map(|t| t.id.clone()).collect();
        for tid in doomed {
            self.close_target(&tid);
        }
        if let Some(i) = self.contexts.iter().position(|c| c.id == id) {
            let c = self.contexts.remove(i);
            let _ = std::fs::remove_file(&c.spec_path);
            self.save_profile(&c);
            self.save_storage(&c);
        }
        self.store.close(id);
        self.kv.remove(id);
        plugin_host::event("context.closed", &json!({ "context": id }).to_string());
        self.drain();
    }

    fn ctx(&self, id: &str) -> Option<&Ctx> {
        self.contexts.iter().find(|c| c.id == id)
    }

    /// The context a browser-level command names (`browserContextId`), else the connection's home -- when the
    /// connection may see it.
    fn context_param(&self, client: u64, params: &Value) -> Option<String> {
        match params.get("browserContextId").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(id) => self.sees(client, id).then(|| id.to_string()),
            None => self.clients.get(&client).map(|c| c.home.clone()),
        }
    }

    /// A page's download began or ended: Chrome's `Browser.downloadWillBegin` / `Browser.downloadProgress`, to every
    /// connection that sees the page's context.
    fn on_download(&mut self, target: &str, json: &str) {
        let Ok(v) = serde_json::from_str::<Value>(json) else { return };
        let Some(context) = self.targets.iter().find(|t| t.id == target).map(|t| t.context.clone()) else { return };
        let ev = if v.get("state").is_some() {
            json!({ "method": "Browser.downloadProgress", "params": v })
        } else {
            json!({ "method": "Browser.downloadWillBegin", "params": v })
        };
        let ids: Vec<u64> = self.clients.keys().copied().collect();
        for cid in ids {
            if self.sees(cid, &context) {
                if let Some(c) = self.clients.get(&cid) {
                    c.send(ev.clone());
                }
            }
        }
    }

    /// How a context downloads (`Browser.setDownloadBehavior`): kept in its spec, for its later pages, and handed to
    /// each of its open pages (`Korvus.setDownloadBehavior`).
    fn set_context_downloads(&mut self, ctx: &str, behavior: Option<(String, String)>) {
        let Some(c) = self.contexts.iter_mut().find(|c| c.id == ctx) else { return };
        c.spec.downloads = behavior.clone();
        if let Ok(text) = serde_json::to_string_pretty(&c.spec) {
            let _ = std::fs::write(&c.spec_path, text);
        }
        let (b, p) = behavior.unwrap_or_else(|| ("default".into(), String::new()));
        let mut cid = self.next_child_id;
        for t in self.targets.iter().filter(|t| t.context == ctx && t.host.is_none()) {
            let cmd = json!({ "id": cid, "method": "Korvus.setDownloadBehavior", "params": { "behavior": b, "downloadPath": p } });
            let _ = t.to_child.send(cmd.to_string());
            cid += 1;
        }
        self.next_child_id = cid;
    }

    /// A context's granted permissions: kept in its spec (and its spec file, which a page process of it reads as it
    /// starts) and handed to each of its open pages (`Korvus.setPermissions`).
    fn set_context_permissions(&mut self, ctx: &str, permissions: Option<Vec<String>>) {
        let Some(c) = self.contexts.iter_mut().find(|c| c.id == ctx) else { return };
        c.spec.permissions = permissions.clone();
        if let Ok(text) = serde_json::to_string_pretty(&c.spec) {
            let _ = std::fs::write(&c.spec_path, text);
        }
        let mut cid = self.next_child_id;
        for t in self.targets.iter().filter(|t| t.context == ctx && t.host.is_none()) {
            let cmd = json!({ "id": cid, "method": "Korvus.setPermissions", "params": { "permissions": permissions } });
            let _ = t.to_child.send(cmd.to_string());
            cid += 1;
        }
        self.next_child_id = cid;
    }

    /// `/persona/<name>`'s context, made on first use.
    fn persona_context(&mut self, name: &str) -> Option<String> {
        if let Some(c) = self.contexts.iter().find(|c| c.kind == Kind::Persona && c.spec.name.as_deref() == Some(name)) {
            return Some(c.id.clone());
        }
        let id = hex32();
        let spec = ContextSpec { name: Some(name.to_string()), ..ContextSpec::default() };
        match self.add_context(id.clone(), Kind::Persona, None, false, spec) {
            Ok(()) => Some(id),
            Err(e) => {
                eprintln!("korvus browser: cannot make persona {name}'s context: {e}");
                None
            }
        }
    }

    /// May `client` see context `ctx`: its home, or one it made.
    fn sees(&self, client: u64, ctx: &str) -> bool {
        let Some(c) = self.clients.get(&client) else { return false };
        c.home == ctx || self.ctx(ctx).is_some_and(|x| x.owner == Some(client))
    }

    fn sees_target(&self, client: u64, tid: &str) -> bool {
        self.targets.iter().find(|t| t.id == tid).is_some_and(|t| self.sees(client, &t.context))
    }

    /// The tenant a connection belongs to (plugin ABI 3): its `on_connect` id, or -1 when it was not gated.
    fn tenant_of(&self, client: u64) -> i64 {
        self.clients.get(&client).map(|c| c.tenant).unwrap_or(-1)
    }

    fn target(&mut self, id: &str) -> Option<&mut Target> {
        self.targets.iter_mut().find(|t| t.id == id)
    }

    fn attached(client: &Client, target: &str) -> bool {
        client.page_socket.as_deref() == Some(target) || client.sessions.values().any(|s| matches!(s, Session::Page(t) if t == target))
    }

    /// The active sessions: every context with at least one open page (D8), and how many pages each has.
    fn sessions(&self) -> Value {
        let rows: Vec<Value> = self
            .contexts
            .iter()
            .map(|c| {
                let targets: Vec<&str> = self.targets.iter().filter(|t| t.context == c.id).map(|t| t.id.as_str()).collect();
                json!({
                    "context": c.id,
                    "name": c.spec.name,
                    "kind": match c.kind { Kind::Default => "default", Kind::Persona => "persona", Kind::Created => "created" },
                    "pages": targets.len(),
                    "targets": targets,
                })
            })
            .collect();
        // sessions as the license counts them: contexts a client made a page in; and those waiting for a place
        json!({ "active": self.sessions_now(), "queued": self.queue.len(), "contexts": rows })
    }

    /// Handle one event; `true` when the browser should close.
    fn on_event(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::Http { path, host, reply } => {
                let _ = reply.send(self.http(&path, &host));
                false
            }
            Ev::Open { client, path, auth, peer, out, ok, door } => {
                let _ = ok.send(self.open(client, &path, &auth, &peer, out, door));
                false
            }
            Ev::Text { client, text } => self.on_client(client, &text),
            Ev::Gone { client } => {
                self.gone(client);
                false
            }
            Ev::Child { target, ev } => {
                self.on_child(&target, ev);
                false
            }
            Ev::Spawned { job, result } => {
                self.spawned(job, result);
                false
            }
            Ev::Metric { target, json } => {
                self.record_metrics(&target, &json);
                false
            }
            Ev::Download { target, json } => {
                self.on_download(&target, &json);
                false
            }
            Ev::Popup { target, line, json } => {
                if line == DIALOG_OPEN_LINE {
                    self.on_dialog(&target, &json);
                } else {
                    self.on_popup_line(&target, line, &json);
                }
                false
            }
            Ev::PluginEvent { method, params } => {
                let msg = if params.is_empty() {
                    json!({ "method": method })
                } else {
                    json!({ "method": method, "params": serde_json::from_str::<Value>(&params).unwrap_or(Value::Null) })
                };
                for c in self.clients.values().filter(|c| c.viewer.is_none()) {
                    c.send(msg.clone());
                }
                false
            }
            Ev::Flush => {
                self.flush_profiles();
                false
            }
            Ev::Cap => {
                self.drain();
                false
            }
            Ev::Sample => {
                self.sample_metrics();
                self.sweep_tickets();
                false
            }
            Ev::Hibernate(ctx) => {
                self.hibernate(&ctx);
                false
            }
            Ev::Resume(ctx) => {
                self.resume(&ctx);
                false
            }
            Ev::KvGet { context, key, reply } => {
                let _ = reply.send(self.kv.get(&context).and_then(|m| m.get(&key).cloned()));
                false
            }
            Ev::KvSet { context, key, value } => {
                self.kv.entry(context).or_default().insert(key, value);
                false
            }
        }
    }

    fn http(&mut self, path: &str, host: &str) -> String {
        let (persona, rest) = split_route(path);
        let (prefix, home) = match &persona {
            Some(n) => match self.persona_context(n) {
                Some(id) => (format!("/persona/{n}"), Some(id)),
                None => return "{}".into(),
            },
            None => (String::new(), None),
        };
        if rest.starts_with("/json/version") {
            VersionInfo::of(&*self.persona).json_version(format!("ws://{host}{prefix}/devtools/browser/{}", self.uuid)).to_string()
        } else if rest.starts_with("/json") {
            Value::Array(
                self.targets
                    .iter()
                    .filter(|t| home.as_ref().is_none_or(|h| *h == t.context))
                    .map(|t| {
                        json!({
                            "id": t.id,
                            "type": "page",
                            "title": t.title,
                            "url": t.url,
                            "webSocketDebuggerUrl": format!("ws://{host}{prefix}/devtools/page/{}", t.id),
                        })
                    })
                    .collect(),
            )
            .to_string()
        } else if rest.starts_with("/korvus/sessions") {
            self.sessions().to_string()
        } else if rest.starts_with("/korvus/metrics") {
            self.metrics_json().to_string()
        } else if rest.starts_with("/korvus/har") {
            // The request ledger as HAR 1.2 (`har_json`). In the browser endpoint the router runs no page of
            // its own -- every page is a child process with its OWN process-wide ledger (`transport::ledger`,
            // module docs) and serves its requests at this same path on its own socket (`handle_conn`,
            // lib.rs). This serves the ROUTER process's ledger, which the router does not populate (it fetches
            // no pages), so it is a valid but empty log on the endpoint.
            // GAP: a browser-endpoint-wide HAR (every context's pages in one log) needs the page processes to
            // report their ledger rows up to the router -- a `KORVUS-LEDGER` stdout line beside `KORVUS-METRIC`
            // (`METRIC_LINE`), emitted from `message_loop` and folded in here per target. That emission is in
            // the page's message loop, outside this http route, so the orchestrator wires it after the build.
            har_json(&transport::ledger().records(), transport::ledger().dropped()).to_string()
        } else if rest.starts_with("/korvus/plugin") {
            // A2: a plugin HTTP surface. The ABI added no plugin HTTP slot, so this is korvus's own minimal view --
            // the tenancy state and the metric totals a tenancy plugin's dashboard would read (GET /korvus/plugin/...).
            let metrics = self.metrics_json();
            json!({
                "tenancy": plugin_host::has_connect_auth(),
                "active": self.sessions_now(),
                "queued": self.queue.len(),
                "metrics": metrics["metrics"].clone(),
            })
            .to_string()
        } else {
            "{}".to_string()
        }
    }

    /// Add a page process's reported metrics (a JSON array of `{name, value, labels}`) to this context's totals.
    fn record_metrics(&mut self, target: &str, json: &str) {
        let Some(ctx) = self.targets.iter().find(|t| t.id == target).map(|t| t.context.clone()) else { return };
        let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(json) else { return };
        for m in arr {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let labels = m.get("labels").and_then(Value::as_str).unwrap_or("").to_string();
            let value = m.get("value").and_then(Value::as_f64).unwrap_or(0.0);
            self.add_metric(ctx.clone(), name.to_string(), labels, value);
        }
    }

    /// Sum `value` into the (context, name, labels) total.
    fn add_metric(&mut self, context: String, name: String, labels: String, value: f64) {
        match self.metrics.iter_mut().find(|r| r.context == context && r.name == name && r.labels == labels) {
            Some(r) => r.value += value,
            None => self.metrics.push(MetricRow { context, name, labels, value }),
        }
    }

    /// Set the (context, name, labels) value to `value` -- a gauge, for a reading that replaces rather than adds to
    /// the running total (the CPU and memory samples of G3).
    fn set_metric(&mut self, context: String, name: String, labels: String, value: f64) {
        match self.metrics.iter_mut().find(|r| r.context == context && r.name == name && r.labels == labels) {
            Some(r) => r.value = value,
            None => self.metrics.push(MetricRow { context, name, labels, value }),
        }
    }

    /// `GET /korvus/metrics`: every context's metric totals, plus the router process's own slots' metrics (context
    /// `""`), drained in on the way out.
    fn metrics_json(&mut self) -> Value {
        for m in plugin_host::drain_metrics() {
            self.add_metric(String::new(), m.name, m.labels, m.value);
        }
        let rows: Vec<Value> = self
            .metrics
            .iter()
            .map(|r| json!({ "context": r.context, "name": r.name, "labels": r.labels, "value": r.value }))
            .collect();
        json!({ "metrics": rows })
    }

    /// G3 (A6): sample each page process's CPU and memory, summed per context, and set them as that context's
    /// resource gauges, so they show at `GET /korvus/metrics` beside the counters the pages report. Only the router
    /// holds the child pids, so only it can read these; the per-context request and byte counts arrive on the
    /// pages' own metric reports (`record_metrics`). Linux reads `/proc`; other OSes leave the gauges absent.
    fn sample_metrics(&mut self) {
        let mut per_ctx: HashMap<String, (u64, u64)> = HashMap::new();
        for t in &self.targets {
            let Some(pid) = t._child.as_ref().and_then(|c| c.id()) else { continue };
            let Some((cpu, rss)) = proc_sample(pid) else { continue };
            let e = per_ctx.entry(t.context.clone()).or_default();
            e.0 += cpu;
            e.1 += rss;
        }
        for (context, (cpu, rss)) in per_ctx {
            self.set_metric(context.clone(), "cpu_ticks".into(), String::new(), cpu as f64);
            self.set_metric(context, "rss_bytes".into(), String::new(), rss as f64);
        }
    }

    fn open(&mut self, client: u64, path: &str, auth: &str, peer: &str, out: mpsc::UnboundedSender<String>, door: Door) -> bool {
        // D88 step 3: a viewer ticket admits its connection to the ticket's context (or, the endpoint's own, to the
        // route), as the tenant that minted it -- or to nothing when it is unknown, spent, or not for this origin.
        let ticket = match &door.ticket {
            Some(t) => match self.tickets.get(t).filter(|g| !g.expired() && g.admits(door.origin.as_deref())) {
                Some(g) => Some((t.clone(), g.clone())),
                None => return false,
            },
            None => None,
        };
        // G1: a tenancy plugin gates the connection. With none loaded every client is admitted as before, tenant
        // -1. A refusal closes the connection here -- before a home is resolved or any session or target is made.
        let tenant = if let Some((_, g)) = &ticket {
            g.tenant
        } else if plugin_host::has_connect_auth() {
            match plugin_host::on_connect(auth, peer) {
                Some(t) => t,
                None => return false,
            }
        } else {
            -1
        };
        let (persona, rest) = split_route(path);
        let rest = rest.split(['?', '#']).next().unwrap_or("").to_string();
        let home = match (ticket.as_ref().and_then(|(_, g)| g.context.clone()), &persona) {
            (Some(ctx), _) if self.ctx(&ctx).is_some() => ctx,
            (Some(_), _) => return false,
            (None, Some(n)) => match self.persona_context(n) {
                Some(id) => id,
                None => return false,
            },
            (None, None) => self.default_context.clone(),
        };
        // A page socket opens any page at the root, as Chrome's `/devtools/page/<id>` does, and only the persona's own
        // pages under `/persona/<name>` -- a viewer's, only its context's.
        let page_socket = match rest.strip_prefix("/devtools/page/") {
            Some(id) => {
                let Some(t) = self.targets.iter().find(|t| t.id == id) else { return false };
                if (persona.is_some() || ticket.is_some()) && t.context != home {
                    return false;
                }
                Some(id.to_string())
            }
            None => None,
        };
        let viewer = ticket.map(|(ticket, g)| Grant { ticket, interactive: g.interactive });
        self.clients.insert(
            client,
            Client {
                out,
                home,
                discover: false,
                auto_attach: None,
                sessions: HashMap::new(),
                page_socket,
                tenant,
                page_on: Default::default(),
                host: door.host,
                viewer,
            },
        );
        true
    }

    /// A connection ended: its sessions go, and every context it made to go with it (`disposeOnDetach`).
    /// The screencasts `client` started (on session `sess`, or on any), stopped: their session went, and Chrome's
    /// screencast ends with its session.
    fn end_screencast(&mut self, client: u64, sess: Option<&str>) {
        let cid = self.next_child_id;
        let mut sent = false;
        for t in self.targets.iter_mut() {
            if t.screencast.as_ref().is_some_and(|(c, s)| *c == client && (sess.is_none() || s.as_deref() == sess)) {
                t.screencast = None;
                // The router's own command: its answer (an id no client holds) is dropped when it comes back.
                let _ = t.to_child.send(json!({ "id": cid, "method": "Page.stopScreencast" }).to_string());
                sent = true;
            }
        }
        if sent {
            self.next_child_id += 1;
        }
    }

    fn gone(&mut self, client: u64) {
        self.end_screencast(client, None);
        self.clients.remove(&client);
        self.pending.retain(|_, (c, _, _)| *c != client);
        self.queue.retain(|j| j.client != client);
        let doomed: Vec<String> = self.contexts.iter().filter(|c| c.owner == Some(client) && c.dispose_on_detach).map(|c| c.id.clone()).collect();
        for id in doomed {
            self.remove_context(&id);
        }
    }

    fn on_client(&mut self, client_id: u64, text: &str) -> bool {
        let req: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let id = req.get("id").cloned().unwrap_or(json!(0));
        let Some(client) = self.clients.get(&client_id) else { return false };
        let Some(method) = req.get("method").and_then(Value::as_str).map(String::from) else {
            client.send(json!({ "id": id, "error": RpcError::no_method().to_json() }));
            return false;
        };
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let session = req.get("sessionId").and_then(Value::as_str).map(String::from);
        // D88 step 3: a viewer connection does what a person at a browser does, and nothing more.
        if let Some(g) = client.viewer.clone() {
            let on_page = session.is_some() || client.page_socket.is_some();
            let url_ok = match method.as_str() {
                "Page.navigate" | "Target.createTarget" => crate::viewer::opens(params.get("url").and_then(Value::as_str).unwrap_or("")),
                _ => true,
            };
            let mine = match method.as_str() {
                // only the viewer's own context's tabs, whatever it names
                "Target.closeTarget" => params.get("targetId").and_then(Value::as_str).is_some_and(|t| self.sees_target(client_id, t)),
                _ => true,
            };
            if !crate::viewer::allows(&method, on_page, g.interactive) || !url_ok || !mine {
                let mut err = json!({ "id": id, "error": crate::viewer::refusal(&method) });
                if let Some(s) = &session {
                    err["sessionId"] = json!(s);
                }
                client.send(err);
                return false;
            }
        } else if session.is_none() && client.page_socket.is_none() && (method == "Korvus.createViewer" || method == "Korvus.revokeViewer") {
            let r = if method == "Korvus.createViewer" { self.create_viewer(client_id, &params) } else { self.revoke_viewer(&params) };
            if let Some(c) = self.clients.get(&client_id) {
                c.send(match r {
                    Ok(v) => json!({ "id": id, "result": v }),
                    Err(e) => json!({ "id": id, "error": e.to_json() }),
                });
            }
            return false;
        }
        let Some(client) = self.clients.get(&client_id) else { return false };
        // A session's command, or a page socket's: relayed to the page's process.
        let route = match (&session, &client.page_socket) {
            (Some(s), _) => match client.sessions.get(s) {
                Some(r) => Some((r.clone(), Some(s.clone()))),
                None => {
                    let err = json!({ "code": -32001, "message": "Session with given id not found." });
                    client.send(json!({ "id": id, "error": err, "sessionId": s }));
                    return false;
                }
            },
            (None, Some(page)) => Some((Session::Page(page.clone()), None)),
            (None, None) => None,
        };
        // A browser session's command is the browser's, answered on that session.
        let browser_session = match &route {
            Some((Session::Browser, Some(s))) => Some(s.clone()),
            _ => None,
        };
        let route = if browser_session.is_some() { None } else { route };
        if let Some((to, sess)) = route {
            // `Korvus.*` is the router's own channel to a page's process (`Korvus.setWindowBounds`); a client's is not
            // relayed, and is refused as any domain the engine does not have.
            if method.starts_with("Korvus.") {
                let mut err = json!({ "id": id, "error": RpcError::method_not_found(&method).to_json() });
                if let Some(s) = &sess {
                    err["sessionId"] = json!(s);
                }
                client.send(err);
                return false;
            }
            let (tid, keep_session) = match &to {
                Session::Page(t) => (t.clone(), false),
                Session::Child(t) => (t.clone(), true),
                Session::Browser => return false,
            };
            if !keep_session {
                let key = sess.clone().unwrap_or_default();
                match method.as_str() {
                    "Page.enable" => {
                        if let Some(c) = self.clients.get_mut(&client_id) {
                            c.page_on.insert(key);
                        }
                    }
                    "Page.disable" => {
                        if let Some(c) = self.clients.get_mut(&client_id) {
                            c.page_on.remove(&key);
                        }
                    }
                    // The page's thread is waiting on its dialog, so the router answers for it (D88 step 2).
                    "Page.handleJavaScriptDialog" => {
                        self.handle_dialog(client_id, id, sess, &tid, &params);
                        return false;
                    }
                    _ => {}
                }
            }
            if !keep_session && (method == "Page.startScreencast" || method == "Page.stopScreencast") {
                if let Some(t) = self.target(&tid) {
                    t.screencast = (method == "Page.startScreencast").then(|| (client_id, sess.clone()));
                }
            }
            let cid = self.next_child_id;
            self.next_child_id += 1;
            let mut msg = req.clone();
            msg["id"] = json!(cid);
            if !keep_session {
                if let Some(o) = msg.as_object_mut() {
                    o.remove("sessionId");
                }
            }
            self.pending.insert(cid, (client_id, id, sess));
            if let Some(t) = self.target(&tid) {
                // a popup its opener's process serves: the command is that popup's
                if t.host.is_some() && !keep_session {
                    msg["korvusTarget"] = json!(tid);
                }
                let _ = t.to_child.send(msg.to_string());
            }
            // a paused new target runs now: its first navigation follows
            if !keep_session && method == "Runtime.runIfWaitingForDebugger" {
                if let Some(url) = self.target(&tid).and_then(|t| t.pending_nav.take()) {
                    self.navigate_new(&tid, &url);
                }
            }
            return false;
        }
        // A call on a CDP domain a plugin serves (`Korvus_<name>.*`, plugin ABI 3, A1) is answered by the plugin,
        // not the engine. Checked before the browser commands so a plugin cannot be shadowed by one, and only for
        // the `Korvus_` names the header reserves.
        if method.starts_with("Korvus_") {
            if let Some(domain) = method.split('.').next() {
                if plugin_host::cdp_domains().iter().any(|d| d == domain) {
                    let params_json = if params.is_null() { String::new() } else { params.to_string() };
                    let resp = match plugin_host::cdp_call(&method, &params_json) {
                        Some(r) => json!({ "id": id, "result": serde_json::from_str::<Value>(&r).unwrap_or(Value::Null) }),
                        None => json!({ "id": id, "error": RpcError::server(format!("the plugin did not answer {method}")).to_json() }),
                    };
                    if let Some(c) = self.clients.get(&client_id) {
                        c.send(resp);
                    }
                    return false;
                }
            }
        }
        if method == "Target.createTarget" {
            self.create_target(client_id, id, &params);
            return false;
        }
        if method == "Target.detachFromTarget" {
            if let Some(s) = params.get("sessionId").and_then(Value::as_str) {
                self.end_screencast(client_id, Some(s));
            }
        }
        let (result, events, after, close) = self.browser_command(client_id, &method, &params);
        // A context's disposal is answered once its pages are gone, as Chrome's is (its callback runs when the
        // context is destroyed): every connection hears the pages detach first, so a client whose `close()` returned
        // reads them closed. A target's own close is answered first, as Chrome answers `closeTarget`.
        let (early, then): (Vec<Then>, Vec<Then>) = std::mem::take(&mut self.then).into_iter().partition(|t| matches!(t, Then::RemoveContext(_)));
        self.then = then;
        for t in early {
            if let Then::RemoveContext(id) = t {
                self.remove_context(&id);
            }
        }
        let Some(client) = self.clients.get(&client_id) else { return close };
        let stamp = |mut v: Value| {
            if let (Some(s), Some(o)) = (&browser_session, v.as_object_mut()) {
                o.insert("sessionId".into(), json!(s));
            }
            v
        };
        for ev in events {
            client.send(stamp(ev));
        }
        client.send(stamp(match result {
            Ok(r) => json!({ "id": id, "result": r }),
            Err(e) => json!({ "id": id, "error": e.to_json() }),
        }));
        for ev in after {
            client.send(stamp(ev));
        }
        for then in std::mem::take(&mut self.then) {
            match then {
                Then::CloseTarget(tid) => self.close_target(&tid),
                Then::RemoveContext(id) => self.remove_context(&id),
            }
        }
        close
    }

    /// `Target.createTarget`: the process starts off the actor, and the answer follows when it listens.
    fn create_target(&mut self, client: u64, id: Value, params: &Value) {
        let str_param = |k: &str| params.get(k).and_then(Value::as_str).map(String::from);
        let Some(c) = self.clients.get(&client) else { return };
        let tenant = c.tenant;
        let ctx = match str_param("browserContextId") {
            None => c.home.clone(),
            Some(x) if self.sees(client, &x) => x,
            Some(_) => {
                c.send(json!({ "id": id, "error": RpcError::server("Failed to find browser context with id".into()).to_json() }));
                return;
            }
        };
        // G2: a tenancy plugin may refuse a tenant a NEW session -- a context with no counted page yet. It never
        // stops a context already running (invariant I3), so one that is a session already is not re-asked.
        if !self.is_session(&ctx) && !plugin_host::context_allow(tenant, &ctx) {
            c.send(json!({ "id": id, "error": RpcError::server("the tenancy plugin refused this context for the tenant".into()).to_json() }));
            return;
        }
        let url = str_param("url").filter(|u| !u.is_empty()).unwrap_or_else(|| "about:blank".into());
        // Step 13 (Frontier M), popup / target control: a plugin may gate a NEW target. `Target.createTarget`
        // opens one at a client's ask; when a target plugin is loaded (`has_target_gate`) it is asked to allow
        // (1) or block (0), and a block refuses the command without starting a process -- answered cleanly, as
        // the tenancy refusal above is. Off entirely with no such plugin: the gate is never asked and every
        // target opens exactly as before (invariant I9/J2). CDP names a new window with `newWindow`; a tab else.
        if plugin_host::has_target_gate() {
            let disposition = if params.get("newWindow").and_then(Value::as_bool).unwrap_or(false) { "newWindow" } else { "newTab" };
            if !plugin_host::on_target_open(&url, disposition) {
                c.send(json!({ "id": id, "error": RpcError::server("a plugin blocked opening this target".into()).to_json() }));
                return;
            }
        }
        let job = Job { client, id, context: ctx, url, popup: None };
        // A page in a context that is not a session yet makes it one: over the license's cap, it waits (its answer
        // with it), and a running session goes on (invariant I3).
        if !self.is_session(&job.context) && (self.sessions_now() >= CAP.load(std::sync::atomic::Ordering::Relaxed) || !self.queue.is_empty()) {
            self.queue.push_back(job);
            eprintln!("[korvus] a new session waits: {} of {} in use, {} waiting", self.sessions_now(), CAP.load(std::sync::atomic::Ordering::Relaxed), self.queue.len());
            self.recount();
            return;
        }
        self.start(job);
    }

    /// Start the page a `createTarget` asked for.
    fn start(&mut self, job: Job) {
        *self.starting.entry(job.context.clone()).or_default() += 1;
        self.recount();
        let ctx = job.context.clone();
        let (launch, ev, persona, path) = (self.launch.clone(), self.ev.clone(), self.persona.clone(), self.spec_path(&ctx));
        // a popup's process knows its opener's origin (`-`: opened `noopener`, no opener)
        let extra = match &job.popup {
            Some(p) => vec![POPUP_FLAG.to_string(), if p.noopener { "-".to_string() } else { p.opener_origin.clone() }],
            None => Vec::new(),
        };
        tokio::spawn(async move {
            let result = spawn_target(launch, ev.clone(), persona, ctx, path, extra).await.map_err(|e| e.to_string());
            let _ = ev.send(Ev::Spawned { job, result });
        });
    }

    /// Is `ctx` a session: a page a client made is in it, or starting.
    fn is_session(&self, ctx: &str) -> bool {
        self.targets.iter().any(|t| t.counted && t.context == ctx) || self.starting.get(ctx).is_some_and(|n| *n > 0)
    }

    /// The sessions running now.
    fn sessions_now(&self) -> usize {
        self.contexts.iter().filter(|c| self.is_session(&c.id)).count()
    }

    fn recount(&self) {
        ACTIVE.store(self.sessions_now(), std::sync::atomic::Ordering::Relaxed);
        QUEUED.store(self.queue.len(), std::sync::atomic::Ordering::Relaxed);
    }

    /// Let in the waiting sessions the cap now has room for, first come first served; a page for a context that is
    /// a session already does not wait for a place, and one whose client or context went goes.
    fn drain(&mut self) {
        let mut waiting: Vec<Job> = Vec::from(std::mem::take(&mut self.queue));
        while !waiting.is_empty() {
            // G4: a tenancy plugin may pick which waiting session is admitted next, by tenant; without one the
            // oldest goes first. The pick only orders who takes a freed slot -- it cannot change the cap.
            let idx = {
                let tenants: Vec<i64> = waiting.iter().map(|j| self.tenant_of(j.client)).collect();
                // A tenancy plugin is untrusted: clamp its index into range so a bad `queue_order` cannot panic
                // `remove` and take down the endpoint. `waiting` is non-empty here, so `len() - 1` is valid.
                plugin_host::queue_order(&tenants).unwrap_or(0).min(waiting.len() - 1)
            };
            let job = waiting.remove(idx);
            if job.popup.is_none() && !self.clients.contains_key(&job.client) {
                continue;
            }
            if self.ctx(&job.context).is_none() {
                if let Some(c) = self.clients.get(&job.client) {
                    c.send(json!({ "id": job.id, "error": RpcError::server("Failed to find browser context with id".into()).to_json() }));
                }
                continue;
            }
            if self.is_session(&job.context) || (self.queue.is_empty() && self.sessions_now() < CAP.load(std::sync::atomic::Ordering::Relaxed)) {
                self.start(job);
            } else {
                self.queue.push_back(job);
            }
        }
        self.recount();
    }

    /// A `createTarget`'s process listens: the target exists, as Chrome reports a new one.
    fn spawned(&mut self, job: Job, result: Result<Target, String>) {
        if let Some(n) = self.starting.get_mut(&job.context) {
            *n = n.saturating_sub(1);
        }
        let mut t = match result {
            Ok(t) => t,
            Err(e) => {
                if let Some(p) = &job.popup {
                    // the opener's `popup.closed` turns true: the window never came
                    self.to_opener(&p.opener, p.rid, json!({ "kind": "closed" }));
                } else if let Some(c) = self.clients.get(&job.client) {
                    c.send(json!({ "id": job.id, "error": RpcError::server(format!("Failed to create the target: {e}")).to_json() }));
                }
                return;
            }
        };
        // The context went while the process started: so does the target.
        if self.ctx(&t.context).is_none() {
            if let Some(c) = self.clients.get(&job.client) {
                c.send(json!({ "id": job.id, "error": RpcError::server("Failed to find browser context with id".into()).to_json() }));
            }
            return;
        }
        // A popup knows its opener; its opener's process hears from it through the router.
        if let Some(p) = &job.popup {
            if self.targets.iter().all(|o| o.id != p.opener) {
                return; // the opener went while the popup's process started
            }
            t.opener = Some((p.opener.clone(), !p.noopener));
            if !p.noopener {
                self.popup_rids.insert((p.opener.clone(), p.rid), t.id.clone());
            }
        }
        let tid = t.id.clone();
        let created = t.info(false);
        plugin_host::event("page.created", &json!({ "target": tid, "context": t.context }).to_string());
        t.counted = true;
        self.targets.insert(0, t);
        self.recount();
        let viewers: Vec<u64> = self.clients.keys().copied().filter(|&c| self.sees(c, &job.context)).collect();
        for &c in &viewers {
            let mut events = Vec::new();
            if self.clients[&c].discover {
                events.push(json!({ "method": "Target.targetCreated", "params": { "targetInfo": created.clone() } }));
            }
            if let Some(wait) = self.clients[&c].auto_attach {
                events.extend(self.attach(c, &tid, wait));
            }
            let client = &self.clients[&c];
            for ev in events {
                client.send(ev);
            }
        }
        if let Some(t) = self.target(&tid) {
            t.title = "about:blank".into();
        }
        if job.url != "about:blank" {
            // Chrome loads the url into the new target itself -- after the client that auto-attached to it paused
            // (`waitForDebuggerOnStart`) has set it up and let it run: Playwright arms the page's lifecycle events
            // then, and a popup that loaded before it did never told it `load` (D74 step 3).
            let paused = viewers.iter().any(|c| self.clients.get(c).is_some_and(|c| c.auto_attach == Some(true)));
            if paused {
                if let Some(t) = self.target(&tid) {
                    t.pending_nav = Some(job.url.clone());
                }
            } else {
                self.navigate_new(&tid, &job.url);
            }
        }
        if job.popup.is_some() {
            // no client asked: nothing is answered
        } else if let Some(c) = self.clients.get(&job.client) {
            c.send(json!({ "id": job.id, "result": { "targetId": tid } }));
        }
        for &c in &viewers {
            let Some(client) = self.clients.get(&c) else { continue };
            if client.discover {
                if let Some(t) = self.targets.iter().find(|t| t.id == tid) {
                    client.send(json!({ "method": "Target.targetInfoChanged", "params": { "targetInfo": t.info(Self::attached(client, &tid)) } }));
                }
            }
        }
    }

    /// A browser-level command: `(answer, events before it, events after it, close the browser)`.
    fn browser_command(&mut self, client: u64, method: &str, params: &Value) -> Answer {
        let ok = |v: Value| (Ok(v), Vec::new(), Vec::new(), false);
        let str_param = |k: &str| params.get(k).and_then(Value::as_str).map(String::from);
        match method {
            "Browser.getVersion" => ok(VersionInfo::of(&*self.persona).cdp()),
            // The browser target as a session of its own (Playwright's `newCDPSession`, Puppeteer's
            // `browser.target().createCDPSession()`).
            "Target.attachToBrowserTarget" => {
                let sid = hex32();
                if let Some(c) = self.clients.get_mut(&client) {
                    c.sessions.insert(sid.clone(), Session::Browser);
                }
                ok(json!({ "sessionId": sid }))
            }
            // A context's cookies as DevTools sees them -- HttpOnly ones included -- read and written in the router's
            // store, where the context's pages share them (`transport::context_store`): Playwright's
            // `context.cookies()`, `addCookies()`, `clearCookies()` and `storageState()`.
            "Storage.getCookies" => {
                let Some(ctx) = self.context_param(client, params) else { return fail("Failed to find browser context for id") };
                let jar = self.store.slot(&ctx, "cookies").unwrap_or_default();
                let cookies: Vec<Value> = transport::cookies::devtools_cookies_in(&jar).iter().map(crate::network::cookie_json).collect();
                ok(json!({ "cookies": cookies }))
            }
            "Storage.setCookies" => {
                let Some(ctx) = self.context_param(client, params) else { return fail("Failed to find browser context for id") };
                let list: Vec<transport::cookies::DevtoolsCookieInit> =
                    params.get("cookies").and_then(Value::as_array).map(|l| l.iter().map(crate::network::cookie_init).collect()).unwrap_or_default();
                let mut refused = false;
                self.store.update_slot(&ctx, "cookies", |jar| refused = !transport::cookies::devtools_set_all_in(jar, &list));
                if refused {
                    return (Err(RpcError::invalid_params("Invalid cookie fields".to_string())), Vec::new(), Vec::new(), false);
                }
                ok(json!({}))
            }
            "Storage.clearCookies" => {
                let Some(ctx) = self.context_param(client, params) else { return fail("Failed to find browser context for id") };
                self.store.update_slot(&ctx, "cookies", |jar| *jar = transport::cookies::empty_jar());
                ok(json!({}))
            }
            // Permissions a client grants a context (`context.grantPermissions`, `newContext({permissions})`): its pages
            // already open are told, and its later ones start with them (`ContextSpec::permissions`).
            "Browser.grantPermissions" | "Browser.resetPermissions" | "Browser.setPermission" => {
                let Some(ctx) = self.context_param(client, params) else { return fail("Failed to find browser context for id") };
                let current = self.ctx(&ctx).and_then(|c| c.spec.permissions.clone());
                let next: Option<Vec<String>> = match method {
                    "Browser.grantPermissions" => Some(
                        params.get("permissions").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(permission_name).collect()).unwrap_or_default(),
                    ),
                    "Browser.resetPermissions" => None,
                    _ => {
                        let name = params.get("permission").and_then(|p| p.get("name")).and_then(Value::as_str).map(permission_name).unwrap_or_default();
                        let granted = params.get("setting").and_then(Value::as_str) == Some("granted");
                        let mut list = current.unwrap_or_default();
                        list.retain(|n| *n != name);
                        if granted && !name.is_empty() {
                            list.push(name);
                        }
                        Some(list)
                    }
                };
                self.set_context_permissions(&ctx, next);
                ok(json!({}))
            }
            "Browser.close" => (Ok(json!({})), Vec::new(), Vec::new(), true),
            "Target.activateTarget" | "Target.setRemoteLocations" => ok(json!({})),
            // Where a context's downloads go (Playwright's `acceptDownloads`): `allow` / `allowAndName` into
            // `downloadPath`, `deny`, or `default` (none). Without a `browserContextId` it is every context's.
            "Browser.setDownloadBehavior" => {
                let behavior = params.get("behavior").and_then(Value::as_str).unwrap_or("default").to_string();
                let path = params.get("downloadPath").and_then(Value::as_str).unwrap_or("").to_string();
                let b = (behavior != "default").then_some((behavior, path));
                let targets: Vec<String> = match params.get("browserContextId").and_then(Value::as_str) {
                    Some(id) if self.sees(client, id) => vec![id.to_string()],
                    Some(_) => return fail("Failed to find browser context for id"),
                    None => self.contexts.iter().filter(|c| self.sees(client, &c.id)).map(|c| c.id.clone()).collect(),
                };
                for ctx in targets {
                    self.set_context_downloads(&ctx, b.clone());
                }
                ok(json!({}))
            }
            "Browser.getWindowForTarget" => {
                let page_socket = self.clients.get(&client).and_then(|c| c.page_socket.clone());
                let t = match str_param("targetId") {
                    Some(id) => self.targets.iter().find(|t| t.id == id && self.sees(client, &t.context)),
                    None => page_socket.and_then(|id| self.targets.iter().find(|t| t.id == id)).or(self.targets.iter().find(|t| self.sees(client, &t.context))),
                };
                match t {
                    Some(t) => ok(json!({ "windowId": t.window_id, "bounds": t.bounds })),
                    None => fail("No target with given id found"),
                }
            }
            "Browser.setWindowBounds" => {
                let wid = params.get("windowId").and_then(Value::as_u64);
                let Some(b) = params.get("bounds").and_then(Value::as_object) else {
                    return fail("Invalid parameters");
                };
                // Chrome's own refusal: a window state other than `normal` takes no position or size with it.
                let state = b.get("windowState").and_then(Value::as_str).unwrap_or("normal");
                if state != "normal" && ["left", "top", "width", "height"].iter().any(|k| b.contains_key(*k)) {
                    return fail("The 'minimized', 'maximized' and 'fullscreen' states cannot be combined with 'left', 'top', 'width' or 'height'");
                }
                let cid = self.next_child_id;
                let Some(t) = wid.and_then(|wid| self.targets.iter_mut().find(|t| u64::from(t.window_id) == wid)) else {
                    return fail("Browser window not found");
                };
                for (k, v) in b {
                    t.bounds[k] = v.clone();
                }
                // The page's process sizes its window from them (D75): its viewport, layout, `resize` and frames
                // follow. The answer is the router's own (an id no client holds), dropped when it comes back.
                let cmd = json!({ "id": cid, "method": "Korvus.setWindowBounds", "params": { "bounds": t.bounds } });
                let _ = t.to_child.send(cmd.to_string());
                self.next_child_id += 1;
                ok(json!({}))
            }
            "Target.getBrowserContexts" => {
                let home = self.clients.get(&client).map(|c| c.home.clone()).unwrap_or_default();
                ok(json!({
                    "browserContextIds": self.contexts.iter().filter(|c| c.owner == Some(client)).map(|c| c.id.clone()).collect::<Vec<_>>(),
                    "defaultBrowserContextId": home,
                }))
            }
            "Target.createBrowserContext" => {
                let id = hex32();
                let dispose = params.get("disposeOnDetach").and_then(Value::as_bool).unwrap_or(false);
                let spec = ContextSpec::from_params(&id, params);
                match self.add_context(id.clone(), Kind::Created, Some(client), dispose, spec) {
                    Ok(()) => ok(json!({ "browserContextId": id })),
                    Err(e) => fail(&format!("Failed to create the browser context: {e}")),
                }
            }
            "Target.disposeBrowserContext" => {
                let Some(ctx) = str_param("browserContextId").filter(|c| self.ctx(c).is_some_and(|x| x.owner == Some(client))) else {
                    return fail("Failed to find context with id");
                };
                // `{}` first, as Chrome answers; the closed pages' events follow it.
                self.then.push(Then::RemoveContext(ctx));
                ok(json!({}))
            }
            "Target.setDiscoverTargets" => {
                let on = params.get("discover").and_then(Value::as_bool).unwrap_or(false);
                let mut events = Vec::new();
                if let Some(c) = self.clients.get(&client) {
                    if on && !c.discover {
                        for t in self.targets.iter().rev().filter(|t| self.sees(client, &t.context)) {
                            events.push(json!({ "method": "Target.targetCreated", "params": { "targetInfo": t.info(Self::attached(c, &t.id)) } }));
                        }
                    }
                }
                if let Some(c) = self.clients.get_mut(&client) {
                    c.discover = on;
                }
                (Ok(json!({})), events, Vec::new(), false)
            }
            "Target.setAutoAttach" => {
                let on = params.get("autoAttach").and_then(Value::as_bool).unwrap_or(false);
                let wait = params.get("waitForDebuggerOnStart").and_then(Value::as_bool).unwrap_or(false);
                let mut events = Vec::new();
                if on {
                    let unattached: Vec<String> = match self.clients.get(&client) {
                        Some(c) => self.targets.iter().rev().filter(|t| self.sees(client, &t.context) && !Self::attached(c, &t.id)).map(|t| t.id.clone()).collect(),
                        None => Vec::new(),
                    };
                    for tid in unattached {
                        events.extend(self.attach(client, &tid, false));
                    }
                }
                if let Some(c) = self.clients.get_mut(&client) {
                    c.auto_attach = on.then_some(wait);
                }
                (Ok(json!({})), events, Vec::new(), false)
            }
            "Target.attachToTarget" => {
                let Some(tid) = str_param("targetId").filter(|id| self.sees_target(client, id)) else {
                    return fail("No target with given id found");
                };
                if params.get("flatten").and_then(Value::as_bool) != Some(true) {
                    return fail("Only flatten sessions are supported");
                }
                let events = self.attach(client, &tid, false);
                let sid = events.last().and_then(|e| e["params"]["sessionId"].as_str()).unwrap_or_default().to_string();
                (Ok(json!({ "sessionId": sid })), events, Vec::new(), false)
            }
            "Target.detachFromTarget" => {
                let Some(c) = self.clients.get_mut(&client) else { return fail("No session with given id") };
                let sid = str_param("sessionId").or_else(|| {
                    let tid = str_param("targetId")?;
                    c.sessions.iter().find(|(_, s)| matches!(s, Session::Page(t) if *t == tid)).map(|(k, _)| k.clone())
                });
                match sid.and_then(|s| c.sessions.remove(&s).map(|r| (s, r))) {
                    Some((s, Session::Page(tid))) => {
                        let mut events = vec![json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": s, "targetId": tid } })];
                        let c = &self.clients[&client];
                        if c.discover && !Self::attached(c, &tid) {
                            if let Some(t) = self.targets.iter().find(|t| t.id == tid) {
                                events.insert(0, json!({ "method": "Target.targetInfoChanged", "params": { "targetInfo": t.info(false) } }));
                            }
                        }
                        (Ok(json!({})), events, Vec::new(), false)
                    }
                    Some((_, Session::Child(_) | Session::Browser)) => ok(json!({})),
                    None => fail("No session with given id"),
                }
            }
            "Target.closeTarget" => {
                let Some(tid) = str_param("targetId").filter(|id| self.sees_target(client, id)) else {
                    return fail("No target with given id found");
                };
                // `{success: true}` first, then the closed page's events to every connection that saw it.
                self.then.push(Then::CloseTarget(tid));
                ok(json!({ "success": true }))
            }
            "Target.getTargets" => {
                let c = &self.clients[&client];
                ok(json!({
                    "targetInfos": self.targets.iter().filter(|t| self.sees(client, &t.context)).map(|t| t.info(Self::attached(c, &t.id))).collect::<Vec<_>>(),
                }))
            }
            "Target.getTargetInfo" => match str_param("targetId") {
                Some(id) => match self.targets.iter().find(|t| t.id == id && self.sees(client, &t.context)) {
                    Some(t) => ok(json!({ "targetInfo": t.info(Self::attached(&self.clients[&client], &t.id)) })),
                    None => fail("No target with given id found"),
                },
                None => ok(json!({ "targetInfo": {
                    "targetId": self.uuid, "type": "browser", "title": "", "url": "", "attached": true, "canAccessOpener": false,
                } })),
            },
            other => (Err(RpcError::method_not_found(other)), Vec::new(), Vec::new(), false),
        }
    }

    /// Attach `client` to page target `tid` with a fresh session: Chrome's `targetInfoChanged` (when the
    /// target was not attached before) and `attachedToTarget`.
    fn attach(&mut self, client: u64, tid: &str, waiting: bool) -> Vec<Value> {
        let Some(info) = self.targets.iter().find(|t| t.id == tid).map(|t| t.info(true)) else { return Vec::new() };
        let Some(c) = self.clients.get_mut(&client) else { return Vec::new() };
        let was = Self::attached(c, tid);
        let sid = hex32();
        c.sessions.insert(sid.clone(), Session::Page(tid.to_string()));
        let mut events = Vec::new();
        if !was && c.discover {
            events.push(json!({ "method": "Target.targetInfoChanged", "params": { "targetInfo": info.clone() } }));
        }
        events.push(json!({ "method": "Target.attachedToTarget", "params": {
            "sessionId": sid, "targetInfo": info, "waitingForDebugger": waiting,
        } }));
        events
    }

    /// Close page target `tid`: every connection on it hears what Chrome sends after `{success: true}`. Dropping the
    /// target ends its process.
    fn close_target(&mut self, tid: &str) {
        // a process that goes takes the popups it serves with it
        let hosted: Vec<String> = self.targets.iter().filter(|t| t.host.as_deref() == Some(tid)).map(|t| t.id.clone()).collect();
        for h in hosted {
            if let Some(t) = self.target(&h) {
                t.host = Some(String::new());
            }
            self.close_target(&h);
        }
        // a popup a process serves is closed there (unless that process is the one that closed it)
        if let Some(host) = self.targets.iter().find(|t| t.id == tid).and_then(|t| t.host.clone()).filter(|h| !h.is_empty()) {
            let cid = self.next_child_id;
            self.next_child_id += 1;
            if let Some(t) = self.targets.iter().find(|t| t.id == host) {
                let _ = t.to_child.send(json!({ "id": cid, "method": "Korvus.closePopup", "korvusTarget": tid }).to_string());
            }
        }
        let info = self.targets.iter().find(|t| t.id == tid).map(|t| t.info(false));
        let context = self.targets.iter().find(|t| t.id == tid).map(|t| t.context.clone()).unwrap_or_default();
        let ids: Vec<u64> = self.clients.keys().copied().collect();
        for cid in ids {
            let sees = self.sees(cid, &context);
            let Some(c) = self.clients.get_mut(&cid) else { continue };
            let sessions: Vec<String> = c.sessions.iter().filter(|(_, s)| matches!(s, Session::Page(t) | Session::Child(t) if t == tid)).map(|(k, _)| k.clone()).collect();
            let mut events = Vec::new();
            for s in &sessions {
                if matches!(c.sessions.get(s), Some(Session::Page(_))) {
                    events.push(json!({ "method": "Inspector.detached", "sessionId": s, "params": { "reason": "Render process gone." } }));
                }
            }
            if let (true, Some(info)) = (c.discover && !sessions.is_empty(), &info) {
                events.push(json!({ "method": "Target.targetInfoChanged", "params": { "targetInfo": info } }));
            }
            for s in &sessions {
                if let Some(Session::Page(_)) = c.sessions.remove(s) {
                    events.push(json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": s, "targetId": tid } }));
                }
            }
            if c.discover && sees {
                events.push(json!({ "method": "Target.targetDestroyed", "params": { "targetId": tid } }));
            }
            if c.page_socket.as_deref() == Some(tid) {
                c.page_socket = None;
            }
            for ev in events {
                c.send(ev);
            }
        }
        if self.targets.iter().any(|t| t.id == tid) {
            plugin_host::event("page.closed", &json!({ "target": tid, "context": context }).to_string());
        }
        // A popup's opener hears it closed (`popup.closed`); an opener's popups hear it gone (`opener.closed`).
        if let Some((opener, true)) = self.targets.iter().find(|t| t.id == tid).and_then(|t| t.opener.clone()) {
            if let Some(rid) = self.popup_rid(tid) {
                self.to_opener(&opener, rid, json!({ "kind": "closed" }));
            }
        }
        for t in &self.targets {
            if t.opener.as_ref().is_some_and(|(o, linked)| o == tid && *linked) {
                let _ = t.to_stdin.send(format!("{}{{}}\n", OPENER_GONE_LINE));
            }
        }
        self.popup_rids.retain(|(a, _), b| a.as_str() != tid && b.as_str() != tid);
        self.targets.retain(|t| t.id != tid);
        // a session may have ended: one waiting takes its place
        self.drain();
    }

    /// Load `url` into new target `tid`, as Chrome does a `createTarget`'s or a popup's. Its answer is the router's own (an
    /// id no client holds), so it is dropped when it comes back.
    /// `Korvus.createViewer` (D88 step 3): a viewer ticket for this connection's context -- or one it made and names
    /// (`browserContextId`) -- `interactive` unless told otherwise, for `ttl` seconds (an hour by default, a week at
    /// most), openable from any page or only from `origins`. Answered with the ticket, the WebSocket URL a client opens,
    /// the stand-alone client's URL, and when it expires (Unix seconds).
    fn create_viewer(&mut self, client: u64, params: &Value) -> Result<Value, RpcError> {
        let Some(c) = self.clients.get(&client) else { return Err(RpcError::server("the connection is gone".into())) };
        let context = match params.get("browserContextId").and_then(Value::as_str) {
            None => c.home.clone(),
            Some(x) if self.sees(client, x) => x.to_string(),
            Some(_) => return Err(RpcError::server("Failed to find browser context with id".into())),
        };
        let ttl = params.get("ttl").and_then(Value::as_f64).unwrap_or(3600.0).clamp(1.0, 7.0 * 86400.0);
        let origins = params
            .get("origins")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(|o| o.trim().trim_end_matches('/').to_ascii_lowercase()).collect())
            .unwrap_or_default();
        let ticket = crate::viewer::Ticket {
            context: Some(context),
            interactive: params.get("interactive").and_then(Value::as_bool).unwrap_or(true),
            expires: Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(ttl)),
            origins,
            tenant: c.tenant,
        };
        let key = crate::viewer::mint();
        let host = c.host.clone();
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) + ttl;
        self.tickets.insert(key.clone(), ticket);
        Ok(json!({
            "ticket": key,
            "webSocketUrl": format!("ws://{host}/?ticket={key}"),
            "clientUrl": format!("http://{host}/client/#ticket={key}"),
            "expires": expires.floor(),
        }))
    }

    /// `Korvus.revokeViewer {ticket}`: the ticket opens nothing more, and its open connections end.
    fn revoke_viewer(&mut self, params: &Value) -> Result<Value, RpcError> {
        let key = params.get("ticket").and_then(Value::as_str).unwrap_or_default().to_string();
        if self.tickets.remove(&key).is_none() {
            return Err(RpcError::server("No such viewer ticket".into()));
        }
        self.end_viewers(|t| t == key);
        Ok(json!({}))
    }

    /// The viewer connections whose ticket `gone` says is spent: ended, their sessions gone with them.
    fn end_viewers(&mut self, gone: impl Fn(&str) -> bool) {
        let ended: Vec<u64> = self.clients.iter().filter(|(_, c)| c.viewer.as_ref().is_some_and(|g| gone(&g.ticket))).map(|(&k, _)| k).collect();
        for k in ended {
            // dropping its sender ends the connection's writer, which closes the socket
            self.gone(k);
        }
    }

    /// Expired tickets go, and every connection they opened.
    fn sweep_tickets(&mut self) {
        let expired: Vec<String> = self.tickets.iter().filter(|(_, t)| t.expired()).map(|(k, _)| k.clone()).collect();
        if expired.is_empty() {
            return;
        }
        for k in &expired {
            self.tickets.remove(k);
        }
        self.end_viewers(|t| expired.iter().any(|k| k == t));
    }

    /// The sessions on page `tid` that enabled `Page` (`None`: a page socket): who hears its dialogs.
    fn page_listeners(&self, tid: &str) -> Vec<(u64, Option<String>)> {
        let mut out = Vec::new();
        for (&cid, c) in &self.clients {
            if c.page_socket.as_deref() == Some(tid) && c.page_on.contains("") {
                out.push((cid, None));
            }
            for (s, r) in &c.sessions {
                if matches!(r, Session::Page(t) if t == tid) && c.page_on.contains(s) {
                    out.push((cid, Some(s.clone())));
                }
            }
        }
        out
    }

    /// An event to each of `to`.
    fn send_to(&self, to: &[(u64, Option<String>)], ev: &Value) {
        for (cid, s) in to {
            if let Some(c) = self.clients.get(cid) {
                let mut e = ev.clone();
                if let Some(s) = s {
                    e["sessionId"] = json!(s);
                }
                c.send(e);
            }
        }
    }

    /// Page `tid` raised a dialog and waits (D88 step 2): Chrome's `javascriptDialogOpening` to each session that
    /// enabled `Page`, or -- with none -- "unhandled" back at once, and the page goes on as it did before (dismissed).
    fn on_dialog(&mut self, tid: &str, json: &str) {
        let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
        let listeners = self.page_listeners(tid);
        let Some(t) = self.target(tid) else { return };
        if listeners.is_empty() {
            let _ = t.to_stdin.send(format!("{}{}\n", DIALOG_CLOSE_LINE, json!({ "unhandled": true })));
            return;
        }
        let params = json!({
            "url": t.url, "frameId": tid, "message": v["message"], "type": v["type"],
            "hasBrowserHandler": true, "defaultPrompt": v["defaultPrompt"],
        });
        t.dialog = Some(params.clone());
        self.send_to(&listeners, &json!({ "method": "Page.javascriptDialogOpening", "params": params }));
    }

    /// `Page.handleJavaScriptDialog` on page `tid`: the answer to the page's process, `{}` to the client, then Chrome's
    /// `javascriptDialogClosed` to each session that enabled `Page`. Refused as Chrome refuses it when no dialog shows,
    /// or to a session that did not enable `Page`.
    fn handle_dialog(&mut self, client: u64, id: Value, sess: Option<String>, tid: &str, params: &Value) {
        let enabled = self.clients.get(&client).is_some_and(|c| c.page_on.contains(sess.as_deref().unwrap_or("")));
        let showing = self.targets.iter().find(|t| t.id == tid).and_then(|t| t.dialog.clone());
        let mut resp = match (enabled, showing) {
            (true, Some(d)) => {
                let accept = params.get("accept").and_then(Value::as_bool).unwrap_or(false);
                let text = if accept && d["type"] == "prompt" { params.get("promptText").and_then(Value::as_str).unwrap_or("") } else { "" };
                if let Some(t) = self.target(tid) {
                    t.dialog = None;
                    let _ = t.to_stdin.send(format!("{}{}\n", DIALOG_CLOSE_LINE, json!({ "accept": accept, "promptText": text })));
                }
                let listeners = self.page_listeners(tid);
                let mut r = json!({ "id": id, "result": {} });
                if let Some(s) = &sess {
                    r["sessionId"] = json!(s);
                }
                if let Some(c) = self.clients.get(&client) {
                    c.send(r);
                }
                self.send_to(&listeners, &json!({ "method": "Page.javascriptDialogClosed", "params": { "frameId": tid, "result": accept, "userInput": text } }));
                return;
            }
            _ => json!({ "id": id, "error": { "code": -32602, "message": "No dialog is showing" } }),
        };
        if let Some(s) = &sess {
            resp["sessionId"] = json!(s);
        }
        if let Some(c) = self.clients.get(&client) {
            c.send(resp);
        }
    }

    /// Chrome's `targetInfoChanged` for target `tid`, to every client that discovers targets and sees its context.
    fn announce_info(&self, tid: &str) {
        let Some(t) = self.targets.iter().find(|t| t.id == tid) else { return };
        for (&cid, c) in &self.clients {
            if c.discover && self.sees(cid, &t.context) {
                c.send(json!({ "method": "Target.targetInfoChanged", "params": { "targetInfo": t.info(Self::attached(c, tid)) } }));
            }
        }
    }

    fn navigate_new(&mut self, tid: &str, url: &str) {
        let cid = self.next_child_id;
        self.next_child_id += 1;
        // a new tab's first URL is Chrome's `auto_toplevel` entry, which takes the place of its `about:blank` (D88 step
        // 2); a popup's is the `link` its opener followed
        let transition = if self.targets.iter().any(|t| t.id == tid && t.opener.is_some()) { "link" } else { "auto_toplevel" };
        let nav = json!({ "id": cid, "method": "Page.navigate", "params": { "url": url, "transitionType": transition } });
        if let Some(t) = self.target(tid) {
            let _ = t.to_child.send(nav.to_string());
        }
    }

    /// The id popup `tid`'s opener knows it by.
    fn popup_rid(&self, tid: &str) -> Option<u64> {
        self.popup_rids.iter().find(|(_, b)| b.as_str() == tid).map(|((_, rid), _)| *rid)
    }

    /// A line to `opener`'s process about its popup `rid`.
    fn to_opener(&self, opener: &str, rid: u64, mut ev: Value) {
        ev["rid"] = json!(rid);
        if let Some(t) = self.targets.iter().find(|t| t.id == opener) {
            let _ = t.to_stdin.send(format!("{}{}\n", POPUP_EV_LINE, ev));
        }
    }

    /// A popup line from page `from`'s process (D74 step 3, `webapi`'s `core/html/popup.rs`).
    fn on_popup_line(&mut self, from: &str, line: &str, json: &str) {
        let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
        let rid = v.get("rid").and_then(Value::as_u64).unwrap_or(0);
        let str_of = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let popup = self.popup_rids.get(&(from.to_string(), rid)).cloned();
        match line {
            POPUP_OPEN_LINE => {
                let Some(context) = self.targets.iter().find(|t| t.id == from).map(|t| t.context.clone()) else { return };
                let url = str_of("url");
                let noopener = v.get("noopener").and_then(Value::as_bool).unwrap_or(false);
                // A plugin's target gate (step 13) was asked in the opener's process, as `window.open` opened it.
                let opener_origin = url::Url::parse(&str_of("referrer")).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".into());
                let job = Job {
                    client: 0,
                    id: Value::Null,
                    context: context.clone(),
                    url,
                    popup: Some(PopupJob { opener: from.to_string(), rid, noopener, opener_origin }),
                };
                // registered now, so what the opener says before the popup's process listens is not lost
                if !noopener {
                    self.popup_rids.insert((from.to_string(), rid), String::new());
                }
                if self.is_session(&context) || self.sessions_now() < CAP.load(std::sync::atomic::Ordering::Relaxed) {
                    self.start(job);
                } else {
                    self.queue.push_back(job);
                    self.recount();
                }
            }
            POPUP_POST_LINE => {
                if let Some(t) = popup.and_then(|b| self.targets.iter().find(|t| t.id == b)) {
                    let _ = t.to_stdin.send(format!("{}{}\n", OPENER_MSG_LINE, json!({ "data": str_of("data"), "origin": str_of("origin") })));
                }
            }
            POPUP_NAV_LINE => {
                let cid = self.next_child_id;
                if let Some(t) = popup.and_then(|b| self.targets.iter().find(|t| t.id == b)) {
                    // The router's own command: its answer (an id no client holds) is dropped when it comes back.
                    let _ = t.to_child.send(json!({ "id": cid, "method": "Page.navigate", "params": { "url": str_of("url") } }).to_string());
                    self.next_child_id += 1;
                }
            }
            POPUP_CLOSE_LINE => {
                if let Some(b) = popup.filter(|b| !b.is_empty()) {
                    self.close_target(&b);
                }
            }
            OPENER_POST_LINE => {
                let link = self.targets.iter().find(|t| t.id == from).and_then(|t| t.opener.clone());
                if let (Some((opener, true)), Some(rid)) = (link, self.popup_rid(from)) {
                    self.to_opener(&opener, rid, json!({ "kind": "message", "data": str_of("data"), "origin": str_of("origin") }));
                }
            }
            WINDOW_CLOSE_LINE => {
                if self.targets.iter().any(|t| t.id == from && t.opener.is_some()) {
                    self.close_target(from);
                }
            }
            POPUP_HOSTED_LINE => self.add_hosted(from, &str_of("targetId"), &str_of("url")),
            POPUP_HOSTED_CLOSE_LINE => {
                let tid = str_of("targetId");
                if self.targets.iter().any(|t| t.id == tid && t.host.as_deref() == Some(from)) {
                    // its process closed it: nothing to send back
                    if let Some(t) = self.target(&tid) {
                        t.host = Some(String::new());
                    }
                    self.close_target(&tid);
                }
            }
            _ => {}
        }
    }

    /// A same-site popup page `host`'s process serves (`POPUP_HOSTED_LINE`): a page target in `host`'s context, opened
    /// by it, announced as a new target is -- its commands go to `host`'s process marked with its id.
    fn add_hosted(&mut self, host: &str, tid: &str, url: &str) {
        if tid.is_empty() || self.targets.iter().any(|t| t.id == tid) {
            return;
        }
        let Some(h) = self.targets.iter().find(|t| t.id == host) else { return };
        let mut entropy = [0u8; 4];
        getrandom::getrandom(&mut entropy).expect("OS entropy for a window id");
        let t = Target {
            id: tid.to_string(),
            context: h.context.clone(),
            url: url.to_string(),
            title: untitled(url),
            window_id: u32::from_be_bytes(entropy) % 0x7fff_0000 + 0x1000,
            bounds: h.bounds.clone(),
            to_child: h.to_child.clone(),
            to_stdin: h.to_stdin.clone(),
            opener: Some((host.to_string(), true)),
            counted: true,
            _child: None,
            host: Some(host.to_string()),
            pending_nav: None,
            screencast: None,
            dialog: None,
        };
        let context = t.context.clone();
        let created = t.info(false);
        plugin_host::event("page.created", &json!({ "target": tid, "context": context }).to_string());
        self.targets.insert(0, t);
        let viewers: Vec<u64> = self.clients.keys().copied().filter(|&c| self.sees(c, &context)).collect();
        for &c in &viewers {
            let mut events = Vec::new();
            if self.clients[&c].discover {
                events.push(json!({ "method": "Target.targetCreated", "params": { "targetInfo": created.clone() } }));
            }
            if let Some(wait) = self.clients[&c].auto_attach {
                events.extend(self.attach(c, tid, wait));
            }
            let client = &self.clients[&c];
            for ev in events {
                client.send(ev);
            }
        }
    }

    /// What a page's process said: an answer to a relayed command, or an event for the sessions on it.
    fn on_child(&mut self, tid: &str, ev: FromChild) {
        let text = match ev {
            FromChild::Text(t) => t,
            FromChild::Closed => {
                // The process is gone (it crashed, or was closed): its sessions end as Chrome's do when a
                // renderer goes.
                if self.targets.iter().any(|t| t.id == tid) {
                    // Its target is still here, so no client closed it: an abnormal exit -- a crash, or an OOM kill
                    // (G6). The exit status, when the child has been reaped, says which.
                    let status = self.target(tid).and_then(|t| t._child.as_mut().and_then(|c| c.try_wait().ok().flatten()));
                    let context = self.targets.iter().find(|t| t.id == tid).map(|t| t.context.clone()).unwrap_or_default();
                    let kind = if oom_killed(status) { "page.oom" } else { "context.crashed" };
                    plugin_host::event(kind, &json!({ "context": context, "target": tid, "status": status_json(status) }).to_string());
                    self.close_target(tid);
                }
                return;
            }
        };
        let mut v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return,
        };
        // what a process says for a popup it serves is that popup target's
        let hosted = v.as_object_mut().and_then(|o| o.remove("korvusTarget")).and_then(|t| t.as_str().map(String::from));
        let tid: &str = match &hosted {
            Some(t) => t.as_str(),
            None => tid,
        };
        if let Some(cid) = v.get("id").and_then(Value::as_u64) {
            if let Some((client, orig, sess)) = self.pending.remove(&cid) {
                v["id"] = orig;
                match sess {
                    Some(s) => v["sessionId"] = json!(s),
                    None => {
                        if let Some(o) = v.as_object_mut() {
                            o.remove("sessionId");
                        }
                    }
                }
                if let Some(c) = self.clients.get(&client) {
                    c.send(v);
                }
            }
            return;
        }
        // An event a child's own child sent carries that session already: it goes to the connections holding it.
        if let Some(s) = v.get("sessionId").and_then(Value::as_str).map(String::from) {
            let detached = v["method"] == "Target.detachedFromTarget";
            let gone = v["params"]["sessionId"].as_str().map(String::from);
            for c in self.clients.values_mut() {
                if c.sessions.contains_key(&s) {
                    if detached {
                        if let Some(g) = &gone {
                            c.sessions.remove(g);
                        }
                    }
                    c.send(v.clone());
                }
            }
            return;
        }
        // A screencast's frame goes to the session that started it, not to every session on the page.
        if v["method"] == "Page.screencastFrame" {
            let to = self.targets.iter().find(|t| t.id == tid).and_then(|t| t.screencast.clone());
            if let Some((client, sess)) = to {
                if let Some(c) = self.clients.get(&client) {
                    let mut e = v;
                    if let Some(s) = sess {
                        e["sessionId"] = json!(s);
                    }
                    c.send(e);
                }
            }
            return;
        }
        let attached_here: Vec<u64> = self.clients.iter().filter(|(_, c)| Self::attached(c, tid)).map(|(&k, _)| k).collect();
        match v["method"].as_str().unwrap_or_default() {
            // A worker the page auto-attached: its session is relayed to this page's process.
            "Target.attachedToTarget" => {
                if let Some(s) = v["params"]["sessionId"].as_str() {
                    for k in &attached_here {
                        // a viewer has no business in a page's workers (D88 step 3)
                        if let Some(c) = self.clients.get_mut(k).filter(|c| c.viewer.is_none()) {
                            c.sessions.insert(s.to_string(), Session::Child(tid.to_string()));
                        }
                    }
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(s) = v["params"]["sessionId"].as_str() {
                    for c in self.clients.values_mut() {
                        c.sessions.remove(s);
                    }
                }
            }
            // The page's title (D88 step 2): kept for the tab, relayed to no one; a page with none is titled by its
            // URL, as Chrome titles it.
            TITLE_EVENT => {
                let title = v["params"]["title"].as_str().unwrap_or_default().to_string();
                if let Some(t) = self.target(tid) {
                    t.title = if title.is_empty() { untitled(&t.url) } else { title };
                }
                return;
            }
            // The main frame moved: the target's url (and Chrome's `targetInfoChanged`, to a discovering client) --
            // a new document, titled by its URL until it has a title, or the same document at a new URL (D88 step 2),
            // whose `targetInfoChanged` comes first, as Chrome sends it.
            "Page.frameNavigated" if v["params"]["frame"].get("parentId").is_none() => {
                let url = v["params"]["frame"]["url"].as_str().unwrap_or_default().to_string();
                if let Some(t) = self.target(tid) {
                    t.title = untitled(&url);
                    t.url = url;
                }
                self.announce_info(tid);
            }
            "Page.navigatedWithinDocument" if v["params"]["frameId"].as_str() == Some(tid) => {
                if let Some(t) = self.target(tid) {
                    t.url = v["params"]["url"].as_str().unwrap_or_default().to_string();
                }
                self.announce_info(tid);
            }
            _ => {}
        }
        let heard = crate::viewer::hears(v["method"].as_str().unwrap_or_default());
        for k in attached_here {
            let Some(c) = self.clients.get(&k) else { continue };
            // a viewer hears the page as a person's client shows it, never another client's network log or console
            if c.viewer.is_some() && !heard {
                continue;
            }
            if c.page_socket.as_deref() == Some(tid) {
                c.send(v.clone());
            }
            for (s, r) in &c.sessions {
                if matches!(r, Session::Page(t) if t == tid) {
                    let mut e = v.clone();
                    e["sessionId"] = json!(s);
                    c.send(e);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Browser {
    /// G5: idle a context -- freeze (SIGSTOP) every page process of it, so it costs no CPU while its store and KV
    /// stay put. A context id the router does not know has no processes, so it is a no-op.
    fn hibernate(&self, ctx: &str) {
        self.signal_context(ctx, 19); // SIGSTOP
    }
    /// G5: wake a hibernated context -- thaw (SIGCONT) its page processes.
    fn resume(&self, ctx: &str) {
        self.signal_context(ctx, 18); // SIGCONT
    }
    fn signal_context(&self, ctx: &str, sig: i32) {
        for t in self.targets.iter().filter(|t| t.context == ctx) {
            if let Some(pid) = t._child.as_ref().and_then(|c| c.id()) {
                // SAFETY: kill(2) on this router's own child pid, with SIGSTOP / SIGCONT.
                unsafe { kill(pid as i32, sig) };
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl Browser {
    // TODO(1021-verify): hibernate/resume freeze the page processes with SIGSTOP / SIGCONT on Linux only; on other
    // OSes they are no-ops until a per-OS suspend is added.
    fn hibernate(&self, _ctx: &str) {}
    fn resume(&self, _ctx: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_names_its_persona() {
        assert_eq!(split_route("/persona/shop/json/version"), (Some("shop".into()), "/json/version".into()));
        assert_eq!(split_route("/persona/shop"), (Some("shop".into()), String::new()));
        assert_eq!(split_route("/persona/shop/devtools/browser/x"), (Some("shop".into()), "/devtools/browser/x".into()));
        assert_eq!(split_route("/json/version"), (None, "/json/version".into()));
        assert_eq!(split_route("/persona/"), (None, "/persona/".into()));
    }

    #[test]
    fn a_context_takes_chrome_s_proxy_and_korvus_s_settings() {
        let p = json!({
            "proxyServer": "http://127.0.0.1:8080", "proxyBypassList": "localhost",
            "korvus": { "timezone": "Asia/Seoul", "plugins": ["/a.so"] },
            "korvus.locale": "ko-KR", "korvus.seed": 7,
        });
        let s = ContextSpec::from_params("C", &p);
        assert_eq!(s.proxy, Some(ProxySpec { server: "http://127.0.0.1:8080".into(), bypass: Some("localhost".into()) }));
        assert_eq!(s.timezone.as_deref(), Some("Asia/Seoul"));
        assert_eq!(s.locale.as_deref(), Some("ko-KR"));
        assert_eq!(s.seed.as_deref(), Some("7"));
        assert_eq!(s.plugins, vec!["/a.so".to_string()]);
        assert_eq!(ContextSpec::from_params("C", &json!({})), ContextSpec { context: "C".into(), ..ContextSpec::default() });
    }
}
