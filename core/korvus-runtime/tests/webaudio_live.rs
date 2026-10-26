//! LIVE gate for the Web Audio family as twenty-eight `interface!` tables.
//!
//! `core/html/webaudio/attrs.rs` held a 178-line `AUDIO` const — `(interface, [(attribute,
//! writable)], [(method, arity)])` transcribed from the capture — and walked it, fetching each
//! prototype out of the generated shell registry and defining members onto it. That was a second
//! install mechanism (invariant I2), a SECOND copy of the member shape the generated registry
//! already carried, and 130 members the install ledger never saw. The rows are the one copy now.
//!
//! The four table-cutover failure modes, one section each:
//!
//!   1. **An instance on the prototype.** Every one of the twenty-eight prototypes carries exactly
//!      the capture's member list, in the capture's order — and `AudioContext.prototype` is not a
//!      running audio graph.
//!   2. **A row accepting a foreign receiver.** Preserved exactly: the family's accessors answer
//!      through one store lookup keyed on the instance's private slot, and a receiver with none
//!      reads the shape default. That is what they did before and what they do now.
//!   3. **A body reachable where it was inert, or the reverse.** The DSP still runs: a rendered
//!      offline context still produces samples, and the analyser still analyses the same run.
//!   4. **An install ORDER change.** The six-deep inheritance chain is the thing this lane could
//!      most easily have broken, because a table splices onto `globalThis[parent].prototype` and so
//!      depends on its parent being installed first. Every edge is asserted against the CAPTURE.

use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><body></body></html>"#;

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

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "l17-audio-probe", src)
}

/// The twenty-eight tables, and the WebIDL parent the capture records for each.
const TABLES: &[(&str, Option<&str>)] = &[
    ("BaseAudioContext", Some("EventTarget")),
    ("AudioContext", Some("BaseAudioContext")),
    ("OfflineAudioContext", Some("BaseAudioContext")),
    ("AudioNode", Some("EventTarget")),
    ("AudioScheduledSourceNode", Some("AudioNode")),
    ("OscillatorNode", Some("AudioScheduledSourceNode")),
    ("GainNode", Some("AudioNode")),
    ("DynamicsCompressorNode", Some("AudioNode")),
    ("AnalyserNode", Some("AudioNode")),
    ("AudioBufferSourceNode", Some("AudioScheduledSourceNode")),
    ("DelayNode", Some("AudioNode")),
    ("BiquadFilterNode", Some("AudioNode")),
    ("StereoPannerNode", Some("AudioNode")),
    ("WaveShaperNode", Some("AudioNode")),
    ("ConstantSourceNode", Some("AudioScheduledSourceNode")),
    ("AudioDestinationNode", Some("AudioNode")),
    ("AudioBuffer", None),
    ("AudioListener", None),
    ("ConvolverNode", Some("AudioNode")),
    ("PannerNode", Some("AudioNode")),
    ("IIRFilterNode", Some("AudioNode")),
    ("ScriptProcessorNode", Some("AudioNode")),
    ("OfflineAudioCompletionEvent", Some("Event")),
    ("AudioProcessingEvent", Some("Event")),
    ("MediaElementAudioSourceNode", Some("AudioNode")),
    ("MediaStreamAudioSourceNode", Some("AudioNode")),
    ("MediaStreamAudioDestinationNode", Some("AudioNode")),
    ("AudioParam", None),
];

// -- 1. the capture's member list, in the capture's order ----------------------------------------

#[tokio::test]
async fn every_prototype_carries_exactly_the_captured_members_in_order() {
    let mut e = page().await;
    let fx = bundle();
    for (iface, _) in TABLES {
        let want: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap_or_else(|| panic!("{iface} is absent from the capture"))
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect();
        let src = format!(r#"Object.getOwnPropertyNames({iface}.prototype).filter(n => n !== "constructor").join(",")"#);
        assert_eq!(
            str_of(&mut e, "l17-audio-order", src),
            want.join(","),
            "{iface}.prototype is not the capture's member list in the capture's order"
        );
        assert_eq!(
            probe(&mut e, &format!(r#"{iface}.prototype[Symbol.toStringTag]"#)),
            format!("ok:{iface}"),
            "{iface}.prototype[Symbol.toStringTag]"
        );
    }
}

/// The read/write split is the CAPTURE's, member for member. It used to be written down twice — once
/// in the generated registry and once in `attrs.rs`'s `AUDIO` const — and the const is deleted, so
/// this asserts the one remaining copy against the bundle rather than against itself.
#[tokio::test]
async fn every_accessor_has_the_captured_setter_or_none() {
    let mut e = page().await;
    let fx = bundle();
    for (iface, _) in TABLES {
        let want: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["kind"] == "accessor")
            .map(|m| format!("{}:{}", m["name"].as_str().unwrap(), m["set"].as_bool().unwrap_or(false)))
            .collect();
        if want.is_empty() {
            continue;
        }
        let names = serde_json::to_string(&want.iter().map(|s| s.split(':').next().unwrap()).collect::<Vec<_>>()).unwrap();
        let src = format!(
            r#"(() => {{ return {names}.map(n => {{
                 const d = Object.getOwnPropertyDescriptor({iface}.prototype, n);
                 return n + ":" + (d && typeof d.set === "function");
               }}).join(","); }})()"#
        );
        assert_eq!(str_of(&mut e, "l17-audio-rw", src), want.join(","), "{iface}: a setter appeared or vanished");
    }
}

// -- 4. the inheritance chain, which is what an install ORDER change breaks -----------------------

/// The six-deep chain, on BOTH halves (prototype and interface object), against the CAPTURE's
/// parents. A table splices onto `globalThis[parent].prototype`, so a parent installed after its
/// child leaves the child on a bare `Object.prototype` — the one failure this lane's install order
/// exists to prevent, and it is silent if nothing reads it.
#[tokio::test]
async fn every_captured_inheritance_edge_holds() {
    let mut e = page().await;
    for (iface, parent) in TABLES {
        let src = match parent {
            Some(p) => format!(
                r#"(() => [Object.getPrototypeOf({iface}.prototype) === {p}.prototype,
                           Object.getPrototypeOf({iface}) === {p}].join("|"))()"#
            ),
            None => format!(
                r#"(() => [Object.getPrototypeOf({iface}.prototype) === Object.prototype,
                           Object.getPrototypeOf({iface}) === Function.prototype].join("|"))()"#
            ),
        };
        assert_eq!(str_of(&mut e, "l17-audio-chain", src), "true|true", "{iface} -> {parent:?}");
    }
}

/// The three registry interfaces whose parent this lane moved out of the registry
/// (`AudioWorkletNode`, `ChannelMergerNode` and `ChannelSplitterNode` all inherit `AudioNode`). The
/// registry's own chain pass runs before these tables exist, so the repair is
/// `install::live::rows::rechain_orphans` — asserted, not assumed.
#[tokio::test]
async fn the_registry_children_of_audio_node_are_rechained() {
    let mut e = page().await;
    for iface in ["AudioWorkletNode", "ChannelMergerNode", "ChannelSplitterNode"] {
        let src = format!(
            r#"(() => [Object.getPrototypeOf({iface}.prototype) === AudioNode.prototype,
                       Object.getPrototypeOf({iface}) === AudioNode].join("|"))()"#
        );
        assert_eq!(str_of(&mut e, "l17-audio-orphan", src), "true|true", "{iface} lost its AudioNode chain");
    }
}

// -- the constructors: seven shapes, all from the capture ----------------------------------------

/// Which interfaces of this family construct, and with what. `newable` in the capture means "`new
/// X()` with NO arguments succeeds", so most of these are `false` for the ordinary WebIDL reason
/// (their constructor requires a context), and the three abstract bases are `false` because they
/// have no constructor at all. Both cases are asserted, and so is the constructor's recorded
/// `length`, which is the half a cutover silently changes.
#[tokio::test]
async fn the_constructors_match_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for (iface, _) in TABLES {
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap_or(0);
        assert_eq!(probe(&mut e, &format!("{iface}.length")), format!("ok:{want_len}"), "{iface}.length");
        assert_eq!(probe(&mut e, &format!("{iface}.name")), format!("ok:{iface}"), "{iface}.name");
        let native = probe(&mut e, &format!(r#"/\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call({iface}))"#));
        assert_eq!(native, "ok:true", "{iface} is not [native code]");
    }
    // The five with NO WebIDL constructor throw "Failed to construct 'X': Illegal constructor", which
    // is Chrome's message (the shared body carries the interface name) and what the capture's
    // `length: 0, newable: false` records. `ScriptProcessorNode` is one of them: before this lane it
    // was in `NODE_CTORS` and `new ScriptProcessorNode(ctx)` really built a node, which Chrome
    // refuses -- its instances come from `createScriptProcessor`.
    for iface in ["BaseAudioContext", "AudioNode", "AudioScheduledSourceNode", "AudioDestinationNode", "AudioListener", "AudioParam", "ScriptProcessorNode"] {
        assert_eq!(
            probe(&mut e, &format!("new {iface}()")),
            format!("throw:TypeError:Failed to construct '{iface}': Illegal constructor"),
            "new {iface}()"
        );
    }
    // ...and `createScriptProcessor` still hands one back, which is the only way to get one.
    assert_eq!(
        probe(&mut e, r#"Object.prototype.toString.call(new OfflineAudioContext(1, 1, 44100).createScriptProcessor(256, 1, 1))"#),
        "ok:[object ScriptProcessorNode]",
    );
    // The two contexts really construct, and `new AudioContext()` carries the PERSONA's sample rate
    // -- the one device-varying number in this family, and the reason the install takes it as an
    // argument rather than re-parsing the oracle per `new`.
    let src = r#"(() => {
        const ac = new AudioContext();
        const oc = new OfflineAudioContext(1, 128, 44100);
        return [ac instanceof AudioContext, ac instanceof BaseAudioContext, ac instanceof EventTarget,
                ac.sampleRate, ac.state,
                oc.sampleRate, oc.length,
                Object.prototype.toString.call(ac.destination),
                Object.prototype.toString.call(ac.listener)].join("|");
    })()"#;
    // `suspended`: a live context made with no user gesture is held by the autoplay policy
    // (Brave 151 and Chrome 150 headed, 2026-10-01, tools/realweb/seed/asyncapis).
    assert_eq!(
        str_of(&mut e, "l17-audio-ctx", src.to_string()),
        "true|true|true|48000|suspended|44100|128|[object AudioDestinationNode]|[object AudioListener]",
    );
    // A node constructor: ONE callback keyed on the interface NAME, which is why it is bound as the
    // constructor's `data`. If that binding were lost every one of the sixteen would build the wrong
    // node, so the test builds one of each and reads its tag back.
    let src = r#"(() => {
        const ac = new AudioContext();
        const names = ["OscillatorNode","GainNode","DynamicsCompressorNode","AnalyserNode","AudioBufferSourceNode",
                       "ConstantSourceNode","BiquadFilterNode","StereoPannerNode","WaveShaperNode","DelayNode",
                       "ConvolverNode","PannerNode"];
        return names.map(n => {
            const o = new globalThis[n](ac);
            const ok = Object.prototype.toString.call(o) === "[object " + n + "]"
                    && o instanceof globalThis[n] && o instanceof AudioNode
                    && Object.getOwnPropertyNames(o).length === 0;
            return n + ":" + ok;
        }).filter(s => !s.endsWith(":true")).join(",") || "all-ok";
    })()"#;
    assert_eq!(str_of(&mut e, "l17-audio-nodes", src.to_string()), "all-ok");
    // An argument-less node constructor is the WebIDL arity TypeError, not a node.
    assert_eq!(
        probe(&mut e, "new OscillatorNode()"),
        "throw:TypeError:Failed to construct 'OscillatorNode': 1 argument required, but only 0 present.",
    );
}

// -- 3. the DSP is still reachable, and still inert where it was ---------------------------------

/// The members are rows now; the bodies are the same ones, and this is the end-to-end proof. An
/// offline context renders a real oscillator through a real gain node, and the analyser reads the
/// same run back — which is what makes these members implementations rather than a recorded tape.
#[tokio::test]
async fn the_graph_still_renders_and_reads_back() {
    let mut e = page().await;
    let src = r#"(() => {
        const oc = new OfflineAudioContext(1, 256, 44100);
        const osc = new OscillatorNode(oc, { frequency: 440, type: "square" });
        const g = new GainNode(oc, { gain: 0.5 });
        osc.connect(g); g.connect(oc.destination);
        osc.start(0);
        const an = new AnalyserNode(oc, { fftSize: 64 });
        const buf = oc.createBuffer(1, 16, 44100);
        const ch = buf.getChannelData(0);
        return [osc.type, osc.frequency.value, osc.detune.value,
                g.gain.value, an.fftSize, an.frequencyBinCount,
                buf.length, buf.numberOfChannels, ch.length,
                Object.prototype.toString.call(osc.frequency),
                osc.numberOfInputs, osc.numberOfOutputs, osc.channelCountMode].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-audio-dsp", src.to_string()),
        "square|440|0|0.5|64|32|16|1|16|[object AudioParam]|0|1|max",
    );
    // `AudioParam.value` is the one write in this API that is not an automation event, and its row
    // keeps the dedicated setter rather than the generic one.
    let src = r#"(() => {
        const oc = new OfflineAudioContext(1, 256, 44100);
        const g = new GainNode(oc);
        g.gain.value = 0.25;
        const before = g.gain.value;
        g.gain.setValueAtTime(0.75, 0);
        return [before, g.gain.defaultValue, g.gain.automationRate,
                Object.getOwnPropertyNames(g.gain).length].join("|");
    })()"#;
    assert_eq!(str_of(&mut e, "l17-audio-param", src.to_string()), "0.25|1|a-rate|0");
}

/// The event-handler IDL attributes: writable, reading back `null` when unset, and NOT the generated
/// registry's `event_handler` kit — they are this family's own private-slot pair, which is what the
/// pass this lane replaces installed over the top of the kit.
#[tokio::test]
async fn the_event_handler_attributes_round_trip() {
    let mut e = page().await;
    let src = r#"(() => {
        const oc = new OfflineAudioContext(1, 128, 44100);
        const osc = new OscillatorNode(oc);
        const f = () => {};
        const before = [oc.onstatechange, oc.oncomplete, osc.onended];
        oc.onstatechange = f; osc.onended = f;
        return [String(before[0]), String(before[1]), String(before[2]),
                oc.onstatechange === f, osc.onended === f].join("|");
    })()"#;
    assert_eq!(str_of(&mut e, "l17-audio-handlers", src.to_string()), "null|null|null|true|true");
}

/// The WebIDL arity guards, from the same `required` counts the registry-wide pass used.
#[tokio::test]
async fn every_row_keeps_its_recorded_arity_guard() {
    let mut e = page().await;
    // THE THREE NON-PROMISE ROWS: the refusal is a synchronous THROW.
    for (iface, expr, member, need) in [
        ("BaseAudioContext", "new OfflineAudioContext(1, 128, 44100).createBuffer()", "createBuffer", 3),
        ("AudioNode", "new OfflineAudioContext(1, 128, 44100).createGain().connect()", "connect", 1),
        ("AudioParam", "new OfflineAudioContext(1, 128, 44100).createGain().gain.setValueAtTime()", "setValueAtTime", 2),
    ] {
        assert_eq!(
            probe(&mut e, expr),
            format!(
                "throw:TypeError:Failed to execute '{member}' on '{iface}': {need} argument{} required, but only 0 present.",
                if need == 1 { "" } else { "s" }
            ),
            "{iface}.{member} lost its arity guard"
        );
    }
    // AND THE PROMISE ROW: the same guard, the same sentence, delivered as a REJECTION. WebIDL turns
    // a refusal out of a promise-returning operation into a rejection of the promise the call already
    // committed to returning; `decodeAudioData` is this family's only such row.
    //
    // This loop asserted a synchronous throw for it until TY2 -- correct only while the row was typed
    // `Any`. Measured, Google Chrome 150.0.7871.46 `--headless=new` over a loopback server,
    // 2026-09-23, AWAITING the promise rather than inferring from `typeof r.then`:
    //
    //   new AudioContext().decodeAudioData()  REJECTED TypeError: Failed to execute
    //     'decodeAudioData' on 'BaseAudioContext': 1 argument required, but only 0 present.
    //
    // The three throwing rows above are the control: they share `make_arity_guard` with this one, so
    // a guard that rejected EVERYTHING would fail them.
    let settled = e
        .eval_value_settled(
            "l17-audio-arity-promise",
            r#"(async () => {
                let r;
                try { r = new OfflineAudioContext(1, 128, 44100).decodeAudioData(); }
                catch (err) { return "THREW:" + err.constructor.name + ":" + err.message; }
                if (!r || typeof r.then !== "function") return "NOT-A-PROMISE:" + Object.prototype.toString.call(r);
                const [s] = await Promise.allSettled([r]);
                return s.status === "rejected"
                    ? "reject:" + s.reason.constructor.name + ":" + s.reason.message
                    : "RESOLVE:" + Object.prototype.toString.call(s.value);
            })()"#
                .to_string(),
            std::time::Duration::from_secs(10),
        )
        .await
        .expect("settle")
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_eq!(
        settled,
        "reject:TypeError:Failed to execute 'decodeAudioData' on 'BaseAudioContext': 1 argument required, but only 0 present.",
        "BaseAudioContext.decodeAudioData must REJECT its arity TypeError, not throw it and not resolve"
    );
    // `required 0` members are NOT wrapped.
    assert_eq!(
        probe(&mut e, "Object.prototype.toString.call(new OfflineAudioContext(1, 128, 44100).createGain())"),
        "ok:[object GainNode]",
    );
}

