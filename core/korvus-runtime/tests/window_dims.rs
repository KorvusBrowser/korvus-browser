//! The `Window`'s thirteen geometry attributes and three scroll methods, read back off the LIVE
//! surface -- plus `devicePixelRatio` and `screen.avail`/`pixelDepth`.
//!
//! They were sixteen `Object.defineProperty(globalThis, …)` sites in
//! `core/korvus-runtime/src/js/host.js`, behind twenty-one `op_nativize` masks. They are rows of the
//! `Window` table now, with bodies in `core/korvus-webapi/src/core/html/window_geometry.rs`, and the
//! two kernel ops that fed them (`op_window_geometry`, `op_device_pixel_ratio`) are deleted.
//!
//! Every expectation in `the_geometry_rows_are_shaped_like_chromes` is Chrome 150.0.7871.46's own
//! answer, measured with `--headless=new` over a `file:` URL.
use engine::Engine;
use spec::ScreenProfile;

#[tokio::test]
async fn window_dimensions() {
    let mut e = Engine::new(persona::Overridden::new().screen(ScreenProfile { width: 1920, height: 1080, color_depth: 30, device_pixel_ratio: 2.0, ..Default::default() }).boxed());
    e.run().await.unwrap();
    e.eval("w", r#"
        print("inner=" + innerWidth + "x" + innerHeight);
        print("dpr=" + devicePixelRatio);
        print("avail=" + screen.availWidth + "x" + screen.availHeight + " pd=" + screen.pixelDepth);
        print("scroll=" + scrollX + "," + scrollY + "," + pageYOffset);
        print("win=" + (window.innerWidth === innerWidth));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "inner=1920x1080",
        "dpr=2",
        "avail=1920x1080 pd=30",
        "scroll=0,0,0",
        "win=true",
    ]);
}


/// The DESCRIPTORS, the `[Replaceable]` setter and the receiver check -- the three things the table
/// rows own that the JS `defineProperty` block did not have right.
#[tokio::test]
async fn the_geometry_rows_are_shaped_like_chromes() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    // THE PAGE HAS ROOM TO SCROLL ON PURPOSE. `<body>x</body>` is 40px tall in a 937px
    // viewport, and a document with nothing to scroll does not scroll: Chrome 150.0.7871.46,
    // measured on that exact markup over CDP (2026-09-13), answers `0,0` to every step of the
    // sequence below -- `scrollTo(10, 20)` included. Pinning the old numbers on that page meant
    // pinning the defect this unit removed, and pinning `0,0` instead would have left a sequence
    // that passes whether or not `scrollTo` moves anything at all. So the page grew a box big
    // enough for every step to be reachable, and every expectation below is what Chrome answers on
    // a page of that shape (measured: 10,20 / 15,0 / 3,4 / 0,7 / 0,0).
    e.load_html(
        "<!doctype html><html><body style='margin:0'>x\
         <div style='width:4000px;height:5000px'></div></body></html>",
    )
    .unwrap();
    e.eval(
        "w4j-geom",
        r#"
        const NAMES = ["innerWidth","innerHeight","outerWidth","outerHeight","devicePixelRatio",
                       "screenX","screenLeft","screenY","screenTop",
                       "scrollX","pageXOffset","scrollY","pageYOffset"];
        // Chrome: every one is an own [Replaceable] accessor pair on `window` and NOT on
        // `Window.prototype` -- `e=1 c=1`, `get X` of length 0, `set X` of length 1, both native.
        const shapes = new Set(NAMES.map((n) => {
            const d = Object.getOwnPropertyDescriptor(window, n);
            return [!!d.get, !!d.set, +d.enumerable, +d.configurable, d.get.name === "get " + n,
                    d.get.length, d.set.name === "set " + n, d.set.length,
                    String(d.get).includes("[native code]"), String(d.set).includes("[native code]"),
                    !Object.getOwnPropertyDescriptor(Window.prototype, n)].join("");
        }));
        print("attrShapes=" + [...shapes].join(" | "));
        const M = ["scrollTo","scroll","scrollBy"];
        const mshapes = new Set(M.map((n) => {
            const d = Object.getOwnPropertyDescriptor(window, n);
            return [+d.writable, +d.enumerable, +d.configurable, d.value.name === n, d.value.length,
                    String(d.value).includes("[native code]"),
                    !Object.getOwnPropertyDescriptor(Window.prototype, n)].join("");
        }));
        print("methShapes=" + [...mshapes].join(" | "));
        // The four alias pairs are the SAME value from the same source.
        print("aliases=" + [screenLeft === screenX, screenTop === screenY,
                            pageXOffset === scrollX, pageYOffset === scrollY].join(","));
        // Chrome throws `Illegal invocation` from every getter AND every setter of this group; the JS
        // bodies ignored `this` and answered.
        const foreign = (n, set) => {
            const d = Object.getOwnPropertyDescriptor(window, n);
            try { set ? d.set.call({}, 1) : d.get.call({}); return "NO-THROW"; }
            catch (err) { return err.name + ":" + err.message; }
        };
        print("foreign=" + [...new Set(NAMES.flatMap((n) => [foreign(n, false), foreign(n, true)]))].join(" | "));
        // ...and does NOT throw from the three scroll methods. Measured, and the only member of this
        // group that behaves that way.
        print("scrollForeign=" + M.map((n) => { try { window[n].call({}); return "ok"; } catch (e) { return e.name; } }).join(","));
        // The scroll bookkeeping: `scrollTo` sets, `scrollBy` adds, both clamp at 0, the dictionary
        // overload reads left/top, an ABSENT member is left alone while a positional `null` is 0,
        // and a non-numeric is 0.
        scrollTo(10, 20);           print("scrollTo=" + scrollX + "," + scrollY);
        scrollBy(5, -100);          print("scrollBy=" + scrollX + "," + scrollY);
        scroll({ left: 3, top: 4 }); print("scrollDict=" + scrollX + "," + scrollY);
        scrollTo({ top: 7 });       print("scrollDictTop=" + scrollX + "," + scrollY);
        scrollTo(null, 7);          print("scrollToNull=" + scrollX + "," + scrollY);
        scrollTo("x", "y");         print("scrollToNaN=" + scrollX + "," + scrollY);
        // ...and ONE positional argument is not the (x, y) overload at all -- it is a
        // ScrollToOptions that names no axis, so nothing moves. Chrome, measured: (100, 100) in,
        // (100, 100) out, for `scrollTo(10)` and for `scrollBy(5)` alike.
        scrollTo(100, 100); scrollTo(10);  print("scrollToOne=" + scrollX + "," + scrollY);
        scrollTo(100, 100); scrollBy(5);   print("scrollByOne=" + scrollX + "," + scrollY);
        scrollTo(100, 100); scrollTo();    print("scrollToNone=" + scrollX + "," + scrollY);
        // [Replaceable]: the setter deletes the accessor and leaves an own data property behind.
        window.innerWidth = 5;
        const d = Object.getOwnPropertyDescriptor(window, "innerWidth");
        print("replace=" + window.innerWidth + "," + !!d.get + "," + (+d.writable) + (+d.enumerable) + (+d.configurable));
        "#
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec![
            // ONE shape for all thirteen: accessor, setter, e, c, get name, get len, set name,
            // set len, get native, set native, absent from Window.prototype.
            "attrShapes=truetrue11true0true1truetruetrue",
            "methShapes=111true0truetrue",
            "aliases=true,true,true,true",
            "foreign=TypeError:Illegal invocation",
            "scrollForeign=ok,ok,ok",
            "scrollTo=10,20",
            "scrollBy=15,0",
            "scrollDict=3,4",
            "scrollDictTop=3,7",
            // Chrome: 0,7 -- a POSITIONAL null is the number 0, and only an absent DICTIONARY
            // member means "leave this axis where it is". The line above is the one that tests the
            // leave-it-alone half, and this one used to claim it for `null` too.
            "scrollToNull=0,7",
            "scrollToNaN=0,0",
            "scrollToOne=100,100",
            "scrollByOne=100,100",
            "scrollToNone=100,100",
            "replace=5,false,111",
        ]
    );
}

/// The fallback chain has ONE definition, and this is the pair that proves it matters.
///
/// `media_context` (what `@media` is evaluated against) carried a HAND COPY of the inner-size chain
/// with a comment admitting it, because the original lived in a kernel op the cascade could not call.
/// Both read `webapi::window_geometry_of` now. A persona whose captured window inner size differs
/// from its screen is what separates the two: reading the SCREEN would make `@media (height: …)`
/// match the screen height while `window.innerHeight` answers the window's.
#[tokio::test]
async fn the_media_query_viewport_is_the_same_number_as_inner_size() {
    let mut e = Engine::new(
        persona::Overridden::new()
            .screen(ScreenProfile {
                width: 1920,
                height: 1080,
                avail_width: 1920,
                avail_height: 1040,
                window_inner_width: 1280,
                window_inner_height: 640,
                ..Default::default()
            })
            .boxed(),
    );
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval(
        "w4j-coherence",
        r#"
        print("inner=" + innerWidth + "x" + innerHeight);
        // The captured WINDOW size, not the screen's and not the work area's.
        print("mqExact=" + matchMedia("(width: 1280px)").matches + "," + matchMedia("(height: 640px)").matches);
        // ...and emphatically not the screen, which is what the duplicated chain could drift to.
        print("mqScreen=" + matchMedia("(height: 1080px)").matches + "," + matchMedia("(height: 1040px)").matches);
        // `device-height` IS the screen -- the one value that legitimately differs.
        print("mqDevice=" + matchMedia("(device-height: 1080px)").matches);
        "#
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec!["inner=1280x640", "mqExact=true,true", "mqScreen=false,false", "mqDevice=true"]
    );
}


/// `getComputedStyle` and the frame/idle quartet, and the ORDER of the two guards.
///
/// The five were the last two `Object.defineProperty(globalThis, …)` helpers in `host.js` and its
/// last two `op_nativize` mask sites. Every line below is Chrome 150.0.7871.46's own answer,
/// measured with `--headless=new` over a `file:` URL.
#[tokio::test]
async fn get_computed_style_and_the_frame_quartet_are_rows() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><head><style>#t{color:red}</style></head><body><div id=t>x</div></body></html>").unwrap();
    e.eval(
        "w4j-gcs",
        r#"
        const err = (f) => { try { return "OK:" + f(); } catch (e) { return e.name + "|" + e.message; } };
        const FIVE = ["getComputedStyle","requestAnimationFrame","cancelAnimationFrame",
                      "requestIdleCallback","cancelIdleCallback"];
        // One shape for all five: own, writable, enumerable, configurable data properties on
        // `window` and not on `Window.prototype`, native, with the member's name and length 1.
        print("shapes=" + [...new Set(FIVE.map((n) => {
            const d = Object.getOwnPropertyDescriptor(window, n);
            return [+d.writable, +d.enumerable, +d.configurable, d.value.name === n, d.value.length,
                    String(d.value).includes("[native code]"),
                    !Object.getOwnPropertyDescriptor(Window.prototype, n)].join("");
        }))].join(" | "));
        // THE GUARD ORDER. The receiver check is OUTSIDE the arity guard, so a foreign receiver
        // answers `Illegal invocation` whether or not the call carries enough arguments -- which is
        // Chrome's answer to all five, and to every other WINQ-guarded Window member.
        print("foreign=" + [...new Set(FIVE.flatMap((n) =>
            [err(() => window[n].call({})), err(() => window[n].call({}, document.body))]))].join(" | "));
        // ...and on the real receiver the count still runs.
        print("arity=" + [...new Set(FIVE.map((n) => err(() => window[n]()).replace(/'[a-zA-Z]+'/, "'X'")))].join(" | "));
        // getComputedStyle: `elt` is an interface type, so four different non-Elements are one message.
        print("gcsType=" + [...new Set([null, 5, window, document.getElementById("t").firstChild]
            .map((v) => err(() => getComputedStyle(v))))].join(" | "));
        // A DETACHED element has no box, and a pseudo-element this engine does not generate is an
        // EMPTY declaration -- both 0, both Chrome's. A string with no leading colon is NOT a
        // pseudo request, so it is the element's own style.
        print("gcsEmpties=" + getComputedStyle(document.createElement("div")).length
            + "," + getComputedStyle(document.body, "::nonsense").length
            + "," + (getComputedStyle(document.body, "before").length > 0));
        // A real CSSStyleDeclaration carrying real cascaded values, with native members.
        const cs = getComputedStyle(document.getElementById("t"));
        print("gcsReal=" + (cs instanceof CSSStyleDeclaration) + "," + cs.color
            + "," + String(cs.getPropertyValue).includes("[native code]"));
        // The frame/idle pair: a non-callable is the WebIDL CONVERSION message (this said "The
        // callback provided as parameter 1 is not a function."), an unknown handle is a silent
        // no-op, and the handles are real and distinct.
        print("rafNonFn=" + err(() => requestAnimationFrame(1)));
        print("ricNonFn=" + err(() => requestIdleCallback(null)));
        print("cafUnknown=" + err(() => String(cancelAnimationFrame(12345))));
        print("handles=" + (requestAnimationFrame(() => {}) !== requestAnimationFrame(() => {})));
        "#
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec![
            "shapes=111true1truetrue",
            "foreign=TypeError|Illegal invocation",
            "arity=TypeError|Failed to execute 'X' on 'Window': 1 argument required, but only 0 present.",
            "gcsType=TypeError|Failed to execute 'getComputedStyle' on 'Window': parameter 1 is not of type 'Element'.",
            "gcsEmpties=0,0,true",
            "gcsReal=true,rgb(255, 0, 0),true",
            "rafNonFn=TypeError|Failed to execute 'requestAnimationFrame' on 'Window': parameter 1 is not of type 'Function'.",
            "ricNonFn=TypeError|Failed to execute 'requestIdleCallback' on 'Window': parameter 1 is not of type 'Function'.",
            "cafUnknown=OK:undefined",
            "handles=true",
        ]
    );
}

/// A frame callback RUNS, with the right argument, and a cancel drops that one and no other.
#[tokio::test]
async fn a_frame_callback_runs_and_a_cancel_drops_only_its_own() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval(
        "w4j-raf-run",
        r#"
        const fired = [];
        const h1 = requestAnimationFrame(() => fired.push("one"));
        // `t` is the frame timestamp, a number on the realm's clock (Chrome 150 reads 0 or a few ms for a page's first
        // frames).
        requestAnimationFrame((t) => fired.push("two:" + (typeof t === "number" && t >= 0)));
        cancelAnimationFrame(h1);
        // An IdleDeadline with time left in the idle period the frame leaves (Chrome 150: 5-16 ms).
        requestIdleCallback((d) => fired.push("idle:" + d.didTimeout + ":" + (d.timeRemaining() > 0 && d.timeRemaining() <= 50)));
        setTimeout(() => print("fired=" + fired.join(",")), 80);
        "#
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    // The frame runs first and the idle callback in the idle period after it (Chrome 150, five runs alike: the frame's
    // callback, then the idle one with 5-16 ms left; this engine ran the idle one first, before the frame).
    assert_eq!(e.take_output(), vec!["fired=two:true,idle:false:true"]);
}
