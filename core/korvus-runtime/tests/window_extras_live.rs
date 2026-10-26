//! THE WINDOW'S REMAINING OWN GLOBALS, read back off the LIVE surface.
//!
//! `Object.getOwnPropertyNames(window)` answered 1,163 on this engine and 1,206 in the
//! `brave-151-linux-20260907T1006` capture it wears — 43 missing, 0 extra, and the same 43 in a
//! nested browsing context. `core/html/window_extras.rs` (plus `gpu_namespaces.rs`) mints **26 of
//! them**, `core/html/bar_props.rs` mints seven more off two tables of its own, and this file asserts
//! that each one has the SHAPE its own browser gives it, not merely that the name exists. The other
//! ten are named in those files' headers with the reason: six need a table of their own before they
//! can carry a body, and four are Brave's wallet objects, which belong to the persona.
//!
//! # Every expectation here is a reading, taken for this unit
//!
//! Real Brave 151.1.93.138 and real Chrome 150.0.7871.46, headed, driven over CDP against a page on
//! 127.0.0.1, 2026-09-19. Headless is not an option on this box: headless Brave and headless Opera
//! HANG on a minimal page rather than fail (FP5.1, measured). Where the brief and the browser
//! disagreed the browser won, and the two places that happened are named in the tests below:
//! `external.IsSearchProviderInstalled()` is `undefined` and not `0`, and `viewport` is the 43rd name
//! the brief's own list never mentions.
//!
//! # Why the assertions are grouped by DESCRIPTOR FAMILY rather than by API
//!
//! Because that is what a page reads. A detector that walks `window`'s own descriptors does not care
//! that `trustedTypes` and `launchQueue` belong to different specifications; it cares that both are
//! getter-only accessors with `enumerable: true, configurable: true`, and that
//! `offscreenBuffering` — alone in the whole set — is `enumerable: false`.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><head><title>t</title></head><body>probe</body></html>").unwrap();
    e.run().await.unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

fn line<'a>(out: &'a [String], key: &str) -> &'a str {
    out.iter()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or_else(|| panic!("no `{key}` line in {out:?}"))
}

/// THE FOUR DESCRIPTOR FAMILIES, asserted as one table because a page reads them as one table.
///
/// ```text
/// value  w=1 e=0 c=1   webkitURL webkitRTCPeerConnection webkitMediaStream webkitSpeech*  GPU*
/// value  w=1 e=1 c=1   find fetchLater queryLocalFonts getScreenDetails webkitRequestFileSystem
///                      webkitResolveLocalFileSystemURL webkitRequestAnimationFrame
///                      webkitCancelAnimationFrame
/// get+set e=1 c=1      external clientInformation locationbar menubar personalbar scrollbars
///                      statusbar toolbar viewport
/// get     e=1 c=1      styleMedia launchQueue
/// get+set e=0 c=1      offscreenBuffering            <- the one exception in the whole set
/// ```
///
/// The six names FP5 does NOT install are absent from every list here on purpose
/// -- `core/html/window_extras.rs`'s header says which and why (each needs a BODY on a member of an
/// interface the generated registry already installs, and `install_ledger.rs` refuses a second writer
/// of a slot the installer mounted).
#[tokio::test]
async fn every_new_window_global_carries_its_own_browsers_descriptor() {
    let out = probe(
        r#"
        const shape = (n) => {
            const d = Object.getOwnPropertyDescriptor(globalThis, n);
            if (!d) return n + ":ABSENT";
            return ("get" in d)
                ? n + ":" + (d.get ? d.get.name : "-") + "/" + (d.set ? d.set.name : "-") + "/" + d.enumerable + d.configurable
                : n + ":v/" + d.writable + d.enumerable + d.configurable;
        };
        print("iface=" + ["webkitURL", "webkitRTCPeerConnection", "webkitMediaStream",
            "webkitSpeechGrammar", "webkitSpeechGrammarList", "webkitSpeechRecognition",
            "webkitSpeechRecognitionError", "webkitSpeechRecognitionEvent",
            "GPUBufferUsage", "GPUColorWrite", "GPUMapMode", "GPUShaderStage", "GPUTextureUsage"]
            .map(shape).join(","));
        print("op=" + ["find", "fetchLater", "queryLocalFonts", "getScreenDetails",
            "webkitRequestFileSystem", "webkitResolveLocalFileSystemURL",
            "webkitRequestAnimationFrame", "webkitCancelAnimationFrame"].map(shape).join(","));
        print("replaceable=" + ["external", "clientInformation", "locationbar", "menubar",
            "personalbar", "scrollbars", "statusbar", "toolbar", "viewport"].map(shape).join(","));
        print("readonly=" + ["styleMedia", "launchQueue"].map(shape).join(","));
        print("seated=" + ["visualViewport", "trustedTypes", "documentPictureInPicture",
            "crashReport", "scheduler", "navigation"].map((n) => n in globalThis).join(","));
        print("offscreen=" + shape("offscreenBuffering"));
        "#,
    )
    .await;

    for entry in line(&out, "iface=").split(',') {
        let (name, shape) = entry.split_once(':').expect("name:shape");
        assert_eq!(shape, "v/truefalsetrue", "{name}: an interface/namespace object's own descriptor on the global is {{w:1,e:0,c:1}}");
    }
    for entry in line(&out, "op=").split(',') {
        let (name, shape) = entry.split_once(':').expect("name:shape");
        assert_eq!(shape, "v/truetruetrue", "{name}: an operation on Window is {{w:1,e:1,c:1}}");
    }
    for entry in line(&out, "replaceable=").split(',') {
        let (name, shape) = entry.split_once(':').expect("name:shape");
        assert_eq!(
            shape,
            format!("get {name}/set {name}/truetrue"),
            "{name}: a [Replaceable] attribute is `get X`/`set X`, enumerable, configurable"
        );
    }
    for entry in line(&out, "readonly=").split(',') {
        let (name, shape) = entry.split_once(':').expect("name:shape");
        assert_eq!(
            shape,
            format!("get {name}/-/truetrue"),
            "{name}: a readonly attribute has a getter and NO setter"
        );
    }
    // No name is installed here without its body: an empty object parked on the window to make a
    // count go up is the one thing FP5's brief forbids outright. Each of these six has its own
    // `*_live.rs` that asserts its BODY rather than its presence -- `visualViewport`
    // (`core/html/visual_viewport.rs`), `trustedTypes` (`core/trusted_types/`),
    // `documentPictureInPicture` and `crashReport` (`core/html/document_pip.rs`,
    // `core/html/crash_report.rs`), `scheduler` (`core/html/scheduler.rs`), and -- the last one
    // Seated over the session history `history` keeps --
    // `navigation` (`core/html/navigation_api.rs`, `navigation_api_live.rs`).
    assert_eq!(
        line(&out, "seated="),
        "true,true,true,true,true,true",
        "the six names with tables are present, and their own live tests assert their bodies"
    );
    assert_eq!(
        line(&out, "offscreen="),
        "offscreenBuffering:get offscreenBuffering/set offscreenBuffering/falsetrue",
        "offscreenBuffering is the one non-enumerable accessor in the set (measured on Brave 151 and Chrome 150)"
    );
}

/// The legacy `webkit` names are the SAME interface objects, not copies — `webkitURL === URL` is
/// `true`, so `webkitURL.name` is `"URL"` and `String(webkitURL)` prints `URL`. A copy would print
/// its own name, which is a one-line detect.
///
/// The two animation-frame names are NOT aliases (`webkitRequestAnimationFrame !==
/// requestAnimationFrame`, measured), carry their own name and `length` 1, and schedule a real frame.
#[tokio::test]
async fn the_webkit_aliases_are_the_same_objects_and_the_raf_pair_is_not() {
    let out = probe(
        r#"
        print("same=" + [[ "webkitURL", "URL"], ["webkitRTCPeerConnection", "RTCPeerConnection"],
            ["webkitMediaStream", "MediaStream"], ["webkitSpeechGrammar", "SpeechGrammar"],
            ["webkitSpeechGrammarList", "SpeechGrammarList"],
            ["webkitSpeechRecognition", "SpeechRecognition"],
            ["webkitSpeechRecognitionError", "SpeechRecognitionErrorEvent"],
            ["webkitSpeechRecognitionEvent", "SpeechRecognitionEvent"]]
            .map(([a, b]) => globalThis[a] === globalThis[b]).join(","));
        print("names=" + webkitURL.name + "," + String(webkitURL));
        print("raf=" + (webkitRequestAnimationFrame !== requestAnimationFrame) + "," +
            webkitRequestAnimationFrame.name + "," + webkitRequestAnimationFrame.length + "," +
            webkitCancelAnimationFrame.name + "," + webkitCancelAnimationFrame.length);
        let fired = "no";
        webkitRequestAnimationFrame(() => { fired = "yes"; });
        print("scheduled=" + (typeof webkitRequestAnimationFrame(() => {}) === "number"));
        "#,
    )
    .await;
    assert_eq!(line(&out, "same="), "true,true,true,true,true,true,true,true");
    assert_eq!(line(&out, "names="), "URL,function URL() { [native code] }");
    assert_eq!(line(&out, "raf="), "true,webkitRequestAnimationFrame,1,webkitCancelAnimationFrame,1");
    assert_eq!(line(&out, "scheduled="), "true", "the alias schedules a real frame and answers its handle");
}

/// `window.external` — THE MOST-READ NAME IN THE SET (a behavioural sensor touches it 21 times in one page
/// load, FP0 §L4). Both methods answer `undefined` with `length` 0.
///
/// **THE BRIEF IS WRONG ABOUT THIS ONE** and this assertion is the record: it says
/// `IsSearchProviderInstalled` answers `0`. Real Brave 151 answers `undefined`, for the no-argument
/// call and the one-argument call alike.
#[tokio::test]
async fn window_external_answers_undefined_from_both_of_its_methods() {
    let out = probe(
        r#"
        print("tag=" + Object.prototype.toString.call(external) + "," + (external instanceof External));
        print("answers=" + [external.AddSearchProvider(), external.AddSearchProvider("http://x/"),
            external.IsSearchProviderInstalled(), external.IsSearchProviderInstalled("http://x/")]
            .map(String).join(","));
        print("lengths=" + external.AddSearchProvider.length + "," + external.IsSearchProviderInstalled.length);
        print("illegal=" + (() => { try { External.prototype.AddSearchProvider.call({}); return "no"; }
            catch (e) { return e.constructor.name + ":" + e.message; } })());
        "#,
    )
    .await;
    assert_eq!(line(&out, "tag="), "[object External],true");
    assert_eq!(line(&out, "answers="), "undefined,undefined,undefined,undefined");
    assert_eq!(line(&out, "lengths="), "0,0");
    assert_eq!(line(&out, "illegal="), "TypeError:Illegal invocation", "a foreign receiver still throws");
}

/// `clientInformation` IS `navigator` — the identity, not a second `Navigator` answering the same
/// values. A duplicate would pass every value comparison and fail `===`.
#[tokio::test]
async fn client_information_is_the_navigator_itself() {
    let out = probe(r#"print("same=" + (clientInformation === navigator) + "," + offscreenBuffering);"#).await;
    assert_eq!(line(&out, "same="), "true,true");
}

/// `window.find` is a real text search over the document, case-insensitively, with `length` 0 —
/// measured, and not the 1 an IDL reading would guess. `fetchLater` carries WebIDL's own arity error
/// and answers a `FetchLaterResult`.
#[tokio::test]
async fn find_searches_the_document_and_fetch_later_has_its_arity_error() {
    let out = probe(
        r#"
        print("find=" + find("probe") + "," + find("PROBE") + "," + find("zzzznothere") + "," + find.length);
        print("later=" + (() => { try { fetchLater(); return "no"; } catch (e) { return e.message; } })());
        print("laterOk=" + Object.prototype.toString.call(fetchLater("http://127.0.0.1:1/x")) + "," + fetchLater.length);
        print("fsLengths=" + webkitRequestFileSystem.length + "," + webkitResolveLocalFileSystemURL.length);
        let err = "none";
        webkitResolveLocalFileSystemURL("filesystem:http://x/temporary/y", () => {}, (e) => { err = e.name; });
        print("fsResolve=" + err);
        "#,
    )
    .await;
    assert_eq!(line(&out, "find="), "true,true,false,0");
    assert_eq!(
        line(&out, "later="),
        "Failed to execute 'fetchLater' on 'Window': 1 argument required, but only 0 present."
    );
    assert_eq!(line(&out, "laterOk="), "[object FetchLaterResult],1");
    assert_eq!(line(&out, "fsLengths="), "3,2");
    assert_eq!(line(&out, "fsResolve="), "SecurityError", "every filesystem URL is foreign to an engine with no filesystem");
}

/// The five WebGPU constant NAMESPACES: the values are the platform's bit positions, each constant is
/// `{w:0, e:1, c:0}`, the object's `[[Prototype]]` is `Object.prototype`, it carries
/// `Symbol.toStringTag` and it is NOT frozen (measured — a `Object.freeze` here would be a tell).
#[tokio::test]
async fn the_gpu_constant_namespaces_are_constant_tables() {
    let out = probe(
        r#"
        print("buf=" + JSON.stringify(GPUBufferUsage));
        print("rest=" + [GPUColorWrite, GPUMapMode, GPUShaderStage, GPUTextureUsage].map(JSON.stringify).join("|"));
        print("desc=" + JSON.stringify(Object.getOwnPropertyDescriptor(GPUBufferUsage, "MAP_READ")));
        print("shape=" + Object.prototype.toString.call(GPUTextureUsage) + "," +
            (Object.getPrototypeOf(GPUBufferUsage) === Object.prototype) + "," +
            Object.isFrozen(GPUBufferUsage) + "," + Object.isExtensible(GPUBufferUsage));
        "#,
    )
    .await;
    assert_eq!(
        line(&out, "buf="),
        r#"{"MAP_READ":1,"MAP_WRITE":2,"COPY_SRC":4,"COPY_DST":8,"INDEX":16,"VERTEX":32,"UNIFORM":64,"STORAGE":128,"INDIRECT":256,"QUERY_RESOLVE":512}"#
    );
    assert_eq!(
        line(&out, "rest="),
        r#"{"RED":1,"GREEN":2,"BLUE":4,"ALPHA":8,"ALL":15}|{"READ":1,"WRITE":2}|{"VERTEX":1,"FRAGMENT":2,"COMPUTE":4}|{"COPY_SRC":1,"COPY_DST":2,"TEXTURE_BINDING":4,"STORAGE_BINDING":8,"RENDER_ATTACHMENT":16,"TRANSIENT_ATTACHMENT":32}"#
    );
    assert_eq!(line(&out, "desc="), r#"{"value":1,"writable":false,"enumerable":true,"configurable":false}"#);
    assert_eq!(line(&out, "shape="), "[object GPUTextureUsage],true,false,true");
}

/// A NESTED CONTEXT WEARS THE SAME SURFACE: the frame's window has every one of these
/// names, in the page's order. The frame realm is built from the page realm's sealed install surface,
/// so this would break the moment one of these globals were created after the seal.
#[tokio::test]
async fn a_frames_window_carries_the_new_globals_too() {
    let out = probe(
        r#"
        const f = document.createElement("iframe");
        document.body.appendChild(f);
        const w = f.contentWindow;
        const names = ["external", "styleMedia", "launchQueue", "clientInformation",
            "offscreenBuffering", "find", "fetchLater", "getScreenDetails", "queryLocalFonts",
            "webkitURL", "webkitRequestFileSystem", "GPUBufferUsage"];
        print("present=" + names.every((n) => n in w));
        const own = (o) => Object.getOwnPropertyNames(o).filter((n) => !/^[0-9]+$/.test(n));
        print("sameOrder=" + (own(w).join(",") === own(globalThis).join(",")));
        "#,
    )
    .await;
    assert_eq!(line(&out, "present="), "true");
    assert_eq!(line(&out, "sameOrder="), "true", "a frame's own-key sequence is the page's, exactly");
}

/// `styleMedia` is the one object in this set whose INTERFACE the device does not expose: no
/// `StyleMedia` on any global, a hidden prototype carrying `Symbol.toStringTag` with
/// `Object.prototype` above it, NO own `constructor` on that prototype, and exactly two members.
/// `matchMedium` is the REAL media query, evaluated through this realm's own `matchMedia`, so the two
/// cannot answer differently for the same string.
#[tokio::test]
async fn style_media_is_a_hidden_prototype_with_a_real_media_query() {
    let out = probe(
        r#"
        print("tag=" + Object.prototype.toString.call(styleMedia) + "," + typeof globalThis.StyleMedia);
        print("keys=" + Object.getOwnPropertyNames(Object.getPrototypeOf(styleMedia)).join("|"));
        print("parent=" + (Object.getPrototypeOf(Object.getPrototypeOf(styleMedia)) === Object.prototype));
        print("answers=" + styleMedia.type + "," + styleMedia.matchMedium("screen") + "," +
            styleMedia.matchMedium("print") + "," +
            (styleMedia.matchMedium("(min-width: 1px)") === matchMedia("(min-width: 1px)").matches));
        print("illegal=" + (() => { const g = Object.getOwnPropertyDescriptor(
            Object.getPrototypeOf(styleMedia), "type").get;
            try { g.call({}); return "no"; } catch (e) { return e.message; } })());
        "#,
    )
    .await;
    assert_eq!(line(&out, "tag="), "[object StyleMedia],undefined");
    assert_eq!(line(&out, "keys="), "type|matchMedium", "no own `constructor` -- measured on both browsers");
    assert_eq!(line(&out, "parent="), "true");
    assert_eq!(line(&out, "answers="), "screen,true,false,true");
    assert_eq!(line(&out, "illegal="), "Illegal invocation", "a foreign receiver throws, as every platform member does");
}

/// THE GATED CALLS, and the point of the test is that these are not stubs: the real browser refuses
/// for a reason that holds here too, so the refusal — including its sentence — is the device's own.
///
/// `launchQueue` is in the same test because it is the same argument one step further: its consumer is
/// never called because this page was not launched as an installed app, which is what the device does.
#[tokio::test]
async fn the_user_activation_gated_calls_refuse_exactly_as_the_browser_does() {
    let out = probe(
        r#"
        const say = (k, p) => p.then(() => print(k + "=RESOLVED"),
            (e) => print(k + "=" + e.name + ":" + e.message));
        say("screens", getScreenDetails());
        say("fonts", queryLocalFonts());
        print("lengths=" + getScreenDetails.length + "," + queryLocalFonts.length);
        print("launch=" + Object.prototype.toString.call(launchQueue) + "," +
            String(launchQueue.setConsumer(() => {})) + "," + launchQueue.setConsumer.length);
        print("launchArity=" + (() => { try { launchQueue.setConsumer(); return "no"; }
            catch (e) { return e.message; } })());
        "#,
    )
    .await;
    assert_eq!(line(&out, "screens="), "NotAllowedError:Transient activation is required to request permission.");
    assert_eq!(line(&out, "fonts="), "SecurityError:User activation is required.");
    assert_eq!(line(&out, "lengths="), "0,0");
    assert_eq!(line(&out, "launch="), "[object LaunchQueue],undefined,1");
    assert_eq!(
        line(&out, "launchArity="),
        "Failed to execute 'setConsumer' on 'LaunchQueue': 1 argument required, but only 0 present."
    );
}

/// The six `BarProp`s are six DISTINCT objects (`locationbar !== menubar`, measured) and every one
/// reports `visible: true` on a headed window. `window.viewport` is a seventh instance of the same
/// shape: one frozen `DOMRect` segment, a NEW array each read.
///
/// THE TWO VIEWPORT OBJECTS DISAGREE ON PURPOSE, and it is the fact a guess gets wrong:
/// `viewport.segments[0]` is the WINDOW's inner box while `visualViewport.width` (not yet installed)
/// is `documentElement.clientWidth` — 1920 against 1905 on the same Brave page load.
///
/// Both interfaces carry their member as a real table row (`core/html/bar_props.rs`) rather than a
/// body clobbered onto the registry's prototype; `install_ledger.rs` is what makes that the only
/// option, and this test is what says the row works.
#[tokio::test]
async fn the_six_bar_props_and_the_viewport_are_real_instances() {
    let out = probe(
        r#"
        const bars = ["locationbar", "menubar", "personalbar", "scrollbars", "statusbar", "toolbar"];
        print("visible=" + bars.map((b) => globalThis[b].visible).join(","));
        print("distinct=" + new Set(bars.map((b) => globalThis[b])).size);
        print("brand=" + bars.every((b) => globalThis[b] instanceof BarProp) + "," +
            Object.prototype.toString.call(locationbar));
        print("illegalBar=" + (() => { try {
            Object.getOwnPropertyDescriptor(BarProp.prototype, "visible").get.call({});
            return "no"; } catch (e) { return e.message; } })());
        const seg = viewport.segments;
        print("seg=" + Array.isArray(seg) + "," + seg.length + "," +
            Object.prototype.toString.call(seg[0]) + "," + Object.isFrozen(seg) + "," +
            (seg[0].width === innerWidth) + "," + (seg[0].height === innerHeight));
        print("segFresh=" + (viewport.segments !== viewport.segments));
        print("vpBrand=" + (viewport instanceof Viewport) + "," +
            Object.prototype.toString.call(viewport));
        print("newable=" + [["BarProp", () => new BarProp()], ["Viewport", () => new Viewport()]]
            .map(([n, f]) => { try { f(); return n + ":no"; } catch (e) { return n + ":" + e.message; } })
            .join("|"));
        "#,
    )
    .await;
    assert_eq!(line(&out, "visible="), "true,true,true,true,true,true");
    assert_eq!(line(&out, "distinct="), "6", "six names, six objects -- not one object six times");
    assert_eq!(line(&out, "brand="), "true,[object BarProp]");
    assert_eq!(line(&out, "illegalBar="), "Illegal invocation", "the row brand-checks, like every platform member");
    assert_eq!(line(&out, "seg="), "true,1,[object DOMRect],true,true,true");
    assert_eq!(line(&out, "segFresh="), "true", "`segments` is a NEW frozen array each read, as on both browsers");
    assert_eq!(line(&out, "vpBrand="), "true,[object Viewport]");
    assert_eq!(
        line(&out, "newable="),
        "BarProp:Failed to construct 'BarProp': Illegal constructor|\
         Viewport:Failed to construct 'Viewport': Illegal constructor",
        "neither interface is constructible, on either browser"
    );
}
