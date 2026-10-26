//! GS.3 Gate (DoD-6): no automation tell. Every global that a patched ChromeDriver / Selenium /
//! PhantomJS / Nightmare injects must read real `undefined`, and `navigator.webdriver` must be a
//! NATIVE `false` getter (not a JS override, which is itself the tell stealth plugins leave behind).
//! Korvus is a from-scratch engine, so these are absent by construction -- this test locks that in so
//! a future change (e.g. a CDP shim that leaks a marker) can't silently reintroduce a tell.
use engine::Engine;

#[tokio::test]
async fn no_automation_tells() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("t", r#"
        // The canonical injected-global tells across the major automation stacks.
        const TELLS = [
            // ChromeDriver document/window key + generic cdc_ scan.
            "$cdc_asdjflasutopfhvcZLmcfl_", "cdc_adoQpoasnfa76pfcZLmcfl_Array",
            "cdc_adoQpoasnfa76pfcZLmcfl_Promise", "cdc_adoQpoasnfa76pfcZLmcfl_Symbol",
            // Selenium / WebDriver.
            "__webdriver_evaluate", "__selenium_evaluate", "__webdriver_script_function",
            "__webdriver_script_func", "__webdriver_script_fn", "__fxdriver_evaluate",
            "__driver_unwrapped", "__webdriver_unwrapped", "__driver_evaluate",
            "__selenium_unwrapped", "__fxdriver_unwrapped", "_Selenium_IDE_Recorder",
            "_selenium", "calledSelenium", "webdriver", "__webdriverFunc", "__lastWatirAlert",
            // PhantomJS.
            "_phantom", "callPhantom", "phantom",
            // Nightmare / Puppeteer-ish / Chrome headless automation controllers.
            "__nightmare", "domAutomation", "domAutomationController", "_WEBDRIVER_ELEM_CACHE",
        ];
        const leaked = TELLS.filter((k) => typeof globalThis[k] !== "undefined");
        // cdc_ scan: any own key on window/document starting with the ChromeDriver prefix.
        const cdcWin = Object.getOwnPropertyNames(globalThis).filter((k) => k.indexOf("cdc_") === 0 || k.indexOf("$cdc_") === 0);
        const cdcDoc = (typeof document !== "undefined")
            ? Object.getOwnPropertyNames(document).filter((k) => k.indexOf("cdc_") === 0 || k.indexOf("$cdc_") === 0) : [];
        print("leaked=" + JSON.stringify(leaked));
        print("cdcWin=" + cdcWin.length + " cdcDoc=" + cdcDoc.length);
        print("webdriver=" + navigator.webdriver);
        // The webdriver getter must be native (a JS-defined false is the stealth-plugin tell).
        const g = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), "webdriver").get;
        print("webdriverNative=" + (typeof g === "function" && g.toString().includes("[native code]")));
        // The permissions/notification stealth mismatch some detectors probe: Notification.permission
        // must not be "denied" while navigator.permissions reports "granted" (a classic headless combo).
        print("notif=" + (typeof Notification !== "undefined" ? Notification.permission : "n/a"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "leaked=[]",
        "cdcWin=0 cdcDoc=0",
        "webdriver=false",
        "webdriverNative=true",
        "notif=default",
    ]);
}

/// The engine's own bookkeeping must never reach `globalThis`.
///
/// This is the general form of the test above, and it exists because the specific form did not catch
/// the real leak. `LIFECYCLE_EVENTS` set `globalThis.__korvusLoadFired = true` on every navigation --
/// a page-visible own global carrying the product's name, which
/// `Object.getOwnPropertyNames(window).filter(k => /korvus/i.test(k))` finds in one line. The list
/// above could not see it: it enumerates the tells OTHER automation stacks inject, and this was ours.
///
/// So the rule is stated structurally instead of by name: after a full page load, no own global may
/// name this engine, and none may start with `__`. The recorded Chrome-151 baseline has no
/// underscore-prefixed global at all (the two in `brave-151.surface.json` are the captured page's own
/// Discourse variables), so "zero" is the honest bar rather than a convenient one.
///
/// It drives `load_page` rather than `load_html` for the same reason -- the leak was written by the
/// lifecycle step, which only the full navigation path runs.
#[tokio::test]
async fn engine_bookkeeping_never_reaches_the_global() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    // No <script src>, so this reaches no network; base_url only has to parse.
    e.load_page(
        "<!doctype html><html><head><title>t</title></head><body><div id=probe>x</div></body></html>",
        "http://127.0.0.1/",
        &[],
    )
    .await
    .unwrap();
    let _ = e.pump(std::time::Duration::from_millis(200)).await;
    e.eval("t", r#"
        const own = Object.getOwnPropertyNames(globalThis);
        print("named=" + JSON.stringify(own.filter((k) => /korvus/i.test(k))));
        print("underscored=" + JSON.stringify(own.filter((k) => k.startsWith("__"))));
        print("symbols=" + JSON.stringify(Object.getOwnPropertySymbols(globalThis).map(String)));
        // AND THE BRIDGE HOLDER IS NOT THERE TO BE FOUND. `symbols=[]` above used to be
        // true because two rewritten intrinsics FILTERED the one symbol out; the window realm mints no
        // holder now, so this asks the direct question the filter could never have answered `false`
        // to. A page that knows the name still gets nothing.
        print("holder=" + typeof globalThis[Symbol.for("korvus.bridge")]);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "named=[]",
        "underscored=[]",
        "symbols=[]",
        "holder=undefined",
    ]);
    // The load signal itself still has to work, or the navigate pump goes back to burning its whole
    // budget on every page. It is the ENGINE's field now, not a slot the page could read -- which is
    // the point of the assertion above it.
    assert!(e.load_fired(), "the `load` marker must be set once the page has loaded");
}

/// The `window` prototype chain is exactly as deep as Chrome's, and each link reports what Chrome's does.
///
/// HTML puts ONE object between `Window.prototype` and `EventTarget.prototype` -- the named-properties
/// exotic -- and the recorded browser confirms it: `Window.proto` is `["EventTarget", "EventTarget"]`,
/// two links, because that intermediate has no own `constructor` and answers with its parent's name.
///
/// Korvus had two objects there, because `generated_shells.js` kept splicing an empty placeholder from
/// before the native named-properties object existed, and `dom/09h-window-named.js` then spliced the
/// real one on top of it. Walking the chain is three lines and a standard fingerprinting probe, so the
/// extra link was directly observable. Asserting the tags as well as the depth keeps a future fix from
/// restoring the count by deleting the wrong object.
#[tokio::test]
async fn window_prototype_chain_matches_the_recorded_browser() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body><div id=probe>x</div></body></html>").unwrap();
    e.eval("t", r#"
        const chain = [];
        for (let o = Object.getPrototypeOf(globalThis); o; o = Object.getPrototypeOf(o)) {
            chain.push(Object.prototype.toString.call(o) + "/" + (o.constructor && o.constructor.name));
        }
        print(JSON.stringify(chain));
        // The named-properties object is not merely a spacer: an element's id is reachable as a global
        // through it, and it must not shadow what EventTarget.prototype provides.
        print("named=" + (typeof probe !== "undefined" && probe.tagName));
        print("inherited=" + (typeof window.addEventListener === "function"
            && !Object.prototype.hasOwnProperty.call(globalThis, "addEventListener")));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        r#"["[object Window]/Window","[object WindowProperties]/EventTarget","[object EventTarget]/EventTarget","[object Object]/Object"]"#,
        "named=DIV",
        "inherited=true",
    ]);
}
