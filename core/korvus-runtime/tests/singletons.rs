//! The five singletons are instances of their own interfaces.
//!
//! `crypto`, `performance`, `history` and `location` were plain objects, and `document` hung straight
//! off `Document.prototype`. So all five of these read `false` where every browser reads `true`, and
//! three of the objects answered `[object Object]`:
//!
//!     crypto instanceof Crypto     performance instanceof Performance     history instanceof History
//!     location instanceof Location                     document instanceof HTMLDocument
//!
//! Each is one line to check and several are in the standard fingerprinting suites. The expected
//! values below are not derived from a spec reading -- they were measured on the Brave 151 installed
//! on this machine, driven over CDP, which is also where the two surprises came from: `location`'s
//! members are OWN and non-configurable with an empty prototype (`[LegacyUnforgeable]`), and
//! `performance.memory` returns an object with no own properties at all.
use engine::Engine;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page(
        "<!doctype html><html><head><title>t</title></head><body><div id=probe>x</div></body></html>",
        "http://127.0.0.1/",
        &[],
    )
    .await
    .unwrap();
    e
}

#[tokio::test]
async fn singletons_are_instances_of_their_interfaces() {
    let mut e = page().await;
    e.eval("s", r#"
        const tag = (v) => Object.prototype.toString.call(v);
        print("instanceof=" + [
            crypto instanceof Crypto, performance instanceof Performance, history instanceof History,
            location instanceof Location, document instanceof HTMLDocument,
        ].join(","));
        print("tags=" + [tag(crypto), tag(performance), tag(history), tag(location), tag(document)].join(" "));
        print("ctor=" + document.constructor.name);
        // Performance is an EventTarget in Chrome, and the chain has to say so.
        print("perfChain=" + (performance instanceof EventTarget)
              + "," + (Object.getPrototypeOf(Performance.prototype) === EventTarget.prototype));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "instanceof=true,true,true,true,true",
        "tags=[object Crypto] [object Performance] [object History] [object Location] [object HTMLDocument]",
        "ctor=HTMLDocument",
        "perfChain=true,true",
    ]);
}

#[tokio::test]
async fn singletons_own_nothing_that_a_browser_does_not() {
    let mut e = page().await;
    e.eval("o", r#"
        // Brave 151: all three report []. Members belong to the interface, not to the instance -- and
        // the observer list that used to ride along as `performance._observers` is bridge state now,
        // because re-homing would otherwise have made an internal a permanent member of the prototype.
        print("own=" + JSON.stringify([crypto, performance, history].map((o) => Object.getOwnPropertyNames(o))));
        // ...and none of them leaks a symbol either.
        print("syms=" + JSON.stringify([crypto, performance, history, location].map((o) => Object.getOwnPropertySymbols(o).length)));
        // `document`'s one own member is `location`, exactly as Chrome's is. It used to be
        // `currentScript`, which a browser keeps on Document.prototype.
        print("docOwn=" + JSON.stringify(Object.getOwnPropertyNames(document)));
        // Location is the mirror image of the others by design: [LegacyUnforgeable] puts every member
        // on the INSTANCE, non-configurable, and leaves the prototype holding only `constructor`.
        print("locProto=" + JSON.stringify(Object.getOwnPropertyNames(Location.prototype)));
        const bad = Object.getOwnPropertyNames(location)
            .filter((n) => Object.getOwnPropertyDescriptor(location, n).configurable);
        print("locOwnConfigurable=" + JSON.stringify(bad));
        print("locHasMembers=" + (Object.getOwnPropertyNames(location).length >= 11));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "own=[[],[],[]]",
        // `location` owns ONE symbol, `@@toPrimitive` (undefined, non-enumerable) -- HTML's Location
        // object defines it and all four browsers of the family have it
        // (`fixtures/p11a/desc.*.txt`). This read 0 until they were asked.
        "syms=[0,0,0,1]",
        r#"docOwn=["location"]"#,
        r#"locProto=["constructor"]"#,
        "locOwnConfigurable=[]",
        "locHasMembers=true",
    ]);
}

#[tokio::test]
async fn singletons_still_work_and_still_report_native() {
    let mut e = page().await;
    e.eval("w", r#"
        // Moving a member to its prototype is only worth anything if it still does its job there.
        const a = new Uint8Array(8); crypto.getRandomValues(a);
        print("crypto=" + (a.some((x) => x !== 0)) + "," + (crypto.randomUUID().length === 36)
              + "," + (typeof crypto.subtle));
        performance.mark("m1");
        print("perf=" + (typeof performance.now() === "number")
              + "," + (performance.getEntriesByName("m1").length === 1)
              + "," + (typeof performance.timeOrigin === "number"));
        // performance.memory is a MemoryInfo: no own properties, three numbers on a prototype Chrome
        // does not expose by name.
        print("memory=" + (performance.memory ? Object.getOwnPropertyNames(performance.memory).length + ","
              + Object.prototype.toString.call(performance.memory) + ","
              + (performance.memory.jsHeapSizeLimit > 0) : "absent"));
        // history's data members are accessors on the prototype now, so pushState cannot write through
        // `this` any more -- it keeps its own state and `state`/`length` report it.
        history.pushState({ a: 1 }, "");
        print("history=" + JSON.stringify(history.state) + "," + history.length);
        history.scrollRestoration = "manual";
        print("scrollRestoration=" + history.scrollRestoration);
        print("location=" + (location.href.length > 0) + "," + (typeof location.assign === "function"));
        // Every one of these has to keep reporting [native code]: a JS-visible function body is the
        // shim tell the whole native-binding thesis exists to avoid.
        const src = (f) => Function.prototype.toString.call(f).includes("[native code]");
        const g = (o, n) => Object.getOwnPropertyDescriptor(o, n).get;
        print("native=" + [
            src(Crypto.prototype.randomUUID), src(Performance.prototype.now),
            src(History.prototype.pushState), src(g(Performance.prototype, "timeOrigin")),
            src(g(History.prototype, "state")), src(g(Document.prototype, "currentScript")),
            src(g(location, "href")), src(location.reload),
        ].join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "crypto=true,true,object",
        "perf=true,true,true",
        "memory=0,[object MemoryInfo],true",
        r#"history={"a":1},2"#,
        "scrollRestoration=manual",
        "location=true,true",
        "native=true,true,true,true,true,true,true,true",
    ]);
}

/// A facade document (DOMParser, `createHTMLDocument`) and the page document are different kinds of
/// object, and both have to classify correctly.
///
/// `HTMLDocument`/`XMLDocument` carry a custom `Symbol.hasInstance` that matched a `__docKind` marker
/// on the facade objects. A custom `@@hasInstance` REPLACES the prototype walk rather than adding to
/// it, so once the page document was given a real chain the hook still said no -- the last thing
/// standing between `document.constructor.name === "HTMLDocument"` (already true) and
/// `document instanceof HTMLDocument` (still false).
#[tokio::test]
async fn both_kinds_of_document_classify_correctly() {
    let mut e = page().await;
    e.eval("k", r#"
        const xml = new DOMParser().parseFromString("<a/>", "text/xml");
        print("xml=" + [xml instanceof XMLDocument, xml instanceof HTMLDocument, xml instanceof Document].join(","));
        const html = document.implementation.createHTMLDocument("t");
        print("facade=" + [html instanceof HTMLDocument, html instanceof XMLDocument, html instanceof Document].join(","));
        print("page=" + [document instanceof HTMLDocument, document instanceof XMLDocument,
                         document instanceof Document, document instanceof Node].join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "xml=true,false,true",
        "facade=true,false,true",
        "page=true,false,true,true",
    ]);
}

/// Every declared member of a natively-built interface is present, not just the interfaces that were
/// missing entirely.
///
/// `generated_shells.js` created whole interfaces the engine had not implemented, and skipped any name
/// already on `globalThis` -- so the interfaces Korvus DOES implement by hand kept their holes, and
/// those are exactly the ones a page touches most. Measured against Brave 151 before this:
/// `Element.prototype` 109 members against 153, `Document.prototype` 222 against 248,
/// `Screen.prototype` 9 against 12, with `el.animate`, `el.requestFullscreen`,
/// `el.setPointerCapture`, `el.getHTML` and `screen.orientation` simply absent.
///
/// The counts below are the worn persona's capture -- Chrome 152's own-name counts (measured 2026-10-16; Chrome
/// 150's `Element.prototype` had 152, without `ariaActionsElements`) -- so a regression shows up as a number rather
/// than as a vague "fewer".
#[tokio::test]
async fn declared_members_are_present_on_the_native_interfaces() {
    let mut e = page().await;
    e.eval("m", r##"
        const n = (o) => Object.getOwnPropertyNames(o).length;
        print("counts=" + [n(Element.prototype), n(Document.prototype), n(Node.prototype),
                           n(HTMLElement.prototype), n(Screen.prototype), n(Crypto.prototype),
                           n(Performance.prototype), n(Event.prototype)].join(","));
        // A sample of what was missing, chosen because each is something a page or a fingerprinting
        // script actually reaches for.
        const el = document.getElementById("probe");
        print("element=" + ["animate", "getAnimations", "requestFullscreen", "setPointerCapture",
                           "releasePointerCapture", "getHTML", "setHTMLUnsafe", "computedStyleMap",
                           "scrollIntoViewIfNeeded", "webkitMatchesSelector", "part"]
              .map((k) => k in el).join(","));
        print("screen=" + ["orientation", "isExtended", "onchange"].map((k) => k in screen).join(","));
        // ...and that filling them did not overwrite anything real. `querySelector` and `id` were
        // implemented before this pass and must still work, not answer undefined.
        print("kept=" + (document.querySelector("#probe") === el) + "," + (el.id === "probe")
              + "," + (typeof el.getBoundingClientRect().width === "number"));
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "counts=153,251,48,143,12,4,19,22",
        "element=true,true,true,true,true,true,true,true,true,true,true",
        "screen=true,true,true",
        "kept=true,true,true",
    ]);
}

/// The capture is a recording of one real page, and that page ran Ember.
///
/// Ember extends `Array.prototype`, so `brave-151.surface.json` records `pushObject`, `mapBy`,
/// `firstObject`, `_super` and thirty more as members of `Array`. A member backfill driven by the
/// capture alone would install every one of them on every page Korvus loads -- a far louder tell than
/// the holes it is closing, and a real break besides. The generator's filter is "does the WebIDL
/// corpus declare this interface", which removes the ECMAScript intrinsics where the contamination
/// lives, at the source rather than by naming symptoms.
#[tokio::test]
async fn the_captures_page_pollution_does_not_reach_the_page() {
    let mut e = page().await;
    e.eval("p", r#"
        const ember = ["pushObject", "pushObjects", "popObject", "shiftObject", "unshiftObject",
                       "mapBy", "filterBy", "findBy", "rejectBy", "sortBy", "uniqBy", "objectsAt",
                       "firstObject", "lastObject", "getEach", "setEach", "isAny", "isEvery",
                       "removeAt", "insertAt", "toArray", "compact", "_super", "invoke"];
        print("array=" + JSON.stringify(ember.filter((k) => k in Array.prototype)));
        // ...and the intrinsics still behave, which is the half a member count cannot show.
        print("intrinsics=" + (typeof Array.prototype.map === "function")
              + "," + ([3, 1, 2].sort()[0] === 1)
              + "," + (Object.keys({ a: 1 }).length === 1));
        // The captured page's own globals are not ours either.
        print("page=" + JSON.stringify(["jQuery", "Discourse", "EmberENV", "I18n", "MessageBus",
                                        "ItsATrap", "moment", "requirejs"].filter((k) => k in globalThis)));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "array=[]",
        "intrinsics=true,true,true",
        "page=[]",
    ]);
}
