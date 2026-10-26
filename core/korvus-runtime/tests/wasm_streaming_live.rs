//! `WebAssembly.instantiateStreaming` / `compileStreaming` as table rows.
//!
//! They exist because deno_core leaves V8's WASM streaming host callback UNSET and V8 invoking it
//! PANICS the process — an abort, not a catchable error, so one call on a page took the whole load
//! down. The bodies were two `async function` expressions in the kernel's `BOOTSTRAP` string wearing
//! a `KB.__markNative` mask; they are Rust now. What has to keep holding:
//!
//!   * both are `[native code]`, with Chrome's measured name/length/descriptor;
//!   * a real module compiles and instantiates through a `Response`-shaped source, a bare
//!     `BufferSource` and a promise of one;
//!   * a bad module REJECTS rather than aborting the process, which is the whole point;
//!   * **and the WORKER realm answers the same, because it runs the same rows**.
//!
//! # The worker half, and why the realm is the interesting axis
//!
//! The unset host callback is a property of a `JsRuntime`, and a worker isolate is one — so the
//! abort was never a window-realm defect. It was a window-realm FIX: `engine.rs` held the only call
//! site, and every worker still ran V8's own streaming path. Measured 2026-09-20 on `master`
//! (`68db6e64`), one page whose only content is `new Worker(...)` and one worker that calls
//! `WebAssembly.compileStreaming`:
//!
//! ```text
//! korvus fetch <page with window-realm call only>   exit 0
//! korvus fetch <page with worker-realm call only>   exit 134 (SIGABRT)
//!     thread '<unnamed>' panicked at deno_core-0.410.0/ops_builtin_v8.rs:1396:5:
//!     wasm streaming callback invoked before the JS handler was set
//! ```
//!
//! That is the OPENS row this came from: the page's wasm runtime instantiates inside a worker,
//! korvus aborted, and the harness recorded `nodes: null` — no result at all, not a wrong one.
//! (Which row is the harness's business, not the engine's — R3; the commit message names it.)
//!
//! **So the assertion here is realm SYMMETRY, run as one probe text in two realms.** Chrome 150 is
//! the reason it is symmetry and not two separate expectations: evaluated in its window and in a
//! dedicated worker off the same page, its answers are byte-identical on all fifteen shape fields
//! (`{enumerable, configurable, writable}` all true, length 1, the member's own name, `[native
//! code]`, no own `prototype`/`arguments`/`caller`), and both realms compile, instantiate and reject
//! alike. A realm test that asserted only "the worker has the names" would pass on the shells that
//! abort when called, which is the failure mode this whole module exists to delete.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The smallest valid WebAssembly module: the 8-byte header (magic + version 1), which `compile`
/// accepts and `instantiate` gives an empty instance for.
const EMPTY_MODULE: &str = "new Uint8Array([0,0x61,0x73,0x6d, 1,0,0,0])";

#[tokio::test]
async fn the_streaming_forms_are_native_with_chromes_shape() {
    let out = probe(
        r#"
        const d = (n) => { const x = Object.getOwnPropertyDescriptor(WebAssembly, n);
            return x ? [x.enumerable, x.configurable, x.writable, x.value.length, x.value.name].join(",") : "absent"; };
        const isNative = (f) => Function.prototype.toString.call(f).includes("[native code]");
        // Measured in headless Chrome 141, 2026-09-12: both are
        // {enumerable: true, configurable: true, writable: true}, length 1, named for the member.
        print("instantiateStreaming=" + d("instantiateStreaming"));
        print("compileStreaming=" + d("compileStreaming"));
        print("native=" + isNative(WebAssembly.instantiateStreaming) + " " + isNative(WebAssembly.compileStreaming));
        // A native function carries no own prototype/arguments/caller -- the JS-shim tell.
        const tell = (f) => ["prototype", "arguments", "caller"].some((k) => Object.prototype.hasOwnProperty.call(f, k));
        print("tell=" + tell(WebAssembly.instantiateStreaming) + " " + tell(WebAssembly.compileStreaming));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "instantiateStreaming=true,true,true,1,instantiateStreaming",
            "compileStreaming=true,true,true,1,compileStreaming",
            "native=true true",
            "tell=false false",
        ]
    );
}

#[tokio::test]
async fn a_response_shaped_source_a_buffer_and_a_promise_all_compile() {
    let out = probe(&format!(
        r#"
        const bytes = {EMPTY_MODULE};
        const asResponse = {{ arrayBuffer: () => Promise.resolve(bytes.buffer) }};
        const say = (label, p) => p.then(
            (v) => print(label + "=" + Object.prototype.toString.call(v)),
            (e) => print(label + "=rejected " + e.constructor.name));
        say("responseCompile", WebAssembly.compileStreaming(asResponse));
        say("bufferCompile", WebAssembly.compileStreaming(bytes));
        say("promiseCompile", WebAssembly.compileStreaming(Promise.resolve(asResponse)));
        say("instantiate", WebAssembly.instantiateStreaming(asResponse).then((r) => r.instance));
        "#
    ))
    .await;
    let mut out = out;
    out.sort();
    assert_eq!(
        out,
        vec![
            "bufferCompile=[object WebAssembly.Module]",
            "instantiate=[object WebAssembly.Instance]",
            "promiseCompile=[object WebAssembly.Module]",
            "responseCompile=[object WebAssembly.Module]",
        ]
    );
}

/// THE WHOLE REASON THE MEMBERS EXIST: a bad module must REJECT, and the process must survive. V8's
/// own streaming path aborts here, because deno_core never set the host callback.
#[tokio::test]
async fn a_bad_module_rejects_and_the_engine_survives() {
    let out = probe(
        r#"
        const bad = { arrayBuffer: () => Promise.resolve(new Uint8Array([1,2,3,4]).buffer) };
        WebAssembly.compileStreaming(bad).then(
            () => print("compile=resolved"),
            (e) => print("compile=rejected " + (e instanceof WebAssembly.CompileError)));
        WebAssembly.instantiateStreaming(Promise.reject(new TypeError("network"))).then(
            () => print("fetchFailed=resolved"),
            (e) => print("fetchFailed=rejected " + e.constructor.name + ": " + e.message));
        WebAssembly.instantiateStreaming({ arrayBuffer: () => { throw new RangeError("boom"); } }).then(
            () => print("throwingSource=resolved"),
            (e) => print("throwingSource=rejected " + e.constructor.name));
        print("alive=yes");
        "#,
    )
    .await;
    let mut out = out;
    out.sort();
    assert_eq!(
        out,
        vec![
            "alive=yes",
            "compile=rejected true",
            "fetchFailed=rejected TypeError: network",
            "throwingSource=rejected RangeError",
        ]
    );
}

// ── The two realms, one probe text ───────────────────────────────────────────────────

/// THE probe, and it is one string on purpose: a symmetry test that evaluated two different scripts
/// would be comparing two measurements rather than one realm against another. It is an expression
/// evaluating to a `Promise<string>`; each realm's harness below only chooses how to report it.
///
/// Nothing in it touches the network — the sources are the `Response`-shaped object, the bare
/// `BufferSource` and the promise of one that the window's own tests above use, so this measures the
/// rows and not a fetch stack that differs between realms for other reasons.
const REALM_PROBE: &str = r#"(async () => {
    const parts = [];
    const WA = WebAssembly;
    for (const n of ["compileStreaming", "instantiateStreaming"]) {
        const d = Object.getOwnPropertyDescriptor(WA, n);
        parts.push(n + "=" + (d ? [
            d.enumerable, d.configurable, d.writable, d.value.length, d.value.name,
            Function.prototype.toString.call(d.value).includes("[native code]"),
            // A native function carries no own prototype/arguments/caller -- the JS-shim tell.
            ["prototype", "arguments", "caller"].some((k) => Object.prototype.hasOwnProperty.call(d.value, k)),
        ].join(",") : "absent"));
    }
    const bytes = new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]);
    const resp = () => ({ arrayBuffer: () => Promise.resolve(bytes.buffer) });
    const say = async (label, thunk) => {
        try { parts.push(label + "=" + Object.prototype.toString.call(await thunk())); }
        catch (e) { parts.push(label + "=rejected " + (e && e.constructor ? e.constructor.name : typeof e)); }
    };
    await say("responseCompile", () => WA.compileStreaming(resp()));
    await say("bufferCompile", () => WA.compileStreaming(bytes));
    await say("promiseCompile", () => WA.compileStreaming(Promise.resolve(resp())));
    await say("instantiate", () => WA.instantiateStreaming(resp()).then((r) => r.instance));
    await say("badModule", () => WA.compileStreaming({ arrayBuffer: () => Promise.resolve(new Uint8Array([1,2,3,4]).buffer) }));
    await say("rejectedSource", () => WA.instantiateStreaming(Promise.reject(new TypeError("network"))));
    await say("throwingSource", () => WA.instantiateStreaming({ arrayBuffer: () => { throw new RangeError("boom"); } }));
    parts.push("alive=yes");
    return parts.join(" | ");
})()"#;

/// [`REALM_PROBE`] in the WINDOW realm, through the same [`probe`] harness the tests above use.
async fn window_realm(probe_src: &str) -> String {
    pick(&probe(&format!("{probe_src}.then((s) => print('RESULT:' + s));")).await)
}

/// [`REALM_PROBE`] in a DEDICATED WORKER, posted back to the page. The spawn shape is
/// `dom_rect_worker_live.rs`'s: register the script text, `new Worker`, pump until it answers.
async fn worker_realm(probe_src: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let body = format!("{probe_src}.then((s) => postMessage(s), (err) => postMessage('THREW ' + err));");
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(&body).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(30) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    pick(&out)
}

fn pick(out: &[String]) -> String {
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NO RESULT (output: {out:?})"))
}

/// What both realms must say. Every row is derived from what Chrome 150 DOES rather than copied off
/// it: the seven shape fields are the ones Chrome answers IDENTICALLY in both of its realms, and the
/// seven behaviour rows are "the streaming form is sugar for fetch-then-compile" — which is why a
/// `Response`-shaped source, a `BufferSource` and a promise of either all reach the module, and why
/// every failure arrives as a REJECTION of the promise the member already returned rather than as a
/// synchronous throw.
///
/// `alive=yes` is the row the whole file exists for: reaching it means the process is still here.
const EXPECTED: &str = "compileStreaming=true,true,true,1,compileStreaming,true,false | \
instantiateStreaming=true,true,true,1,instantiateStreaming,true,false | \
responseCompile=[object WebAssembly.Module] | \
bufferCompile=[object WebAssembly.Module] | \
promiseCompile=[object WebAssembly.Module] | \
instantiate=[object WebAssembly.Instance] | \
badModule=rejected CompileError | \
rejectedSource=rejected TypeError | \
throwingSource=rejected RangeError | \
alive=yes";

/// THE REGRESSION TEST. On `master` this does not report red, it ABORTS: the worker reaches V8's own
/// streaming path, deno_core's unset host callback panics, and that panic cannot unwind out of the
/// worker thread ("failed to initiate panic, error 5, aborting"), so the test binary dies with
/// SIGABRT. That is the defect stated precisely, and it is why it is worth a test of its own: a page
/// does not get a wrong row out of this, it gets no row at all.
#[tokio::test]
async fn the_worker_realm_answers_exactly_what_the_window_realm_answers() {
    let window = window_realm(REALM_PROBE).await;
    let worker = worker_realm(REALM_PROBE).await;
    assert_eq!(
        window, worker,
        "the two realms disagree about WebAssembly streaming:\n  window {window}\n  worker {worker}"
    );
    // ... and both are RIGHT. Symmetry alone would pass with the two realms broken the same way.
    assert_eq!(window, EXPECTED, "the window realm's answer moved");
}

/// The narrowest statement of the defect: a worker that touches the streaming form at all must leave
/// the process alive. Separate from the symmetry test because this is the part that is NOT about
/// shape — a realm check that only read the descriptors passes on V8's own members, which carry
/// Chrome's exact shape and abort when called.
#[tokio::test]
async fn a_worker_that_calls_the_streaming_form_leaves_the_process_alive() {
    let out = worker_realm(
        r#"(async () => {
            const bytes = new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]);
            const m = await WebAssembly.compileStreaming({ arrayBuffer: () => Promise.resolve(bytes.buffer) });
            return "alive " + Object.prototype.toString.call(m);
        })()"#,
    )
    .await;
    assert_eq!(out, "alive [object WebAssembly.Module]");
}
