//! LIVE one-table gate for the CSS Font Loading pair: `FontFace`,
//! `FontFaceSet` and `document.fonts` -- the last two interfaces of `dom/09k-media-rtc.js`, which dies
//! with them.
//!
//! The grammar behind these interfaces is ~280 lines of CSS parsing, and it is the whole risk of this
//! lane: a list of allowed strings would pass every recorded case and be wrong for every value not in
//! it. So the gate asks the grammar things the bundle does not, on both sides of every rule.
//!
//!   1. **Byte-identical prototypes** against the capture, `constructor` last, in BOTH realms.
//!   2. **Every recorded oracle case**, read from the bundle at RUNTIME (54 across the two), the
//!      awaiting ones included -- which is where `FontFace.load()`'s rejection is graded.
//!   3. **The chains, the mint sites and `[native code]`** -- failure mode 1.
//!   4. **The receiver checks** -- failure mode 2.
//!   5. **No instance tell**, on a face, on `document.fonts`, and on a set iterator.
//!   6. **The grammar, on both sides of each rule** -- failure mode 3, a body that answers a case
//!      without doing the work.
//!   7. **The setlike**, and that `document.fonts` is one object per document.
//!   8. **The two refusals**, which are measured decisions and not gaps.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

const IFACES: &[&str] = &["FontFace", "FontFaceSet"];

/// WHERE EACH PROTOTYPE IS. Chrome 152 exposes `FontFaceSet` (Chrome 150 had no such global, and the prototype of
/// `document.fonts` read EventTarget's `constructor`); its rows sit on the prototype of `document.fonts`.
const PROTO_OF: &str = r#"const protoOf = (i) => i === "FontFaceSet" ? Object.getPrototypeOf(document.fonts) : globalThis[i].prototype;"#;

/// `Object.getPrototypeOf(document.fonts)`, measured on Chrome 152.0.7977.82 over a loopback server, 2026-10-16:
/// Chrome 150's rows, and the interface's own `constructor` last.
const FONT_FACE_SET_PROTO: &[&str] = &[
    "onloading:accessor:e=1:c=1:s=1",
    "onloadingdone:accessor:e=1:c=1:s=1",
    "onloadingerror:accessor:e=1:c=1:s=1",
    "ready:accessor:e=1:c=1:s=0",
    "status:accessor:e=1:c=1:s=0",
    "size:accessor:e=1:c=1:s=0",
    "check:data:e=1:c=1:w=1",
    "load:data:e=1:c=1:w=1",
    "add:data:e=1:c=1:w=1",
    "clear:data:e=1:c=1:w=1",
    "delete:data:e=1:c=1:w=1",
    "entries:data:e=1:c=1:w=1",
    "forEach:data:e=1:c=1:w=1",
    "has:data:e=1:c=1:w=1",
    "keys:data:e=1:c=1:w=1",
    "values:data:e=1:c=1:w=1",
    "constructor:data:e=0:c=1:w=1",
];

/// No row of either interface is a D2 stub: every one of the 32 has a Rust body.
const STUBS: &[&str] = &[];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

const FACE: &str = r#"new FontFace("probe", "url(data:,)")"#;

const MINTS: &[(&str, &str)] = &[("FontFace", FACE), ("FontFaceSet", "document.fonts")];

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
async fn every_font_prototype_is_byte_identical_to_the_window_capture() {
    let mut e = page().await;
    let src = format!(r#"(() => {{ {DESCRIBE} return desc(FontFace.prototype); }})()"#);
    let got = str_of(&mut e, "ff-desc", src);
    assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto("surface", "FontFace"), "FontFace.prototype != the window capture");
    let src = format!(r#"(() => {{ {DESCRIBE} return desc(Object.getPrototypeOf(document.fonts)); }})()"#);
    let got = str_of(&mut e, "ff-desc-set", src);
    assert_eq!(got.lines().collect::<Vec<_>>(), FONT_FACE_SET_PROTO, "document.fonts' prototype != Chrome 152's");
}

/// `FontFace` is `[Exposed=(Window,Worker)]` and `worker.surface.json` records it with a member list
/// byte-identical to the window's -- so the SAME table builds it there (invariant I8), where the
/// worker shell gave every member `undefined`. (A worker reaches a `FontFaceSet` only through
/// `self.fonts`, which Chrome 150 answers and this engine still stubs -- a counted D2 row.)
#[tokio::test]
async fn every_font_prototype_is_byte_identical_to_the_worker_capture() {
    let body = format!(
        r#"{DESCRIBE}
        self.postMessage([desc(FontFace.prototype),
                          new FontFace("probe", "url(data:,)").family,
                          Object.prototype.toString.call(new FontFace("probe", "url(data:,)"))].join("\n--\n"));"#
    );
    let got = worker_probe(&body).await;
    let want = format!("{}\n--\nprobe\n--\n[object FontFace]", expected_proto("worker-surface", "FontFace").join("\n"));
    assert_eq!(got, want, "the worker font prototype != the worker capture");
}

// -- 2. every recorded oracle case -----------------------------------------------------------------

#[tokio::test]
async fn every_font_oracle_case_answers_what_the_capture_recorded() {
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
            let got = if expr.trim_start().starts_with("(async") {
                let src = format!(
                    r#"(() => {{ globalThis.__ff = "PENDING";
                        const done = (v) => {{ globalThis.__ff = v === undefined ? "U" : "V" + JSON.stringify(v); }};
                        const fail = (err) => {{ globalThis.__ff = "T" + (err && err.name ? err.name : String(err)); }};
                        try {{ Promise.resolve({expr}).then(done, fail); }} catch (err) {{ fail(err); }}
                        return "ok"; }})()"#
                );
                str_of(&mut e, "ff-oracle-async", src);
                e.run().await.unwrap();
                str_of(&mut e, "ff-oracle-read", "String(globalThis.__ff)".to_string())
            } else {
                let src = format!(
                    r#"(() => {{ try {{ const v = ({expr});
                        return v === undefined ? "U" : "V" + JSON.stringify(v);
                    }} catch (err) {{ return "T" + (err && err.name ? err.name : String(err)); }} }})()"#
                );
                str_of(&mut e, "ff-oracle", src)
            };
            assert_eq!(got, want, "{iface}.{} -- {expr}", c["member"].as_str().unwrap());
            checked += 1;
        }
    }
    assert!(checked >= 54, "only {checked} oracle cases ran for the font pair");
}

// -- 3. the chains, the mint sites and [native code] -----------------------------------------------

#[tokio::test]
async fn every_font_mint_lands_on_the_tables_prototype() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "ff-mint",
        format!(
            r#"(() => {{
            const out = [];
            out.push("face:" + (Object.getPrototypeOf({FACE}) === FontFace.prototype)
                + ":" + Object.prototype.toString.call({FACE})
                + ":" + (Object.getPrototypeOf(FontFace.prototype) === Object.prototype)
                + ":" + (FontFace.prototype.constructor === FontFace)
                + ":" + (Object.getPrototypeOf(FontFace) === Function.prototype));
            // Chrome 152: a `FontFaceSet` global, the prototype's own `constructor` (Chrome 150 had neither).
            const setProto = Object.getPrototypeOf(document.fonts);
            out.push("set:" + typeof globalThis.FontFaceSet
                + ":" + Object.prototype.toString.call(document.fonts)
                + ":" + (Object.getPrototypeOf(setProto) === EventTarget.prototype)
                + ":" + Object.prototype.hasOwnProperty.call(setProto, "constructor")
                + ":" + (setProto.constructor === EventTarget));
            // `document.fonts` is a real EventTarget, and the same object every read.
            out.push("stable:" + (document.fonts === document.fonts));
            let fired = 0;
            document.fonts.addEventListener("loadingdone", () => {{ fired++; }});
            document.fonts.dispatchEvent(new Event("loadingdone"));
            out.push("et:" + fired);
            return out.join("\n");
        }})()"#
        ),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "face:true:[object FontFace]:true:true:true",
            "set:function:[object FontFaceSet]:true:true:false",
            "stable:true",
            "et:1",
        ]
    );
}

#[tokio::test]
async fn every_font_member_reports_native_code() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "ff-native",
        format!(
            r#"(() => {{
            const bad = [];
            const nat = (f) => /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f));
            {PROTO_OF}
            for (const iface of [{list}]) {{
                const C = globalThis[iface];
                if (C && !nat(C)) bad.push(iface + ".ctor");
                const P = protoOf(iface);
                for (const n of Object.getOwnPropertyNames(P)) {{
                    const d = Object.getOwnPropertyDescriptor(P, n);
                    for (const f of [d.get, d.set, typeof d.value === "function" ? d.value : null]) {{
                        if (f && !nat(f)) bad.push(iface + "." + n);
                    }}
                }}
            }}
            // `document.fonts`' own getter, and the set iterator's `next`, are natives too.
            const fd = Object.getOwnPropertyDescriptor(Document.prototype, "fonts");
            if (!fd || !fd.get || !nat(fd.get)) bad.push("Document.fonts");
            const it = document.fonts.keys();
            if (!nat(it.next)) bad.push("FontFaceSetIterator.next");
            if (!nat(it[Symbol.iterator])) bad.push("FontFaceSetIterator.@@iterator");
            return bad.join(",");
        }})()"#
        ),
    );
    assert_eq!(got, "");
}

// -- 4. the receiver checks ------------------------------------------------------------------------

/// Failure mode 2. The exclusions are the two engine-wide ones the earlier lanes named: the shared
/// `event_handler` kit (an `on*` accessor answers for any receiver, in every table in the tree) and a
/// `required > 0` row whose arity guard runs BEFORE the body, so each probe passes an argument.
///
/// A REFUSAL HAS TWO SHAPES, AND THIS TEST KNEW ONLY ONE. It already SAW the promise -- it attached
/// a `.catch` to silence the unhandled rejection -- and then scored it `no-throw`. Re-measured on
/// headless Chrome 150.0.7871.46 over a loopback server with `--dump-dom`:
///
/// ```text
///   get FontFace.prototype.loaded .call({})   REJECTED TypeError: Failed to read the 'loaded'
///                                               property from 'FontFace': Illegal invocation
///   FontFace.prototype.load.call({})          REJECTED TypeError: Failed to execute 'load' on
///                                               'FontFace': Illegal invocation
///   get <FontFaceSet>.ready .call({})         REJECTED  (Chrome 150 exposes no `FontFaceSet`
///   <FontFaceSet>.load.call({})               REJECTED   global; the rows sit on the prototype of
///                                                        `document.fonts`)
///   get FontFace.prototype.family .call({})   SYNC-THROW TypeError: Illegal invocation
///   <FontFaceSet>.check .call({})             SYNC-THROW TypeError: Illegal invocation
/// ```
///
/// `install/receiver.rs` rule 2 -- a `Promise`-typed member REJECTS, it does not throw -- is what
/// the four promise rows now follow. A RESOLVED promise, or any non-promise return, is still a
/// failure, so this is the same strength of check in the right shape.
#[tokio::test]
async fn every_font_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let stubs = STUBS.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    // Phase 1: probe every row. A promise-shaped refusal is recorded when it SETTLES.
    let probed = str_of(
        &mut e,
        "ff-illegal",
        format!(
            r#"(() => {{
            const bad = [];
            globalThis.__ffBad = bad;
            const stubs = new Set([{stubs}]);
            {PROTO_OF}
            for (const iface of [{list}]) {{
                const P = protoOf(iface);
                for (const recv of [P, Object.create(P), {{}}]) {{
                    for (const n of Object.getOwnPropertyNames(P)) {{
                        if (n === "constructor" || n.startsWith("on") || stubs.has(iface + "." + n)) continue;
                        const d = Object.getOwnPropertyDescriptor(P, n);
                        const probes = [];
                        if (d.get) probes.push(() => d.get.call(recv));
                        if (d.set) probes.push(() => d.set.call(recv, "normal"));
                        if (typeof d.value === "function") probes.push(() => d.value.call(recv, "10px serif"));
                        for (const p of probes) {{
                            const label = iface + "." + n;
                            try {{
                                const r = p();
                                if (r && typeof r.then === "function") {{
                                    r.then(
                                        () => bad.push(label + "=RESOLVED (a refusal must reject)"),
                                        err => {{
                                            if (!(err instanceof TypeError) || String(err.message).indexOf("Illegal invocation") < 0) {{
                                                bad.push(label + "=REJECTED:" + err.name + ":" + err.message);
                                            }}
                                        }});
                                }} else {{
                                    bad.push(label + "=no-throw:" + Object.prototype.toString.call(r));
                                }}
                            }} catch (err) {{
                                const answer = err.constructor.name + ":" + err.message;
                                if (answer !== "TypeError:Illegal invocation") bad.push(label + "=" + answer);
                            }}
                        }}
                    }}
                }}
            }}
            // The set iterator's `next` is branded too.
            try {{ document.fonts.keys().next.call({{}}); bad.push("iterator.next=no-throw"); }}
            catch (err) {{ if (err.message !== "Illegal invocation") bad.push("iterator.next=" + err.message); }}
            return "probed";
        }})()"#
        ),
    );
    assert_eq!(probed, "probed");
    // Phase 2: run the microtask checkpoint, so every rejection above has been observed.
    e.pump(std::time::Duration::from_millis(50)).await.expect("pump");
    let got = str_of(&mut e, "ff-illegal-read", r#"globalThis.__ffBad.join("\n")"#.to_string());
    assert_eq!(got, "");
}

// -- 5. no instance tell ---------------------------------------------------------------------------

/// Every instance state this lane holds is a v8 private slot. Measured, headless Chrome
/// 150.0.7871.46 over a loopback server: `Object.getOwnPropertyNames(document.fonts.keys())` is `[]`
/// -- which the JS this replaces did not manage (it parked `next` and `@@iterator` on the iterator
/// itself), so the iterator is checked here beside the two interfaces.
#[tokio::test]
async fn no_font_instance_carries_an_own_property() {
    let mut e = page().await;
    let mut lines = Vec::new();
    let mut want = Vec::new();
    for (iface, expr) in MINTS {
        lines.push(format!(
            r#"out.push("{iface}:" + JSON.stringify(Object.getOwnPropertyNames({expr})) + ":" + Object.getOwnPropertySymbols({expr}).length);"#
        ));
        want.push(format!("{iface}:[]:0"));
    }
    lines.push(
        r#"const it = document.fonts.keys();
           out.push("iterator:" + JSON.stringify(Object.getOwnPropertyNames(it))
                + ":" + Object.getOwnPropertySymbols(it).length
                + ":" + Object.prototype.toString.call(it)
                + ":" + (Object.getPrototypeOf(it) === Object.getPrototypeOf(document.fonts.values())));"#
            .to_string(),
    );
    want.push("iterator:[]:0:[object FontFaceSet Iterator]:true".to_string());
    let body = lines.join("\n");
    let got = str_of(&mut e, "ff-own", format!(r#"(() => {{ const out = []; {body} return out.join("\n"); }})()"#));
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

// -- 6. the grammar, on both sides of every rule ---------------------------------------------------

/// Failure mode 3. The bundle asks each descriptor two questions (its default, and `"x"`); a list of
/// allowed strings would pass both and be wrong for everything else. These are the grammar's own
/// boundaries, asked in both directions, and the ACCEPTED column is the serialisation -- which is
/// where a parser that only validates would show.
#[tokio::test]
async fn the_font_face_descriptors_are_a_grammar_not_a_list() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "ff-grammar",
        format!(
            r#"(() => {{
            const out = [];
            const set = (n, v) => {{
                const o = {FACE};
                try {{ o[n] = v; return JSON.stringify(o[n]); }}
                catch (err) {{ return err.name; }}
            }};
            const cases = [
                ["style", "oblique   20DEG", "italic 20deg"],
                ["style", "oblique 10deg 20deg", "oblique 1deg 2deg 3deg"],
                ["weight", "1 1000", "1001"],
                ["weight", "BOLD", "0"],
                ["stretch", " CONDENSED ", "-1%"],
                ["stretch", "50% 200%", "50"],
                ["unicodeRange", "u+0041", "U+5a-41"],
                ["unicodeRange", "U+4??", "U+??????"],
                ["featureSettings", "\"liga\" off", "\"lig\""],
                ["featureSettings", "\"liga\" 007", "liga"],
                ["variationSettings", "\"wght\" 400.50", "\"wght\""],
                ["display", "SWAP", "sway"],
                ["ascentOverride", "90.0%", "90"],
                ["sizeAdjust", "50%", "normal"],
                ["family", "  My   Font  ", "inherit"]
            ];
            for (const [n, good, bad] of cases) out.push(n + ":" + set(n, good) + ":" + set(n, bad));
            return out.join("\n");
        }})()"#
        ),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            // Whitespace collapses and the value lowercases; `italic <angle>` is not a `<font-style>`.
            r#"style:"oblique 20deg":SyntaxError"#,
            r#"style:"oblique 10deg 20deg":SyntaxError"#,
            // A weight range is two values in [1,1000]; 1001 is out of range.
            r#"weight:"1 1000":SyntaxError"#,
            r#"weight:"bold":SyntaxError"#,
            // A stretch is a keyword or a non-negative percentage -- a bare number is not.
            r#"stretch:"condensed":SyntaxError"#,
            r#"stretch:"50% 200%":SyntaxError"#,
            // Serialised UPPERCASE with no leading zeros; an inverted range is rejected.
            r#"unicodeRange:"U+41":SyntaxError"#,
            // `U+4??` is 0x400-0x4FF; six `?` would reach 0xFFFFFF, past U+10FFFF.
            r#"unicodeRange:"U+400-4FF":SyntaxError"#,
            // `off` serialises as `0`; a tag is a 4-character STRING.
            r#"featureSettings:"\"liga\" 0":SyntaxError"#,
            r#"featureSettings:"\"liga\" 7":SyntaxError"#,
            // The number is re-serialised by JS number rules; the value is required.
            r#"variationSettings:"\"wght\" 400.5":SyntaxError"#,
            r#"display:"swap":SyntaxError"#,
            // A percentage is re-serialised through parseFloat; a bare number is not a percentage.
            r#"ascentOverride:"90%":SyntaxError"#,
            // `size-adjust` has no `normal` -- the one descriptor that differs from the other overrides.
            r#"sizeAdjust:"50%":SyntaxError"#,
            // The `family` SETTER stores the string verbatim and never throws -- Chrome 150, measured:
            // `f.family = "  x  y "` reads back `"  x  y "`, `"inherit"` reads `"inherit"`.
            r#"family:"  My   Font  ":"inherit""#,
        ]
    );
}

/// The `src` descriptor, and the bug this grammar exists to avoid: `url(data:,)` contains a comma, and
/// a naive comma split cuts the most ordinary font source there is into two `SyntaxError`s.
#[tokio::test]
async fn the_font_face_constructor_parses_its_source() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "ff-src",
        r#"(() => {
            const out = [];
            // A source the grammar rejects does NOT throw: the face comes back with `status` "error"
            // (CSS Font Loading; Chrome 150 measured).
            const mk = (src) => { try { return new FontFace("probe", src).status; } catch (err) { return err.name; } };
            for (const s of ["url(data:,)", "url(a)", 'url("f.woff2") format("woff2")',
                             "URL(f.woff) TECH(variations)", "local(Arial)", 'local("Helvetica Neue")',
                             "url(a), local(b)", 'local("x" y)',
                             "x", "", "url(a", "local()", "url(a) format"]) {
                out.push(JSON.stringify(s) + ":" + mk(s));
            }
            // A binary source is not parsed at all.
            out.push("buffer:" + (() => { try { return new FontFace("probe", new ArrayBuffer(4)).status; }
                                          catch (err) { return err.name; } })());
            // The descriptor dictionary is applied by the constructor; a bad one is the same "error" state,
            // and keeps its initial value.
            out.push("init:" + (() => { try { return new FontFace("probe", "url(a)", { weight: "700", style: "italic" }).weight; }
                                        catch (err) { return err.name; } })());
            out.push("init-bad:" + (() => { try { const f = new FontFace("probe", "url(a)", { weight: "x" }); return f.status + " " + f.weight; }
                                            catch (err) { return err.name; } })());
            out.push("argc:" + (() => { try { new FontFace("probe"); return "ok"; } catch (err) { return err.constructor.name; } })());
            out.push("call:" + (() => { try { FontFace("a", "url(b)"); return "ok"; } catch (err) { return err.constructor.name; } })());
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            r#""url(data:,)":unloaded"#,
            r#""url(a)":unloaded"#,
            r#""url(\"f.woff2\") format(\"woff2\")":unloaded"#,
            r#""URL(f.woff) TECH(variations)":unloaded"#,
            r#""local(Arial)":unloaded"#,
            r#""local(\"Helvetica Neue\")":unloaded"#,
            r#""url(a), local(b)":unloaded"#,
            // A string followed by an identifier is not a `<family-name>` (Chrome 150: "error").
            r#""local(\"x\" y)":error"#,
            r#""x":error"#,
            r#""":error"#,
            // The tokenizer closes the block at EOF (Chrome 150: "unloaded").
            r#""url(a":unloaded"#,
            r#""local()":error"#,
            r#""url(a) format":error"#,
            // Chrome 150 answers "error" (it decodes the buffer synchronously and four zero bytes are no
            // font); this engine decodes no font data. Recorded, not hidden.
            // An ArrayBuffer source is parsed at construction: four zero bytes are not a font,
            // so the face is `error` before `new` returns -- stock Chrome 150, measured 2026-10-02.
            "buffer:error",
            "init:700",
            "init-bad:error normal",
            "argc:TypeError",
            "call:TypeError",
        ]
    );
}

/// A constructor argument the grammar rejects is the face's STATE, not a throw (row C17 of
/// the L14 findings: a page building a face from `{unicodeRange: ""}` died on `new`). Every expected
/// line is Chrome 150.0.7871.46's answer to the same script on a loopback page.
#[tokio::test]
async fn a_rejected_constructor_argument_rejects_loaded_instead_of_throwing() {
    let mut e = page().await;
    e.eval(
        "ff-ctor-error",
        r#"(async () => {
            const out = [];
            const face = async (label, ...a) => {
                let f;
                try { f = new FontFace(...a); } catch (err) { out.push(label + ":THREW " + err.name); return; }
                const head = [String(f), f.status];
                try { await f.loaded; head.push("resolved"); } catch (err) { head.push(err.name + ": " + err.message); }
                head.push(f.status);
                out.push(label + ":" + head.join(" | "));
                return f;
            };
            const f1 = await face("empty-range", "x", "url(x.woff)", { unicodeRange: "" });
            out.push("kept-initial:" + f1.unicodeRange);
            const f2 = await face("partial", "x", "url(x.woff)", { style: "italic", weight: "bogus", stretch: "condensed" });
            out.push("partial-values:" + [f2.style, f2.weight, f2.stretch].join(" "));
            await face("first-in-order", "x", "url(x.woff)", { weight: "bogus", style: "bad" });
            await face("source", "x", "bogus(");
            await face("descriptor-before-source", "x", "bogus(", { style: "st" });
            await face("empty-source", "x", "");
            // load() on such a face hands back the same rejection.
            const f3 = new FontFace("x", "url(x.woff)", { unicodeRange: "" });
            try { await f3.load(); out.push("load:resolved"); } catch (err) { out.push("load:" + err.name + " " + f3.status); }
            document.fonts.add(f3);
            out.push("add:" + document.fonts.has(f3));
            out.push("family:" + ["probe", "Open Sans", "1bad", "inherit", "serif", "-x"].map((n) => new FontFace(n, "url(a)").family).join(","));
            print("RESULT:" + out.join("\n"));
        })()"#
            .to_string(),
    )
    .unwrap();
    e.run().await.unwrap();
    let got = e.take_output().into_iter().find(|l| l.starts_with("RESULT:")).unwrap_or_default();
    assert_eq!(
        got.trim_start_matches("RESULT:").lines().collect::<Vec<_>>(),
        vec![
            "empty-range:[object FontFace] | error | SyntaxError: Failed to set '' as a property value. | error",
            "kept-initial:U+0-10FFFF",
            "partial:[object FontFace] | error | SyntaxError: Failed to set 'bogus' as a property value. | error",
            "partial-values:italic normal condensed",
            "first-in-order:[object FontFace] | error | SyntaxError: Failed to set 'bad' as a property value. | error",
            "source:[object FontFace] | error | SyntaxError: The source provided ('bogus(') could not be parsed as a value list. | error",
            "descriptor-before-source:[object FontFace] | error | SyntaxError: Failed to set 'st' as a property value. | error",
            "empty-source:[object FontFace] | error | SyntaxError: The source provided ('') could not be parsed as a value list. | error",
            "load:SyntaxError error",
            "add:true",
            r#"family:probe,"Open Sans","1bad","inherit","serif",-x"#,
        ]
    );
}

/// `check()`'s `font` argument is the `font` SHORTHAND, not a family: it needs a size AND a family,
/// which is why `check("x")` is a `SyntaxError` and not `false`.
#[tokio::test]
async fn the_font_face_set_font_argument_is_the_shorthand() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "ff-shorthand",
        r#"(() => {
            const out = [];
            const chk = (f) => { try { return String(document.fonts.check(f)); } catch (err) { return err.name; } };
            for (const f of ["10px serif", "bold 10px/1.2 serif", 'italic small-caps bold condensed medium "My Font", serif',
                             "MENU", "0 serif", "12pt Arial, sans-serif", "10Q serif",
                             "x", "10px", "serif", "", "a b c d e 10px serif", "10px inherit"]) {
                out.push(JSON.stringify(f) + ":" + chk(f));
            }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            // An empty set can render everything: the system fonts are always available.
            r#""10px serif":true"#,
            r#""bold 10px/1.2 serif":true"#,
            r#""italic small-caps bold condensed medium \"My Font\", serif":true"#,
            r#""MENU":true"#,
            r#""0 serif":true"#,
            r#""12pt Arial, sans-serif":true"#,
            r#""10Q serif":true"#,
            // No size and no family.
            r#""x":SyntaxError"#,
            r#""10px":SyntaxError"#,
            r#""serif":SyntaxError"#,
            r#""":SyntaxError"#,
            // The prefix run before the size is at most four values.
            r#""a b c d e 10px serif":SyntaxError"#,
            r#""10px inherit":SyntaxError"#,
        ]
    );
}

// -- 7. the setlike, and one set per document ------------------------------------------------------

#[tokio::test]
async fn the_font_face_set_is_a_setlike_over_the_faces_the_page_added() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "ff-setlike",
        format!(
            r#"(() => {{
            const out = [];
            const s = document.fonts;
            const a = {FACE}, b = {FACE};
            out.push("empty:" + s.size + ":" + s.status + ":" + s.has(a));
            out.push("add:" + (s.add(a) === s) + ":" + s.size + ":" + s.has(a));
            out.push("add-twice:" + (s.add(a), s.size));
            out.push("add-b:" + (s.add(b), s.size));
            out.push("iter:" + [...s.keys()].length + ":" + [...s.values()].length
                     + ":" + JSON.stringify([...s.entries()].map(p => p.length)));
            const seen = [];
            s.forEach(function (v, k, set) {{ seen.push((v === k) + ":" + (set === s) + ":" + (this === null || this === undefined ? "noThis" : this.tag)); }}, {{ tag: "T" }});
            out.push("forEach:" + seen.join("|"));
            out.push("delete:" + s.delete(a) + ":" + s.delete(a) + ":" + s.size);
            // `check` is false while a face the font NAMES has not loaded; a family no face in the set
            // names renders from the system fonts (stock Chrome 150, measured 2026-10-02: true, false).
            out.push("check-unloaded:" + s.check("10px serif") + ":" + s.check("10px probe"));
            out.push("clear:" + String(s.clear()) + ":" + s.size + ":" + s.check("10px serif"));
            out.push("add-foreign:" + (() => {{ try {{ s.add({{}}); return "ok"; }} catch (err) {{ return err.constructor.name; }} }})());
            out.push("delete-foreign:" + s.delete({{}}));
            out.push("forEach-bad:" + (() => {{ try {{ s.forEach(1); return "ok"; }} catch (err) {{ return err.constructor.name; }} }})());
            out.push("ctor:" + (() => {{ try {{ new FontFaceSet(); return "ok"; }} catch (err) {{ return err.constructor.name; }} }})());
            return out.join("\n");
        }})()"#
        ),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "empty:0:loaded:false",
            "add:true:1:true",
            "add-twice:1",
            "add-b:2",
            "iter:2:2:[2,2]",
            "forEach:true:true:T|true:true:T",
            "delete:true:false:1",
            // A face in the set has not loaded, so nothing the page added can render the text.
            "check-unloaded:true:false",
            "clear:undefined:0:true",
            "add-foreign:TypeError",
            "delete-foreign:false",
            "forEach-bad:TypeError",
            // No `FontFaceSet` global in Chrome 150.
            "ctor:TypeError",
        ]
    );
}

/// One set per document, created on first read and stable thereafter -- and a SECOND document gets its
/// own, which is what a per-document slot buys over one global set.
#[tokio::test]
async fn document_fonts_is_one_set_per_document() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "ff-doc",
        r#"(() => {
            const out = [];
            out.push("same:" + (document.fonts === document.fonts));
            const other = document.implementation.createHTMLDocument("t");
            out.push("other:" + (other.fonts !== document.fonts)
                     + ":" + Object.prototype.toString.call(other.fonts)
                     + ":" + other.fonts.size
                     + ":" + (other.fonts === other.fonts));
            document.fonts.add(new FontFace("probe", "url(data:,)"));
            out.push("independent:" + document.fonts.size + ":" + other.fonts.size);
            // The row lives on Document.prototype and is an accessor with no setter, as the capture says.
            const d = Object.getOwnPropertyDescriptor(Document.prototype, "fonts");
            out.push("row:" + (!!d.get) + ":" + (d.set === undefined) + ":" + d.enumerable + ":" + d.configurable);
            out.push("foreign:" + (() => { try { d.get.call({}); return "ok"; } catch (err) { return err.constructor.name; } })());
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "same:true",
            "other:true:[object FontFaceSet]:0:true",
            "independent:1:0",
            "row:true:true:true:true",
            // A FOREIGN RECEIVER THROWS, which is what Chrome does and what this line used to say the
            // opposite of. It read `foreign:ok` with the note "a foreign receiver gets its
            // own fresh set rather than throwing: `document.fonts` is `Document`'s ROW, and the
            // registry's Document rows do not brand-check their receiver" -- an accurate description of
            // a defect, pinned as if it were the specification. It was true because the body was
            // mounted by a pass OUTSIDE the installer (`install_document_fonts`), and only
            // `installer::put_member` applies a row's receiver check. The row names its body now, so
            // the check is applied. Chrome 150.0.7871.46, headless, measured 2026-09-12:
            // `Object.getOwnPropertyDescriptor(Document.prototype, "fonts").get.call({})` ->
            // `TypeError: Illegal invocation`.
            "foreign:TypeError",
        ]
    );
}

// -- 8. the loads ----------------------------------------------------------------------------------

/// A FACE LOADS AND THE SET'S LOAD SETTLES. This pinned two refusals as measured decisions:
/// `load()` rejecting synchronously with the face already `error` and its original `loaded` promise
/// replaced unsettled, and `FontFaceSet.load()` of a parseable font never settling. Stock Chrome 150,
/// measured 2026-10-02 on this exact script (two identical runs): `load()` moves the face to `loading`
/// and returns `loaded` itself, which rejects with `NetworkError` once the empty `data:` URL turns out
/// not to be a font (after the unparseable set load's `SyntaxError`, which rejects at once); the set's
/// `load("10px serif")` resolves with `[]`, because no face in the set names serif.
#[tokio::test]
async fn a_face_load_and_a_set_load_settle_as_chrome() {
    let mut e = page().await;
    let src = format!(
        r#"(() => {{
        globalThis.__r = [];
        const o = {FACE};
        __r.push("before:" + o.status + ":" + Object.prototype.toString.call(o.loaded));
        // The face's `loaded` promise has NOT settled before `load()` is called.
        let settled = "no";
        o.loaded.then(() => {{ settled = "resolved"; }}, () => {{ settled = "rejected"; }});
        const p = o.load();
        __r.push("returns-loaded:" + (p === o.loaded));
        __r.push("after:" + o.status);
        p.then(() => __r.push("face-load:resolved"), (err) => __r.push("face-load:" + err.name));
        // A second `load()` returns the same promise.
        __r.push("second:" + (o.load() === p));
        // The SET's load: a font no face names resolves with [], an unparseable one rejects.
        let setSettled = "pending";
        document.fonts.load("10px serif").then(() => {{ setSettled = "resolved"; }}, () => {{ setSettled = "rejected"; }});
        document.fonts.load("x").then(() => __r.push("set-load-bad:resolved"), (err) => __r.push("set-load-bad:" + err.name));
        globalThis.__read = () => [settled, setSettled].join("|");
        return "ok"; }})()"#
    );
    str_of(&mut e, "ff-refusal", src);
    e.run().await.unwrap();
    let log = str_of(&mut e, "ff-refusal-read", "globalThis.__r.join(\"\\n\")".to_string());
    let settled = str_of(&mut e, "ff-refusal-settled", "globalThis.__read()".to_string());
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec![
            "before:unloaded:[object Promise]",
            "returns-loaded:true",
            "after:loading",
            "second:true",
            "set-load-bad:SyntaxError",
            "face-load:NetworkError",
        ]
    );
    // The face's `loaded` promise is the one that rejected; the set's load resolved.
    assert_eq!(settled, "rejected|resolved", "Chrome 150's settlements");
}
