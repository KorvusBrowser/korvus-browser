//! LIVE one-table gate for the Media Source Extensions pair: `MediaSource` and
//! `SourceBufferList`, in BOTH realms.
//!
//! `MediaSource` is this tree's first UNION TABLE, and that is what most of this file exists to
//! prove. The capture's two surfaces disagree about the interface: `worker.surface.json` records a
//! 13th prototype member, `handle`, that `surface.json` does not. A table's NAME removes an interface
//! from both generated registries, so ONE table serves both realms -- it carries the worker's list and
//! the window install passes a member selector. If the selector were wrong in either direction the
//! window would grow a member Chrome does not have there, or the worker would lose one it does.
//!
//!   1. **Byte-identical prototypes** against the capture, `constructor` last -- the WINDOW list for
//!      the window, and the WORKER list (`handle` included, in its recorded position) for the worker.
//!   2. **Every recorded oracle case**, read from the bundle at RUNTIME (59 across the two).
//!   3. **The chains, the mint sites and `[native code]`** -- failure mode 1.
//!   4. **The receiver checks** -- failure mode 2.
//!   5. **No instance tell.**
//!   6. **The readyState state machine**: that every member the spec gates on `open` refuses, that the
//!      two argument checks run in the order the algorithm gives them, and that the two buffer lists
//!      are stable and distinct -- failure mode 3, a body that answers without the spec's checks.
//!   7. **`isTypeSupported` answers from the recorded byte-stream table**, not a constant.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

const IFACES: &[&str] = &["MediaSource", "SourceBufferList"];

/// No row of either interface is a D2 stub: every one of the seventeen has a Rust body.
const STUBS: &[&str] = &[];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

const MINTS: &[(&str, &str)] = &[
    ("MediaSource", "new MediaSource()"),
    ("SourceBufferList", "new MediaSource().sourceBuffers"),
];

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle(which: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact(which)).unwrap()).unwrap()
}

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        "value" => format!(
            "{n}:data:e={}:c={}:w=0",
            m["enumerable"].as_bool().unwrap() as u8,
            m["configurable"].as_bool().unwrap() as u8
        ),
        k => panic!("unexpected kind {k}"),
    }
}

/// The recorded prototype of `iface` in the given capture (`surface` = window, `worker-surface` =
/// worker), with `constructor` appended last.
fn expected_proto(which: &str, iface: &str) -> Vec<String> {
    let fx = bundle(which);
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the {which} surface"))
        .clone();
    let mut out: Vec<String> = members.iter().map(describe_line).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// Spawn a worker running `body` and return what it posts back.
async fn worker_probe(body: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document(PAGE).unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(body).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("driver", "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into())
        .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
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
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NONE (output: {out:?})"))
}

// -- 1. the two prototypes, in both realms ---------------------------------------------------------

#[tokio::test]
async fn every_mse_prototype_is_byte_identical_to_the_window_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "mse-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto("surface", iface), "{iface}.prototype != the window capture");
    }
}

/// THE POINT OF THE UNION TABLE. The worker's `MediaSource.prototype` must carry `handle` -- in the
/// recorded position, after `setLiveSeekableRange` -- while the window's must not.
#[tokio::test]
async fn every_mse_prototype_is_byte_identical_to_the_worker_capture() {
    let body = format!(
        r#"{DESCRIBE}
        self.postMessage([desc(MediaSource.prototype), desc(SourceBufferList.prototype)].join("\n--\n"));"#
    );
    let got = worker_probe(&body).await;
    let want = format!(
        "{}\n--\n{}",
        expected_proto("worker-surface", "MediaSource").join("\n"),
        expected_proto("worker-surface", "SourceBufferList").join("\n")
    );
    assert_eq!(got, want, "the worker MSE prototypes != the worker capture");
}

// -- 2. every recorded oracle case -----------------------------------------------------------------

#[tokio::test]
async fn every_mse_oracle_case_answers_what_the_capture_recorded() {
    let mut e = page().await;
    let dir = persona::shipped_artifact("oracle");
    let mut checked = 0usize;
    for iface in IFACES {
        let raw = std::fs::read_to_string(dir.join(format!("{iface}.json"))).unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        for c in &cases {
            let expr = c["expr"].as_str().unwrap();
            let want = if let Some(t) = c["throws"].as_str() {
                format!("T{t}")
            } else if c["undefined"].as_bool() == Some(true) {
                "U".to_string()
            } else {
                format!("V{}", serde_json::to_string(&c["expected"]).unwrap())
            };
            let src = format!(
                r#"(() => {{ try {{ const v = ({expr});
                    return v === undefined ? "U" : "V" + JSON.stringify(v);
                }} catch (err) {{ return "T" + (err && err.name ? err.name : String(err)); }} }})()"#
            );
            let got = str_of(&mut e, "mse-oracle", src);
            assert_eq!(got, want, "{iface}.{} -- {expr}", c["member"].as_str().unwrap());
            checked += 1;
        }
    }
    assert!(checked >= 59, "only {checked} oracle cases ran for the MSE pair");
}

// -- 3. the chains, the mint sites and [native code] -----------------------------------------------

/// Failure mode 1: an instance on some OTHER prototype than the table's. `sourceBuffers` is the mint
/// site a page cannot reach directly -- a `SourceBufferList` is only ever minted through a
/// `MediaSource` -- so it is the one that would land on the wrong prototype unnoticed.
#[tokio::test]
async fn every_mse_mint_lands_on_the_tables_prototype() {
    let mut e = page().await;
    let mut lines = Vec::new();
    let mut want = Vec::new();
    for (iface, expr) in MINTS {
        lines.push(format!(
            r#"out.push("{iface}:" + (Object.getPrototypeOf({expr}) === {iface}.prototype)
                + ":" + Object.prototype.toString.call({expr})
                + ":" + (Object.getPrototypeOf({iface}.prototype) === EventTarget.prototype)
                + ":" + ({iface}.prototype.constructor === {iface})
                + ":" + (Object.getPrototypeOf({iface}) === EventTarget));"#
        ));
        want.push(format!("{iface}:true:[object {iface}]:true:true:true"));
    }
    // Both lists are real EventTargets, and both are STABLE and DISTINCT.
    lines.push(
        r#"const ms = new MediaSource();
           out.push("stable:" + (ms.sourceBuffers === ms.sourceBuffers)
                + ":" + (ms.activeSourceBuffers === ms.activeSourceBuffers)
                + ":" + (ms.sourceBuffers !== ms.activeSourceBuffers)
                + ":" + (new MediaSource().sourceBuffers !== ms.sourceBuffers));
           let fired = 0;
           ms.addEventListener("sourceopen", () => { fired++; });
           ms.dispatchEvent(new Event("sourceopen"));
           out.push("et:" + fired);"#
            .to_string(),
    );
    want.push("stable:true:true:true:true".to_string());
    want.push("et:1".to_string());
    let body = lines.join("\n");
    let got = str_of(&mut e, "mse-mint", format!(r#"(() => {{ const out = []; {body} return out.join("\n"); }})()"#));
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

#[tokio::test]
async fn every_mse_member_reports_native_code() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "mse-native",
        format!(
            r#"(() => {{
            const bad = [];
            const nat = (f) => /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f));
            for (const iface of [{list}]) {{
                const C = globalThis[iface];
                if (!nat(C)) bad.push(iface + ".ctor");
                for (const o of [C, C.prototype]) {{
                    for (const n of Object.getOwnPropertyNames(o)) {{
                        if (n === "prototype" || n === "length" || n === "name") continue;
                        const d = Object.getOwnPropertyDescriptor(o, n);
                        for (const f of [d.get, d.set, typeof d.value === "function" ? d.value : null]) {{
                            if (f && !nat(f)) bad.push(iface + "." + n);
                        }}
                    }}
                }}
            }}
            return bad.join(",");
        }})()"#
        ),
    );
    assert_eq!(got, "");
}

// -- 4. the receiver checks ------------------------------------------------------------------------

/// Failure mode 2. Two exclusions, both engine-wide and both named the way the earlier lanes named
/// theirs: the shared `event_handler` kit (an `on*` accessor reads the EventTarget handler store and
/// answers for any receiver, in every table in the tree), and a `required > 0` row whose arity guard
/// runs BEFORE the body -- so each probe passes an argument.
#[tokio::test]
async fn every_mse_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let stubs = STUBS.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "mse-illegal",
        format!(
            r#"(() => {{
            const bad = [];
            const stubs = new Set([{stubs}]);
            for (const iface of [{list}]) {{
                const P = globalThis[iface].prototype;
                for (const recv of [P, Object.create(P), {{}}]) {{
                    for (const n of Object.getOwnPropertyNames(P)) {{
                        if (n === "constructor" || n.startsWith("on") || stubs.has(iface + "." + n)) continue;
                        const d = Object.getOwnPropertyDescriptor(P, n);
                        const probes = [];
                        if (d.get) probes.push(() => d.get.call(recv));
                        if (d.set) probes.push(() => d.set.call(recv, 1));
                        if (typeof d.value === "function") probes.push(() => d.value.call(recv, 1, 1));
                        for (const p of probes) {{
                            let answer = "no-throw";
                            try {{ const r = p(); if (r && typeof r.then === "function") {{ r.catch(() => {{}}); }} }}
                            catch (err) {{ answer = err.constructor.name + ":" + err.message; }}
                            if (answer !== "TypeError:Illegal invocation") bad.push(iface + "." + n + "=" + answer);
                        }}
                    }}
                }}
            }}
            return bad.join("\n");
        }})()"#
        ),
    );
    assert_eq!(got, "");
}

// -- 5. no instance tell ---------------------------------------------------------------------------

#[tokio::test]
async fn no_mse_instance_carries_an_own_property() {
    let mut e = page().await;
    let mut lines = Vec::new();
    let mut want = Vec::new();
    for (iface, expr) in MINTS {
        lines.push(format!(
            r#"out.push("{iface}:" + JSON.stringify(Object.getOwnPropertyNames({expr})) + ":" + Object.getOwnPropertySymbols({expr}).length);"#
        ));
        want.push(format!("{iface}:[]:0"));
    }
    let body = lines.join("\n");
    let got = str_of(&mut e, "mse-own", format!(r#"(() => {{ const out = []; {body} return out.join("\n"); }})()"#));
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

// -- 6. the readyState state machine ---------------------------------------------------------------

/// Failure mode 3: a body reachable where it used to be a silent no-op, without the spec's checks.
/// Every member gated on `open` must refuse a `closed` source, the `duration` setter's TWO checks must
/// run in the algorithm's order (the value check FIRST, so `o.duration = -1` is the TypeError and
/// `o.duration = "1"` reaches the InvalidStateError), and `addSourceBuffer` must reject an EMPTY type
/// differently from an unsupported one.
#[tokio::test]
async fn a_closed_media_source_refuses_every_gated_member() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "mse-closed",
        r#"(() => {
            const out = [];
            const o = new MediaSource();
            out.push("state:" + o.readyState + "|" + String(o.duration) + "|" + o.sourceBuffers.length
                     + "|" + o.activeSourceBuffers.length);
            const t = (label, fn) => {
                try { const v = fn(); out.push(label + ":none:" + String(v)); }
                catch (err) { out.push(label + ":" + err.name + ":" + err.message); }
            };
            t("dur-1", () => { o.duration = 1; });
            t("dur-str", () => { o.duration = "1"; });
            t("dur-neg", () => { o.duration = -1; });
            t("dur-nan", () => { o.duration = "x"; });
            t("eos", () => o.endOfStream());
            t("slsr", () => o.setLiveSeekableRange(1, 2));
            t("clsr", () => o.clearLiveSeekableRange());
            t("asb-empty", () => o.addSourceBuffer(""));
            t("asb-x", () => o.addSourceBuffer("x"));
            t("rsb", () => o.removeSourceBuffer(null));
            out.push("after:" + o.readyState + "|" + String(o.duration));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "state:closed|NaN|0|0",
            "dur-1:InvalidStateError:Failed to set the 'duration' property on 'MediaSource': The MediaSource's readyState is not 'open'.",
            "dur-str:InvalidStateError:Failed to set the 'duration' property on 'MediaSource': The MediaSource's readyState is not 'open'.",
            "dur-neg:TypeError:Failed to set the 'duration' property on 'MediaSource': The value provided ('-1') is negative or NaN.",
            "dur-nan:TypeError:Failed to set the 'duration' property on 'MediaSource': The value provided ('x') is negative or NaN.",
            "eos:InvalidStateError:Failed to execute 'endOfStream' on 'MediaSource': The MediaSource's readyState is not 'open'.",
            "slsr:InvalidStateError:Failed to execute 'setLiveSeekableRange' on 'MediaSource': The MediaSource's readyState is not 'open'.",
            "clsr:InvalidStateError:Failed to execute 'clearLiveSeekableRange' on 'MediaSource': The MediaSource's readyState is not 'open'.",
            "asb-empty:TypeError:Failed to execute 'addSourceBuffer' on 'MediaSource': The type provided is empty",
            "asb-x:NotSupportedError:Failed to execute 'addSourceBuffer' on 'MediaSource': The type provided ('x') is unsupported.",
            "rsb:TypeError:Failed to execute 'removeSourceBuffer' on 'MediaSource': parameter 1 is not of type 'SourceBuffer'.",
            // Nothing the page did changed the source: every gated member refused before it wrote.
            "after:closed|NaN",
        ]
    );
}

/// The WebIDL arity guards, which the table's `required` columns carry (a table-owned method never
/// sees the registry's separate arity pass).
#[tokio::test]
async fn the_media_source_arity_guards_run_before_the_body() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "mse-arity",
        r#"(() => {
            const out = [];
            const o = new MediaSource();
            for (const [n, args] of [["addSourceBuffer", []], ["removeSourceBuffer", []],
                                     ["setLiveSeekableRange", []], ["setLiveSeekableRange", [1]]]) {
                try { o[n](...args); out.push(n + "/" + args.length + ":none"); }
                catch (err) { out.push(n + "/" + args.length + ":" + err.constructor.name); }
            }
            out.push("lengths:" + [MediaSource.prototype.addSourceBuffer.length,
                                   MediaSource.prototype.setLiveSeekableRange.length,
                                   MediaSource.prototype.endOfStream.length,
                                   MediaSource.length, MediaSource.isTypeSupported.length].join("|"));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "addSourceBuffer/0:TypeError",
            "removeSourceBuffer/0:TypeError",
            "setLiveSeekableRange/0:TypeError",
            "setLiveSeekableRange/1:TypeError",
            "lengths:1|2|0|0|1",
        ]
    );
}

// -- 7. isTypeSupported answers from the recorded table --------------------------------------------

/// A constant `false` passed the corpus's one case (`isTypeSupported("x")`) while being wrong for
/// every type Brave 151 actually supports. These are the bundle's own recorded answers, spot-checked
/// in both directions so a regression to a constant -- either constant -- fails.
#[tokio::test]
async fn media_source_is_type_supported_is_computed_not_constant() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "mse-types",
        r#"(() => [
            MediaSource.isTypeSupported("x"),
            MediaSource.isTypeSupported(""),
            MediaSource.isTypeSupported("video/mp4"),
            MediaSource.isTypeSupported("audio/mpeg"),
            MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"'),
            MediaSource.isTypeSupported('video/mp4; codecs="avc1"'),
            MediaSource.isTypeSupported('video/webm; codecs="vp9"'),
            MediaSource.isTypeSupported('video/webm; codecs="vp8.0"'),
            MediaSource.canConstructInDedicatedWorker,
        ].join("|"))()"#
            .to_string(),
    );
    assert_eq!(got, "false|false|false|true|true|false|true|false|true");
}

// -- the worker realm's behaviour, and `handle` ----------------------------------------------------

/// `handle` is the union table's whole reason to exist, and it is a REAL answer: asked of headless
/// Chrome 150.0.7871.46 over a loopback server inside a dedicated worker, `new MediaSource().handle`
/// is `[object MediaSourceHandle]`, `o.handle === o.handle` is true, its `[[Prototype]]` is
/// `MediaSourceHandle.prototype` and it has no own property. `MediaSourceHandle` has zero prototype
/// members in the capture, so that is the whole of the recorded object.
#[tokio::test]
async fn the_worker_media_source_answers_handle_and_keeps_it() {
    let body = r#"
        const o = new MediaSource();
        const parts = [
            Object.prototype.toString.call(o),
            o.readyState,
            String(o.duration),
            Object.prototype.toString.call(o.handle),
            o.handle === o.handle,
            Object.getPrototypeOf(o.handle) === MediaSourceHandle.prototype,
            JSON.stringify(Object.getOwnPropertyNames(o.handle)),
            JSON.stringify(Object.getOwnPropertyNames(o)),
            Object.prototype.toString.call(o.sourceBuffers),
            o.sourceBuffers.length,
            MediaSource.canConstructInDedicatedWorker,
            MediaSource.isTypeSupported("audio/mpeg"),
            MediaSource.isTypeSupported("x"),
        ];
        // The window has no `handle` row at all, and the worker's must still refuse a foreign receiver.
        try { Object.getOwnPropertyDescriptor(MediaSource.prototype, "handle").get.call({}); parts.push("foreign:none"); }
        catch (err) { parts.push("foreign:" + err.constructor.name + ":" + err.message); }
        self.postMessage(parts.join("|"));
    "#;
    let got = worker_probe(body).await;
    assert_eq!(
        got,
        "[object MediaSource]|closed|NaN|[object MediaSourceHandle]|true|true|[]|[]|[object SourceBufferList]|0|true|true|false|foreign:TypeError:Illegal invocation"
    );
}

/// The window must NOT have grown the worker-only member -- the other half of the selector's job.
#[tokio::test]
async fn the_window_media_source_has_no_handle() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "mse-no-handle",
        r#"(() => [
            Object.getOwnPropertyNames(MediaSource.prototype).includes("handle"),
            "handle" in MediaSource.prototype,
            String(new MediaSource().handle),
            Object.prototype.toString.call(new MediaSource().handle),
        ].join("|"))()"#
            .to_string(),
    );
    assert_eq!(got, "false|false|undefined|[object Undefined]");
}

/// A MEDIA ELEMENT ATTACHES A `MediaSource`; IT DOES NOT FETCH ITS OBJECT URL.
/// `URL.createObjectURL(mediaSource)` names an object, not bytes, and MSE's "attaching to a media
/// element" is what the element runs for it. This engine attaches nothing (see
/// `core/media/media_source.rs`), and the loader FETCHED the URL instead: the blob registry holds no
/// bytes for a MediaSource, so the fetch failed and `error` fired at the `<video>` with `video.error`
/// still null. Wistia's player reports exactly that as "Unspecified error in media playback" -- twice
/// on twilio.com, where Chrome's replay reference throws nothing. Chrome attaches the source and fires
/// no `error`; the `sourceopen` that follows in Chrome is not asserted, because nothing here attaches.
#[tokio::test]
async fn a_media_source_object_url_is_not_fetched_and_fires_no_error() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(
        r#"<!doctype html><html><body><script>
        window.C = { errors: 0 };
        var v = document.createElement("video");
        v.addEventListener("error", function () { C.errors++; });
        v.src = URL.createObjectURL(new MediaSource());
        document.body.appendChild(v);
        </script></body></html>"#,
        "http://127.0.0.1/page.html",
        &[],
    )
    .await
    .expect("load_page");
    e.pump_until_loaded(std::time::Duration::from_secs(5)).await.expect("pump");
    e.pump(std::time::Duration::from_millis(300)).await.expect("pump 2");
    let v = e
        .eval_value("read", "JSON.stringify({ errors: C.errors, error: document.querySelector('video').error })".to_string())
        .expect("eval");
    assert_eq!(v.as_str(), Some(r#"{"errors":0,"error":null}"#));
}

/// A SUPERSEDED MEDIA FETCH REPORTS NOTHING. HTML's media element load algorithm
/// aborts the running fetch when a new load starts, so a `src` that was rewritten while its fetch was
/// in flight never fires `error` at the element. Wistia does exactly this -- `src` = its MP4, then,
/// 6 ms later, a `MediaSource` object URL -- and the MP4 fetch's failure landed on the `<video>` as
/// "Unspecified error in media playback" on twilio.com, 1 replay in 3. The first `src` here is a
/// loopback port nothing listens on, so its fetch fails for certain.
#[tokio::test]
async fn a_media_fetch_superseded_by_a_new_src_fires_no_error() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(
        r#"<!doctype html><html><body><script>
        window.C = { errors: 0 };
        var v = document.createElement("video");
        v.addEventListener("error", function () { C.errors++; });
        document.body.appendChild(v);
        v.src = "http://127.0.0.1:9/nothing-listens.mp4";
        v.src = URL.createObjectURL(new MediaSource());
        </script></body></html>"#,
        "http://127.0.0.1/page.html",
        &[],
    )
    .await
    .expect("load_page");
    e.pump_until_loaded(std::time::Duration::from_secs(5)).await.expect("pump");
    e.pump(std::time::Duration::from_millis(500)).await.expect("pump 2");
    let v = e.eval_value("read", "JSON.stringify({ errors: C.errors })".to_string()).expect("eval");
    assert_eq!(v.as_str(), Some(r#"{"errors":0}"#));
}
