//! LIVE one-table gate for the WebRTC interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against the bundle's own
//! oracle answers, that lane L4's `interface!` tables reproduce -- byte- and value-identically -- what
//! `dom/09k-media-rtc.js` installed:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` last
//!      (the hand-table shape).
//!   2. **The interface objects and the chains.** Neither interface inherits (`parent: None` in the
//!      capture), both are NEWABLE, and an instance's prototype is the table's own -- failure mode 1
//!      of a table cutover (an instance left on some other prototype).
//!   3. **Every recorded oracle case**, read from the bundle at RUNTIME, so the value half of the
//!      cutover is checked against the capture and not against a value copied into this file.
//!   4. **The receiver checks** (failure mode 2): every body this lane wrote answers Chrome's
//!      `TypeError: Illegal invocation` for a receiver that is not an instance of its interface --
//!      including the prototype itself, which the `WeakMap` this replaces also rejected.
//!   5. **No instance tell**: an instance has no own string-keyed property, so the state slot is
//!      invisible the way a browser's internal slots are.
//!   6. **The candidate-line grammar** (RFC 5245 §15.1): the derived fields of a line the parser
//!      accepts, and all-null for one it does not.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The two this lane's tables own.
const IFACES: &[&str] = &["RTCSessionDescription", "RTCIceCandidate"];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

/// One well-formed host candidate, the same line the bundle's oracle uses.
const LINE: &str = "candidate:1 1 UDP 1 127.0.0.1 1 typ host";

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

// -- 1. the two prototypes, byte-identical to the capture ------------------------------------------

#[tokio::test]
async fn every_l4_value_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l4-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

// -- 2. the interface objects, the chains and where an instance lands -------------------------------

/// Failure mode 1 of a table cutover: an instance left on a prototype that is not the table's. Both
/// interfaces are `parent: None` in the capture, so their prototypes chain straight to
/// `Object.prototype` and their constructors to `Function.prototype`.
#[tokio::test]
async fn the_l4_interface_objects_chain_and_mint_onto_the_tables_prototype() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4-chain",
        format!(
            r#"(() => {{
            const out = [];
            const sd = new RTCSessionDescription();
            const ic = new RTCIceCandidate({{ candidate: "{LINE}", sdpMid: "0" }});
            out.push("sd-proto:" + (Object.getPrototypeOf(sd) === RTCSessionDescription.prototype));
            out.push("ic-proto:" + (Object.getPrototypeOf(ic) === RTCIceCandidate.prototype));
            out.push("sd-instanceof:" + (sd instanceof RTCSessionDescription));
            out.push("ic-instanceof:" + (ic instanceof RTCIceCandidate));
            out.push("sd-parent:" + (Object.getPrototypeOf(RTCSessionDescription.prototype) === Object.prototype));
            out.push("ic-parent:" + (Object.getPrototypeOf(RTCIceCandidate.prototype) === Object.prototype));
            out.push("sd-ctor-parent:" + (Object.getPrototypeOf(RTCSessionDescription) === Function.prototype));
            out.push("ic-ctor-parent:" + (Object.getPrototypeOf(RTCIceCandidate) === Function.prototype));
            out.push("sd-tag:" + Object.prototype.toString.call(sd));
            out.push("ic-tag:" + Object.prototype.toString.call(ic));
            out.push("sd-len:" + RTCSessionDescription.length + ":" + RTCSessionDescription.name);
            out.push("ic-len:" + RTCIceCandidate.length + ":" + RTCIceCandidate.name);
            out.push("sd-ctorprop:" + (RTCSessionDescription.prototype.constructor === RTCSessionDescription));
            out.push("ic-ctorprop:" + (RTCIceCandidate.prototype.constructor === RTCIceCandidate));
            // A WebIDL constructor is not callable without `new`.
            try {{ RTCSessionDescription(); out.push("sd-call:none"); }}
            catch (err) {{ out.push("sd-call:" + err.constructor.name); }}
            try {{ RTCIceCandidate({{ sdpMid: "0" }}); out.push("ic-call:none"); }}
            catch (err) {{ out.push("ic-call:" + err.constructor.name); }}
            return out.join("\n");
        }})()"#
        ),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "sd-proto:true",
            "ic-proto:true",
            "sd-instanceof:true",
            "ic-instanceof:true",
            "sd-parent:true",
            "ic-parent:true",
            "sd-ctor-parent:true",
            "ic-ctor-parent:true",
            "sd-tag:[object RTCSessionDescription]",
            "ic-tag:[object RTCIceCandidate]",
            "sd-len:0:RTCSessionDescription",
            "ic-len:0:RTCIceCandidate",
            "sd-ctorprop:true",
            "ic-ctorprop:true",
            "sd-call:TypeError",
            "ic-call:TypeError",
        ]
    );
}

/// Every body here is a native `v8::Function`, which is what `Function.prototype.toString` has to
/// report -- a JS body behind a mask is the thing this goal deletes.
#[tokio::test]
async fn every_l4_member_reports_native_code() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4-native",
        r#"(() => {
            const bad = [];
            for (const iface of ["RTCSessionDescription", "RTCIceCandidate"]) {
                const P = globalThis[iface];
                if (!/\{\s*\[native code\]\s*\}/.test(Function.prototype.toString.call(P))) bad.push(iface + ".ctor");
                for (const n of Object.getOwnPropertyNames(P.prototype)) {
                    const d = Object.getOwnPropertyDescriptor(P.prototype, n);
                    for (const f of [d.get, d.set, typeof d.value === "function" ? d.value : null]) {
                        if (f && !/\{\s*\[native code\]\s*\}/.test(Function.prototype.toString.call(f))) {
                            bad.push(iface + "." + n);
                        }
                    }
                }
            }
            return bad.join(",");
        })()"#
            .to_string(),
    );
    assert_eq!(got, "", "a member of the L4 tables does not report [native code]");
}

// -- 3. every recorded oracle case -----------------------------------------------------------------

/// The bundle's own recorded answers for the two, read at RUNTIME from
/// `<bundle>/oracle/<Iface>.json`. Nothing is copied into this file: if the bundle is re-captured the
/// expectations move with it.
#[tokio::test]
async fn every_l4_oracle_case_answers_what_the_capture_recorded() {
    let mut e = page().await;
    let dir = persona::shipped_artifact("oracle");
    let mut checked = 0usize;
    for iface in IFACES {
        let raw = std::fs::read_to_string(dir.join(format!("{iface}.json"))).unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        for c in &cases {
            let expr = c["expr"].as_str().unwrap();
            let src = format!(
                r#"(() => {{ try {{ const v = ({expr});
                    return v === undefined ? "U" : "V" + JSON.stringify(v);
                }} catch (err) {{ return "T" + (err && err.name ? err.name : String(err)); }} }})()"#
            );
            let got = str_of(&mut e, "l4-oracle", src);
            let want = if let Some(t) = c["throws"].as_str() {
                format!("T{t}")
            } else if c["undefined"].as_bool() == Some(true) {
                "U".to_string()
            } else {
                format!("V{}", serde_json::to_string(&c["expected"]).unwrap())
            };
            assert_eq!(got, want, "{iface}.{} -- {expr}", c["member"].as_str().unwrap());
            checked += 1;
        }
    }
    // A guard against the loop silently reading nothing (an empty or moved oracle directory).
    assert!(checked >= 29, "only {checked} oracle cases ran for the two L4 value interfaces");
}

// -- 4. the receiver checks ------------------------------------------------------------------------

/// Failure mode 2 of a table cutover: a row that accepts a foreign receiver. Chrome's binding layer
/// answers `TypeError: Illegal invocation` for every member reached on something that is not an
/// instance of its interface -- including the prototype itself and an `Object.create` of it.
#[tokio::test]
async fn every_l4_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4-illegal",
        r#"(() => {
            const bad = [];
            for (const iface of ["RTCSessionDescription", "RTCIceCandidate"]) {
                const P = globalThis[iface].prototype;
                for (const recv of [P, Object.create(P), {}, Object.create(null)]) {
                    for (const n of Object.getOwnPropertyNames(P)) {
                        if (n === "constructor") continue;
                        const d = Object.getOwnPropertyDescriptor(P, n);
                        const probes = [];
                        if (d.get) probes.push(() => d.get.call(recv));
                        if (d.set) probes.push(() => d.set.call(recv, "offer"));
                        if (typeof d.value === "function") probes.push(() => d.value.call(recv));
                        for (const p of probes) {
                            let answer = "no-throw";
                            try { p(); } catch (err) { answer = err.constructor.name + ":" + err.message; }
                            if (answer !== "TypeError:Illegal invocation") bad.push(iface + "." + n + "=" + answer);
                        }
                    }
                }
            }
            return bad.join(",");
        })()"#
            .to_string(),
    );
    assert_eq!(got, "", "an L4 member accepted a foreign receiver");
}

// -- 5. no instance tell ---------------------------------------------------------------------------

/// Instance state is a v8 private slot, not an own property: a page walking the object sees nothing,
/// which is what the quality gate counts as an instance tell.
#[tokio::test]
async fn an_l4_instance_carries_no_own_property() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4-own",
        format!(
            r#"(() => {{
            const sd = new RTCSessionDescription({{ type: "offer", sdp: "v=0" }});
            const ic = new RTCIceCandidate({{ candidate: "{LINE}", sdpMid: "0" }});
            return [
                "sd:" + JSON.stringify(Object.getOwnPropertyNames(sd)) + JSON.stringify(Object.getOwnPropertySymbols(sd).length),
                "ic:" + JSON.stringify(Object.getOwnPropertyNames(ic)) + JSON.stringify(Object.getOwnPropertySymbols(ic).length),
            ].join("\n");
        }})()"#
        ),
    );
    assert_eq!(got.lines().collect::<Vec<_>>(), vec!["sd:[]0", "ic:[]0"]);
}

// -- 6. the two value objects' own behaviour -------------------------------------------------------

/// `RTCSessionDescription`'s legacy WRITABLE pair: the capture records a setter on both `type` and
/// `sdp`, the enum check is the binding layer's, and `toJSON` is the dictionary it was built from.
#[tokio::test]
async fn the_session_description_round_trips_its_two_writable_members() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4-sd",
        r#"(() => {
            const out = [];
            const o = new RTCSessionDescription({ type: "offer", sdp: "v=0" });
            out.push("init:" + o.type + "|" + JSON.stringify(o.sdp));
            o.type = "answer"; o.sdp = "x";
            out.push("set:" + o.type + "|" + o.sdp);
            out.push("json:" + JSON.stringify(o.toJSON()));
            try { o.type = "nope"; out.push("bad:none"); }
            catch (err) { out.push("bad:" + err.constructor.name); }
            out.push("after-bad:" + o.type);
            // A non-string `sdp` is ToString'd, not rejected.
            o.sdp = 5;
            out.push("num-sdp:" + JSON.stringify(o.sdp));
            // The default construction: a null type and an empty sdp.
            const d = new RTCSessionDescription();
            out.push("default:" + JSON.stringify([d.type, d.sdp]));
            // An explicit null `type` is NOT absent: the member is a non-nullable RTCSdpType, so null converts to the
            // string "null", which is no RTCSdpType (Chrome 150, measured 2026-10-11 -- this row had assumed absent).
            try { new RTCSessionDescription({ type: null }); out.push("null-type:none"); }
            catch (err) { out.push("null-type:" + err.constructor.name + ":" + err.message); }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            r#"init:offer|"v=0""#,
            "set:answer|x",
            r#"json:{"type":"answer","sdp":"x"}"#,
            "bad:TypeError",
            "after-bad:answer",
            r#"num-sdp:"5""#,
            "default:[null,\"\"]",
            "null-type:TypeError:Failed to construct 'RTCSessionDescription': Failed to read the 'type' property from 'RTCSessionDescriptionInit': The provided value 'null' is not a valid enum value of type RTCSdpType.",
        ]
    );
}

/// The candidate-line grammar. A line the parser accepts fills the derived fields; one it does not leaves
/// every one of them null, which is what a browser answers for the same input -- and the constructor does
/// NOT reject it, because the dictionary member is a plain DOMString.
///
/// `usernameFragment` is NOT one of the line's fields: Blink's `RTCIceCandidatePlatform::PopulateFields`
/// never sets it, so a constructed candidate's is the dictionary's (null when absent) whatever `ufrag` the
/// line carries; and `toJSON()` is `RTCIceCandidate::toJSONForBinding`, whose `AddString` writes a null
/// `sdpMid` / `usernameFragment` as `""`. Every line below is Google Chrome 150.0.7871.46's answer to this
/// exact script (2026-10-12, headed under Xvfb, no DevTools, three runs alike; PHASE 3 F7 -- the
/// expectation used to say `"Xy1"`, `sdpMid: null` and `usernameFragment: "Xy1"`, which no browser answered).
#[tokio::test]
async fn the_ice_candidate_derives_its_fields_from_the_line() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l4-ice",
        r#"(() => {
            const F = ["foundation","component","priority","address","protocol","port","type",
                       "tcpType","relatedAddress","relatedPort","usernameFragment","relayProtocol","url"];
            const dump = (o) => JSON.stringify(F.map(f => o[f]));
            const out = [];
            const relay = new RTCIceCandidate({
                candidate: "a=candidate:842163049 2 tcp 1686052607 10.0.0.5 51772 typ relay raddr 1.2.3.4 rport 9 tcptype passive ufrag Xy1",
                sdpMLineIndex: 0,
            });
            out.push("relay:" + dump(relay));
            out.push("relay-mid:" + JSON.stringify([relay.sdpMid, relay.sdpMLineIndex]));
            const junk = new RTCIceCandidate({ candidate: "not a candidate", sdpMid: "0" });
            out.push("junk:" + dump(junk));
            out.push("junk-keeps-line:" + junk.candidate);
            // The dictionary's usernameFragment, whatever the line's `ufrag` says.
            const uf = new RTCIceCandidate({ candidate: "candidate:1 1 udp 1 1.1.1.1 1 typ host ufrag AAA", sdpMid: "0", usernameFragment: "BBB" });
            out.push("ufrag:" + uf.usernameFragment);
            // `sdpMLineIndex` is an unsigned SHORT.
            const wrap = new RTCIceCandidate({ candidate: "", sdpMLineIndex: 65537 });
            out.push("wrap:" + wrap.sdpMLineIndex);
            out.push("json:" + JSON.stringify(relay.toJSON()));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            r#"relay:["842163049","rtcp",1686052607,"10.0.0.5","tcp",51772,"relay","passive","1.2.3.4",9,null,null,null]"#,
            "relay-mid:[null,0]",
            "junk:[null,null,null,null,null,null,null,null,null,null,null,null,null]",
            "junk-keeps-line:not a candidate",
            "ufrag:BBB",
            "wrap:1",
            r#"json:{"candidate":"a=candidate:842163049 2 tcp 1686052607 10.0.0.5 51772 typ relay raddr 1.2.3.4 rport 9 tcptype passive ufrag Xy1","sdpMid":"","sdpMLineIndex":0,"usernameFragment":""}"#,
        ]
    );
}
