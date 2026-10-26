//! LIVE one-table gate for the WebRTC/media OBJECT GRAPH.
//!
//! Eleven interfaces that had to move together, because they were one dependency web through
//! `dom/09k-media-rtc.js`'s module-private `TRACK` and `MS` WeakMaps: `RTCPeerConnection`,
//! `RTCDataChannel`, `RTCStatsReport`, `RTCRtpSender`, `RTCRtpReceiver`, `RTCRtpTransceiver`,
//! `RTCDTMFSender`, `MediaStreamTrack`, `CanvasCaptureMediaStreamTrack`, `MediaStream` and
//! `MediaRecorder` -- plus `HTMLCanvasElement.captureStream`, the one member of the lane on an
//! interface the lane does not own.
//!
//! What this asserts, against the shipped capture bundle and the bundle's own oracle answers:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list, in
//!      the capture's order, with the capture's descriptors, `constructor` last.
//!   2. **The chains and where an instance lands** -- failure mode 1 of a table cutover. Every object
//!      the graph mints (`pc`, its transceiver, sender, receiver, dtmf, the receiver's track, a data
//!      channel, a stats report, a canvas capture track, a stream, a recorder) is on the TABLE's
//!      prototype, and the inheritance splices are the capture's.
//!   3. **Every recorded oracle case** for the eleven, read from the bundle at RUNTIME -- including
//!      the `(async () => ...)` ones, which are settled through the engine's own event loop rather
//!      than skipped.
//!   4. **The receiver checks** -- failure mode 2. Every body this lane wrote answers Chrome's
//!      `TypeError: Illegal invocation` for a receiver that is not an instance of its interface.
//!   5. **No instance tell**: nothing the graph mints carries an own property.
//!   6. **The graph itself**: `addTransceiver` / `addTrack` / `removeTrack` / `createDataChannel` and
//!      the state each leaves behind, and `canvas.captureStream()`'s track settings.
//!   7. **The refusals stay refusals** -- failure mode 3. `createOffer` on a connection with something
//!      to negotiate, `createAnswer` on a stable one, `setParameters`, `insertDTMF` and
//!      `MediaRecorder.start()` on an empty stream all still say no, with the recorded error name.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The eleven this lane's tables own beyond the two value objects `rtc_live.rs` covers.
const IFACES: &[&str] = &[
    "RTCPeerConnection",
    "RTCDataChannel",
    "RTCStatsReport",
    "RTCRtpSender",
    "RTCRtpReceiver",
    "RTCRtpTransceiver",
    "RTCDTMFSender",
    "MediaStream",
    "MediaStreamTrack",
    "CanvasCaptureMediaStreamTrack",
    "MediaRecorder",
];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

/// Every object the graph can mint, by the expression that mints one. `Object.create(P)` is NOT one of
/// these -- that is the foreign receiver, checked separately.
const MINTS: &[(&str, &str)] = &[
    ("RTCPeerConnection", "new RTCPeerConnection()"),
    ("RTCRtpTransceiver", r#"new RTCPeerConnection().addTransceiver("audio")"#),
    ("RTCRtpSender", r#"new RTCPeerConnection().addTransceiver("audio").sender"#),
    ("RTCRtpReceiver", r#"new RTCPeerConnection().addTransceiver("audio").receiver"#),
    ("RTCDTMFSender", r#"new RTCPeerConnection().addTransceiver("audio").sender.dtmf"#),
    ("MediaStreamTrack", r#"new RTCPeerConnection().addTransceiver("audio").receiver.track"#),
    ("RTCDataChannel", r#"new RTCPeerConnection().createDataChannel("x")"#),
    ("MediaStream", "new MediaStream()"),
    ("MediaRecorder", "new MediaRecorder(new MediaStream())"),
    ("CanvasCaptureMediaStreamTrack", r#"document.createElement("canvas").captureStream().getVideoTracks()[0]"#),
];

/// The rows this lane deliberately left as the D2 `stub`, and the reason each is one. A stub answers
/// its honest empty value for any receiver, so it cannot take part in the foreign-receiver probe --
/// and `tests/stub_rows_are_honest` below checks that each really does answer nothing rather than
/// having quietly grown a body.
const STUBS: &[&str] = &[
    // The legacy pre-transceiver API (`addStream`/`removeStream`/`createDTMFSender`) -- superseded by
    // `addTrack` / `addTransceiver`, and a body would have to invent a stream this engine never has.
    "RTCPeerConnection.addStream",
    "RTCPeerConnection.removeStream",
    "RTCPeerConnection.createDTMFSender",
    // An encoded-frame pipeline: there is no encoder and no transport to read frames off.
    "RTCRtpSender.createEncodedStreams",
    "RTCRtpReceiver.createEncodedStreams",
    // A jitter-buffer knob with no jitter buffer behind it.
    "RTCRtpReceiver.playoutDelayHint",
    // The Capture Handle API needs a captured TAB; there is none. The row is nullable, so the stub
    // answers `null`, which is what the capture recorded off a canvas track.
    "MediaStreamTrack.getCaptureHandle",
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

// -- 1. the eleven prototypes, byte-identical to the capture ----------------------------------------

#[tokio::test]
async fn every_l4_graph_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l4g-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

// -- 2. the chains and where an instance lands ------------------------------------------------------

/// Failure mode 1 of a table cutover: an instance left on a prototype that is not the table's. Every
/// minted object in the graph is probed, including the ones a page can only reach THROUGH another
/// object (a receiver's track, a sender's DTMF sender).
#[tokio::test]
async fn every_minted_object_lands_on_the_tables_prototype() {
    let mut e = page().await;
    let mut lines = Vec::new();
    let mut want = Vec::new();
    for (iface, expr) in MINTS {
        lines.push(format!(
            r#"out.push("{iface}:" + (Object.getPrototypeOf({expr}) === {iface}.prototype) + ":" + Object.prototype.toString.call({expr}));"#
        ));
        want.push(format!("{iface}:true:[object {iface}]"));
    }
    let body = lines.join("\n");
    let got = str_of(&mut e, "l4g-mint", format!(r#"(() => {{ const out = []; {body} return out.join("\n"); }})()"#));
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

/// The inheritance splices the capture records, and the two interfaces whose constructor is REAL
/// (`MediaStream`, `MediaRecorder`, `RTCPeerConnection`) against the nine whose constructor is the
/// WebIDL "Illegal constructor".
#[tokio::test]
async fn the_l4_graph_chains_and_constructors_are_the_captures() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4g-chain",
        r#"(() => {
            const out = [];
            const parent = {
                RTCPeerConnection: "EventTarget", RTCDataChannel: "EventTarget", MediaStream: "EventTarget",
                MediaStreamTrack: "EventTarget", MediaRecorder: "EventTarget", RTCDTMFSender: "EventTarget",
                CanvasCaptureMediaStreamTrack: "MediaStreamTrack",
                RTCStatsReport: null, RTCRtpSender: null, RTCRtpReceiver: null, RTCRtpTransceiver: null,
            };
            for (const k of Object.keys(parent)) {
                const p = parent[k];
                const proto = Object.getPrototypeOf(globalThis[k].prototype);
                const ok = p === null ? proto === Object.prototype : proto === globalThis[p].prototype;
                out.push(k + ":" + ok);
            }
            // The three that really construct, and the eight that cannot.
            for (const k of ["MediaStream", "MediaRecorder", "RTCPeerConnection"]) {
                try { new globalThis[k](k === "MediaRecorder" ? new MediaStream() : undefined); out.push(k + ":new-ok"); }
                catch (err) { out.push(k + ":" + err.message); }
            }
            for (const k of ["RTCDataChannel", "RTCStatsReport", "RTCRtpSender", "RTCRtpReceiver",
                             "RTCRtpTransceiver", "RTCDTMFSender", "MediaStreamTrack",
                             "CanvasCaptureMediaStreamTrack"]) {
                try { new globalThis[k](); out.push(k + ":constructed"); }
                catch (err) { out.push(k + ":" + err.message); }
            }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    let mut want: Vec<String> = vec![
        "RTCPeerConnection:true",
        "RTCDataChannel:true",
        "MediaStream:true",
        "MediaStreamTrack:true",
        "MediaRecorder:true",
        "RTCDTMFSender:true",
        "CanvasCaptureMediaStreamTrack:true",
        "RTCStatsReport:true",
        "RTCRtpSender:true",
        "RTCRtpReceiver:true",
        "RTCRtpTransceiver:true",
        "MediaStream:new-ok",
        "MediaRecorder:new-ok",
        "RTCPeerConnection:new-ok",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for k in [
        "RTCDataChannel",
        "RTCStatsReport",
        "RTCRtpSender",
        "RTCRtpReceiver",
        "RTCRtpTransceiver",
        "RTCDTMFSender",
        "MediaStreamTrack",
        "CanvasCaptureMediaStreamTrack",
    ] {
        // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
        want.push(format!("{k}:Failed to construct '{k}': Illegal constructor"));
    }
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

/// TWO REGISTRY INTERFACES INHERIT FROM ONE THIS LANE TOOK OUT OF THE REGISTRY.
/// `BrowserCaptureMediaStreamTrack` and `MediaStreamTrackGenerator` are both `: MediaStreamTrack` in
/// the capture. The registry's chain pass runs during the ESM layer, when `MediaStreamTrack` is no
/// longer a registry name and not yet a table one, so it leaves both on the bare `Object.prototype`
/// their shells were minted with -- which is what the re-captured installer golden records for the
/// registry ALONE, and it is a broken chain a page can see. `install_window_media_rtc` repairs it with
/// `rechain_orphans`, derived from the registry. This is that repair, measured in the live realm.
#[tokio::test]
async fn the_registry_children_of_this_lanes_interfaces_keep_their_chain() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4g-orphans",
        r#"(() => {
            const out = [];
            for (const k of ["BrowserCaptureMediaStreamTrack", "MediaStreamTrackGenerator"]) {
                const C = globalThis[k];
                if (typeof C !== "function") { out.push(k + ":absent"); continue; }
                out.push(k + ":" + (Object.getPrototypeOf(C.prototype) === MediaStreamTrack.prototype));
            }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec!["BrowserCaptureMediaStreamTrack:true", "MediaStreamTrackGenerator:true"]
    );
}

/// Every body here is a native `v8::Function`; a JS body behind a mask is what this goal deletes.
#[tokio::test]
async fn every_l4_graph_member_reports_native_code() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "l4g-native",
        format!(
            r#"(() => {{
            const bad = [];
            const nat = (f) => /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f));
            for (const iface of [{list}]) {{
                const C = globalThis[iface];
                if (!nat(C)) bad.push(iface + ".ctor");
                for (const holder of [C, C.prototype]) {{
                    for (const n of Object.getOwnPropertyNames(holder)) {{
                        if (n === "prototype") continue;
                        const d = Object.getOwnPropertyDescriptor(holder, n);
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
    assert_eq!(got, "", "a member of the L4 graph does not report [native code]");
}

// -- 3. every recorded oracle case ------------------------------------------------------------------

/// The bundle's own recorded answers for the eleven, read at RUNTIME from
/// `<bundle>/oracle/<Iface>.json`. Nothing is copied into this file.
///
/// A case whose expression is an `(async () => ...)` IIFE settles a PROMISE, which a synchronous
/// `eval` cannot observe: the value is parked on a global and the engine's own event loop is run
/// before it is read. Skipping those cases would be deleting recorded cases, so they are not skipped.
#[tokio::test]
async fn every_l4_graph_oracle_case_answers_what_the_capture_recorded() {
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
                    r#"(() => {{ globalThis.__l4g = "PENDING";
                        const done = (v) => {{ globalThis.__l4g = v === undefined ? "U" : "V" + JSON.stringify(v); }};
                        const fail = (err) => {{ globalThis.__l4g = "T" + (err && err.name ? err.name : String(err)); }};
                        try {{ Promise.resolve({expr}).then(done, fail); }} catch (err) {{ fail(err); }}
                        return "ok"; }})()"#
                );
                str_of(&mut e, "l4g-oracle-async", src);
                e.run().await.unwrap();
                str_of(&mut e, "l4g-oracle-read", "String(globalThis.__l4g)".to_string())
            } else {
                let src = format!(
                    r#"(() => {{ try {{ const v = ({expr});
                        return v === undefined ? "U" : "V" + JSON.stringify(v);
                    }} catch (err) {{ return "T" + (err && err.name ? err.name : String(err)); }} }})()"#
                );
                str_of(&mut e, "l4g-oracle", src)
            };
            assert_eq!(got, want, "{iface}.{} -- {expr}", c["member"].as_str().unwrap());
            checked += 1;
        }
    }
    // A guard against the loop silently reading nothing (an empty or moved oracle directory).
    assert!(checked >= 250, "only {checked} oracle cases ran for the eleven L4 graph interfaces");
}

// -- 4. the receiver checks -------------------------------------------------------------------------

/// Failure mode 2: a row that accepts a foreign receiver. Chrome REFUSES every member reached on
/// something that is not an instance of its interface -- but a refusal has TWO shapes, and this
/// test knew only one. It already SAW the promise (it attached a `.catch` to silence the unhandled
/// rejection) and then scored it `no-throw`. Re-measured on headless Chrome 150.0.7871.46 over a
/// loopback server with `--dump-dom`:
///
/// ```text
///   RTCPeerConnection.prototype.createOffer.call({})  REJECTED TypeError: Failed to execute
///                                        'createOffer' on 'RTCPeerConnection': Illegal invocation
///   RTCPeerConnection.prototype.getStats.call({})     REJECTED TypeError: ... Illegal invocation
///   RTCRtpSender.prototype.getStats.call({})          REJECTED TypeError: ... Illegal invocation
///   AudioContext.prototype.resume.call({})            REJECTED TypeError: ... Illegal invocation
///   MediaStreamTrack.prototype.stop.call({})          SYNC-THROW TypeError: Illegal invocation
///   RTCPeerConnection.prototype.close.call({})        SYNC-THROW TypeError: Illegal invocation
/// ```
///
/// `install/receiver.rs` rule 2 -- a `Promise`-typed member REJECTS, it does not throw -- is what
/// the promise rows now follow. A RESOLVED promise, or any non-promise return, is still a failure,
/// so this is the same strength of check in the right shape.
///
/// THREE EXCLUSIONS, each measured and each engine-wide rather than this lane's:
///
///   * The `on*` EVENT-HANDLER rows, for every interface except `MediaRecorder`. They are installed by
///     the shared `install::kits::event_handler`, which has no per-interface brand; giving the
///     engine's whole handler surface one is not this lane's to do. `MediaRecorder`'s six ARE branded
///     here, because the bundle's oracle records all six as a TypeError on `Object.create(...)`.
///   * The D2 `stub` rows listed in `STUBS`. A stub answers its honest empty value for ANY receiver,
///     by construction -- it has no body to check one. Each name below is a row this lane
///     deliberately left `= stub`, and the reason is in the table's own file header.
///   * Each method is called with ONE argument. A row with `required > 0` wears
///     `binding::protos::make_arity_guard`, and the guard runs BEFORE the body, so a no-argument call
///     answers the ARITY TypeError rather than the receiver's. That ordering is the shared guard's and
///     is the same for every table in the engine; the probe supplies an argument so it reaches the
///     body's check, which is what this test is about.
#[tokio::test]
async fn every_l4_graph_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let list = IFACES.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let stubs = STUBS.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "l4g-illegal",
        format!(
            r#"(() => {{
            const bad = [];
            globalThis.__l4gBad = bad;
            const stubs = new Set([{stubs}]);
            for (const iface of [{list}]) {{
                const P = globalThis[iface].prototype;
                for (const recv of [P, Object.create(P), {{}}]) {{
                    for (const n of Object.getOwnPropertyNames(P)) {{
                        if (n === "constructor") continue;
                        if (n.startsWith("on") && iface !== "MediaRecorder") continue;
                        if (stubs.has(iface + "." + n)) continue;
                        const d = Object.getOwnPropertyDescriptor(P, n);
                        const probes = [];
                        if (d.get) probes.push(() => d.get.call(recv));
                        if (d.set) probes.push(() => d.set.call(recv, null));
                        if (typeof d.value === "function") probes.push(() => d.value.call(recv, null));
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
            return "probed";
        }})()"#
        ),
    );
    assert_eq!(got, "probed");
    // Phase 2: run the microtask checkpoint, so every rejection above has been observed.
    e.pump(std::time::Duration::from_millis(50)).await.expect("pump");
    let got = str_of(&mut e, "l4g-illegal-read", r#"globalThis.__l4gBad.join("\n")"#.to_string());
    assert_eq!(got, "", "an L4 graph member accepted a foreign receiver");
}

/// The `STUBS` roster is honest: every row on it answers the D2 stub's empty value -- `undefined` for
/// a method, `undefined`/`null` for an attribute -- rather than having quietly grown a body that the
/// foreign-receiver probe then has to skip. A row that gains a body must leave the roster with it,
/// which is what makes the exclusion above safe rather than a hole.
#[tokio::test]
async fn every_stub_row_is_still_an_honest_stub() {
    let mut e = page().await;
    let stubs = STUBS.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
    let got = str_of(
        &mut e,
        "l4g-stubs",
        format!(
            r#"(() => {{
            // on REAL instances: a stub row refuses `Object.create(P)` like every other member
            const pc = new RTCPeerConnection();
            const tr = pc.addTransceiver("audio");
            const real = {{ RTCPeerConnection: pc, RTCRtpSender: tr.sender, RTCRtpReceiver: tr.receiver, MediaStreamTrack: tr.receiver.track }};
            const out = [];
            for (const row of [{stubs}]) {{
                const [iface, n] = row.split(".");
                const P = globalThis[iface].prototype;
                const d = Object.getOwnPropertyDescriptor(P, n);
                if (!d) {{ out.push(row + ":absent"); continue; }}
                const v = d.get ? d.get.call(real[iface])
                                : (typeof d.value === "function" ? d.value.call(real[iface], null) : "not-a-row");
                out.push(row + ":" + String(v));
            }}
            return out.join("\n");
        }})()"#
        ),
    );
    // Every one answers `null`, because the D2 stub's shape default for an `Object`-typed row is
    // `null` (`install/stub.rs::stub_default`) and the capture types every member of these interfaces
    // `Object`. That is the SAME answer the generated-registry row gave before this lane -- the stub
    // is one mechanism, and the rows only changed owner.
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "RTCPeerConnection.addStream:null",
            "RTCPeerConnection.removeStream:null",
            "RTCPeerConnection.createDTMFSender:null",
            "RTCRtpSender.createEncodedStreams:null",
            "RTCRtpReceiver.createEncodedStreams:null",
            "RTCRtpReceiver.playoutDelayHint:null",
            "MediaStreamTrack.getCaptureHandle:null",
        ]
    );
}

// -- 5. no instance tell ----------------------------------------------------------------------------

#[tokio::test]
async fn no_l4_graph_instance_carries_an_own_property() {
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
    let got = str_of(&mut e, "l4g-own", format!(r#"(() => {{ const out = []; {body} return out.join("\n"); }})()"#));
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

// -- 6. the graph itself ----------------------------------------------------------------------------

/// `addTransceiver` / `addTrack` / `removeTrack` / `createDataChannel` and the state each leaves. The
/// un-negotiated values (`mid` null, `currentDirection` null, `transport` null, a muted receiver track)
/// are the state the objects are genuinely in, not placeholders.
#[tokio::test]
async fn the_peer_connection_builds_the_object_graph_the_capture_describes() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4g-graph",
        r#"(() => {
            const out = [];
            const pc = new RTCPeerConnection();
            out.push("fresh:" + [pc.signalingState, pc.iceGatheringState, pc.iceConnectionState,
                                pc.connectionState, pc.getSenders().length, pc.getTransceivers().length,
                                JSON.stringify(pc.canTrickleIceCandidates), JSON.stringify(pc.sctp)].join("|"));
            out.push("config:" + JSON.stringify(pc.getConfiguration()));
            const x = pc.addTransceiver("audio");
            out.push("xcvr:" + [JSON.stringify(x.mid), x.direction, JSON.stringify(x.currentDirection),
                                x.stopped, JSON.stringify(x.sender.transport)].join("|"));
            out.push("lists:" + [pc.getSenders().length, pc.getReceivers().length, pc.getTransceivers().length].join("|"));
            out.push("identity:" + (pc.getSenders()[0] === x.sender) + ":" + (pc.getTransceivers()[0] === x));
            out.push("copy:" + (pc.getSenders() !== pc.getSenders()));
            const rt = x.receiver.track;
            out.push("recv-track:" + [rt.kind, rt.muted, rt.enabled, rt.readyState, rt.label, typeof rt.id].join("|"));
            out.push("dtmf:" + [x.sender.dtmf.canInsertDTMF, JSON.stringify(x.sender.dtmf.toneBuffer)].join("|"));
            // A VIDEO transceiver has no DTMF sender: tones are an audio-track feature.
            out.push("video-dtmf:" + JSON.stringify(pc.addTransceiver("video").sender.dtmf));
            // addTrack reuses a compatible sender with no track before adding one.
            const t = document.createElement("canvas").captureStream().getVideoTracks()[0];
            const s = pc.addTrack(t);
            out.push("addTrack-reuse:" + (s === pc.getTransceivers()[1].sender) + ":" + (s.track === t));
            out.push("addTrack-count:" + pc.getTransceivers().length);
            pc.removeTrack(s);
            out.push("removeTrack:" + JSON.stringify(s.track));
            // A data channel with no SCTP association.
            const dc = pc.createDataChannel("chat", { protocol: "p", ordered: false, maxRetransmits: 3 });
            out.push("dc:" + [dc.label, dc.ordered, dc.protocol, dc.negotiated, JSON.stringify(dc.id),
                              dc.readyState, dc.bufferedAmount, dc.binaryType, dc.reliable,
                              JSON.stringify(dc.maxRetransmits), JSON.stringify(dc.maxPacketLifeTime)].join("|"));
            try { pc.createDataChannel("x", { maxRetransmits: 1, maxPacketLifeTime: 1 }); out.push("dc-both:none"); }
            catch (err) { out.push("dc-both:" + err.constructor.name); }
            // close() takes the connection and every channel on it to their terminal state.
            pc.close();
            out.push("closed:" + [pc.signalingState, pc.iceConnectionState, pc.connectionState, dc.readyState].join("|"));
            try { pc.createDataChannel("y"); out.push("after-close:none"); }
            catch (err) { out.push("after-close:" + err.name); }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    let lines: Vec<&str> = got.lines().collect();
    assert_eq!(
        lines,
        vec![
            "fresh:stable|new|new|new|0|0|null|null",
            // Chrome 150's eight keys in its order (measured: fixtures/webrtc/webrtc.chrome-150.txt);
            // korvus's own five were pinned here before the configuration was converted.
            r#"config:{"alwaysNegotiateDataChannels":false,"bundlePolicy":"balanced","certificates":[],"encodedInsertableStreams":false,"iceCandidatePoolSize":0,"iceServers":[],"iceTransportPolicy":"all","rtcpMuxPolicy":"require"}"#,
            "xcvr:null|sendrecv|null|false|null",
            "lists:1|1|1",
            "identity:true:true",
            "copy:true",
            "recv-track:audio|true|true|live||string",
            "dtmf:false|\"\"",
            "video-dtmf:null",
            "addTrack-reuse:true:true",
            "addTrack-count:2",
            "removeTrack:null",
            "dc:chat|false|p|false|null|connecting|0|arraybuffer|false|3|null",
            "dc-both:TypeError",
            "closed:closed|closed|closed|closed",
            "after-close:InvalidStateError",
        ]
    );
}

/// `canvas.captureStream()` -- the one track source on this machine that needs no device. The track's
/// settings are the canvas's OWN geometry, and the stream is active because the track is live.
#[tokio::test]
async fn the_canvas_capture_stream_carries_the_canvas_geometry() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4g-canvas",
        r#"(() => {
            const out = [];
            const c = document.createElement("canvas");
            c.width = 640; c.height = 360;
            const s = c.captureStream();
            const t = s.getVideoTracks()[0];
            out.push("stream:" + [s.active, s.getTracks().length, s.getAudioTracks().length, typeof s.id].join("|"));
            out.push("track:" + [t.kind, t.enabled, t.muted, t.readyState, t.contentHint].join("|"));
            out.push("settings:" + JSON.stringify(t.getSettings()));
            out.push("canvas:" + (t.canvas === c));
            out.push("byid:" + (s.getTrackById(t.id) === t) + ":" + JSON.stringify(s.getTrackById("nope")));
            const fps = c.captureStream(30).getVideoTracks()[0];
            out.push("fps:" + JSON.stringify(fps.getSettings().frameRate));
            try { c.captureStream(-1); out.push("bad-fps:none"); } catch (err) { out.push("bad-fps:" + err.constructor.name); }
            // A clone keeps the prototype and the settings, and takes a new id.
            const cl = t.clone();
            out.push("clone:" + [Object.prototype.toString.call(cl), cl.id !== t.id,
                                 JSON.stringify(cl.getSettings()) === JSON.stringify(t.getSettings())].join("|"));
            // contentHint: an unrecognised hint is IGNORED, a recognised one sticks.
            t.contentHint = "x"; out.push("hint-bad:" + JSON.stringify(t.contentHint));
            t.contentHint = "motion"; out.push("hint-ok:" + t.contentHint);
            // stop() ends the track, and a stream with no live track is inactive.
            t.stop();
            out.push("stopped:" + t.readyState + "|" + s.active);
            // MediaStream's set semantics: adding twice is one track, removing takes it out.
            const s2 = new MediaStream();
            s2.addTrack(cl); s2.addTrack(cl);
            out.push("add:" + s2.getTracks().length);
            s2.removeTrack(cl);
            out.push("remove:" + s2.getTracks().length);
            out.push("from-stream:" + new MediaStream(s).getTracks().length);
            out.push("from-seq:" + new MediaStream([cl]).getTracks().length);
            try { new MediaStream([{}]); out.push("bad-seq:none"); } catch (err) { out.push("bad-seq:" + err.constructor.name); }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "stream:true|1|0|string",
            "track:video|true|false|live|",
            r#"settings:{"width":640,"height":360,"aspectRatio":1.7778}"#,
            "canvas:true",
            "byid:true:null",
            "fps:30",
            "bad-fps:TypeError",
            "clone:[object CanvasCaptureMediaStreamTrack]|true|true",
            r#"hint-bad:"""#,
            "hint-ok:motion",
            "stopped:ended|false",
            "add:1",
            "remove:0",
            "from-stream:1",
            "from-seq:1",
            "bad-seq:TypeError",
        ]
    );
}

/// `MediaRecorder`'s state machine, which is observable in full without an encoder -- and the bitrates,
/// which are the capture's numbers and not chosen here.
#[tokio::test]
async fn the_media_recorder_runs_its_state_machine_without_an_encoder() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4g-recorder",
        r#"(() => {
            const out = [];
            const c = document.createElement("canvas"); c.width = 8; c.height = 8;
            const r = new MediaRecorder(c.captureStream());
            out.push("fresh:" + [r.state, JSON.stringify(r.mimeType), r.videoBitsPerSecond,
                                 r.audioBitsPerSecond, r.audioBitrateMode].join("|"));
            out.push("split:" + (() => { const q = new MediaRecorder(new MediaStream(), { bitsPerSecond: 200000 });
                                         return q.videoBitsPerSecond + "/" + q.audioBitsPerSecond; })());
            out.push("stream-identity:" + (() => { const s = new MediaStream(); return new MediaRecorder(s).stream === s; })());
            r.start(); out.push("start:" + r.state);
            try { r.start(); out.push("restart:none"); } catch (err) { out.push("restart:" + err.name); }
            r.pause(); out.push("pause:" + r.state);
            r.pause(); out.push("pause2:" + r.state);
            r.resume(); out.push("resume:" + r.state);
            out.push("requestData:" + String(r.requestData()));
            r.stop(); out.push("stop:" + r.state);
            out.push("stop2:" + String(r.stop()) + "|" + r.state);
            try { r.pause(); out.push("pause-inactive:none"); } catch (err) { out.push("pause-inactive:" + err.name); }
            try { new MediaRecorder(new MediaStream()).start(); out.push("empty:none"); }
            catch (err) { out.push("empty:" + err.name); }
            try { new MediaRecorder(new MediaStream(), { audioBitrateMode: "nope" }); out.push("mode:none"); }
            catch (err) { out.push("mode:" + err.constructor.name); }
            try { new MediaRecorder(new MediaStream(), { mimeType: "video/webm" }); out.push("mime:ok"); }
            catch (err) { out.push("mime:" + err.name); }
            out.push("isTypeSupported:" + MediaRecorder.isTypeSupported("video/webm"));
            try { new MediaRecorder({}); out.push("not-stream:none"); } catch (err) { out.push("not-stream:" + err.constructor.name); }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            r#"fresh:inactive|""|2500000|128000|variable"#,
            "split:180000/20000",
            "stream-identity:true",
            "start:recording",
            "restart:InvalidStateError",
            "pause:paused",
            "pause2:paused",
            "resume:recording",
            "requestData:undefined",
            "stop:inactive",
            "stop2:undefined|inactive",
            "pause-inactive:InvalidStateError",
            "empty:NotSupportedError",
            "mode:TypeError",
            "mime:ok",
            "isTypeSupported:true",
            "not-stream:TypeError",
        ]
    );
}

/// `RTCStatsReport` is a real maplike with Blink's own iterator brand -- `[object RTCStatsReport
/// Iterator]`, not `[object Map Iterator]`.
#[tokio::test]
async fn the_stats_report_is_a_maplike_with_the_interfaces_iterator_brand() {
    let mut e = page().await;
    str_of(
        &mut e,
        "l4g-stats-run",
        r#"(() => { globalThis.__rep = null;
            new RTCPeerConnection().getStats().then(r => { globalThis.__rep = r; });
            return "ok"; })()"#
            .to_string(),
    );
    e.run().await.unwrap();
    let got = str_of(
        &mut e,
        "l4g-stats",
        r#"(() => {
            const r = globalThis.__rep;
            const out = [];
            out.push("tag:" + Object.prototype.toString.call(r));
            out.push("size:" + r.size);
            out.push("get:" + String(r.get("x")) + "|" + r.has("x"));
            out.push("iter-tag:" + Object.prototype.toString.call(r.keys()));
            out.push("spread:" + JSON.stringify([...r.entries()]) + JSON.stringify([...r.keys()]) + JSON.stringify([...r.values()]));
            let n = 0; r.forEach(() => { n++; }); out.push("forEach:" + n);
            try { r.forEach(5); out.push("forEach-bad:none"); } catch (err) { out.push("forEach-bad:" + err.constructor.name); }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "tag:[object RTCStatsReport]",
            "size:0",
            "get:undefined|false",
            "iter-tag:[object RTCStatsReport Iterator]",
            "spread:[][][]",
            "forEach:0",
            "forEach-bad:TypeError",
        ]
    );
}

// -- 7. the refusals stay refusals ------------------------------------------------------------------

/// Failure mode 3: a body that becomes reachable where it was a silent no-op, WITHOUT the spec's
/// checks. Each of these is a measured decision this lane inherited verbatim, and each must still say
/// no, with the recorded error name.
#[tokio::test]
async fn the_deliberate_refusals_are_still_refusals() {
    let mut e = page().await;
    str_of(
        &mut e,
        "l4g-refuse-run",
        r#"(() => {
            globalThis.__out = [];
            const push = (k, p) => p.then(
                v => globalThis.__out.push(k + ":resolved:" + JSON.stringify(v)),
                err => globalThis.__out.push(k + ":" + (err && err.name ? err.name : String(err))));
            const pc = new RTCPeerConnection();
            // An EMPTY connection has a complete session-level offer; a data channel's is the SCTP
            // section Chrome writes.
            push("offer-empty", pc.createOffer().then(d => d.type + "|" + d.sdp.split("\r\n")[0]));
            push("answer-stable", pc.createAnswer());
            const pc2 = new RTCPeerConnection();
            pc2.createDataChannel("x");
            push("offer-negotiable", pc2.createOffer().then(d => d.type + "|" + d.sdp.split("\n").length + "|" + /\r\nm=application 9 UDP\/DTLS\/SCTP webrtc-datachannel\r\n/.test(d.sdp)));
            push("setLocal-implicit", pc2.setLocalDescription());
            // An unrecognised `RTCSdpType` is the binding layer's TypeError, thrown SYNCHRONOUSLY.
            try { pc.setRemoteDescription({ type: "nope" }); globalThis.__out.push("setRemote-bad:none"); }
            catch (err) { globalThis.__out.push("setRemote-bad:" + err.constructor.name); }
            push("setRemote-empty", pc.setRemoteDescription({}));
            push("setRemote-ok", pc.setRemoteDescription({ type: "offer", sdp: "v=0" }));
            // No candidate is the end-of-candidates signal and RESOLVES; an actual candidate with no
            // remote description is the algorithm's InvalidStateError.
            push("addIce-none", pc.addIceCandidate());
            push("addIce-no-remote", pc.addIceCandidate(new RTCIceCandidate({ candidate: "", sdpMid: "0" })));
            push("setParameters", pc.addTransceiver("audio").sender.setParameters({}));
            try { pc.addTransceiver("audio").sender.dtmf.insertDTMF("1"); globalThis.__out.push("insertDTMF:none"); }
            catch (err) { globalThis.__out.push("insertDTMF:" + err.name); }
            try { pc.createDataChannel("z").send("x"); globalThis.__out.push("dc-send:none"); }
            catch (err) { globalThis.__out.push("dc-send:" + err.name); }
            return "ok";
        })()"#
            .to_string(),
    );
    e.run().await.unwrap();
    let got = str_of(&mut e, "l4g-refuse", r#"globalThis.__out.slice().sort().join("\n")"#.to_string());
    let mut want = vec![
        "addIce-none:resolved:undefined",
        "addIce-no-remote:InvalidStateError",
        "answer-stable:InvalidStateError",
        "dc-send:InvalidStateError",
        "insertDTMF:InvalidStateError",
        r#"offer-empty:resolved:"offer|v=0""#,
        // A data-channel offer is Chrome's 17-line SCTP section (Brave 151 measured
        // `sdp-lines=18` over `split("\n")`); it was refused for want of a DTLS fingerprint, and
        // rambler's page did not survive the rejection. Media sections are still refused.
        r#"offer-negotiable:resolved:"offer|18|true""#,
        "setLocal-implicit:resolved:undefined",
        "setParameters:InvalidStateError",
        "setRemote-bad:TypeError",
        "setRemote-empty:OperationError",
        "setRemote-ok:OperationError",
    ];
    want.sort();
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}
