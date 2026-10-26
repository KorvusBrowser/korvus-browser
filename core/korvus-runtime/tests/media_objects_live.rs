//! LIVE one-table gate for the media VALUE objects: `VideoFrame`,
//! `VideoColorSpace` and `MediaMetadata`.
//!
//! Three interfaces a page constructs by hand, none of which needs a decoder, a device or a platform
//! media session to be completely correct -- everything they report is either the constructor's own
//! arguments, the source's own dimensions, or a computation over those.
//!
//!   1. **Byte-identical prototypes** against the capture, `constructor` last.
//!   2. **Every recorded oracle case**, read from the bundle at RUNTIME (37 across the three).
//!   3. **The chains, the mint sites and `[native code]`** -- failure mode 1.
//!   4. **The receiver checks** -- failure mode 2.
//!   5. **No instance tell.**
//!   6. **`VideoFrame`'s lifetime and geometry**: what `close()` does to all twelve readable members,
//!      what `allocationSize()` computes, what a `visibleRect` outside the frame is rejected with, and
//!      that a clone is independent of its original -- failure mode 3, a body that answers without
//!      the spec's checks.
//!   7. **`MediaMetadata.artwork`'s sequence conversion**, which is the one place this trio can throw.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

const IFACES: &[&str] = &["VideoFrame", "VideoColorSpace", "MediaMetadata"];

/// The rows this trio deliberately leaves as the D2 stub. `copyTo` copies the frame's PIXELS into a
/// buffer and a frame here carries geometry and no pixels -- it is the one member of `VideoFrame` that
/// cannot be answered without a decoder.
const STUBS: &[&str] = &["VideoFrame.copyTo"];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

/// A frame over the DEFAULT canvas, which is 300x150 -- the same source the bundle's oracle uses.
const FRAME: &str = r#"new VideoFrame(document.createElement("canvas"), { timestamp: 0 })"#;

const MINTS: &[(&str, &str)] = &[
    ("VideoFrame", FRAME),
    ("VideoColorSpace", "new VideoColorSpace()"),
    ("MediaMetadata", "new MediaMetadata()"),
];

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

fn expected_proto(iface: &str) -> Vec<String> {
    let fx = bundle();
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the bundle surface"))
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

// -- 1. the three prototypes -----------------------------------------------------------------------

#[tokio::test]
async fn every_media_value_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "mv-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

// -- 2. every recorded oracle case -----------------------------------------------------------------

#[tokio::test]
async fn every_media_value_oracle_case_answers_what_the_capture_recorded() {
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
                    r#"(() => {{ globalThis.__mv = "PENDING";
                        const done = (v) => {{ globalThis.__mv = v === undefined ? "U" : "V" + JSON.stringify(v); }};
                        const fail = (err) => {{ globalThis.__mv = "T" + (err && err.name ? err.name : String(err)); }};
                        try {{ Promise.resolve({expr}).then(done, fail); }} catch (err) {{ fail(err); }}
                        return "ok"; }})()"#
                );
                str_of(&mut e, "mv-oracle-async", src);
                e.run().await.unwrap();
                str_of(&mut e, "mv-oracle-read", "String(globalThis.__mv)".to_string())
            } else {
                let src = format!(
                    r#"(() => {{ try {{ const v = ({expr});
                        return v === undefined ? "U" : "V" + JSON.stringify(v);
                    }} catch (err) {{ return "T" + (err && err.name ? err.name : String(err)); }} }})()"#
                );
                str_of(&mut e, "mv-oracle", src)
            };
            assert_eq!(got, want, "{iface}.{} -- {expr}", c["member"].as_str().unwrap());
            checked += 1;
        }
    }
    assert!(checked >= 37, "only {checked} oracle cases ran for the three media value interfaces");
}

// -- 3. the chains, the mint sites and [native code] -----------------------------------------------

#[tokio::test]
async fn every_media_value_mint_lands_on_the_tables_prototype() {
    let mut e = page().await;
    let mut lines = Vec::new();
    let mut want = Vec::new();
    for (iface, expr) in MINTS {
        lines.push(format!(
            r#"out.push("{iface}:" + (Object.getPrototypeOf({expr}) === {iface}.prototype)
                + ":" + Object.prototype.toString.call({expr})
                + ":" + (Object.getPrototypeOf({iface}.prototype) === Object.prototype)
                + ":" + ({iface}.prototype.constructor === {iface}));"#
        ));
        want.push(format!("{iface}:true:[object {iface}]:true:true"));
    }
    // A frame's `colorSpace` is minted THROUGH the frame, so it is the mint site a page cannot reach
    // directly -- and it must land on the table's prototype too.
    lines.push(format!(
        r#"out.push("colorSpace:" + (Object.getPrototypeOf({FRAME}.colorSpace) === VideoColorSpace.prototype));"#
    ));
    want.push("colorSpace:true".to_string());
    let body = lines.join("\n");
    let got = str_of(&mut e, "mv-mint", format!(r#"(() => {{ const out = []; {body} return out.join("\n"); }})()"#));
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

#[tokio::test]
async fn every_media_value_member_reports_native_code() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "mv-native",
        format!(
            r#"(() => {{
            const bad = [];
            const nat = (f) => /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f));
            for (const iface of [{list}]) {{
                const C = globalThis[iface];
                if (!nat(C)) bad.push(iface + ".ctor");
                for (const n of Object.getOwnPropertyNames(C.prototype)) {{
                    const d = Object.getOwnPropertyDescriptor(C.prototype, n);
                    for (const f of [d.get, d.set, typeof d.value === "function" ? d.value : null]) {{
                        if (f && !nat(f)) bad.push(iface + "." + n);
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

/// Failure mode 2. The exclusions are the same two the graph gate documents: a D2 `stub` row has no
/// body to check a receiver with, and a `required > 0` row's arity guard runs first, so each probe
/// passes an argument.
#[tokio::test]
async fn every_media_value_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let stubs = STUBS.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "mv-illegal",
        format!(
            r#"(() => {{
            const bad = [];
            const stubs = new Set([{stubs}]);
            for (const iface of [{list}]) {{
                const P = globalThis[iface].prototype;
                for (const recv of [P, Object.create(P), {{}}]) {{
                    for (const n of Object.getOwnPropertyNames(P)) {{
                        if (n === "constructor" || stubs.has(iface + "." + n)) continue;
                        const d = Object.getOwnPropertyDescriptor(P, n);
                        const probes = [];
                        if (d.get) probes.push(() => d.get.call(recv));
                        if (d.set) probes.push(() => d.set.call(recv, null));
                        if (typeof d.value === "function") probes.push(() => d.value.call(recv, null));
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
async fn no_media_value_instance_carries_an_own_property() {
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
    let got = str_of(&mut e, "mv-own", format!(r#"(() => {{ const out = []; {body} return out.join("\n"); }})()"#));
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

// -- 6. VideoFrame's lifetime and geometry ---------------------------------------------------------

/// `close()` detaches the frame from its media resource: every nullable member becomes `null`, every
/// numeric one `0`, `flip` `false`, and the three members that need the resource throw
/// `InvalidStateError`. A body that answered anyway would be failure mode 3.
#[tokio::test]
async fn a_closed_video_frame_answers_empty_for_every_member() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "mv-closed",
        format!(
            r#"(() => {{
            const out = [];
            const o = {FRAME};
            out.push("open:" + [o.format, o.timestamp, JSON.stringify(o.duration), o.codedWidth, o.codedHeight,
                                o.displayWidth, o.displayHeight, o.rotation, o.flip].join("|"));
            out.push("open-rects:" + [o.codedRect.width, o.codedRect.height, o.visibleRect.width, o.visibleRect.height].join("|"));
            out.push("alloc:" + o.allocationSize());
            out.push("close:" + String(o.close()) + "|" + String(o.close()));
            out.push("closed:" + [JSON.stringify(o.format), o.timestamp, JSON.stringify(o.duration), o.codedWidth,
                                  o.codedHeight, o.displayWidth, o.displayHeight, o.rotation, o.flip,
                                  JSON.stringify(o.codedRect), JSON.stringify(o.visibleRect)].join("|"));
            for (const m of ["allocationSize", "metadata", "clone"]) {{
                try {{ o[m](); out.push(m + ":none"); }} catch (err) {{ out.push(m + ":" + err.name); }}
            }}
            return out.join("\n");
        }})()"#
        ),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "open:BGRA|0|null|300|150|300|150|0|false",
            "open-rects:300|150|300|150",
            "alloc:180000",
            "close:undefined|undefined",
            "closed:null|0|null|0|0|0|0|0|false|null|null",
            "allocationSize:InvalidStateError",
            "metadata:InvalidStateError",
            "clone:InvalidStateError",
        ]
    );
}

/// The constructor's own geometry rules, and that a clone is INDEPENDENT of its original.
#[tokio::test]
async fn the_video_frame_constructor_computes_its_geometry() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "mv-geom",
        r#"(() => {
            const out = [];
            const c = document.createElement("canvas");
            c.width = 64; c.height = 32;
            const o = new VideoFrame(c, { timestamp: 5, duration: 10, visibleRect: { x: 4, y: 2, width: 16, height: 8 } });
            out.push("coded:" + o.codedWidth + "x" + o.codedHeight);
            out.push("visible:" + [o.visibleRect.x, o.visibleRect.y, o.visibleRect.width, o.visibleRect.height].join(","));
            // displayWidth/Height default to the VISIBLE rectangle, not the coded one.
            out.push("display:" + o.displayWidth + "x" + o.displayHeight);
            out.push("alloc:" + o.allocationSize());
            out.push("times:" + o.timestamp + "|" + o.duration);
            // rotation is taken modulo 360 and is never negative.
            out.push("rot:" + new VideoFrame(c, { timestamp: 0, rotation: -90 }).rotation
                     + "|" + new VideoFrame(c, { timestamp: 0, rotation: 450 }).rotation);
            // A frame FROM a frame keeps the format and the color space, and needs no timestamp.
            const from = new VideoFrame(o);
            out.push("from-frame:" + from.format + "|" + (from.colorSpace !== o.colorSpace)
                     + "|" + from.codedWidth + "x" + from.codedHeight);
            // A clone shares no geometry object with its original.
            const cl = o.clone();
            out.push("clone:" + [cl.codedWidth, cl.visibleRect.width, cl.timestamp].join("|"));
            out.push("clone-rect-copy:" + (cl.visibleRect !== o.visibleRect));
            // A rectangle outside the frame is the constructor's own TypeError.
            try { new VideoFrame(c, { timestamp: 0, visibleRect: { x: 60, y: 0, width: 16, height: 8 } }); out.push("outside:none"); }
            catch (err) { out.push("outside:" + err.constructor.name); }
            // A source with no dimensions is not a CanvasImageSource, and a canvas source needs a timestamp.
            try { new VideoFrame({}); out.push("no-size:none"); } catch (err) { out.push("no-size:" + err.constructor.name); }
            try { new VideoFrame(c); out.push("no-ts:none"); } catch (err) { out.push("no-ts:" + err.constructor.name); }
            // sRGB full range is what a canvas' backing store is.
            out.push("space:" + JSON.stringify(o.colorSpace.toJSON()));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "coded:64x32",
            "visible:4,2,16,8",
            "display:16x8",
            "alloc:512",
            "times:5|10",
            "rot:270|90",
            // `new VideoFrame(frame)` reports the SOURCE frame's display size as its coded size.
            "from-frame:BGRA|false|16x8",
            "clone:64|16|5",
            "clone-rect-copy:true",
            "outside:TypeError",
            "no-size:TypeError",
            "no-ts:TypeError",
            r#"space:{"primaries":"bt709","transfer":"iec61966-2-1","matrix":"rgb","fullRange":true}"#,
        ]
    );
}

// -- 7. MediaMetadata's sequence conversion --------------------------------------------------------

#[tokio::test]
async fn the_media_metadata_artwork_converts_its_sequence() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "mv-artwork",
        r#"(() => {
            const out = [];
            const m = new MediaMetadata({ title: "t", artist: "a", album: "b",
                                          artwork: [{ src: "u", sizes: "1x1" }] });
            out.push("init:" + [m.title, m.artist, m.album].join("|"));
            out.push("art:" + JSON.stringify(m.artwork));
            // Reading gives back a FRESH FROZEN array (Chrome 150, C2): a push throws and changes nothing.
            const got = m.artwork;
            try { got.push({ src: "z" }); out.push("pushed"); } catch (err) { out.push("push:" + err.constructor.name); }
            out.push("fresh:" + m.artwork.length + "|" + Object.isFrozen(got) + "|" + (got !== m.artwork));
            // A DOMString member is ToString'd and never rejects.
            m.title = 5; out.push("num-title:" + JSON.stringify(m.title));
            m.title = undefined; out.push("undef-title:" + JSON.stringify(m.title));
            // Assigning replaces the whole list.
            m.artwork = [{ src: "v" }];
            out.push("replaced:" + JSON.stringify(m.artwork));
            for (const bad of [5, [5], [{}]]) {
                try { m.artwork = bad; out.push("bad:none"); } catch (err) { out.push("bad:" + err.constructor.name); }
            }
            out.push("after-bad:" + JSON.stringify(m.artwork));
            out.push("chapters:" + JSON.stringify(m.chapterInfo));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "init:t|a|b",
            r#"art:[{"sizes":"1x1","src":"u","type":""}]"#,
            "push:TypeError",
            "fresh:1|true|true",
            r#"num-title:"5""#,
            r#"undef-title:"undefined""#,
            r#"replaced:[{"sizes":"","src":"v","type":""}]"#,
            "bad:TypeError",
            "bad:TypeError",
            "bad:TypeError",
            r#"after-bad:[{"sizes":"","src":"v","type":""}]"#,
            "chapters:[]",
        ]
    );
}
