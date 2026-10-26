//! The `Window` / `Document` surface, and the singletons' IDENTITY.
//!
//! `crypto`, `performance`, `history` and `location` were plain objects and `document` hung directly
//! off `Document.prototype`, so every one of these was false where a browser says true:
//!
//! ```text
//! crypto instanceof Crypto            performance instanceof Performance
//! history instanceof History          location instanceof Location
//! document instanceof HTMLDocument
//! ```
//!
//! and three of them reported `[object Object]` from `Object.prototype.toString`. Each is a one-line
//! check, several are in the standard fingerprinting suites, and together they were five of the eight
//! outright tells an audit against a real Brave 151 turned up.
//!
//! This file is the gate that keeps them fixed while the lane moves the work out of
//! `dom/09f-window.js`, `dom/09h-window-named.js` and `dom/09i-singletons.js` and into Rust tables.
//! It was written BEFORE the first cutover and passed against the JS layer, so every assertion here
//! is a statement about the surface, not about the implementation that produces it -- which is what
//! makes it a parallel-install proof rather than a description of whatever the tree happens to do.
use engine::Engine;

/// One document for every case here: the named-properties test needs an element with an id, and the
/// `document` identity cases need a document at all (it is built per NAVIGATION, not at boot).
const HTML: &str = r#"<!doctype html><html><body><form id="korvusform"></form></body></html>"#;

/// The five `instanceof` lines and the three `Object.prototype.toString` tags, live.
///
/// `location` and `document` are the two that were ALREADY tagged (`[object Location]`,
/// `[object Document]` -- the wrong interface, but a tag); `crypto`, `performance` and `history` are
/// the three that read `[object Object]`.
#[tokio::test]
async fn the_singletons_are_instances_of_their_own_interfaces() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const S = Object.prototype.toString;
        print("crypto=" + (crypto instanceof Crypto) + " " + S.call(crypto));
        print("performance=" + (performance instanceof Performance) + " " + S.call(performance));
        print("history=" + (history instanceof History) + " " + S.call(history));
        print("location=" + (location instanceof Location) + " " + S.call(location));
        print("document=" + (document instanceof HTMLDocument) + " " + S.call(document));
        // `document instanceof Document` must ALSO hold: HTMLDocument.prototype chains onto it.
        print("docIsDocument=" + (document instanceof Document));
        print("docCtor=" + document.constructor.name);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "crypto=true [object Crypto]",
        "performance=true [object Performance]",
        "history=true [object History]",
        "location=true [object Location]",
        "document=true [object HTMLDocument]",
        "docIsDocument=true",
        "docCtor=HTMLDocument",
    ]);
}

/// A real singleton OWNS NOTHING: every member is inherited from its interface prototype. Measured on
/// Brave 151 -- `Object.getOwnPropertyNames(crypto)` is `[]` there, and so are `performance`'s and
/// `history`'s. An internal that rode along as an own property would otherwise be a permanently
/// visible invented member.
///
/// `location` is the deliberate exception and the reason it is listed here with a NUMBER rather than
/// zero: its members are `[LegacyUnforgeable]`, which WebIDL puts on the object as own,
/// non-configurable properties, with `Location.prototype` carrying nothing but `constructor`. Brave
/// 151 reports fifteen own members and a one-member prototype; this engine used to have exactly the
/// inverse.
///
/// The number is FIFTEEN, Brave's and Chrome's, Edge's and Opera's
/// (`fixtures/p11a/desc.*.txt`): the capture rig records no unforgeable own member, so this read twelve
/// -- what `JsLocation` implemented -- until the four browsers were asked directly. `ancestorOrigins`,
/// `port` and `valueOf` were the three missing, and `location.rs` has all fifteen.
#[tokio::test]
async fn the_singletons_own_nothing_except_location_which_owns_everything() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const own = (o) => Object.getOwnPropertyNames(o).length;
        print("crypto=" + own(crypto));
        print("performance=" + own(performance));
        print("history=" + own(history));
        print("locationOwn=" + own(location));
        print("locationProto=" + JSON.stringify(Object.getOwnPropertyNames(Location.prototype)));
        // Unforgeable: own AND non-configurable.
        const d = Object.getOwnPropertyDescriptor(location, "href");
        print("hrefOwn=" + (!!d) + " configurable=" + (d ? d.configurable : "n/a"));
        // The document's one own member in a browser, for the same WebIDL reason.
        const dd = Object.getOwnPropertyDescriptor(document, "location");
        print("docLocationOwn=" + (!!dd) + " configurable=" + (dd ? dd.configurable : "n/a"));
        print("docProtoHasLocation=" + Object.prototype.hasOwnProperty.call(Document.prototype, "location"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "crypto=0",
        "performance=0",
        "history=0",
        "locationOwn=15",
        "locationProto=[\"constructor\"]",
        "hrefOwn=true configurable=false",
        "docLocationOwn=true configurable=false",
        "docProtoHasLocation=false",
    ]);
}

/// The singletons still WORK after the re-parent, and each keeps its identity across two reads
/// (`window.crypto === window.crypto`) -- failure mode 1 of the lane's four.
#[tokio::test]
async fn the_singletons_keep_one_identity_and_their_behaviour() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("t", r#"
        print("same=" + (window.crypto === window.crypto) + (window.performance === window.performance)
              + (window.history === window.history) + (window.location === window.location));
        const a = new Uint8Array(8);
        crypto.getRandomValues(a);
        print("random=" + (a.some((x) => x !== 0)));
        print("uuid=" + /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(crypto.randomUUID()));
        print("subtle=" + (typeof crypto.subtle) + " " + (crypto.subtle instanceof SubtleCrypto));
        print("now=" + (typeof performance.now() === "number"));
        print("histLength=" + (typeof history.length === "number") + " scroll=" + history.scrollRestoration);
        print("href=" + (typeof location.href === "string"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "same=truetruetruetrue",
        "random=true",
        "uuid=true",
        "subtle=object true",
        "now=true",
        "histLength=true scroll=auto",
        "href=true",
    ]);
}

/// The Window prototype chain, and the named-properties object spliced into it.
///
/// ```text
/// window -> Window.prototype -> [named properties] -> EventTarget.prototype -> Object.prototype
/// ```
///
/// FOUR links, not five: an empty placeholder left between `Window.prototype` and the named object
/// was a real defect `tests/automation_tells.rs` caught, and naming `EventTarget.prototype`
/// explicitly is what keeps the splice self-healing. `Symbol.toStringTag` makes the middle object
/// report `[object WindowProperties]`, as Chrome's does.
#[tokio::test]
async fn the_window_named_properties_object_is_spliced_once() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const chain = [];
        for (let p = Object.getPrototypeOf(window); p; p = Object.getPrototypeOf(p)) {
            chain.push(Object.prototype.toString.call(p));
        }
        print("chain=" + JSON.stringify(chain));
        const np = Object.getPrototypeOf(Window.prototype);
        print("npOwn=" + JSON.stringify(Object.getOwnPropertyNames(np)));
        print("npParentIsET=" + (Object.getPrototypeOf(np) === EventTarget.prototype));
        // Named access still resolves through it, and still yields to what is further down.
        print("named=" + (window.korvusform === document.getElementById("korvusform")));
        print("noShadow=" + (window.addEventListener === EventTarget.prototype.addEventListener));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "chain=[\"[object Window]\",\"[object WindowProperties]\",\"[object EventTarget]\",\"[object Object]\"]",
        "npOwn=[]",
        "npParentIsET=true",
        "named=true",
        "noShadow=true",
    ]);
}

/// `navigator.plugins` and `navigator.mimeTypes`: the four interfaces, their instances' measured
/// shape, and the two lines a fingerprinting suite actually runs.
///
/// `Array.isArray(navigator.plugins)` must be FALSE and the tag must be `[object PluginArray]`. The
/// list itself is the persona's (`ClientHints::plugins`); the two PDF mime essences are a Chromium
/// structural constant, identical across every Chrome build.
#[tokio::test]
async fn the_plugin_family_is_four_interfaces_not_two_arrays() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const S = Object.prototype.toString;
        const p = navigator.plugins, m = navigator.mimeTypes;
        print("isArray=" + Array.isArray(p) + Array.isArray(m));
        print("tags=" + S.call(p) + S.call(m) + S.call(p[0]) + S.call(m[0]));
        print("instof=" + (p instanceof PluginArray) + (m instanceof MimeTypeArray)
              + (p[0] instanceof Plugin) + (m[0] instanceof MimeType));
        print("same=" + (navigator.plugins === navigator.plugins));
        print("counts=" + p.length + " " + m.length + " " + p[0].length);
        print("named=" + (p.namedItem(p[0].name) === p[0]) + (m.namedItem("application/pdf") === m[0]));
        print("item=" + (p.item(0) === p[0]) + (p.item(99) === null));
        print("linked=" + (m[0].enabledPlugin === p[0]));
        print("mimeOwn=" + JSON.stringify(Object.getOwnPropertyNames(m[0])));
        print("plugKeys=" + JSON.stringify(Object.getOwnPropertyNames(p[0])));
        print("protoKeys=" + JSON.stringify(Object.getOwnPropertyNames(PluginArray.prototype)));
        print("refresh=" + p.refresh());
        // The two accessors live on `Navigator.prototype`, where the interface declares them -- not as
        // own properties of the `navigator` singleton, which is what the deleted JS wrote.
        print("navOwn=" + Object.prototype.hasOwnProperty.call(navigator, "plugins")
              + Object.prototype.hasOwnProperty.call(navigator, "mimeTypes"));
        print("navProto=" + Object.prototype.hasOwnProperty.call(Navigator.prototype, "plugins")
              + Object.prototype.hasOwnProperty.call(Navigator.prototype, "mimeTypes"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    assert_eq!(out[0], "isArray=falsefalse");
    assert_eq!(out[1], "tags=[object PluginArray][object MimeTypeArray][object Plugin][object MimeType]");
    assert_eq!(out[2], "instof=truetruetruetrue");
    assert_eq!(out[3], "same=true");
    assert_eq!(out[5], "named=truetrue");
    assert_eq!(out[6], "item=truetrue");
    assert_eq!(out[7], "linked=true");
    // A MimeType owns nothing at all -- every member is an accessor on its prototype.
    assert_eq!(out[8], "mimeOwn=[]");
    // A Plugin owns its mime indices AND the two essences by name; `length` is on the prototype in
    // both collections, over a private slot.
    assert_eq!(out[9], "plugKeys=[\"0\",\"1\",\"application/pdf\",\"text/pdf\"]");
    assert_eq!(out[10], "protoKeys=[\"length\",\"item\",\"namedItem\",\"refresh\",\"constructor\"]");
    assert_eq!(out[11], "refresh=undefined");
    assert_eq!(out[12], "navOwn=falsefalse");
    assert_eq!(out[13], "navProto=truetrue");
}

/// Failure mode 2: a row that accepts a foreign receiver. Chrome throws `Illegal invocation` from
/// every one of these; the deleted JS bodies answered `null` (the methods) or `undefined` (the slot
/// getters), because `this[i]` on a plain object is simply a miss.
#[tokio::test]
async fn a_plugin_row_refuses_a_foreign_receiver() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const msg = (f) => { try { f(); return "no throw"; } catch (e) { return e.constructor.name + ": " + e.message; } };
        const alien = { 0: 1, length: 1, name: "x" };
        print("item=" + msg(() => PluginArray.prototype.item.call(alien, 0)));
        print("named=" + msg(() => MimeTypeArray.prototype.namedItem.call(alien, "x")));
        print("refresh=" + msg(() => PluginArray.prototype.refresh.call(alien)));
        const g = Object.getOwnPropertyDescriptor(Plugin.prototype, "name").get;
        print("nameGet=" + msg(() => g.call(alien)));
        print("onProto=" + msg(() => PluginArray.prototype.item.call(PluginArray.prototype, 0)));
        print("order=" + msg(() => PluginArray.prototype.item.call(alien)));
        print("arity=" + msg(() => navigator.plugins.item()));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "item=TypeError: Illegal invocation",
        "named=TypeError: Illegal invocation",
        "refresh=TypeError: Illegal invocation",
        "nameGet=TypeError: Illegal invocation",
        "onProto=TypeError: Illegal invocation",
        "order=TypeError: Illegal invocation",
        "arity=TypeError: Failed to execute 'item' on 'PluginArray': 1 argument required, but only 0 present.",
    ]);
}

/// `window.speechSynthesis`: an own member of the global (the WebIDL rule for a `[Global]` interface's
/// members), an instance of `SpeechSynthesis`, and the answers a browser with no installed voice pack
/// gives. Headless Chrome 150.0.7871.46 on this machine answers
/// `hasOwnProperty.call(window, "speechSynthesis")` true and the tag `[object SpeechSynthesis]`
/// (probed 2026-09-11, over a loopback server).
#[tokio::test]
async fn the_speech_synthesis_singleton_is_an_own_window_member() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        print("own=" + Object.prototype.hasOwnProperty.call(window, "speechSynthesis"));
        print("tag=" + Object.prototype.toString.call(speechSynthesis));
        print("instof=" + (speechSynthesis instanceof SpeechSynthesis) + (speechSynthesis instanceof EventTarget));
        print("same=" + (window.speechSynthesis === window.speechSynthesis));
        print("state=" + speechSynthesis.pending + speechSynthesis.speaking + speechSynthesis.paused);
        print("voices=" + JSON.stringify(speechSynthesis.getVoices()));
        print("handler=" + speechSynthesis.onvoiceschanged);
        speechSynthesis.onvoiceschanged = function f() {};
        print("handlerSet=" + (typeof speechSynthesis.onvoiceschanged));
        print("keys=" + JSON.stringify(Object.getOwnPropertyNames(SpeechSynthesis.prototype)));
        print("own0=" + Object.getOwnPropertyNames(speechSynthesis).length);
        print("native=" + SpeechSynthesis.prototype.getVoices.toString().includes("[native code]"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "own=true",
        "tag=[object SpeechSynthesis]",
        "instof=truetrue",
        "same=true",
        "state=falsefalsefalse",
        "voices=[]",
        "handler=null",
        "handlerSet=function",
        // The capture's order, `constructor` last -- the hand-table shape.
        "keys=[\"pending\",\"speaking\",\"paused\",\"onvoiceschanged\",\"cancel\",\"getVoices\",\"pause\",\"resume\",\"speak\",\"constructor\"]",
        "own0=0",
        "native=true",
    ]);
}

/// `Audio` and `Option`: legacy FACTORIES, i.e. distinct function objects that share the element
/// interface's prototype. Chrome prints `function Audio() { [native code] }` and answers
/// `Object.getPrototypeOf(new Audio()) === HTMLAudioElement.prototype` true (same probe).
#[tokio::test]
async fn the_legacy_element_factories_share_their_interfaces_prototype() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        print("audioProto=" + (Audio.prototype === HTMLAudioElement.prototype)
              + (Option.prototype === HTMLOptionElement.prototype));
        const a = new Audio();
        print("audio=" + (a instanceof HTMLAudioElement) + " " + a.tagName + " " + a.preload);
        print("audioSrc=" + new Audio("x.mp3").getAttribute("src"));
        const o = new Option("hello", "v", true, true);
        print("option=" + (o instanceof HTMLOptionElement) + " " + o.tagName + " " + o.text);
        print("optionAttrs=" + o.getAttribute("value") + " " + o.hasAttribute("selected") + " " + o.selected);
        print("bare=" + new Option().text + "|" + new Option().hasAttribute("value"));
        print("native=" + String(Audio) + " | " + String(Option));
        print("len=" + Audio.length + Option.length);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "audioProto=truetrue",
        "audio=true AUDIO auto",
        "audioSrc=x.mp3",
        "option=true OPTION hello",
        "optionAttrs=v true true",
        "bare=|false",
        "native=function Audio() { [native code] } | function Option() { [native code] }",
        "len=00",
    ]);
}

/// `Crypto` and `History` from their tables: the recorded prototype shape, the oracle's values, and
/// failure mode 2 on every row of both.
///
/// Chrome (same probe) answers `Object.getOwnPropertyNames(Crypto.prototype)` with `constructor`
/// SECOND, not last; the engine's installer puts `constructor` last on every table it builds, which is
/// a whole-installer convention and not this lane's to change -- `generated_order::ORDER` does not pin
/// `constructor`'s position for any interface. The MEMBERS and their order are asserted; where
/// `constructor` falls among them is not.
#[tokio::test]
async fn crypto_and_history_are_tables_with_checked_receivers() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const msg = (f) => { try { return "ok:" + String(f()); } catch (e) { return e.constructor.name + ": " + e.message; } };
        const named = (o) => JSON.stringify(Object.getOwnPropertyNames(o).filter((k) => k !== "constructor"));
        print("cryptoProto=" + named(Crypto.prototype));
        print("histProto=" + named(History.prototype));
        // The bundle's oracle: length 1, scrollRestoration "auto", state null on a fresh document.
        print("histValues=" + history.length + " " + history.scrollRestoration + " " + history.state);
        history.scrollRestoration = "bogus";
        print("histBogus=" + history.scrollRestoration);
        history.scrollRestoration = "manual";
        print("histManual=" + history.scrollRestoration);
        history.pushState({ a: 1 }, "");
        print("histPushed=" + history.length + " " + JSON.stringify(history.state));
        print("grvNull=" + msg(() => crypto.getRandomValues(null)));
        print("grvFloat=" + msg(() => crypto.getRandomValues(new Float32Array(4))));
        print("grvForeign=" + msg(() => Crypto.prototype.getRandomValues.call({}, new Uint8Array(4))));
        print("uuidForeign=" + msg(() => Crypto.prototype.randomUUID.call({})));
        print("subtleForeign=" + msg(() => Object.getOwnPropertyDescriptor(Crypto.prototype, "subtle").get.call({})));
        print("histForeign=" + msg(() => Object.getOwnPropertyDescriptor(History.prototype, "length").get.call({})));
        print("pushForeign=" + msg(() => History.prototype.pushState.call({}, 1, "")));
        print("pushArity=" + msg(() => history.pushState()));
        // A view over PART of a buffer fills only its own window.
        const buf = new ArrayBuffer(8);
        const win = new Uint8Array(buf, 4, 4);
        crypto.getRandomValues(win);
        const all = new Uint8Array(buf);
        print("window=" + (all[0] === 0 && all[1] === 0 && all[2] === 0 && all[3] === 0));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "cryptoProto=[\"getRandomValues\",\"subtle\",\"randomUUID\"]",
        "histProto=[\"length\",\"scrollRestoration\",\"state\",\"back\",\"forward\",\"go\",\"pushState\",\"replaceState\"]",
        "histValues=1 auto null",
        "histBogus=auto",
        "histManual=manual",
        "histPushed=2 {\"a\":1}",
        "grvNull=TypeError: Failed to execute 'getRandomValues' on 'Crypto': parameter 1 is not of type 'ArrayBufferView'.",
        "grvFloat=DOMException: Failed to execute 'getRandomValues' on 'Crypto': The provided ArrayBufferView is of type 'Float32', which is not an integer array type.",
        "grvForeign=TypeError: Illegal invocation",
        "uuidForeign=TypeError: Illegal invocation",
        "subtleForeign=TypeError: Illegal invocation",
        "histForeign=TypeError: Illegal invocation",
        "pushForeign=TypeError: Illegal invocation",
        "pushArity=TypeError: Failed to execute 'pushState' on 'History': 2 arguments required, but only 0 present.",
        "window=true",
    ]);
}

/// `performance.memory` is Chrome's non-standard heap window, and it returns a `MemoryInfo`: an object
/// with NO own properties whose three numbers sit on a prototype Chrome does not expose as a global.
/// This engine returned a frozen plain object with three own keys.
///
/// Two things the Chrome probe (150.0.7871.46, loopback server, 2026-09-11) corrected in the JS this
/// replaces: the prototype's member ORDER is `totalJSHeapSize, usedJSHeapSize, jsHeapSizeLimit`, and
/// `performance.memory !== performance.memory` -- a FRESH object per read, where the JS cached one.
#[tokio::test]
async fn performance_memory_is_a_memory_info() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const m = performance.memory;
        print("tag=" + Object.prototype.toString.call(m));
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(m)));
        print("proto=" + JSON.stringify(Object.getOwnPropertyNames(Object.getPrototypeOf(m))));
        print("fresh=" + (performance.memory !== performance.memory));
        print("numbers=" + [typeof m.totalJSHeapSize, typeof m.usedJSHeapSize, typeof m.jsHeapSizeLimit].join(","));
        print("ordered=" + (m.jsHeapSizeLimit >= m.totalJSHeapSize && m.totalJSHeapSize >= m.usedJSHeapSize));
        print("desc=" + JSON.stringify((() => { const d = Object.getOwnPropertyDescriptor(Performance.prototype, "memory");
            return { e: d.enumerable, c: d.configurable, g: typeof d.get }; })()));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "tag=[object MemoryInfo]",
        "own=[]",
        "proto=[\"totalJSHeapSize\",\"usedJSHeapSize\",\"jsHeapSizeLimit\"]",
        "fresh=true",
        "numbers=number,number,number",
        "ordered=true",
        "desc={\"e\":true,\"c\":true,\"g\":\"function\"}",
    ]);
}

/// The `Window` table: its `[Global]` rows land on `globalThis` and NOT on `Window.prototype`, which
/// owns exactly `TEMPORARY` and `PERSISTENT` -- what the bundle records
/// (`singletons.window.members` is those two and nothing else). Every accessor and method is a real
/// `v8::Function`, which is the whole reason the first attempt at this interface was rejected: a plain
/// `get() { ... }` on the global object is a first-tier automation tell.
#[tokio::test]
async fn the_window_globals_are_native_and_own() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        const own = (k) => Object.prototype.hasOwnProperty.call(window, k);
        print("protoOwn=" + JSON.stringify(Object.getOwnPropertyNames(Window.prototype)));
        print("consts=" + Window.TEMPORARY + Window.PERSISTENT + Window.prototype.TEMPORARY + Window.prototype.PERSISTENT);
        print("onGlobal=" + own("name") + own("status") + own("closed") + own("length") + own("opener") + own("onload"));
        print("notOnProto=" + Object.prototype.hasOwnProperty.call(Window.prototype, "name"));
        const d = Object.getOwnPropertyDescriptor(window, "name");
        print("nameNative=" + d.get.toString().includes("[native code]") + d.set.toString().includes("[native code]")
              + " " + d.enumerable + d.configurable);
        print("nameFns=" + d.get.name + "|" + d.set.name + "|" + d.get.length + d.set.length);
        // A native function carries no own prototype/arguments/caller -- the shim tell.
        print("noShimKeys=" + JSON.stringify(Object.getOwnPropertyNames(d.get).sort()));
        print("handlerNative=" + Object.getOwnPropertyDescriptor(window, "onload").get.toString().includes("[native code]"));
        print("methodNative=" + window.alert.toString().includes("[native code]") + window.postMessage.toString().includes("[native code]"));
        // EventTarget is INHERITED, never an own property of the global.
        print("etInherited=" + (!own("addEventListener")) + (window.addEventListener === EventTarget.prototype.addEventListener));
        // Failure mode 2: every Window row refuses a foreign receiver, as Chrome's does.
        const msg = (f) => { try { f(); return "no throw"; } catch (e) { return e.constructor.name; } };
        print("foreign=" + msg(() => d.get.call({})) + msg(() => window.alert.call({}))
              + msg(() => window.postMessage.call({}, 1, "*"))
              + msg(() => Object.getOwnPropertyDescriptor(window, "closed").get.call({}))
              + msg(() => Object.getOwnPropertyDescriptor(window, "onload").get.call({})));
        // A FRAME's window IS a Window for the check's purposes -- `browsing_context.rs` re-parents
        // every one it mints onto `Window.prototype`, so the guard lets it through rather than
        // throwing. It carries the realm's `[Global]` members too (built from the
        // page realm's install surface) and its own `name`, so `typeof name` is Chrome's `string`.
        const fr = document.createElement("iframe");
        document.body.appendChild(fr);
        // The frame's OWN `Window` (Chrome 150: `instanceof Window` from the parent is false).
        print("frameIsWindow=" + (fr.contentWindow instanceof fr.contentWindow.Window) + " " + (typeof fr.contentWindow.name));
        print("globalIsWindow=" + (Object.getPrototypeOf(Object.getPrototypeOf(Object.getPrototypeOf(window))) === EventTarget.prototype));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "protoOwn=[\"TEMPORARY\",\"PERSISTENT\",\"constructor\"]",
        "consts=0101",
        "onGlobal=truetruetruetruetruetrue",
        "notOnProto=false",
        "nameNative=truetrue truetrue",
        "nameFns=get name|set name|01",
        "noShimKeys=[\"length\",\"name\"]",
        "handlerNative=true",
        "methodNative=truetrue",
        "etInherited=truetrue",
        "foreign=TypeErrorTypeErrorTypeErrorTypeErrorTypeError",
        // Chrome 150 answers `string` (the frame's own name): a frame's window has the
        // realm's global surface, and its own `name`.
        "frameIsWindow=true string",
        "globalIsWindow=true",
    ]);
}

/// The Window rows' BEHAVIOUR: the three writable attributes, the computed origin facts, the
/// `[Replaceable]` pair, and `postMessage`'s overload resolution and real delivery.
#[tokio::test]
async fn the_window_rows_answer_what_the_deleted_glue_answered() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><iframe></iframe><iframe></iframe></body></html>"#).unwrap();
    e.eval("t", r#"
        const msg = (f) => { try { return "ok:" + String(f()); } catch (e) { return e.constructor.name; } };
        window.name = "n"; window.status = "s"; window.opener = 7;
        print("writable=" + window.name + window.status + window.opener);
        // WebIDL's ES-to-DOMString conversion is ToString, which THROWS on a Symbol -- `String()` does
        // not, and the deleted JS's first version stored the string "Symbol()".
        print("symbol=" + msg(() => { window.name = Symbol(); }));
        print("undef=" + msg(() => { window.name = undefined; return window.name; }));
        print("readonly=" + window.closed + " " + window.frameElement + " " + window.credentialless);
        // `length` is the count of NESTED BROWSING CONTEXTS -- the frame-owning elements -- not 0.
        print("length=" + window.length);
        print("origin=" + window.origin + " secure=" + window.isSecureContext + " oac=" + window.originAgentCluster);
        print("coi=" + window.crossOriginIsolated);
        // `[Replaceable]`: assignment replaces the accessor with an own data property.
        print("event=" + window.event);
        window.event = 1;
        print("replaced=" + window.event + " " + (Object.getOwnPropertyDescriptor(window, "event").get === undefined));
        print("methods=" + window.alert() + " " + window.confirm() + " " + window.prompt() + " " + Object.prototype.toString.call(window.open("about:blank")));
        print("noops=" + window.moveBy(1, 1) + window.captureEvents() + window.releaseEvents());
        // The capture's own required-argument counts (generated_order::WINQ), on the rows.
        print("winq=" + msg(() => window.moveBy()) + msg(() => window.resizeTo(1)) + msg(() => window.reportError()));
        // postMessage: a non-null OBJECT second argument is the options dictionary, anything else a
        // targetOrigin -- so `postMessage(1)` is fine and `postMessage(1, "x")` throws.
        print("pmBare=" + msg(() => window.postMessage(1)));
        print("pmBadOrigin=" + msg(() => window.postMessage(1, "x")));
        print("pmArity=" + msg(() => window.postMessage()));
        globalThis.got = null;
        window.addEventListener("message", (ev) => { globalThis.got = ev.data; });
        window.postMessage({ hello: 1 }, "*");
        print("pmPending=" + globalThis.got);
    "#.into()).unwrap();
    e.run().await.unwrap();
    e.eval("t2", r#"print("pmDelivered=" + JSON.stringify(globalThis.got));"#.into()).unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    assert_eq!(out[0], "writable=ns7");
    assert_eq!(out[1], "symbol=TypeError");
    assert_eq!(out[2], "undef=ok:undefined");
    assert_eq!(out[3], "readonly=false null false");
    assert_eq!(out[4], "length=2");
    assert_eq!(out[6], "coi=false");
    assert_eq!(out[7], "event=undefined");
    assert_eq!(out[8], "replaced=1 true");
    // `open` hands back a REAL auxiliary browsing context (core/html/browsing_context.rs), which is
    // what the row body has done since W5.0d; it returns null only where the SPEC returns null.
    assert_eq!(out[9], "methods=undefined false null [object Window]");
    assert_eq!(out[10], "noops=undefinedundefinedundefined");
    // The capture's own required-argument counts, carried on the rows.
    assert_eq!(out[11], "winq=TypeErrorTypeErrorTypeError");
    assert_eq!(out[12], "pmBare=ok:undefined");
    assert_eq!(out[13], "pmBadOrigin=DOMException");
    assert_eq!(out[14], "pmArity=TypeError");
    assert_eq!(out[15], "pmPending=null");
    // Delivery is a LATER TASK, exactly as a browser's same-window post is.
    assert_eq!(out[16], "pmDelivered={\"hello\":1}");
}

/// `window.length` and `window[i]` follow every mutation that adds, removes or moves a
/// frame element -- `sync_frames` keeps its frame list between mutations and walks the document again
/// only when the DOM journal says a frame can have come, gone or moved -- and stay put across the ones
/// that cannot.
#[tokio::test]
async fn window_frames_follow_every_frame_mutation() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="a"></div><iframe name="one"></iframe></body></html>"#).unwrap();
    e.eval("t", r#"
        const b = document.body, a = document.getElementById("a");
        const order = () => window.length + ":" + [...document.querySelectorAll("iframe, frame")].map((f, i) => window[i] === f.contentWindow).join(",");
        // (The parser's frame alone: `window[0]` is not yet its `contentWindow` until a frame is
        // inserted -- a separate matter, so only the count is read until then.)
        print("start=" + window.length);
        a.appendChild(document.createElement("p"));
        print("noframe=" + window.length);
        a.appendChild(document.createElement("iframe"));
        print("before=" + order());
        const wrap = document.createElement("section");
        wrap.appendChild(document.createElement("iframe"));
        b.appendChild(wrap);
        print("subtree=" + order());
        a.remove();
        print("removed=" + order());
        b.insertBefore(wrap, b.firstChild);
        print("moved=" + order());
        b.innerHTML += "";
        print("replaced=" + order());
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "start=1",
        "noframe=1",
        "before=2:true,true",
        "subtree=3:true,true,true",
        "removed=2:true,true",
        "moved=2:true,true",
        "replaced=2:true,true",
    ]);
}

/// Frames moved among one parent's children, twice before `window[i]` is read, and
/// `window[i]` follows the new tree order (`sync_frames`' kept frame list, D2).
#[tokio::test]
async fn window_frames_follow_frames_reordered_among_their_siblings() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d"></div></body></html>"#).unwrap();
    e.eval("t", r#"
        const d = document.getElementById("d");
        const f = ["f1", "f2", "f3"].map(n => { const x = document.createElement("iframe"); x.name = n; d.appendChild(x); return x; });
        const order = () => window.length + ":" + [...d.children].map((x, i) => window[i] === x.contentWindow).join(",");
        print("start=" + order());
        d.appendChild(f[0]);
        d.appendChild(f[1]);
        print("reordered=" + order() + " first=" + d.firstChild.name);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["start=3:true,true,true", "reordered=3:true,true,true first=f3"]);
}
