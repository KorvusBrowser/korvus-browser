//! `new Worker(url)`/`new SharedWorker(url)` fetch their own top script (through the
//! transport, on the worker's OWN thread -- never on the main thread, never off disk) and `importScripts`
//! actually loads. Measured end to end against a local HTTP server, exactly like the unit brief's probe:
//! `new Worker("http://127.0.0.1:PORT/w.js")` must deliver a message, not time out.
use engine::Engine;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
/// A tiny multi-route HTTP/1.1 server: each accepted connection is read once, the request line's path is
/// looked up in `routes`, and the matching body is served as `application/javascript` (404 if absent).
/// Runs until the listener is dropped.
async fn serve_routes(listener: TcpListener, routes: HashMap<&'static str, &'static str>) {
    let routes = Arc::new(routes);
    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let routes = routes.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = match sock.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");
            match routes.get(path) {
                Some(body) => {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                }
                None => {
                    let body = format!("not found: {path}");
                    let resp = format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                }
            }
            let _ = sock.flush().await;
        });
    }
}

async fn spawn_server(routes: HashMap<&'static str, &'static str>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_routes(listener, routes));
    addr
}

/// Real workers deliver cross-thread (a fetch + a spawned isolate on its own OS thread), so pump the event
/// loop repeatedly rather than a single `run()`, exactly like `worker.rs`'s own harness.
async fn pump_until_result(e: &mut Engine, deadline: std::time::Duration) -> Vec<String> {
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out
}

fn result_of(out: &[String]) -> String {
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .cloned()
        .unwrap_or_else(|| format!("RESULT:NONE (output: {out:?})"))
}

/// DONE WHEN #1 (the brief's exact probe): `w.js` posts a message; the page does
/// `new Worker("http://127.0.0.1:PORT/w.js")` and the message ARRIVES. Before this unit, the worker spawned
/// with an empty source and the page timed out waiting for `onmessage`.
#[tokio::test]
async fn worker_loads_script_from_http_url() {
    let mut routes = HashMap::new();
    routes.insert("/w.js", "self.postMessage({from:'worker', ok:true});");
    let addr = spawn_server(routes).await;

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    // the document is the server's own: a page may start a worker only from its own origin's script (Chrome 150 throws
    // a SecurityError for any other -- `tests/worker_script_loading_live.rs`)
    e.set_url(&format!("http://{addr}/"));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("load_document");
    e.eval(
        "worker-driver",
        format!(
            "const w = new Worker('http://{addr}/w.js'); \
             w.onmessage = (ev) => print('RESULT:' + ev.data.from + ':' + ev.data.ok);"
        ),
    )
    .expect("eval driver");

    let out = pump_until_result(&mut e, std::time::Duration::from_secs(10)).await;
    assert_eq!(result_of(&out), "RESULT:worker:true", "new Worker(url) must fetch + run the script: {out:?}");
}

/// DONE WHEN #2: `importScripts` works for BOTH an absolute path (`/resources/testharness.js`, resolved
/// against the server root) and a relative one (`helper.js`, resolved against the WORKER's own URL, i.e.
/// `/dir/`, not the page's) -- the two forms every generated `.any.worker.js` WPT file uses.
#[tokio::test]
async fn worker_import_scripts_absolute_and_relative() {
    let mut routes = HashMap::new();
    routes.insert(
        "/dir/w2.js",
        "importScripts('/resources/testharness.js'); \
         importScripts('helper.js'); \
         self.postMessage(globalThis.__fromAbs + ':' + globalThis.__fromRel);",
    );
    routes.insert("/resources/testharness.js", "globalThis.__fromAbs = 'abs-ok';");
    routes.insert("/dir/helper.js", "globalThis.__fromRel = 'rel-ok';");
    let addr = spawn_server(routes).await;

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    // the document is the server's own: a page may start a worker only from its own origin's script (Chrome 150 throws
    // a SecurityError for any other -- `tests/worker_script_loading_live.rs`)
    e.set_url(&format!("http://{addr}/"));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("load_document");
    e.eval(
        "worker-driver",
        format!(
            "const w = new Worker('http://{addr}/dir/w2.js'); \
             w.onmessage = (ev) => print('RESULT:' + ev.data);"
        ),
    )
    .expect("eval driver");

    let out = pump_until_result(&mut e, std::time::Duration::from_secs(10)).await;
    assert_eq!(
        result_of(&out),
        "RESULT:abs-ok:rel-ok",
        "importScripts must load both an absolute path and one relative to the WORKER's own URL: {out:?}"
    );
}

/// DONE WHEN #2 (continued): a failed `importScripts` load (404) reports an uncaught worker error rather
/// than silently continuing past it -- FORBIDDEN item 4 in the unit brief ("Making importScripts a no-op").
#[tokio::test]
async fn worker_import_scripts_404_reports_error_not_silence() {
    let mut routes = HashMap::new();
    routes.insert(
        "/w3.js",
        "try { importScripts('missing.js'); self.postMessage('no-throw'); } \
         catch (e) { self.postMessage('threw:' + e.name); }",
    );
    let addr = spawn_server(routes).await;

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    // the document is the server's own: a page may start a worker only from its own origin's script (Chrome 150 throws
    // a SecurityError for any other -- `tests/worker_script_loading_live.rs`)
    e.set_url(&format!("http://{addr}/"));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("load_document");
    e.eval(
        "worker-driver",
        format!(
            "const w = new Worker('http://{addr}/w3.js'); \
             w.onmessage = (ev) => print('RESULT:' + ev.data);"
        ),
    )
    .expect("eval driver");

    let out = pump_until_result(&mut e, std::time::Duration::from_secs(10)).await;
    assert_eq!(result_of(&out), "RESULT:threw:NetworkError", "a 404 importScripts load must throw NetworkError, not run past it: {out:?}");
}

/// DONE WHEN #3: `new SharedWorker(url)` loads too -- same URL-fetch loader, plumbed through the keyed
/// singleton isolate + `connect` event. Exercises the connect/load race guard (`worker.rs`'s
/// `CONNECT_PRELUDE`/`CONNECT_FLUSH`): the connect marker is sent essentially instantly after spawn, well
/// before the network fetch of `sw.js` can complete.
#[tokio::test]
async fn shared_worker_loads_script_from_http_url() {
    let mut routes = HashMap::new();
    routes.insert(
        "/sw.js",
        "self.onconnect = (e) => { \
           const port = e.ports[0]; \
           port.onmessage = (ev) => { port.postMessage('echo:' + ev.data); }; \
         };",
    );
    let addr = spawn_server(routes).await;

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    // the document is the server's own: a page may start a worker only from its own origin's script (Chrome 150 throws
    // a SecurityError for any other -- `tests/worker_script_loading_live.rs`)
    e.set_url(&format!("http://{addr}/"));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("load_document");
    e.eval(
        "worker-driver",
        format!(
            "const sw = new SharedWorker('http://{addr}/sw.js'); \
             sw.port.onmessage = (ev) => print('RESULT:' + ev.data); \
             sw.port.postMessage('hi');"
        ),
    )
    .expect("eval driver");

    let out = pump_until_result(&mut e, std::time::Duration::from_secs(10)).await;
    assert_eq!(result_of(&out), "RESULT:echo:hi", "new SharedWorker(url) must fetch + run the script: {out:?}");
}

/// 4.3b — `navigator.serviceWorker.register(url)` fetches and runs the script on the SW isolate's own
/// thread, and the page<->SW client channel round-trips: the page posts on `registration.installing`, the
/// SW answers through `event.source` (the Client port), and the reply arrives as a `message` event on
/// `navigator.serviceWorker`. That is exactly the handshake testharness.js's ServiceWorkerTestEnvironment
/// uses to publish a `.any.serviceworker.html` file's results, and registration used to be source-only --
/// `register()` had no URL-loading path at all, so nothing ran and every such file scored 0/0.
#[tokio::test]
async fn service_worker_loads_script_from_http_url_and_answers_its_client() {
    let mut routes = HashMap::new();
    routes.insert(
        "/swk.js",
        "self.addEventListener('message', (e) => { \
           if (e.source) e.source.postMessage('echo:' + e.data); \
         });",
    );
    let addr = spawn_server(routes).await;

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    // the document is the server's own: a page may start a worker only from its own origin's script (Chrome 150 throws
    // a SecurityError for any other -- `tests/worker_script_loading_live.rs`)
    e.set_url(&format!("http://{addr}/"));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("load_document");
    e.eval(
        "sw-driver",
        format!(
            "navigator.serviceWorker.addEventListener('message', (ev) => print('RESULT:' + ev.data)); \
             navigator.serviceWorker.register('http://{addr}/swk.js').then((reg) => {{ \
               print('TAG:' + Object.prototype.toString.call(reg.installing)); \
               reg.installing.postMessage('hi'); \
             }});"
        ),
    )
    .expect("eval driver");

    let out = pump_until_result(&mut e, std::time::Duration::from_secs(10)).await;
    assert!(
        out.iter().any(|l| l == "TAG:[object ServiceWorker]"),
        "registration.installing must be a real ServiceWorker (testharness.js identifies one by exactly this): {out:?}"
    );
    assert_eq!(result_of(&out), "RESULT:echo:hi", "the SW client channel must round-trip: {out:?}");
}
