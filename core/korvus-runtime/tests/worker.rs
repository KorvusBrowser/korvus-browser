//! Real multi-isolate Web Worker platform. This suite drives worker behaviour end-to-end
//! (spawn, structured-clone messaging, lifecycle) and grows one DoD at a time.
//!
//! W1.0 establishes the harness against the CURRENT same-isolate `with(ws)` sim (`host.js` `class
//! Worker`). W1.1+ replace that with a real V8-isolate-per-worker; these tests must stay green across
//! the swap.
use engine::Engine;
const ECHO_WORKER: &str = include_str!("fixtures/worker/echo-worker.js");
const PORT_WORKER: &str = include_str!("fixtures/worker/port-worker.js");
const PORT_REFUSAL_WORKER: &str = include_str!("fixtures/worker/port-transfer-refusal-worker.js");
const SHARED_WORKER: &str = include_str!("fixtures/worker/shared-worker.js");
const SERVICE_WORKER: &str = include_str!("fixtures/worker/service-worker.js");
const CACHE_WORKER: &str = include_str!("fixtures/worker/cache-worker.js");
const FETCH_WORKER: &str = include_str!("fixtures/worker/fetch-worker.js");
const CLIENTS_WORKER: &str = include_str!("fixtures/worker/clients-worker.js");

/// Build an engine, register worker scripts by URL (the `__workerScripts` seam), eval a driver, and
/// drain the event loop. The driver prints `RESULT:<...>`; return the collected print output.
async fn run_worker(scripts: &[(&str, &str)], driver: &str) -> Result<Vec<String>, String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.map_err(|x| x.to_string())?;
    e.load_document("<!doctype html><html><head></head><body></body></html>")
        .map_err(|x| x.to_string())?;
    let entries: Vec<String> = scripts
        .iter()
        .map(|(url, src)| {
            format!(
                "{}: {}",
                serde_json::to_string(url).unwrap(),
                serde_json::to_string(src).unwrap()
            )
        })
        .collect();
    e.eval(
        "worker-scripts",
        format!("globalThis.__workerScripts = {{ {} }};", entries.join(", ")),
    )
    .map_err(|x| x.to_string())?;
    e.run().await.map_err(|x| x.to_string())?;
    e.eval("worker-driver", driver.to_string()).map_err(|x| x.to_string())?;
    // Real workers deliver cross-thread, so pump until RESULT: (or a deadline) rather than a single run().
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Ok(out)
}

fn result_of(out: &[String]) -> String {
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .cloned()
        .unwrap_or_else(|| format!("RESULT:NONE (output: {out:?})"))
}

/// W1.0 — harness smoke: a worker echoes `data + 1` back to the page (bidirectional postMessage).
/// Establishes the test harness; passes on the current same-isolate sim and must stay green as W1.1+
/// swap in the real isolate.
#[tokio::test]
async fn worker_echo_smoke() {
    let out = run_worker(
        &[("./echo-worker.js", ECHO_WORKER)],
        "const w = new Worker('./echo-worker.js'); \
         w.onmessage = (e) => print('RESULT:' + e.data); \
         w.postMessage(41);",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:42", "worker echo smoke failed: {out:?}");
}

/// W3b — transfer a MessageChannel port to a real worker isolate and round-trip a message across the boundary.
/// The page keeps `port1`; `port2` is transferred (its endpoint re-homes into the worker as `e.ports[0]`).
/// A `port1.postMessage` reaches the worker's port; the worker relays it back over the same port to `port1`.
#[tokio::test]
async fn worker_transfer_message_port() {
    let out = run_worker(
        &[("./port-worker.js", PORT_WORKER)],
        "const w = new Worker('./port-worker.js'); \
         const mc = new MessageChannel(); \
         mc.port1.onmessage = (e) => print('RESULT:' + e.data); \
         w.postMessage('init', [mc.port2]); \
         mc.port1.postMessage('hello');",
    )
    .await
    .expect("engine ran");
    assert_eq!(
        result_of(&out),
        "RESULT:worker-port-got:hello",
        "cross-isolate MessagePort transfer failed: {out:?}"
    );
}

/// A port cannot be transferred THROUGH a cross-isolate port, and the engine says so
/// instead of dropping it.
///
/// This is the one half of `MessagePort.postMessage`'s transfer list that MP1 did not implement, and
/// it is pinned here so the limit is a measured behaviour rather than a comment. The frame a remote
/// port's post rides (`[u32 channel_id][structured-clone bytes]`) has no slot for a transferred port,
/// and a worker isolate has no allocator for a new channel id -- main owns that counter. A silent
/// drop is the thing the unit contract calls a shape a caller can reach through: the page would post
/// a port, get no error, and read `undefined.postMessage` a turn later, which is exactly the live
/// cluster MP1 was sent after. So it throws.
///
/// The test also pins what the refusal must NOT do: it must not neuter the port it refused, and it
/// must not refuse an ArrayBuffer, which rides the same frame perfectly well.
#[tokio::test]
async fn a_port_is_refused_through_a_cross_isolate_port() {
    let out = run_worker(
        &[("./port-refusal-worker.js", PORT_REFUSAL_WORKER)],
        "const w = new Worker('./port-refusal-worker.js'); \
         w.onmessage = (e) => print('RESULT:' + e.data); \
         const mc = new MessageChannel(); \
         w.postMessage('init', [mc.port2]);",
    )
    .await
    .expect("engine ran");
    assert_eq!(
        result_of(&out),
        "RESULT:port: DataCloneError: Failed to execute 'postMessage' on 'MessagePort': a \
         MessagePort cannot be transferred through a port whose other end is in another agent. | \
         refused port still usable: yes | buffer: byteLength 0",
        "the cross-isolate refusal is not what it says it is: {out:?}"
    );
}

/// And the PAGE side of the same channel is a remote port too, so it refuses the same
/// way. `cb_worker_post` registers the transferred port's PEER (the one the page kept) as the
/// page-side endpoint of the new cross-isolate channel, which is what makes this the other half of
/// the test above rather than a repeat of it.
#[tokio::test]
async fn the_page_side_of_a_worker_channel_refuses_a_port_too() {
    let out = run_worker(
        &[("./port-worker.js", PORT_WORKER)],
        "const w = new Worker('./port-worker.js'); \
         const mc = new MessageChannel(); \
         w.postMessage('init', [mc.port2]); \
         const ch = new MessageChannel(); \
         let r; \
         try { mc.port1.postMessage('x', [ch.port1]); r = 'NO THROW'; } \
         catch (e) { r = e.name + ': ' + e.message; } \
         print('RESULT:' + r);",
    )
    .await
    .expect("engine ran");
    assert_eq!(
        result_of(&out),
        "RESULT:DataCloneError: Failed to execute 'postMessage' on 'MessagePort': a MessagePort \
         cannot be transferred through a port whose other end is in another agent.",
        "the page side of a cross-isolate channel did not refuse: {out:?}"
    );
}

/// W4 — a SharedWorker isolate fires `connect` with a working `MessagePort`; the page's `.port` round-trips
/// a message through the shared worker's connection handler.
#[tokio::test]
async fn shared_worker_connect_and_message() {
    let out = run_worker(
        &[("./shared-worker.js", SHARED_WORKER)],
        "const sw = new SharedWorker('./shared-worker.js', 'w1'); \
         sw.port.onmessage = (e) => print('RESULT:' + e.data); \
         sw.port.postMessage('hi');",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:conn1:hi", "SharedWorker connect/message failed: {out:?}");
}

/// 4.3b — the three worker realms are three DIFFERENT global scopes, and the difference is what
/// testharness.js branches on: `create_test_environment` tests `instanceof DedicatedWorkerGlobalScope`
/// FIRST, so a shared or service worker wearing the dedicated scope picks the dedicated environment and
/// publishes its results down a plane its page holds no end of. Each realm must therefore report its own
/// interface, must NOT expose `DedicatedWorkerGlobalScope`, and must carry the members of the scope it
/// claims to be (`onconnect` for shared; a dedicated `postMessage` for neither).
#[tokio::test]
async fn worker_realms_are_their_own_global_scopes() {
    let probe = "const R = (tag) => tag + '|' + Object.prototype.toString.call(self) \
        + '|dwgs=' + (typeof DedicatedWorkerGlobalScope) \
        + '|post=' + (typeof self.postMessage) \
        + '|onconnect=' + ('onconnect' in self);";
    // Dedicated: unchanged -- DedicatedWorkerGlobalScope, and it keeps postMessage.
    let out = run_worker(
        &[("./probe.js", &format!("{probe} self.postMessage(R('dedicated'));"))],
        "const w = new Worker('./probe.js'); w.onmessage = (e) => print('RESULT:' + e.data);",
    )
    .await
    .expect("engine ran");
    assert_eq!(
        result_of(&out),
        "RESULT:dedicated|[object DedicatedWorkerGlobalScope]|dwgs=function|post=function|onconnect=false",
        "the dedicated realm must be unchanged: {out:?}"
    );

    // Shared: SharedWorkerGlobalScope, no DedicatedWorkerGlobalScope, no dedicated postMessage, onconnect.
    let out = run_worker(
        &[(
            "./probe.js",
            &format!("{probe} self.onconnect = (e) => e.ports[0].postMessage(R('shared'));"),
        )],
        "const sw = new SharedWorker('./probe.js', 'realm'); \
         sw.port.onmessage = (e) => print('RESULT:' + e.data); sw.port.postMessage('go');",
    )
    .await
    .expect("engine ran");
    assert_eq!(
        result_of(&out),
        "RESULT:shared|[object SharedWorkerGlobalScope]|dwgs=undefined|post=undefined|onconnect=true",
        "a SharedWorker isolate must BE a SharedWorkerGlobalScope: {out:?}"
    );

    // Service: ServiceWorkerGlobalScope, reached over the client port a page postMessage opens.
    let out = run_worker(
        &[(
            "./probe.js",
            &format!("{probe} self.addEventListener('message', (e) => e.source.postMessage(R('service')));"),
        )],
        "navigator.serviceWorker.addEventListener('message', (e) => print('RESULT:' + e.data)); \
         navigator.serviceWorker.register('./probe.js').then((reg) => reg.installing.postMessage('go'));",
    )
    .await
    .expect("engine ran");
    assert_eq!(
        result_of(&out),
        "RESULT:service|[object ServiceWorkerGlobalScope]|dwgs=undefined|post=undefined|onconnect=false",
        "a registered ServiceWorker must BE a ServiceWorkerGlobalScope: {out:?}"
    );
}

/// 4.3b — `SharedWorkerGlobalScope.name` is the name the page constructed the worker with, not the empty
/// string the dedicated scope reports.
#[tokio::test]
async fn shared_worker_scope_carries_its_name() {
    let out = run_worker(
        &[("./named.js", "self.onconnect = (e) => e.ports[0].postMessage(self.name);")],
        "const sw = new SharedWorker('./named.js', 'the-name'); \
         sw.port.onmessage = (e) => print('RESULT:' + e.data); sw.port.postMessage('go');",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:the-name", "SharedWorkerGlobalScope.name: {out:?}");
}

/// W4 — two SharedWorker handles to the SAME name share ONE isolate: the fixture's module-level connection
/// counter yields conn1 + conn2 (a per-handle isolate would give conn1 twice).
#[tokio::test]
async fn shared_worker_keyed_singleton() {
    let out = run_worker(
        &[("./shared-worker.js", SHARED_WORKER)],
        "const a = new SharedWorker('./shared-worker.js', 'shared'); \
         const b = new SharedWorker('./shared-worker.js', 'shared'); \
         const got = []; \
         const check = (e) => { got.push(e.data); if (got.length === 2) print('RESULT:' + got.slice().sort().join(',')); }; \
         a.port.onmessage = check; b.port.onmessage = check; \
         a.port.postMessage('x'); b.port.postMessage('x');",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:conn1:x,conn2:x", "SharedWorker keyed singleton failed: {out:?}");
}

/// W5.1 — navigator.serviceWorker.register spawns a REAL ServiceWorker isolate that runs its install ->
/// activate lifecycle; the SW signals each phase to the page over the cross-isolate BroadcastChannel bridge.
#[tokio::test]
async fn service_worker_register_and_activate() {
    let out = run_worker(
        &[("./sw.js", SERVICE_WORKER)],
        "const bc = new BroadcastChannel('sw-status'); \
         const got = []; \
         bc.onmessage = (e) => { got.push(e.data); if (got.length === 2) print('RESULT:' + got.join(',')); }; \
         navigator.serviceWorker.register('./sw.js');",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:installed,activated", "SW register/lifecycle failed: {out:?}");
}

/// W5.3 — the native Cache API (caches.open / cache.put / cache.match) round-trips a Response body through
/// the per-isolate cache store inside a worker.
#[tokio::test]
async fn cache_api_put_match() {
    let out = run_worker(
        &[("./cache-worker.js", CACHE_WORKER)],
        "const w = new Worker('./cache-worker.js'); w.onmessage = (e) => print('RESULT:' + e.data);",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:cache:cached-hello", "Cache API put/match failed: {out:?}");
}

/// W5.2 — a controlling ServiceWorker intercepts the page's fetch via its `fetch` event and serves a
/// synthetic Response (respondWith); the page's fetch resolves with the SW's body, not the network's.
#[tokio::test]
async fn service_worker_fetch_interception() {
    let out = run_worker(
        &[("./fetch-worker.js", FETCH_WORKER)],
        "const bc = new BroadcastChannel('sw-status'); \
         bc.onmessage = (e) => { if (e.data === 'ready') { \
           fetch('/data').then((res) => res.text()).then((body) => print('RESULT:' + body)); \
         } }; \
         navigator.serviceWorker.register('./fetch-worker.js');",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:from-sw:/data", "SW fetch interception failed: {out:?}");
}

/// W5.4 — the SW reaches its controlled client via clients.matchAll() + Client.postMessage; the page
/// receives it on navigator.serviceWorker.onmessage.
#[tokio::test]
async fn service_worker_clients_postmessage() {
    let out = run_worker(
        &[("./clients-worker.js", CLIENTS_WORKER)],
        "navigator.serviceWorker.onmessage = (e) => print('RESULT:' + e.data); \
         navigator.serviceWorker.register('./clients-worker.js');",
    )
    .await
    .expect("engine ran");
    assert_eq!(result_of(&out), "RESULT:hello-from-sw", "SW clients.postMessage failed: {out:?}");
}
