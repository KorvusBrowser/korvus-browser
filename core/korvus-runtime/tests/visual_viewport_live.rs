//! `window.visualViewport` on the LIVE surface, against the four browsers' own answers.
//!
//! # The defect
//!
//! `VisualViewport` the interface object was on the global and the INSTANCE was not, which is a shape
//! no browser has. FP13 measured it on `zed.dev`: `'visualViewport' in window` is `true` on Chrome 150
//! and was `false` here, and a BARE `visualViewport` reference — the form a bundler emits — threw
//! `ReferenceError`. So it was a load-axis failure as well as a surface one.
//!
//! # Where the numbers come from
//!
//! Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93 and Opera 135.0.5973.133, headed over
//! CDP under Xvfb, a 1920x1080 window, 2026-09-19, at two origins. All four agreed on every shape and
//! every value below. The one that decides the lane:
//!
//! ```text
//! visualViewport.width / height             1904 / 976
//! documentElement.clientWidth / clientHeight 1904 / 976     <- THIS is what it tracks
//! innerWidth / innerHeight                  1919 / 991      <- and this is what it is NOT
//! ```
//!
//! # What is asserted, and why it is the RULE rather than the number
//!
//! `1904` is this box's window minus this build's scrollbar; a test that asserted it would be
//! asserting a screen size. The rule is "`width` IS `documentElement.clientWidth`", and the second
//! half — "and is therefore NOT `innerWidth`" — is asserted as an INEQUALITY of the two sources on a
//! page where they differ, so the assertion has content without naming a pixel.

use engine::Engine;
use std::time::Duration;

const URL: &str = "http://127.0.0.1:8123/page.html";

/// A page tall and wide enough to scroll on both axes, then `js`, as JSON.
async fn on_a_scrolling_page(js: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = "<!doctype html><html><head><title>t</title><style>body{margin:0}\
               #tall{height:6000px;width:4000px}</style></head><body><div id=tall>x</div></body></html>";
    e.load_page(doc, URL, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval_value("vv", js.to_string()).expect("eval")
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().expect("a string").to_string()).collect()
}

/// The name exists, answers an object, and a BARE reference resolves — the `zed.dev` row.
#[tokio::test]
async fn the_name_is_an_own_global_and_a_bare_reference_resolves() {
    let v = on_a_scrolling_page(
        r#"({
            inWindow: 'visualViewport' in window,
            own: Object.prototype.hasOwnProperty.call(window, 'visualViewport'),
            bare: (() => { try { return Object.prototype.toString.call(visualViewport); }
                           catch (e) { return e.constructor.name + ': ' + e.message; } })(),
            tag: Object.prototype.toString.call(window.visualViewport),
            same: window.visualViewport === window.visualViewport,
            isEventTarget: window.visualViewport instanceof EventTarget,
            isVisualViewport: window.visualViewport instanceof VisualViewport,
            addEventListener: typeof window.visualViewport.addEventListener,
            ownProps: Object.getOwnPropertyNames(window.visualViewport),
            desc: (d => [!!d.get, !!d.set, d.enumerable, d.configurable])(
                Object.getOwnPropertyDescriptor(window, 'visualViewport')),
            getterName: Object.getOwnPropertyDescriptor(window, 'visualViewport').get.name,
        })"#,
    )
    .await;
    assert_eq!(v["inWindow"], true);
    assert_eq!(v["own"], true, "`visualViewport` is an OWN property of the window on all four browsers");
    assert_eq!(v["bare"], "[object VisualViewport]", "a bare reference threw ReferenceError before FP5.5");
    assert_eq!(v["tag"], "[object VisualViewport]");
    assert_eq!(v["same"], true, "one object per realm — measured `true` on all four");
    assert_eq!(v["isEventTarget"], true);
    assert_eq!(v["isVisualViewport"], true);
    assert_eq!(v["addEventListener"], "function");
    // The instance carries NO own properties on any of the four: everything is on the prototype.
    assert_eq!(strings(&v["ownProps"]), Vec::<String>::new());
    // `[Replaceable]`: get + set, enumerable, configurable — measured on all four.
    assert_eq!(v["desc"], serde_json::json!([true, true, true, true]));
    assert_eq!(v["getterName"], "get visualViewport");
}

/// THE RULE: `width`/`height` are the DOCUMENT ELEMENT'S client area, and are therefore not
/// `innerWidth`/`innerHeight` on a page where those differ.
#[tokio::test]
async fn width_and_height_are_the_document_elements_client_area() {
    let v = on_a_scrolling_page(
        r#"({
            width: visualViewport.width, height: visualViewport.height,
            clientWidth: document.documentElement.clientWidth,
            clientHeight: document.documentElement.clientHeight,
            innerWidth: window.innerWidth, innerHeight: window.innerHeight,
        })"#,
    )
    .await;
    eprintln!("[fp5.5] visualViewport {}x{}  client {}x{}  inner {}x{}",
        v["width"], v["height"], v["clientWidth"], v["clientHeight"], v["innerWidth"], v["innerHeight"]);
    // Non-vacuity: a viewport of zero would satisfy any identity here.
    assert!(v["width"].as_f64().expect("a number") > 0.0, "the viewport has no width at all");
    assert!(v["height"].as_f64().expect("a number") > 0.0, "the viewport has no height at all");
    assert_eq!(v["width"], v["clientWidth"], "visualViewport.width IS documentElement.clientWidth");
    assert_eq!(v["height"], v["clientHeight"], "visualViewport.height IS documentElement.clientHeight");
    // THE OTHER HALF, and the one a guess gets wrong. This page scrolls on both axes, so the client
    // area is the inner box less the scrollbars and the two sources DIFFER — which is what makes the
    // identity above a claim about which source is read rather than a coincidence. Measured 1904 vs
    // 1919 on Chrome 150 and 1905 vs 1920 here: the same 15px, the same two numbers.
    assert_ne!(v["width"], v["innerWidth"], "the two viewport sources must differ, or nothing is proven");
    assert_ne!(v["height"], v["innerHeight"], "the two viewport sources must differ, or nothing is proven");
    // …and `viewport.segments[0]` (core/html/bar_props.rs) is the OTHER one, on the same page. The two
    // viewport objects disagree on purpose; this asserts that they still do.
    let seg = on_a_scrolling_page(
        "({ segW: viewport.segments[0].width, segH: viewport.segments[0].height, \
            innerWidth: window.innerWidth, innerHeight: window.innerHeight })",
    )
    .await;
    assert_eq!(seg["segW"], seg["innerWidth"], "viewport.segments[0] IS the inner box");
    assert_eq!(seg["segH"], seg["innerHeight"], "viewport.segments[0] IS the inner box");
}

/// `pageLeft`/`pageTop` follow the document's scroll; `offsetLeft`/`offsetTop` and `scale` do not move
/// without pinch-zoom, which this engine has no input for.
#[tokio::test]
async fn page_offsets_follow_the_scroll_and_the_offsets_do_not() {
    let v = on_a_scrolling_page(
        r#"(() => {
            const snap = () => [visualViewport.offsetLeft, visualViewport.offsetTop,
                                visualViewport.pageLeft, visualViewport.pageTop,
                                visualViewport.scale, window.scrollX, window.scrollY];
            const top = snap();
            window.scrollTo(120, 300);
            const scrolled = snap();
            window.scrollTo(0, 0);
            return { top, scrolled, back: snap() };
        })()"#,
    )
    .await;
    eprintln!("[fp5.5] top={} scrolled={} back={}", v["top"], v["scrolled"], v["back"]);
    let top = v["top"].as_array().expect("an array");
    let sc = v["scrolled"].as_array().expect("an array");
    // The scroll has to have happened, or every claim below is vacuous.
    assert!(sc[6].as_f64().expect("a number") > 0.0, "the document did not scroll; nothing below means anything");
    // offsetLeft/offsetTop: the VISUAL viewport's displacement inside the LAYOUT viewport — 0 without
    // pinch-zoom, on all four browsers, at the top of the page and after a 300px scroll alike.
    assert_eq!(top[0], 0.0);
    assert_eq!(top[1], 0.0);
    assert_eq!(sc[0], 0.0);
    assert_eq!(sc[1], 0.0);
    // pageLeft/pageTop ARE the scroll position plus that offset.
    assert_eq!(sc[2], sc[5], "pageLeft is scrollX + offsetLeft");
    assert_eq!(sc[3], sc[6], "pageTop is scrollY + offsetTop");
    assert_eq!(v["back"][2], 0.0);
    assert_eq!(v["back"][3], 0.0);
    // scale is 1 throughout: no pinch-zoom exists here, which is the same reason it is 1 on the
    // unzoomed page the rig measured.
    assert_eq!(top[4], 1.0);
    assert_eq!(sc[4], 1.0);
}

/// The interface object and its prototype, exactly as all four captures record them.
#[tokio::test]
async fn the_interface_object_is_the_browsers() {
    let v = on_a_scrolling_page(
        r#"({
            protoMembers: Object.getOwnPropertyNames(VisualViewport.prototype),
            tagOnProto: VisualViewport.prototype[Symbol.toStringTag],
            parent: Object.getPrototypeOf(VisualViewport.prototype) === EventTarget.prototype,
            ctorParent: Object.getPrototypeOf(VisualViewport) === EventTarget,
            ctorLength: VisualViewport.length,
            newThrows: (() => { try { new VisualViewport(); return "no throw"; }
                                catch (e) { return e.constructor.name + ': ' + e.message; } })(),
            bareCall: (() => { try { VisualViewport(); return "no throw"; }
                               catch (e) { return e.constructor.name + ': ' + e.message; } })(),
            foreignGetter: (() => { try {
                    Object.getOwnPropertyDescriptor(VisualViewport.prototype, 'width').get.call({});
                    return "no throw";
                } catch (e) { return e.constructor.name + ': ' + e.message; } })(),
            handlers: ['onresize', 'onscroll', 'onscrollend'].map(n => [
                visualViewport[n],
                (d => [!!d.get, !!d.set, d.enumerable, d.configurable])(
                    Object.getOwnPropertyDescriptor(VisualViewport.prototype, n))]),
            native: Object.getOwnPropertyNames(VisualViewport.prototype)
                .map(n => Object.getOwnPropertyDescriptor(VisualViewport.prototype, n))
                .every(d => !d.get || String(d.get).includes('[native code]')),
        })"#,
    )
    .await;
    // The ORDER and the SET are the four captures', which agree name for name. `constructor` sits at
    // index 10 in all four; the create places it from the capture (`Mount::ctor_at`),
    // and this asserts where it landed rather than restating a "constructor is last" constant.
    assert_eq!(
        strings(&v["protoMembers"]),
        [
            "offsetLeft", "offsetTop", "pageLeft", "pageTop", "width", "height", "scale",
            "onresize", "onscroll", "onscrollend", "constructor"
        ]
    );
    assert_eq!(v["tagOnProto"], "VisualViewport");
    assert_eq!(v["parent"], true, "VisualViewport inherits from EventTarget in all four captures");
    assert_eq!(v["ctorParent"], true);
    assert_eq!(v["ctorLength"], 0);
    assert_eq!(v["newThrows"], "TypeError: Failed to construct 'VisualViewport': Illegal constructor");
    assert_eq!(v["bareCall"], "TypeError: Illegal constructor");
    assert_eq!(v["foreignGetter"], "TypeError: Illegal invocation");
    // Each handler reads `null` before anything is assigned, and is a get/set pair, e=1 c=1.
    assert_eq!(
        v["handlers"],
        serde_json::json!([
            [serde_json::Value::Null, [true, true, true, true]],
            [serde_json::Value::Null, [true, true, true, true]],
            [serde_json::Value::Null, [true, true, true, true]]
        ])
    );
    assert_eq!(v["native"], true);
}

/// It is a real `EventTarget`: a listener added to it is remembered and dispatched to.
#[tokio::test]
async fn it_is_a_real_event_target() {
    let v = on_a_scrolling_page(
        r#"(() => {
            const log = [];
            visualViewport.addEventListener('resize', () => log.push('listener'));
            visualViewport.onresize = () => log.push('handler');
            visualViewport.dispatchEvent(new Event('resize'));
            const after = visualViewport.onresize;
            visualViewport.onresize = null;
            return { log, hadHandler: typeof after, cleared: visualViewport.onresize };
        })()"#,
    )
    .await;
    assert_eq!(strings(&v["log"]), ["listener", "handler"], "both the listener and the handler ran");
    assert_eq!(v["hadHandler"], "function");
    assert_eq!(v["cleared"], serde_json::Value::Null);
}
