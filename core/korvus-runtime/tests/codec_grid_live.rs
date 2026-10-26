//! THE ENGINE ANSWERS THE WORN DEVICE'S CODEC GRID — every recorded question, every bundle (P4b).
//!
//! # What this gates, and what it deliberately does not
//!
//! For each bundle in `plugins/korvus-persona/bundles/`, this boots the engine wearing that bundle
//! and asks it — in a real page, through `document.createElement("video").canPlayType`,
//! `MediaSource.isTypeSupported` and `MediaRecorder.isTypeSupported` — every one of the **9,104**
//! questions that bundle's own `codecs.json` records an answer to, and requires the engine's answer
//! to equal the recorded one. Not a sample, not a list kept here: the questions and the answers are
//! both read off the bundle (derived, never counted), so a capture with a wider sweep widens
//! this gate by being dropped in.
//!
//! **IT IS A GATE ON A CLAIM, NOT ON PLAYBACK** (ruling R7, stated here so a green run is not misread
//! later). This engine has no demuxer and no decoder. `readyState` is `HAVE_NOTHING` everywhere,
//! `play()` rejects, and none of that changes because this file is green. `canPlayType` has always
//! been the recorded browser's answer rather than a statement about this process; what P4b changed is
//! WHICH recorded browser's, and this is the measurement of that.
//!
//! # Why it has to exist, measured
//!
//! The grid was one compile-time constant derived from the SHIPPED bundle
//! (`generated_media_codecs.rs`, from `tools/sync/sync.py`'s `--from-bundle shipped` line), so the
//! engine answered Brave 151's codecs whichever device it wore. Over the four captures in this tree:
//!
//! ```text
//!                        answers differing from the shipped bundle (brave-151)
//!   chrome-150             0
//!   edge-151              16    the audio/3gpp family, "" -> maybe / probably
//!   opera-135            274    H.264 + AAC, probably -> "" ; 60 of them MediaSource /
//!                               MediaRecorder booleans, true -> false
//! ```
//!
//! Opera's Linux build genuinely ships without the proprietary H.264/AAC decoders, so those 274 were
//! the engine telling a page it could play `avc1.42E01E` while wearing a device that cannot — the
//! largest single hard-coded difference this goal has measured, and one no gate saw.
//!
//! # The audio element is asked too, on purpose
//!
//! The grid records `<video>.canPlayType` and `<audio>.canPlayType` separately, and this asks both —
//! against ONE engine member, because `canPlayType` is one row of `HTMLMediaElement` and both
//! elements inherit it. In all four captures the two answers are identical for all 2,276 questions,
//! so that is correct today and is asserted rather than assumed. A future capture on which they
//! differ turns this gate red, which is exactly the signal that the member has to be split — much
//! better than the silence a video-only gate would give.

use std::collections::BTreeMap;

use engine::Engine;
use serde_json::{json, Value};

// ── the bundles, and their recorded grids ────────────────────────────────────────────────────────

fn bundles_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles")
}

/// Every bundle directory that carries a manifest, sorted. The denominator is the tree, not a list.
fn bundles() -> Vec<String> {
    let dir = bundles_dir();
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no persona bundle in the tree");
    out
}

/// One recorded question: which member was asked, and the exact MIME string it was asked with.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Question {
    /// `v` = `<video>.canPlayType`, `a` = `<audio>.canPlayType`, `s` = `MediaSource`, `r` = `MediaRecorder`.
    member: char,
    mime: String,
}

/// Rebuild the question a grid key encodes.
///
/// The key format is `codecs.mjs`'s and is documented there: `<member>|<shape> :: <part> :: <part>`,
/// where the shape says how the parts make a MIME string. This is a PARSE OF THE KEY, not a second
/// copy of the question lists — which containers and codecs were asked is whatever the file contains.
fn question_of(key: &str) -> Option<Question> {
    let (head, rest) = key.split_once('|')?;
    let member = match head {
        "v" | "a" | "s" | "r" => head.chars().next()?,
        _ => return None,
    };
    let parts: Vec<&str> = rest.split(" :: ").collect();
    let mime = match parts.as_slice() {
        // the container on its own
        ["base", c] => (*c).to_string(),
        // a container and one codec, or a container and a codec LIST
        ["pair" | "combo", c, k] => format!("{c}; codecs=\"{k}\""),
        // a spelling test: the third field IS the MIME string, verbatim, quoting and all
        ["quote", _tag, m] => (*m).to_string(),
        _ => return None,
    };
    Some(Question { member, mime })
}

/// One recorded row: the grid key it came from, the question it encodes, and the device's answer.
///
/// KEYED BY THE GRID KEY AND NOT BY THE QUESTION, because the sweep deliberately asks some strings
/// twice: `pair :: video/webm :: vp8` and `quote :: quoted :: video/webm; codecs="vp8"` are the same
/// MIME string reached two ways, and the second exists to pin the parameter QUOTING. Collapsing them
/// would silently shrink the denominator from the 9,104 answers the capture recorded, so each
/// recorded answer is one row and duplicates are asked — and checked — twice.
#[derive(Debug, Clone)]
struct Row {
    key: String,
    question: Question,
    answer: Value,
}

/// One bundle's recorded grid, in key order, read out of its own directory.
fn recorded(id: &str) -> Vec<Row> {
    let path = bundles_dir().join(id).join("codecs.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    assert_eq!(
        raw.get("source").and_then(Value::as_str),
        Some(id),
        "{id}: the grid must name the capture it came from"
    );
    let answers = raw.get("answers").and_then(Value::as_object).unwrap_or_else(|| {
        panic!("{}: the grid has no `answers` object", path.display())
    });
    // BTreeMap ordering, so two bundles' row lists are comparable position by position whatever
    // order their files happen to be written in.
    let mut keys: Vec<&String> = answers.keys().collect();
    keys.sort();
    let mut out = Vec::with_capacity(keys.len());
    for k in keys {
        let Some(question) = question_of(k) else {
            panic!("{id}: grid key {k:?} is not a question this gate can reconstruct");
        };
        let answer = answers[k].get("v").cloned().unwrap_or(Value::Null);
        assert!(!answer.is_null(), "{id}: {k:?} records no answer");
        out.push(Row { key: k.clone(), question, answer });
    }
    assert!(out.len() > 9000, "{id}: only {} answers -- this capture did not run the grid", out.len());
    out
}

// ── the engine half ──────────────────────────────────────────────────────────────────────────────

/// Ask one booted engine every question, in one eval.
///
/// ONE ROUND TRIP, because 9,104 `eval_value` calls cost more than the four boots put together. The
/// questions go in as a JSON literal and the answers come back in the same order, so nothing here
/// has to agree with anything about WHICH questions were asked.
async fn ask(id: &str, questions: &[Question]) -> Vec<Value> {
    let mut e = Engine::new(persona::worn_on_host(
        persona::CapturedPersona::select(Some(id)).unwrap_or_else(|err| panic!("select {id}: {err}")),
    ));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><head><title>t</title></head><body>x</body></html>").unwrap();
    e.run().await.unwrap();

    let payload: Vec<Value> =
        questions.iter().map(|q| json!([q.member.to_string(), q.mime])).collect();
    let program = format!(
        r#"JSON.stringify((() => {{
  const Q = {};
  const v = document.createElement("video");
  const a = document.createElement("audio");
  const out = [];
  for (let i = 0; i < Q.length; i++) {{
    const k = Q[i][0], m = Q[i][1];
    try {{
      out.push(k === "v" ? v.canPlayType(m)
             : k === "a" ? a.canPlayType(m)
             : k === "s" ? MediaSource.isTypeSupported(m)
             :             MediaRecorder.isTypeSupported(m));
    }} catch (err) {{ out.push("THREW: " + err.message); }}
  }}
  return out;
}})())"#,
        serde_json::to_string(&payload).unwrap()
    );

    let raw = e.eval_value("codec-grid-probe", program).unwrap();
    let got: Vec<Value> = serde_json::from_str(
        raw.as_str().unwrap_or_else(|| panic!("{id}: the probe must return a JSON string, got {raw}")),
    )
    .unwrap_or_else(|err| panic!("{id}: the probe's JSON must parse: {err}"));
    assert_eq!(got.len(), questions.len(), "{id}: the probe answered a different number of questions");
    got
}

// ── the gate ─────────────────────────────────────────────────────────────────────────────────────

/// EVERY BUNDLE, EVERY RECORDED QUESTION: the engine's answer equals that device's own.
#[tokio::test]
async fn every_bundle_answers_its_own_recorded_codec_grid() {
    let ids = bundles();
    let mut engine_answers: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut capture_answers: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut all_keys: Option<Vec<String>> = None;
    let mut questions: Vec<Question> = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();

    for id in &ids {
        let grid = recorded(id);
        let keys: Vec<String> = grid.iter().map(|r| r.key.clone()).collect();
        // THE FOUR DEVICES WERE ASKED THE SAME THINGS. Without this, a bundle whose grid is short
        // would be graded on fewer answers and still pass -- and the pairwise check below compares
        // answer vectors position by position, which needs one key order.
        match &all_keys {
            None => {
                questions = grid.iter().map(|r| r.question.clone()).collect();
                all_keys = Some(keys);
            }
            Some(first) => assert_eq!(
                &keys, first,
                "{id}: this bundle's grid asks different questions from {} -- the captures are not \
                 comparable and every count in this gate would be measured against a different sweep",
                ids[0]
            ),
        }

        let got = ask(id, &questions).await;
        let mut differing = 0usize;
        for (row, engine) in grid.iter().zip(got.iter()) {
            if engine != &row.answer {
                differing += 1;
                mismatches.push(format!(
                    "  {id}  {}({:?})  capture {}  engine {engine}",
                    match row.question.member {
                        'v' => "video.canPlayType",
                        'a' => "audio.canPlayType",
                        's' => "MediaSource.isTypeSupported",
                        _ => "MediaRecorder.isTypeSupported",
                    },
                    row.question.mime,
                    row.answer
                ));
            }
        }
        eprintln!("[codec-grid] {id}: {} recorded answers, {differing} differing", grid.len());
        capture_answers.insert(id.clone(), grid.into_iter().map(|r| r.answer).collect());
        engine_answers.insert(id.clone(), got);
    }

    let total = questions.len() * ids.len();
    assert!(
        mismatches.is_empty(),
        "{} of {total} recorded codec answers are not this device's ({} bundles).\n{}",
        mismatches.len(),
        ids.len(),
        mismatches.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );

    // ── NON-VACUITY: the devices must not all answer alike ───────────────────────────────────────
    //
    // A gate that passes because every bundle carries the same grid would prove nothing at all. The
    // engine's answer vectors must differ between at least one pair, and -- P1, both directions --
    // they must differ EXACTLY where the captures do: a pair differing in the captures and not in
    // the engines is a hard-coded leak, and the reverse is an invented difference.
    let mut pairs = 0usize;
    let mut differing_pairs = 0usize;
    let keys = all_keys.as_ref().unwrap();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            let (a, b) = (&ids[i], &ids[j]);
            let cap_diff: Vec<&String> =
                keys.iter().enumerate().filter(|(n, _)| capture_answers[a][*n] != capture_answers[b][*n]).map(|(_, k)| k).collect();
            let eng_diff: Vec<&String> =
                keys.iter().enumerate().filter(|(n, _)| engine_answers[a][*n] != engine_answers[b][*n]).map(|(_, k)| k).collect();
            eprintln!("[codec-grid] {a} vs {b}: captures differ on {}, engines on {}", cap_diff.len(), eng_diff.len());
            assert_eq!(
                cap_diff, eng_diff,
                "{a} vs {b}: the engines' codec difference is not the captures' -- \
                 {} in the captures, {} in the engines",
                cap_diff.len(),
                eng_diff.len()
            );
            pairs += 1;
            if !cap_diff.is_empty() {
                differing_pairs += 1;
            }
        }
    }
    if ids.len() > 1 {
        assert!(
            differing_pairs > 0,
            "all {pairs} pairs of bundles answer the codec grid identically -- this gate cannot tell a \
             per-device grid from a compiled-in one and is vacuous"
        );
    }
    // With one persona (Chrome, since 2026-10-16) there is no pair: that the grid is READ per device is
    // `media_codecs`' stripped-grid unit test. What stays here is that the recorded answers are not one
    // answer, so a constant could not have matched them.
    let distinct: std::collections::BTreeSet<String> = capture_answers.values().flatten().map(|v| v.to_string()).collect();
    assert!(distinct.len() >= 3, "the recorded codec answers are {distinct:?} -- too few to grade an engine against");
    eprintln!(
        "[codec-grid] {} bundles x {} recorded answers = {total}, {pairs} pairs, {differing_pairs} of them differing",
        ids.len(),
        questions.len()
    );
}

// ── the other half of the same sync line ─────────────────────────────────────────────────────────

/// EVERY BUNDLE'S FONT NAMES ARE COMPILED IN, AND THE ENGINE MEASURES WITH THE WORN ONE'S.
///
/// It lives here because it is the same defect as the codec grid's: `tools/sync/sync.py` derived TWO
/// Rust files from `--from-bundle shipped`, and `codecs.json` was one of them. The font half held a
/// measured 0 -- all four captures resolved every family to the same face on this one host -- and it
/// still does now that V2 captures each browser's fonts in full (`plugins/korvus-persona/capture/
/// fonts-probe.html`, headed UNDER XVFB in Brave 151, Chrome 150, Edge 151 and Opera 135, 2026-09-25/26):
/// the four agree on every face and every name but one: `system-ui`. Chrome on Linux answers it with
/// the desktop's interface font (Blink's `FontCache::SystemFontFamily`, from GTK -- here `Ubuntu Sans`,
/// GNOME's `font-name`), which a browser under Xvfb has no session to read, so it fell back to
/// fontconfig's sans. A real user's Chrome runs in the desktop session: since 2026-10-08 the
/// chrome-150 bundle carries its desktop capture (`chrome-150-fonts-20260926-wayland-desktop.json`,
/// identical but for `system-ui`), measured against stock Chrome 150 on this desktop (`inline_layout_
/// live.rs`); an interactive challenge widget sets every word in `system-ui`.
///
/// The fix the old gate prescribed is in place anyway, because the day a capture from another machine
/// lands the names WILL differ: `font_table.rs` holds every face once and one name map per bundle
/// (`FONT_MAPS`), and the persona's theme names its map (`ThemeProfile::font_map`). This checks that
/// every name in every bundle's `fonts/metrics.json` resolves, under THAT bundle's map, to the face the
/// file names -- so a bundle whose map was not re-derived goes red here.
#[test]
fn every_bundles_font_names_are_compiled_and_selected_by_its_key() {
    for id in bundles() {
        let p = bundles_dir().join(&id).join("fonts/metrics.json");
        let m: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())))
            .unwrap_or_else(|e| panic!("parse {}: {e}", p.display()));
        let map = page::font::font_map(spec::font_map_key(&id));
        assert_eq!(map.bundle, id, "{id} has no map of its own in font_table.rs: re-derive it (emit.py --from-bundle shipped --role font-metrics)");
        for (table, generic) in [("families", false), ("generics", true)] {
            for (name, key) in m[table].as_object().unwrap_or_else(|| panic!("{id}: no `{table}`")) {
                let spelled = if generic { name.clone() } else { format!("\"{name}\"") };
                let face = page::font::resolve_face_in(map, &spelled).unwrap_or_else(|| panic!("{id}: {name} resolves to nothing"));
                assert_eq!(face.key, key.as_str().unwrap(), "{id}: {name}");
            }
        }
    }
}


// ── MediaCapabilities.decodingInfo, the FOURTH question over the same grid ──────────

/// `navigator.mediaCapabilities.decodingInfo` answers the worn device's grid, under every bundle.
///
/// It used to answer `null` -- a D2 stub of IDL type `Object`, so the shape default was `null` rather
/// than a promise -- for every configuration in every realm. Every rule asserted here was measured on
/// 2026-09-18 against real Brave 151.1.93.138 AND stock Chrome 150.0.7871.46 over a loopback page,
/// **56 of 56 cells identical between the two browsers**, which is what makes them the engine's and
/// not a device's.
///
/// WHAT THIS DOES NOT DO: pin a supported/unsupported answer per content type. That is the GRID's,
/// it differs per bundle by design (P4b), and `every_bundle_answers_its_own_recorded_codec_grid`
/// above is its gate. This asserts the RELATIONS -- which member of the grid each `type` reads, and
/// how the other three fields follow from `supported` -- so it stays true when a capture changes.
#[tokio::test]
async fn decoding_info_answers_the_worn_grid_and_its_three_derived_fields() {
    /// (contentType, is-audio-configuration)
    const CASES: &[(&str, bool)] = &[
        ("video/mp4; codecs=\"avc1.42E01E\"", false),
        ("video/webm; codecs=\"vp8\"", false),
        ("video/webm; codecs=\"vp09.00.10.08\"", false),
        ("video/mp4; codecs=\"av01.0.05M.08\"", false),
        ("video/ogg; codecs=\"theora\"", false),
        ("video/x-nope; codecs=\"zzz\"", false),
        ("video/mp4", false),
        ("audio/mp4; codecs=\"mp4a.40.2\"", true),
        ("audio/webm; codecs=\"opus\"", true),
        ("audio/mpeg", true),
        ("audio/ogg; codecs=\"flac\"", true),
        ("audio/wav; codecs=\"1\"", true),
        ("audio/x-nope; codecs=\"zzz\"", true),
    ];

    const PROBE: &str = r#"
      globalThis.__done = null;
      (async () => {
        const out = { cells: [], errs: [], keys: null, amb: null };
        const v = document.createElement("video");
        for (const [ct, isAudio] of CASES) {
          for (const type of ["file", "media-source"]) {
            const cfg = isAudio
              ? { type, audio: { contentType: ct } }
              : { type, video: { contentType: ct, width: 1280, height: 720, bitrate: 1000000, framerate: 30 } };
            let r;
            try { r = await navigator.mediaCapabilities.decodingInfo(cfg); }
            catch (e) { r = "THREW " + e.message; }
            out.cells.push({ ct, isAudio, type, r,
                             canPlay: v.canPlayType(ct), mse: MediaSource.isTypeSupported(ct) });
          }
        }
        const one = await navigator.mediaCapabilities.decodingInfo(
          { type: "file", video: { contentType: 'video/mp4; codecs="avc1.42E01E"', width: 1, height: 1, bitrate: 1, framerate: 1 } });
        out.keys = Object.keys(one);
        // The legacy VP9 spelling is ambiguous to MediaCapabilities and not to the other two members.
        const amb = async (ct) => (await navigator.mediaCapabilities.decodingInfo(
          { type: "file", video: { contentType: ct, width: 1, height: 1, bitrate: 1, framerate: 1 } })).supported;
        out.amb = { vp9: await amb('video/webm; codecs="vp9"'),
                    vp09: await amb('video/webm; codecs="vp09.00.10.08"'),
                    canPlayVp9: v.canPlayType('video/webm; codecs="vp9"') };
        const bad = [
          ["no-arg", undefined],
          ["not-an-object", "file"],
          ["no-type", {}],
          ["bad-type", { type: "nope", video: { contentType: "video/mp4", width: 1, height: 1, bitrate: 1, framerate: 1 } }],
          ["neither", { type: "file" }],
          ["no-bitrate", { type: "file", video: { contentType: "video/mp4", width: 1, height: 1, framerate: 1 } }],
          ["no-contentType", { type: "file", video: { width: 1, height: 1, bitrate: 1, framerate: 1 } }],
          ["audio-no-contentType", { type: "file", audio: {} }],
          ["unparseable", { type: "file", audio: { contentType: "not a mime" } }],
          ["framerate-0", { type: "file", video: { contentType: "video/mp4", width: 1, height: 1, bitrate: 1, framerate: 0 } }],
          ["framerate-NaN", { type: "file", video: { contentType: "video/mp4", width: 1, height: 1, bitrate: 1, framerate: NaN } }],
          ["enc-record", null],
          ["enc-nope", null],
        ];
        for (const [label, arg] of bad) {
          try {
            if (label === "enc-record" || label === "enc-nope") {
              await navigator.mediaCapabilities.encodingInfo(
                { type: label === "enc-record" ? "record" : "nope",
                  video: { contentType: 'video/webm; codecs="vp8"', width: 1, height: 1, bitrate: 1, framerate: 1 } });
            } else if (label === "no-arg") {
              await navigator.mediaCapabilities.decodingInfo();
            } else {
              await navigator.mediaCapabilities.decodingInfo(arg);
            }
            out.errs.push(label + " || NO THROW");
          } catch (e) { out.errs.push(label + " || " + e.constructor.name + ": " + e.message); }
        }
        globalThis.__done = JSON.stringify(out);
      })();
    "#;

    /// Verbatim from the reference run; both browsers produced every one of these.
    const WANT_ERRS: &[&str] = &[
        "no-arg || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': 1 argument required, but only 0 present.",
        "not-an-object || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': The provided value is not of type 'MediaDecodingConfiguration'.",
        "no-type || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': Failed to read the 'type' property from 'MediaDecodingConfiguration': Required member is undefined.",
        "bad-type || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': Failed to read the 'type' property from 'MediaDecodingConfiguration': The provided value 'nope' is not a valid enum value of type MediaDecodingType.",
        "neither || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': The configuration dictionary has neither |video| nor |audio| specified and needs at least one of them.",
        "no-bitrate || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': Failed to read the 'video' property from 'MediaConfiguration': Failed to read the 'bitrate' property from 'VideoConfiguration': Required member is undefined.",
        "no-contentType || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': Failed to read the 'video' property from 'MediaConfiguration': Failed to read the 'contentType' property from 'VideoConfiguration': Required member is undefined.",
        "audio-no-contentType || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': Failed to read the 'audio' property from 'MediaConfiguration': Failed to read the 'contentType' property from 'AudioConfiguration': Required member is undefined.",
        "unparseable || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': The audio configuration dictionary is not valid.",
        "framerate-0 || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': The video configuration dictionary is not valid.",
        "framerate-NaN || TypeError: Failed to execute 'decodingInfo' on 'MediaCapabilities': Failed to read the 'video' property from 'MediaConfiguration': Failed to read the 'framerate' property from 'VideoConfiguration': The provided double value is non-finite.",
        "enc-record || TypeError: Failed to execute 'encodingInfo' on 'MediaCapabilities': The provided value 'record' is not a valid enum value of type MediaEncodingType.",
        "enc-nope || TypeError: Failed to execute 'encodingInfo' on 'MediaCapabilities': Failed to read the 'type' property from 'MediaEncodingConfiguration': The provided value 'nope' is not a valid enum value of type MediaEncodingType.",
    ];

    for id in bundles() {
        let mut e = Engine::new(persona::worn_on_host(
            persona::CapturedPersona::select(Some(&id)).unwrap_or_else(|err| panic!("select {id}: {err}")),
        ));
        e.run().await.unwrap();
        e.load_html("<!doctype html><html><head><title>t</title></head><body>x</body></html>").unwrap();
        e.run().await.unwrap();
        let program = format!("const CASES = {};\n{PROBE}", serde_json::to_string(&CASES).unwrap());
        e.eval_value("mediacapabilities-probe", program).unwrap();
        e.run().await.unwrap();
        let raw = e.eval_value("mediacapabilities-read", "globalThis.__done".to_string()).unwrap();
        let out: Value = serde_json::from_str(
            raw.as_str().unwrap_or_else(|| panic!("{id}: the probe must settle into a JSON string, got {raw}")),
        )
        .unwrap();

        assert_eq!(
            out["keys"],
            json!(["powerEfficient", "smooth", "supported", "keySystemAccess"]),
            "{id}: the MediaCapabilitiesInfo key order is the reference browsers'"
        );

        for cell in out["cells"].as_array().unwrap() {
            let (ct, ty) = (cell["ct"].as_str().unwrap(), cell["type"].as_str().unwrap());
            let r = &cell["r"];
            assert!(r.is_object(), "{id}: decodingInfo({ty}, {ct}) did not resolve an object: {r}");
            let supported = r["supported"].as_bool().unwrap();
            // WHICH member of the grid each `type` reads. `audio/ogg; codecs="flac"` and
            // `audio/wav; codecs="1"` are the cells that prove the two are different questions:
            // `file` says true for both and `media-source` says false, in both browsers.
            let want = if ty == "media-source" {
                cell["mse"].as_bool().unwrap()
            } else {
                cell["canPlay"].as_str().unwrap() == "probably"
            };
            assert_eq!(
                supported, want,
                "{id}: decodingInfo({ty}, {ct}).supported must equal the grid's own answer \
                 (canPlayType={:?}, MediaSource={})",
                cell["canPlay"], cell["mse"]
            );
            assert_eq!(r["smooth"].as_bool().unwrap(), supported, "{id}: smooth == supported ({ty}, {ct})");
            let audio_only = cell["isAudio"].as_bool().unwrap();
            assert_eq!(
                r["powerEfficient"].as_bool().unwrap(),
                supported && audio_only,
                "{id}: powerEfficient is `supported AND no video member` ({ty}, {ct})"
            );
            assert!(r["keySystemAccess"].is_null(), "{id}: keySystemAccess is null ({ty}, {ct})");
        }

        // The ambiguity rule, which is MediaCapabilities' alone: a VP9 named with no profile is not an
        // answerable question here, while `canPlayType` answers "probably" for it and `vp09.00.10.08`
        // is supported. Asserted only on a device whose grid carries VP9 at all.
        let amb = &out["amb"];
        if amb["canPlayVp9"].as_str().unwrap() == "probably" {
            assert_eq!(amb["vp9"], json!(false), "{id}: codecs=\"vp9\" is ambiguous to decodingInfo");
            assert_eq!(amb["vp09"], json!(true), "{id}: codecs=\"vp09.00.10.08\" is not");
        }

        let errs: Vec<&str> = out["errs"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(errs, WANT_ERRS, "{id}: the error surface is the reference browsers', verbatim");
    }
}
