//! LIVE one-table gate for the six streams interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle, that the six `interface!`
//! tables reproduce -- byte- and value-identically -- what the deleted 95-line streams block of
//! `dom/08g-observers.js` installed:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` last.
//!   2. **The interface objects.** The capture's `Symbol.toStringTag`, `length`, `name` and (empty)
//!      static list; NEWABILITY straight out of `surface.json`, so the three the capture records as
//!      `"newable": true` construct and the three it records as `false` throw; and every constructor
//!      reads `[native code]` -- the point of retiring the two `op_nativize_ctor` masks and the plain
//!      JS `function ReadableStream(src)`.
//!   3. **The chains.** All six inherit nothing: `Object.getPrototypeOf(C.prototype) === Object.prototype`,
//!      which is what the capture's empty `proto` list records.
//!   4. **A ReadableStream read to completion**, both synchronously fed from `start(controller)` and
//!      fed AFTER the read parked -- the live-read path, and the proof the controller the Rust
//!      constructor hands `start()` really reaches the queue.
//!   5. **A TransformStream piped**: the default identity transform through `pipeThrough`, and a
//!      transformer whose `transform`/`flush` run through the controller.
//!   6. **Instances wear the TABLE's prototype**, not the cppgc class template's -- the brief's
//!      failure mode (1): every mint site (`new`, `getReader`, `tee`, `getWriter`,
//!      `TransformStream`'s internal pair) is checked.
//!   7. **The WORKER realm owns the same six**, from the same tables (invariant I8).

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The six this lane's tables own.
const IFACES: &[&str] = &[
    "ReadableStream",
    "ReadableStreamDefaultReader",
    "ReadableStreamDefaultController",
    "WritableStream",
    "WritableStreamDefaultWriter",
    "TransformStream",
];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

fn worker_bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("worker-surface")).unwrap()).unwrap()
}

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        k => panic!("unexpected kind {k}"),
    }
}

/// The seat this REALM'S PERSONA SURFACE gives `iface`, which is the engine's own source rather than
/// a file this test picks. Both realms' artifacts record `ctorIndex` since the
/// worker surfaces were re-read from the browsers; before that the worker's came from a cross-realm
/// borrow, and this function read through the persona so that it followed whichever answer the
/// engine was using. It still does, which is the point: one source, both realms.
fn seat(realm: spec::SurfaceRealm, iface: &str, n: usize) -> usize {
    use spec::Persona;
    persona::CapturedPersona::default()
        .surface(realm)
        .and_then(|s| s.members_of(iface).and_then(|o| o.ctor_index))
        .unwrap_or(n)
        .min(n)
}

fn expected_proto(fx: &serde_json::Value, iface: &str, realm: spec::SurfaceRealm) -> Vec<String> {
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the surface"))
        .clone();
    let mut out: Vec<String> = members.iter().map(describe_line).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = seat(realm, iface, out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// Same, but for an expression that evaluates to a promise (every stream case here is async).
async fn str_of_async(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value_settled(name, src, std::time::Duration::from_secs(10))
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

// ── 1. the six prototypes, byte-identical to the capture ────────────────────────────────────────

#[tokio::test]
async fn every_l11b_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l11b-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(&fx, iface, spec::SurfaceRealm::Window), "{iface}.prototype != the capture");
    }
}

// ── 2. the interface objects: tag, length, name, statics, newability, native-ness ───────────────

#[tokio::test]
async fn every_l11b_interface_object_matches_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                let newable = "yes";
                try {{ new C(); }} catch (err) {{ newable = err.constructor.name + ":" + err.message; }}
                const native = /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(C));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name,
                        statics.join(","), newable, native,
                        Object.getPrototypeOf(C.prototype) === Object.prototype].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l11b-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        // Newability is the CAPTURE's, never this test's opinion: the three 0-argument constructors
        // succeed, the three the capture records as not newable throw.
        // Newability is the CAPTURE's for four of the six. The other two are the `Model::Value`
        // interfaces whose WebIDL constructor takes a REQUIRED argument: the capture records `newable:
        // false` because Chrome answers "1 argument required, but only 0 present." for `new
        // ReadableStreamDefaultReader()` / `new WritableStreamDefaultWriter()` -- and so does this
        // engine since 2026-10-08 (the oracle ratchet; the D2 stub constructor accepted zero
        // arguments until then, a divergence this test pinned).
        let takes_an_argument = matches!(*iface, "ReadableStreamDefaultReader" | "WritableStreamDefaultWriter");
        // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
        let want_newable = if fx["interfaces"][iface]["newable"].as_bool().unwrap() {
            "yes".to_string()
        } else if takes_an_argument {
            format!("TypeError:Failed to construct '{iface}': 1 argument required, but only 0 present.")
        } else {
            format!("TypeError:Failed to construct '{iface}': Illegal constructor")
        };
        assert_eq!(
            got,
            format!("{iface}|0|0|1|{want_len}|{iface}||{want_newable}|true|true"),
            "{iface} interface object != the capture"
        );
        // The capture agrees the constructor is native for all six.
        assert!(fx["interfaces"][iface]["ctorNative"].as_bool().unwrap(), "{iface} ctorNative");
        assert!(fx["interfaces"][iface]["proto"].as_array().unwrap().is_empty(), "{iface} inherits nothing");
    }
}

// ── 3. a ReadableStream read to completion, synchronously fed and live-fed ──────────────────────

#[tokio::test]
async fn a_readable_stream_reads_to_completion() {
    let mut e = page().await;
    let got = str_of_async(
        &mut e,
        "l11b-rs-read",
        r#"(async () => {
            const out = [];
            // (a) chunks enqueued synchronously from start(controller).
            const s = new ReadableStream({ start(c) { c.enqueue("a"); c.enqueue("b"); c.close(); } });
            out.push("locked0=" + s.locked);
            const r = s.getReader();
            out.push("locked1=" + s.locked);
            out.push("proto=" + (Object.getPrototypeOf(r) === ReadableStreamDefaultReader.prototype));
            for (;;) { const { value, done } = await r.read(); if (done) break; out.push("chunk=" + value); }
            out.push("done");
            r.releaseLock();
            out.push("locked2=" + s.locked);
            // (b) a chunk enqueued AFTER the read parked -- the live-read path.
            let ctl;
            const live = new ReadableStream({ start(c) { ctl = c; } });
            const lr = live.getReader();
            const pending = lr.read();
            ctl.enqueue("late");
            const first = await pending;
            out.push("late=" + first.value + ":" + first.done);
            ctl.close();
            out.push("after=" + (await lr.read()).done);
            // (c) tee gives two branches on the TABLE's prototype.
            const t = new ReadableStream({ start(c) { c.enqueue(1); c.close(); } }).tee();
            out.push("tee=" + t.length + ":" + (Object.getPrototypeOf(t[0]) === ReadableStream.prototype));
            out.push("teeread=" + (await t[0].getReader().read()).value + ":" + (await t[1].getReader().read()).value);
            return out.join("|");
        })()"#
            .to_string(),
    )
    .await;
    assert_eq!(
        got,
        "locked0=false|locked1=true|proto=true|chunk=a|chunk=b|done|locked2=false|late=late:false|after=true|tee=2:true|teeread=1:1"
    );
}

// ── 4. a WritableStream sink really receives the chunks, through the stream and through a writer ─

#[tokio::test]
async fn a_writable_stream_drives_its_sink() {
    let mut e = page().await;
    let got = str_of_async(
        &mut e,
        "l11b-ws",
        r#"(async () => {
            const seen = [];
            const ws = new WritableStream({ write(c) { seen.push("w:" + c); }, close() { seen.push("closed"); } });
            const out = [];
            out.push("locked0=" + ws.locked);
            const w = ws.getWriter();
            out.push("proto=" + (Object.getPrototypeOf(w) === WritableStreamDefaultWriter.prototype));
            out.push("locked1=" + ws.locked);
            out.push("desired=" + w.desiredSize);
            await w.write("x");
            await w.close();
            w.releaseLock();
            out.push("locked2=" + ws.locked);
            // A RELEASED writer's `desiredSize` THROWS -- it does not answer null. Measured on
            // google-chrome 150.0.7871.46 and recorded in
            // `tests/fixtures/stream-closed-probe.chrome-150.txt`; this line read
            // `desired2=null` until then, which is what the engine used to answer and Chrome
            // never did.
            try { out.push("desired2=" + w.desiredSize); }
            catch (err) { out.push("desired2=" + err.name + ": " + err.message); }
            // pipeTo must actually MOVE the chunks into the sink, not just settle.
            const seen2 = [];
            const dest = new WritableStream({ write(c) { seen2.push(c); }, close() { seen2.push("!"); } });
            await new ReadableStream({ start(c) { c.enqueue("p"); c.enqueue("q"); c.close(); } }).pipeTo(dest);
            out.push("piped=" + seen2.join(","));
            // pipeTo REJECTS (not throws) with no argument -- WebIDL's rule for a promise-returning op.
            let rejected = "no";
            try { await new ReadableStream({}).pipeTo(); } catch (err) { rejected = err.constructor.name; }
            out.push("noarg=" + rejected);
            return out.join("|") + "|sink=" + seen.join(",");
        })()"#
            .to_string(),
    )
    .await;
    assert_eq!(
        got,
        "locked0=false|proto=true|locked1=true|desired=1|locked2=false|\
desired2=TypeError: Failed to read the 'desiredSize' property from 'WritableStreamDefaultWriter': \
This writable stream writer has been released and cannot be used to get the desiredSize|\
piped=p,q,!|noarg=TypeError|sink=w:x,closed"
    );
}

// ── 5. a TransformStream piped: identity, and a real transformer ────────────────────────────────

#[tokio::test]
async fn a_transform_stream_is_piped_through() {
    let mut e = page().await;
    let got = str_of_async(
        &mut e,
        "l11b-ts",
        r#"(async () => {
            const out = [];
            // (a) the default (identity) transform, through pipeThrough.
            const ident = new TransformStream();
            out.push("rproto=" + (Object.getPrototypeOf(ident.readable) === ReadableStream.prototype));
            out.push("wproto=" + (Object.getPrototypeOf(ident.writable) === WritableStream.prototype));
            const src = new ReadableStream({ start(c) { c.enqueue("a"); c.enqueue("b"); c.close(); } });
            const r = src.pipeThrough(ident).getReader();
            const got = [];
            for (;;) { const { value, done } = await r.read(); if (done) break; got.push(value); }
            out.push("identity=" + got.join(","));
            // (b) a transformer whose transform() and flush() run through the controller.
            const up = new TransformStream({
                transform(chunk, c) { c.enqueue(chunk.toUpperCase()); },
                flush(c) { c.enqueue("!"); },
            });
            const r2 = new ReadableStream({ start(c) { c.enqueue("x"); c.enqueue("y"); c.close(); } })
                .pipeThrough(up).getReader();
            const got2 = [];
            for (;;) { const { value, done } = await r2.read(); if (done) break; got2.push(value); }
            out.push("upper=" + got2.join(","));
            return out.join("|");
        })()"#
            .to_string(),
    )
    .await;
    assert_eq!(got, "rproto=true|wproto=true|identity=a,b|upper=X,Y,!");
}

// ── 6. every member reads [native code], and the async-iterator alias still works ────────────────

#[tokio::test]
async fn every_l11b_member_is_native_and_async_iteration_holds() {
    let mut e = page().await;
    let fx = bundle();
    let mut names: Vec<String> = Vec::new();
    for iface in IFACES {
        for m in fx["interfaces"][iface]["members"].as_array().unwrap() {
            names.push(format!("{iface}.{}", m["name"].as_str().unwrap()));
        }
    }
    let list = names.join(",");
    let src = format!(
        r#"(() => {{
            const bad = [];
            for (const spec of "{list}".split(",")) {{
                const [iface, member] = spec.split(".");
                const d = Object.getOwnPropertyDescriptor(globalThis[iface].prototype, member);
                for (const f of [d.value, d.get, d.set]) {{
                    if (typeof f !== "function") continue;
                    if (!/\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f))) bad.push(spec);
                }}
            }}
            return bad.join(",");
        }})()"#
    );
    assert_eq!(str_of(&mut e, "l11b-native", src), "", "a member is not [native code]");

    // `@@asyncIterator` is not a row (the capture records `iterable: false`), so the install function
    // puts it there; `for await` over a stream is load-bearing on real sites.
    let got = str_of_async(
        &mut e,
        "l11b-aiter",
        r#"(async () => {
            const out = [];
            const s = new ReadableStream({ start(c) { c.enqueue(1); c.enqueue(2); c.close(); } });
            for await (const chunk of s) out.push(chunk);
            const d = Object.getOwnPropertyDescriptor(ReadableStream.prototype, Symbol.asyncIterator);
            return out.join(",") + "|" + (+d.enumerable) + (+d.configurable) + (+d.writable);
        })()"#
            .to_string(),
    )
    .await;
    assert_eq!(got, "1,2|011");
}

// ── 7. the WORKER realm owns the same six, from the same tables ─────────────────────────────────

#[tokio::test]
async fn the_worker_realm_owns_the_same_six() {
    let fxw = worker_bundle();
    let mut expected: Vec<String> = Vec::new();
    for iface in IFACES {
        expected.push(format!("{iface}\n{}", expected_proto(&fxw, iface, spec::SurfaceRealm::Worker).join("\n")));
    }
    let list = IFACES.join(",");
    // The worker source is registered as a script, NOT built from a JS template literal in the page
    // realm: `DESCRIBE` carries `${...}` of its own, which such a literal would interpolate before the
    // worker ever saw it. A real worker isolate runs cross-thread, so the page is pumped until it
    // posts (the `dom_rect_worker_live.rs` pattern).
    let worker_src = format!(
        r#"{DESCRIBE}
const out = [];
for (const iface of "{list}".split(",")) out.push(iface + "\n" + desc(globalThis[iface].prototype));
const s = new ReadableStream({{ start(c) {{ c.enqueue("w"); c.close(); }} }});
s.getReader().read().then(r => postMessage(out.join("\n@@\n") + "\n@@\nchunk=" + r.value));
"#
    );

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document(PAGE).unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './l11b.js': {} }};",
        serde_json::to_string(&worker_src).unwrap()
    );
    e.eval("l11b-worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "l11b-worker-driver",
        "const w = new Worker('./l11b.js'); w.onmessage = (ev) => print('L11B:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    let got = loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(l) = out.iter().find(|l| l.starts_with("L11B:")) {
            break l["L11B:".len()..].to_string();
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(20), "the worker did not post within 20s; output: {out:?}");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    let mut want = expected.join("\n@@\n");
    want.push_str("\n@@\nchunk=w");
    assert_eq!(got, want, "the worker realm's six != the worker capture");
}
