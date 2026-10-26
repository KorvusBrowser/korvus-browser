//! LIVE one-table gate for the thirteen media-family interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against the bundle's own
//! oracle answers, that the thirteen `interface!` tables reproduce -- byte- and value-identically --
//! what `dom/08z-media-element.js`, `dom/09a-media-parts.js` and `dom/00c-media-codecs.js` installed:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` last
//!      (the hand-table shape).
//!   2. **The interface objects, the constants on both holders, and the chains.** `audio` and `video`
//!      chain through `HTMLMediaElement` to `HTMLElement`; `document.createElement(tag)` is an
//!      `instanceof` of the table's own interface object, which is what proves the tag map points at
//!      the TABLE's prototype.
//!   3. **`canPlayType`** for the codec strings the bundle's oracle and the constant-answers roster
//!      record, and the SAME answers out of `MediaSource.isTypeSupported` /
//!      `MediaRecorder.isTypeSupported` -- the three members that now share one Rust grammar.
//!   4. **The media element's state**: `buffered`/`seekable`/`played` are real `TimeRanges` instances
//!      with `length` 0 whose `start`/`end` throw the recorded `IndexSizeError`; `play()` returns a
//!      rejecting promise and `pause()` is a no-op that fires nothing; the settable numbers hold what
//!      script gives them and reject the non-finite and out-of-range values; `load()` moves
//!      `networkState` and nothing else does.
//!   5. **`<option>`**: `index`/`label`/`text`/`value`/`selected` against a real `<select>`, and
//!      `option.selected = true` moving the select's `selectedIndex`.
//!   6. **`MediaError`'s constants on both holders**, and the arity guard on `MediaCapabilities`.
//!   7. **The five members `dom/09k-media-rtc.js` used to own** -- `textTracks`, `addTextTrack`,
//!      `getVideoPlaybackQuality`, and `TextTrack`/`TextTrackList` -- still ANSWER, with the same
//!      answers. They were the install-ORDER hazard this lane's placement existed to prevent; W5.1
//!      lane L4 gave all five Rust bodies in `core/html/text_tracks.rs` that this family's rows name,
//!      so nothing reaches in after the install and the arity-guard repair pass is gone.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The thirteen this lane's tables own.
const IFACES: &[&str] = &[
    "HTMLMediaElement",
    "HTMLVideoElement",
    "HTMLAudioElement",
    "HTMLTrackElement",
    "HTMLSourceElement",
    "HTMLOptionElement",
    "TimeRanges",
    "MediaError",
    "MediaCapabilities",
    "TextTrack",
    "TextTrackList",
    "TextTrackCue",
    "VTTCue",
];

/// The six element interfaces of the thirteen and the tag that mints one.
const TAGS: &[(&str, &str)] = &[
    ("HTMLVideoElement", "video"),
    ("HTMLAudioElement", "audio"),
    ("HTMLTrackElement", "track"),
    ("HTMLSourceElement", "source"),
    ("HTMLOptionElement", "option"),
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

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        // A WebIDL constant: the capture spells it `kind: "value"`, and it is a NON-configurable,
        // non-writable, enumerable data property on both holders.
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

// -- 1. the thirteen prototypes, byte-identical to the capture ------------------------------------

#[tokio::test]
async fn every_l3_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l3-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l3_interface_object_carries_the_capture_tag_length_and_name() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l3-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        assert_eq!(got, format!("{iface}|0|0|1|{want_len}|{iface}"), "{iface} interface object != the capture");
    }
}

// -- 2. the chains, the tag map, the constructors --------------------------------------------------

#[tokio::test]
async fn every_l3_element_chains_and_owns_its_tag() {
    let mut e = page().await;
    let fx = bundle();
    for (iface, tag) in TAGS {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const chain = [];
                let p = Object.getPrototypeOf(C.prototype);
                while (p && p.constructor && p.constructor.name && p !== Object.prototype) {{ chain.push(p.constructor.name); p = Object.getPrototypeOf(p); }}
                const el = document.createElement("{tag}");
                let ctorThrew = "no-throw";
                try {{ new C(); }} catch (err) {{ ctorThrew = err.constructor.name + ":" + err.message; }}
                return [chain.join(","), el instanceof C, Object.getPrototypeOf(el) === C.prototype,
                        Object.prototype.toString.call(el), ctorThrew].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l3-chain", src);
        let want_chain: Vec<&str> =
            fx["interfaces"][iface]["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            got,
            // `new X()` on a non-newable interface object carries Chrome's construction
            // context. (Only the `new` shape is probed here; the bare call differs per interface --
            // these six are `[HTMLConstructor]` bar `HTMLMediaElement`, and `dom_spine_live` /
            // `installer_parity` are where that distinction is gated.)
            format!("{}|true|true|[object {iface}]|TypeError:Failed to construct '{iface}': Illegal constructor", want_chain.join(",")),
            "{iface} (<{tag}>) chain / tag map / constructor"
        );
    }
}

/// `<audio>` and `<video>` share `HTMLMediaElement.prototype`, and the legacy `Audio` factory is
/// still chained onto it (the registry creates `Audio` BEFORE `install_window_media`, so
/// `install::live::rechain_orphans` is what repairs the link -- if that stopped happening, `new
/// Audio()` would lose every media member).
#[tokio::test]
async fn the_media_chain_is_shared_by_audio_and_video() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-media-chain",
        r#"(() => {
            const out = [];
            const v = document.createElement("video");
            const a = document.createElement("audio");
            out.push("video:" + (v instanceof HTMLMediaElement) + ":" + (v instanceof HTMLElement) + ":" + (v instanceof Node));
            out.push("audio:" + (a instanceof HTMLMediaElement) + ":" + (a instanceof HTMLElement));
            out.push("shared:" + (Object.getPrototypeOf(HTMLVideoElement.prototype) === HTMLMediaElement.prototype) +
                     ":" + (Object.getPrototypeOf(HTMLAudioElement.prototype) === HTMLMediaElement.prototype));
            out.push("ctorchain:" + (Object.getPrototypeOf(HTMLVideoElement) === HTMLMediaElement) +
                     ":" + (Object.getPrototypeOf(HTMLMediaElement) === HTMLElement));
            out.push("audiofactory:" + (Object.getPrototypeOf(Audio.prototype) === HTMLMediaElement.prototype));
            out.push("native:" + /\[native code\]/.test(Function.prototype.toString.call(HTMLMediaElement)) +
                     ":" + /\[native code\]/.test(Function.prototype.toString.call(
                         Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "currentTime").get)));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "video:true:true:true",
            "audio:true:true",
            "shared:true:true",
            "ctorchain:true:true",
            "audiofactory:true",
            "native:true:true",
        ]
        .join("\n")
    );
}

// -- 3. the codec grammar, from all three members --------------------------------------------------

/// The four `canPlayType` cases `tests/fixtures/constant-answers.js` grades, plus the shapes the
/// deleted `dom/00c-media-codecs.js` documented; then the SAME question through
/// `MediaSource.isTypeSupported` and `MediaRecorder.isTypeSupported`, which now read the same Rust
/// grammar instead of three `KB.__codec*` bridge slots.
#[tokio::test]
async fn the_three_codec_members_answer_from_one_grammar() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-codecs",
        r#"(() => {
            const v = document.createElement("video");
            const a = document.createElement("audio");
            const out = [];
            out.push("mp4-avc1:" + v.canPlayType('video/mp4; codecs="avc1.42E01E"'));
            out.push("webm-vp8:" + v.canPlayType('video/webm; codecs="vp8"'));
            out.push("mp4-bare:" + v.canPlayType("video/mp4"));
            out.push("mp4-ambig:" + v.canPlayType('video/mp4; codecs="avc1"'));
            out.push("nope:" + JSON.stringify(v.canPlayType("application/x-not-a-type")));
            out.push("x:" + JSON.stringify(v.canPlayType("x")));
            out.push("mpeg:" + a.canPlayType("audio/mpeg"));
            out.push("ogg-vorbis:" + a.canPlayType('audio/ogg; codecs="vorbis"'));
            // <audio> and <video> answer the same question through the same row.
            out.push("same:" + (a.canPlayType('video/mp4; codecs="avc1.42E01E"') ===
                                v.canPlayType('video/mp4; codecs="avc1.42E01E"')));
            out.push("ms:" + MediaSource.isTypeSupported("x") + ":" +
                     MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"') + ":" +
                     MediaSource.isTypeSupported("audio/mpeg") + ":" +
                     MediaSource.isTypeSupported("video/mp4"));
            out.push("mr:" + MediaRecorder.isTypeSupported("x") + ":" +
                     MediaRecorder.isTypeSupported("video/webm;codecs=vp8") + ":" +
                     MediaRecorder.isTypeSupported("audio/webm;codecs=opus") + ":" +
                     MediaRecorder.isTypeSupported(""));
            // Not a constant: the grammar discriminates, which is the `constant-answers` axis.
            out.push("cardinality:" + new Set([
                v.canPlayType("video/mp4"), v.canPlayType('video/mp4; codecs="avc1.42E01E"'),
                v.canPlayType("application/x-not-a-type")]).size);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "mp4-avc1:probably",
            "webm-vp8:probably",
            "mp4-bare:maybe",
            "mp4-ambig:maybe",
            r#"nope:"""#,
            r#"x:"""#,
            "mpeg:probably",
            "ogg-vorbis:probably",
            "same:true",
            "ms:false:true:true:false",
            "mr:false:true:true:true",
            "cardinality:3",
        ]
        .join("\n")
    );
}

// -- 4. the media element's state ------------------------------------------------------------------

#[tokio::test]
async fn the_media_element_reports_the_state_of_an_element_that_loaded_nothing() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-state",
        r#"(() => {
            const v = document.createElement("video");
            const out = [];
            out.push("error:" + v.error + ":" + v.mediaKeys + ":" + v.srcObject);
            out.push("src:" + JSON.stringify(v.currentSrc) + ":" + JSON.stringify(v.sinkId));
            out.push("states:" + v.networkState + ":" + v.readyState + ":" + v.paused + ":" +
                     v.seeking + ":" + v.ended + ":" + Number.isNaN(v.duration));
            out.push("counts:" + v.webkitAudioDecodedByteCount + ":" + v.webkitVideoDecodedByteCount +
                     ":" + v.videoWidth + ":" + v.videoHeight + ":" + v.webkitDecodedFrameCount +
                     ":" + v.webkitDroppedFrameCount);
            out.push("defaults:" + v.volume + ":" + v.playbackRate + ":" + v.defaultPlaybackRate +
                     ":" + v.preservesPitch + ":" + v.muted + ":" + v.currentTime);
            // The enumerated reflections' missing AND invalid value defaults.
            out.push("preload:" + v.preload);
            v.preload = "bogus"; out.push("preload-bad:" + v.preload + ":" + v.getAttribute("preload"));
            v.preload = "AUTO"; out.push("preload-case:" + v.preload);
            out.push("loading:" + v.loading);
            out.push("crossOrigin:" + v.crossOrigin);
            v.crossOrigin = "bogus"; out.push("crossOrigin-bad:" + v.crossOrigin);
            // The settable numbers hold what script gives them.
            v.currentTime = 12.5; v.volume = 0.25; v.playbackRate = 2; v.preservesPitch = false;
            out.push("set:" + v.currentTime + ":" + v.volume + ":" + v.playbackRate + ":" + v.preservesPitch);
            let thrown = [];
            try { v.currentTime = NaN; } catch (err) { thrown.push(err.constructor.name); }
            try { v.volume = 2; } catch (err) { thrown.push(err.name + ":" + err.message); }
            out.push("reject:" + thrown.join("|"));
            // `muted` is NOT the `muted` content attribute -- that one is `defaultMuted`.
            const m = document.createElement("audio");
            m.setAttribute("muted", "");
            out.push("muted:" + m.muted + ":" + m.defaultMuted);
            m.muted = false;
            out.push("muted-dirty:" + m.muted + ":" + m.defaultMuted + ":" + m.hasAttribute("muted"));
            // srcObject stores and returns the MediaProvider it is given (anything else is the setter's
            // TypeError in Chrome 150 -- the oracle records `video.srcObject = 1` throwing).
            const o = new MediaStream();
            v.srcObject = o; out.push("srcObject:" + (v.srcObject === o));
            v.srcObject = undefined; out.push("srcObject-null:" + v.srcObject);
            // load() runs the resource selection algorithm; nothing else moves networkState.
            const n = document.createElement("video");
            out.push("network:" + n.networkState);
            n.load();
            out.push("network-loaded:" + n.networkState);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "error:null:null:null",
            r#"src:"":"""#,
            "states:0:0:true:false:false:true",
            "counts:0:0:0:0:0:0",
            "defaults:1:1:1:true:false:0",
            "preload:metadata",
            "preload-bad:metadata:bogus",
            "preload-case:auto",
            "loading:eager",
            "crossOrigin:null",
            "crossOrigin-bad:anonymous",
            "set:12.5:0.25:2:false",
            "reject:TypeError|IndexSizeError:Failed to set the 'volume' property on 'HTMLMediaElement': The volume provided (2) is outside the range [0, 1].",
            "muted:true:true",
            "muted-dirty:false:true:true",
            "srcObject:true",
            "srcObject-null:null",
            "network:0",
            "network-loaded:3",
        ]
        .join("\n")
    );
}

/// `buffered`, `played` and `seekable` are real `TimeRanges` instances with `length` 0, and the three
/// rows refuse a foreign receiver the way Chrome does.
#[tokio::test]
async fn the_time_ranges_are_real_empty_instances() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-ranges",
        r#"(() => {
            const v = document.createElement("video");
            const out = [];
            for (const n of ["buffered", "played", "seekable"]) {
                const r = v[n];
                out.push(n + ":" + (r instanceof TimeRanges) + ":" + r.length + ":" +
                         Object.prototype.toString.call(r));
            }
            // Each read hands back a fresh list, as a browser does for a live range set.
            out.push("fresh:" + (v.buffered !== v.buffered));
            let msg = "no-throw";
            try { v.buffered.start(0); } catch (err) { msg = err.name + ":" + err.message; }
            out.push("start:" + msg);
            try { v.buffered.end(0); } catch (err) { msg = err.name + ":" + err.message; }
            out.push("end:" + msg);
            // Arity: both rows are `required 1`.
            try { v.buffered.start(); } catch (err) { msg = err.constructor.name; }
            out.push("arity:" + msg);
            // A foreign receiver is an illegal invocation, not a 0.
            try { msg = String(Object.getOwnPropertyDescriptor(TimeRanges.prototype, "length").get.call({})); }
            catch (err) { msg = err.constructor.name + ":" + err.message; }
            out.push("foreign:" + msg);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "buffered:true:0:[object TimeRanges]",
            "played:true:0:[object TimeRanges]",
            "seekable:true:0:[object TimeRanges]",
            "fresh:true",
            "start:IndexSizeError:Failed to execute 'start' on 'TimeRanges': The index provided (0) is greater than or equal to the maximum bound (0).",
            "end:IndexSizeError:Failed to execute 'end' on 'TimeRanges': The index provided (0) is greater than or equal to the maximum bound (0).",
            "arity:TypeError",
            "foreign:TypeError:Illegal invocation",
        ]
        .join("\n")
    );
}

/// `play()` returns a promise, and on an unmuted element with no user activation it rejects with the
/// AUTOPLAY POLICY's `NotAllowedError` -- which Chrome asks before anything about the source (fixture
/// `M-media-play`, case `nosrc`, measured on Chrome 150). It read `NotSupportedError` while every media
/// element here was a constant with nothing loaded. `pause()` returns undefined and fires
/// nothing.
#[tokio::test]
async fn play_returns_a_promise_and_pause_fires_nothing() {
    let mut e = page().await;
    e.eval_value(
        "l3-play",
        r#"(() => {
            globalThis.__l3 = { fired: [], out: [] };
            const v = document.createElement("video");
            for (const t of ["pause", "play", "playing", "timeupdate", "ratechange", "volumechange"]) {
                v.addEventListener(t, () => globalThis.__l3.fired.push(t));
            }
            const p = v.play();
            globalThis.__l3.out.push("promise:" + (p instanceof Promise));
            p.then(() => globalThis.__l3.out.push("resolved"),
                   (err) => globalThis.__l3.out.push("rejected:" + err.name + ":" + err.message));
            globalThis.__l3.out.push("pause:" + v.pause());
            v.volume = 0.5;
            return "ok";
        })()"#
            .to_string(),
    )
    .unwrap();
    e.run().await.unwrap();
    let got = str_of(
        &mut e,
        "l3-play-read",
        r#"(globalThis.__l3.out.join("\n") + "\nfired:" + globalThis.__l3.fired.join(","))"#.to_string(),
    );
    assert_eq!(
        got,
        [
            "promise:true",
            "pause:undefined",
            "rejected:NotAllowedError:play() failed because the user didn't interact with the document first. https://goo.gl/xX8pDD",
            "fired:",
        ]
        .join("\n")
    );
}

/// `setSinkId("")` and `setMediaKeys(null)` are the two states this element genuinely is in, so both
/// resolve; anything else rejects with the spec's own error rather than with silence.
#[tokio::test]
async fn the_two_subsystem_promises_answer_for_the_state_the_element_is_in() {
    let mut e = page().await;
    e.eval_value(
        "l3-promises",
        r#"(() => {
            globalThis.__l3p = [];
            const v = document.createElement("video");
            const note = (tag) => [() => globalThis.__l3p.push(tag + ":resolved"),
                                   (err) => globalThis.__l3p.push(tag + ":" + err.name)];
            v.setSinkId("").then(...note("sink-default"));
            v.setSinkId("other").then(...note("sink-other"));
            v.setMediaKeys(null).then(...note("keys-null"));
            v.setMediaKeys({}).then(...note("keys-other"));
            return "ok";
        })()"#
            .to_string(),
    )
    .unwrap();
    e.run().await.unwrap();
    let got = str_of(&mut e, "l3-promises-read", r#"globalThis.__l3p.slice().sort().join("\n")"#.to_string());
    assert_eq!(
        got,
        ["keys-null:resolved", "keys-other:NotSupportedError", "sink-default:resolved", "sink-other:NotFoundError"].join("\n")
    );
}

/// `requestVideoFrameCallback` hands back a real, monotonic handle and never calls back (no frame is
/// ever presented); both it and `cancelVideoFrameCallback` keep their argument checks.
#[tokio::test]
async fn the_video_frame_callbacks_are_real_handles_that_never_fire() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-vfc",
        r#"(() => {
            const v = document.createElement("video");
            const out = [];
            const a = v.requestVideoFrameCallback(() => {});
            const b = v.requestVideoFrameCallback(() => {});
            out.push("monotonic:" + (typeof a === "number") + ":" + (b === a + 1));
            out.push("cancel:" + v.cancelVideoFrameCallback(a));
            const errs = [];
            try { v.requestVideoFrameCallback(); } catch (err) { errs.push(err.constructor.name); }
            try { v.requestVideoFrameCallback(1); } catch (err) { errs.push(err.constructor.name + ":" + /not of type/.test(err.message)); }
            try { v.cancelVideoFrameCallback(); } catch (err) { errs.push(err.constructor.name); }
            out.push("checks:" + errs.join("|"));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        ["monotonic:true:true", "cancel:undefined", "checks:TypeError|TypeError:true|TypeError"].join("\n")
    );
}

// -- 5. <option> -----------------------------------------------------------------------------------

#[tokio::test]
async fn the_option_element_reads_its_text_and_its_select() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-option",
        r#"(() => {
            const out = [];
            // Built with createElement/appendChild: this is the tree the algorithms walk. (Fragment
            // parsing takes its CONTEXT element as of W9.11, so the markup form works too.)
            const sel = document.createElement("select");
            const mk = (text, attrs) => {
                const o = document.createElement("option");
                o.appendChild(document.createTextNode(text));
                for (const k in attrs) o.setAttribute(k, attrs[k]);
                sel.appendChild(o);
                return o;
            };
            const a = mk("  Alpha\n  beta ", {});
            const b = mk("Bravo", { label: "B", value: "bee" });
            const c = mk("Charlie", { selected: "" });
            document.body.appendChild(sel);
            // `text` is the descendant text content, ASCII whitespace stripped and collapsed.
            out.push("text:" + JSON.stringify(a.text));
            // `label`/`value` fall back to `text` when the attribute is absent.
            out.push("fallback:" + JSON.stringify(a.label) + ":" + JSON.stringify(a.value));
            out.push("attrs:" + b.label + ":" + b.value);
            // The setters always write the attribute.
            b.label = "L"; b.value = "V";
            out.push("set:" + b.getAttribute("label") + ":" + b.getAttribute("value"));
            // `index` is the position in the select's option list; 0 for an option in no list.
            out.push("index:" + a.index + ":" + b.index + ":" + c.index);
            out.push("orphan:" + document.createElement("option").index);
            // An <option> in an <optgroup> is still in the grandparent select's list.
            const g = document.createElement("optgroup");
            const d = document.createElement("option");
            d.appendChild(document.createTextNode("Delta"));
            g.appendChild(d);
            sel.appendChild(g);
            out.push("grouped:" + d.index);
            // `selected` tracks the attribute until script assigns; `defaultSelected` is the attribute.
            out.push("selected:" + a.selected + ":" + c.selected + ":" + c.defaultSelected);
            a.selected = true;
            out.push("dirty:" + a.selected + ":" + a.defaultSelected + ":" + a.hasAttribute("selected"));
            out.push("selectedIndex:" + sel.selectedIndex);
            // `text`'s setter sets the child text content.
            a.text = "Zulu";
            out.push("settext:" + a.text + ":" + a.textContent + ":" + a.childNodes.length);
            // `form` is the <select>'s form owner.
            const f = document.createElement("form");
            document.body.appendChild(f);
            f.appendChild(sel);
            out.push("form:" + (a.form === f));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            r#"text:"Alpha beta""#,
            r#"fallback:"Alpha beta":"Alpha beta""#,
            "attrs:B:bee",
            "set:L:V",
            "index:0:1:2",
            "orphan:0",
            "grouped:3",
            "selected:false:true:true",
            "dirty:true:false:false",
            "selectedIndex:0",
            "settext:Zulu:Zulu:1",
            "form:true",
        ]
        .join("\n")
    );
}

// -- 6. MediaError's constants on both holders, and the arity guards --------------------------------

#[tokio::test]
async fn the_media_constants_are_on_both_holders_and_non_configurable() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-consts",
        r#"(() => {
            const out = [];
            const pending = globalThis.__mcPending = [];
            const pair = (C, n) => {
                const p = Object.getOwnPropertyDescriptor(C.prototype, n);
                const s = Object.getOwnPropertyDescriptor(C, n);
                return `${n}=${s.value}/${p.value}:e=${+s.enumerable}${+p.enumerable}:c=${+s.configurable}${+p.configurable}:w=${+s.writable}${+p.writable}`;
            };
            for (const n of ["MEDIA_ERR_ABORTED", "MEDIA_ERR_NETWORK", "MEDIA_ERR_DECODE", "MEDIA_ERR_SRC_NOT_SUPPORTED"]) {
                out.push(pair(MediaError, n));
            }
            out.push(pair(HTMLMediaElement, "NETWORK_EMPTY"));
            out.push(pair(HTMLMediaElement, "NETWORK_NO_SOURCE"));
            out.push(pair(HTMLMediaElement, "HAVE_ENOUGH_DATA"));
            out.push(pair(HTMLTrackElement, "ERROR"));
            // The oracle records `new MediaError()` and `new TimeRanges()` as TypeErrors.
            const threw = (f) => { try { f(); return "no-throw"; } catch (err) { return err.constructor.name; } };
            out.push("newable:" + threw(() => new MediaError()) + ":" + threw(() => new TimeRanges()) +
                     ":" + threw(() => new MediaCapabilities()) + ":" + threw(() => new TextTrack()));
            // MediaCapabilities' two rows are `required 1` AND `Promise`-returning, so the arity
            // refusal is a REJECTION, not a throw -- WebIDL's rule for a promise-returning operation.
            // Measured in Google Chrome 150.0.7871.46 `--headless=new`, 2026-09-23, awaiting each:
            //   navigator.mediaCapabilities.decodingInfo()  REJECTED TypeError: Failed to execute
            //     'decodingInfo' on 'MediaCapabilities': 1 argument required, but only 0 present.
            //   encodingInfo()                              REJECTED, same sentence
            // This line asserted a synchronous THROW until TY2, which is what a row typed `Object`
            // instead of `Promise` produced. `threw()` cannot see a rejection, so the promise is
            // stashed and awaited by the caller; "promise" here only means "did not throw and did
            // not answer a value", and the REJECTION is what is actually asserted, below.
            const refused = (f) => {
                let v;
                try { v = f(); } catch (err) { return "throw:" + err.constructor.name; }
                if (v && typeof v.then === "function") { pending.push(v); return "promise"; }
                return "value:" + Object.prototype.toString.call(v);
            };
            out.push("arity:" + refused(() => navigator.mediaCapabilities.decodingInfo()) + ":" +
                     refused(() => navigator.mediaCapabilities.encodingInfo()));
            // VTTCue is the one newable of the thirteen.
            const cue = new VTTCue(0, 1, "hi");
            out.push("vttcue:" + (cue instanceof VTTCue) + ":" + (cue instanceof TextTrackCue) + ":" +
                     Object.prototype.toString.call(cue));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "MEDIA_ERR_ABORTED=1/1:e=11:c=00:w=00",
            "MEDIA_ERR_NETWORK=2/2:e=11:c=00:w=00",
            "MEDIA_ERR_DECODE=3/3:e=11:c=00:w=00",
            "MEDIA_ERR_SRC_NOT_SUPPORTED=4/4:e=11:c=00:w=00",
            "NETWORK_EMPTY=0/0:e=11:c=00:w=00",
            "NETWORK_NO_SOURCE=3/3:e=11:c=00:w=00",
            "HAVE_ENOUGH_DATA=4/4:e=11:c=00:w=00",
            "ERROR=3/3:e=11:c=00:w=00",
            "newable:TypeError:TypeError:TypeError:TypeError",
            "arity:promise:promise",
            "vttcue:true:true:[object VTTCue]",
        ]
        .join("\n")
    );

    // AND THE HALF THE LINE ABOVE CANNOT SEE: both promises must REJECT with the arity `TypeError`,
    // whole sentence included, because a page reads `err.message`. A refusal that RESOLVES is not a
    // refusal -- scoring `[object Promise]` alone is exactly how a resolved refusal passed for a
    // rejection elsewhere in this tree (CONF TY1.3).
    let settled = e
        .eval_value_settled(
            "l3-consts-settle",
            r#"(async () => {
                const s = await Promise.allSettled(globalThis.__mcPending);
                return s.map(x => x.status === "rejected"
                    ? "reject:" + x.reason.constructor.name + ":" + x.reason.message
                    : "RESOLVE:" + Object.prototype.toString.call(x.value)).join("\n");
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
        [
            "reject:TypeError:Failed to execute 'decodingInfo' on 'MediaCapabilities': 1 argument required, but only 0 present.",
            "reject:TypeError:Failed to execute 'encodingInfo' on 'MediaCapabilities': 1 argument required, but only 0 present.",
        ]
        .join("\n"),
        "a promise-returning arity refusal must REJECT the TypeError, not resolve and not throw"
    );
}

// -- 7. the five members that were dom/09k-media-rtc.js's ------------------------------------------

/// THE INSTALL-ORDER GATE, and now also the proof that the order no longer matters for these five.
///
/// `install_window_media` runs from `js/generated_shells.js`, BEFORE `dom/09k-media-rtc.js` -- the last
/// ESM module -- because 09k replaced these five bodies in place: `HTMLMediaElement`'s `textTracks`
/// and `addTextTrack`, `HTMLVideoElement.getVideoPlaybackQuality`, and `TextTrack` / `TextTrackList`.
/// W5.1 lane L4 moved all five into `core/html/text_tracks.rs` and this family's table rows now name
/// them, so no later pass reaches in -- and the ANSWERS below are unchanged, which is what makes that
/// move a cutover rather than a rewrite. The assertions are byte-for-byte what they were when 09k
/// answered them.
#[tokio::test]
async fn the_members_dom_09k_used_to_own_keep_answering() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-09k",
        r#"(() => {
            const v = document.createElement("video");
            const out = [];
            const tt = v.textTracks;
            out.push("textTracks:" + (tt instanceof TextTrackList) + ":" + tt.length);
            out.push("same:" + (v.textTracks === tt));
            const t = v.addTextTrack("captions", "L", "en");
            out.push("addTextTrack:" + (t instanceof TextTrack) + ":" + t.kind + ":" + t.label + ":" +
                     t.language + ":" + t.mode + ":" + v.textTracks.length);
            out.push("getTrackById:" + v.textTracks.getTrackById("nope"));
            // The row is `required 1`. 09k used to replace the body with a BARE native function,
            // which dropped the guard `installer::method_row` had put on -- and
            // `install::live::finalize_window_l3` existed to put it back. The row now carries its own
            // body, so the guard is never dropped and that pass is gone; this case is what measures
            // it, together with `tests/arity_guards.rs`.
            let arity = "no-throw";
            try { v.textTracks.getTrackById(); } catch (err) { arity = err.constructor.name; }
            out.push("getTrackById-arity:" + arity);
            try { v.addTextTrack(); } catch (err) { arity = err.constructor.name; }
            out.push("addTextTrack-arity:" + arity);
            out.push("cues:" + (t.cues !== null) + ":" + t.cues.length);
            t.mode = "showing";
            out.push("mode:" + t.mode);
            let msg = "no-throw";
            try { t.mode = "bogus"; } catch (err) { msg = err.constructor.name; }
            out.push("mode-bad:" + msg);
            const q = v.getVideoPlaybackQuality();
            out.push("quality:" + (typeof q.creationTime === "number") + ":" + q.totalVideoFrames + ":" +
                     q.droppedVideoFrames + ":" + q.corruptedVideoFrames);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "textTracks:true:0",
            "same:true",
            "addTextTrack:true:captions:L:en:hidden:1",
            "getTrackById:null",
            "getTrackById-arity:TypeError",
            "addTextTrack-arity:TypeError",
            "cues:true:0",
            "mode:showing",
            "mode-bad:TypeError",
            "quality:true:0:0:0",
        ]
        .join("\n")
    );
}

/// `<track>` and `<source>`: the reflections, `<track kind>`'s two different defaults, and the
/// readyState that never leaves NONE.
#[tokio::test]
async fn the_track_and_source_elements_reflect_what_the_recording_says() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l3-parts",
        r#"(() => {
            const out = [];
            const t = document.createElement("track");
            // MISSING value default "subtitles"; INVALID value default "metadata". They differ.
            out.push("kind:" + t.kind);
            t.kind = "x"; out.push("kind-bad:" + t.kind + ":" + t.getAttribute("kind"));
            t.kind = "CAPTIONS"; out.push("kind-case:" + t.kind);
            t.srclang = "en"; t.label = "L"; t.default = true;
            out.push("refl:" + t.srclang + ":" + t.label + ":" + t.default + ":" + t.hasAttribute("default"));
            out.push("readyState:" + t.readyState + ":" + HTMLTrackElement.NONE);
            const s = document.createElement("source");
            s.type = "video/mp4"; s.srcset = "a 1x"; s.sizes = "100vw"; s.media = "(min-width: 1px)";
            out.push("source:" + s.type + ":" + s.srcset + ":" + s.sizes + ":" + s.media);
            out.push("dims:" + s.width + ":" + s.height);
            s.width = 640; s.height = 480;
            out.push("dims-set:" + s.width + ":" + s.height + ":" + s.getAttribute("width"));
            // [ReflectURL]: the IDL value is the resolved URL, the attribute is what was written.
            s.setAttribute("src", "https://example.test/clip.mp4");
            out.push("url:" + s.src);
            t.setAttribute("src", "https://example.test/c.vtt");
            out.push("track-url:" + t.src);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "kind:subtitles",
            "kind-bad:metadata:x",
            "kind-case:captions",
            "refl:en:L:true:true",
            "readyState:0:0",
            "source:video/mp4:a 1x:100vw:(min-width: 1px)",
            "dims:0:0",
            "dims-set:640:480:640",
            "url:https://example.test/clip.mp4",
            "track-url:https://example.test/c.vtt",
        ]
        .join("\n")
    );
}
