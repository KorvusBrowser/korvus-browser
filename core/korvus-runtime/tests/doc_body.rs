//! document.body/head/documentElement + element.click/hidden.
use engine::Engine;
#[tokio::test]
async fn body_head_click() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><head><title>T</title></head><body><button id="b">x</button></body></html>"#).unwrap();
    e.eval("b", r#"
        print("root=" + document.documentElement.tagName);
        print("head=" + (document.head.tagName) + " body=" + document.body.tagName);
        print("bodyChild=" + document.body.firstElementChild.tagName);
        let clicked = 0;
        const btn = document.getElementById("b");
        btn.addEventListener("click", () => clicked++);
        btn.click();
        print("clicked=" + clicked);
        print("hidden0=" + btn.hidden);
        btn.hidden = true;
        print("hidden1=" + btn.hidden + " attr=" + btn.hasAttribute("hidden"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "root=HTML",
        "head=HEAD body=BODY",
        "bodyChild=BUTTON",
        "clicked=1",
        "hidden0=false",
        "hidden1=true attr=true",
    ]);
}

/// `HTMLBodyElement`'s window-forwarding event handlers -- the table that replaced
/// `js/dom/08q-body-element.js`.
///
/// EVERY EXPECTATION BELOW IS HEADLESS CHROME 151's, measured before the port, because the spec does
/// not say which names forward and the answer is not the whole list. Getting the direction wrong
/// loses a page's load handler silently, which is why this is a test and not a comment.
///
/// TWO CASES CHROME ANSWERS DIFFERENTLY ARE DELIBERATELY NOT ASSERTED HERE, because each is a defect
/// in a seam this lane does not own and both predate it (the deleted JS had the identical logic):
///
///   * `new DOMParser().parseFromString(…).body.onload = f` forwards to the page's Window, where
///     Chrome stores nothing and reads null. The forwarding rule is "the node document is the ACTIVE
///     document", and it is right -- `pd.body.ownerDocument === document` is TRUE here, so the
///     DEFECT is in `DOMParser`'s document identity, not in the handler.
///   * `body.removeAttribute("onload")` does not clear the Window's handler, where Chrome does. The
///     removal branch exists (an absent attribute writes null) but never runs: `remove_attribute_
///     notify` in `core/dom/shared.rs` does not call `notify_resource_attr`, so attribute REMOVAL
///     reaches neither this nor the subresource loader. Adding it there would also fire the loader
///     on every removal, which belongs to that seam's own unit.
#[tokio::test]
async fn body_handlers_forward_to_the_window() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("h", r#"
        const f = function () {}, g = function () {};
        // Both directions, one slot.
        document.body.onload = f;
        print("b2w=" + (window.onload === f) + " readback=" + (document.body.onload === f));
        window.onload = g;
        print("w2b=" + (document.body.onload === g));
        // A non-callable CLEARS.
        document.body.onload = 42;
        print("cleared=" + window.onload);
        // The whole forwarded set, both ways.
        let both = 0;
        for (const t of ["blur","error","focus","load","resize","scroll","afterprint","beforeprint",
                         "beforeunload","hashchange","languagechange","message","messageerror","offline",
                         "online","pagehide","pageshow","popstate","rejectionhandled","storage",
                         "unhandledrejection","unload"]) {
            const h = function () {};
            document.body["on" + t] = h;
            if (window["on" + t] !== h) continue;
            const h2 = function () {};
            window["on" + t] = h2;
            if (document.body["on" + t] === h2) both++;
        }
        print("forwarded=" + both);
        // The two the Gamepad API adds are NOT forwarded, in either direction: element-local.
        const gp = function () {};
        document.body.ongamepadconnected = gp;
        print("gp=" + (document.body.ongamepadconnected === gp) + "," + window.ongamepadconnected);
        // The receiver check: Chrome throws Illegal invocation for `{}` AND for another element.
        const d = Object.getOwnPropertyDescriptor(HTMLBodyElement.prototype, "onload");
        const probe = (fn) => { try { fn(); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } };
        print("foreignGet=" + probe(() => d.get.call({})));
        print("divGet=" + probe(() => d.get.call(document.createElement("div"))));
        // A created (unattached) body DOES forward: its node document is the active one.
        window.onload = null;
        const nb = document.createElement("body");
        nb.onload = f;
        print("created=" + (nb.onload === f) + " win=" + (window.onload === f));
        // The six presentation reflections are plain strings -- no colour parsing, no URL resolution.
        document.body.bgColor = "x"; document.body.vLink = "y"; document.body.background = "b.png";
        print("refl=" + document.body.bgColor + "," + document.body.getAttribute("bgcolor") +
              "," + document.body.getAttribute("vlink") + "," + document.body.background);
        // And `setAttribute("onload", …)` goes through the attribute seam, compiling onto the window
        // -- named after its attribute, which is what Chrome names a content-attribute handler.
        window.onload = null;
        document.body.setAttribute("onload", "globalThis.viaAttr = 1");
        print("viaAttr=" + (typeof window.onload) + " name=" + (window.onload && window.onload.name));
        window.onload();
        print("ran=" + globalThis.viaAttr);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "b2w=true readback=true",
        "w2b=true",
        "cleared=null",
        "forwarded=22",
        "gp=true,null",
        "foreignGet=TypeError:Illegal invocation",
        "divGet=TypeError:Illegal invocation",
        "created=true win=true",
        "refl=x,x,y,b.png",
        "viaAttr=function name=onload",
        "ran=1",
    ]);
}

/// `<body onload="…">` IN MARKUP compiles onto `window.onload` and runs on the load event.
///
/// A SEPARATE TEST BECAUSE IT NEEDS A DIFFERENT PATH, and that is the whole finding. The parser
/// writes `onload` straight into the arena, through none of the JS-visible attribute seams, so the
/// runtime's `ACTIVATE_BODY_HANDLERS` driver walks the body's attributes once the parse is over --
/// and that driver runs on `load_page` (a navigation), not on `load_html`. So the sibling test above,
/// which uses `load_html`, cannot see this at all.
///
/// IT CAUGHT A REAL DEFECT, on the release binary, after the L19 body commit: moving
/// `__bodyHandlerAttr`'s body to Rust left the bridge BIND a silent no-op, because
/// `op_stash_bridge` (deleted since) put the bridge holder on its private slot from
/// `BOOTSTRAP` -- which runs AFTER
/// every table install -- and `bridge_set` looked only there. `<body onload>` stopped working with
/// every test green. `bridge_set` now falls back to `globalThis[Symbol.for("korvus.bridge")]`, where
/// the holder has lived since the ESM layer, and this test is what holds that.
///
/// Chrome 151, measured: `typeof window.onload` is "function" with no script having assigned one, it
/// is the same object `document.body.onload` reads back, and the handler has run by load.
#[tokio::test]
async fn a_markup_body_onload_becomes_the_window_handler() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page(
        r#"<!doctype html><html><body onload="globalThis.markupRan = (globalThis.markupRan || 0) + 1"></body></html>"#,
        "http://127.0.0.1:8731/onload.html",
        &[],
    )
    .await
    .expect("load_page");
    // Pump to the load event before reading `markupRan`: the handler is INSTALLED by the driver
    // during the parse and RUN when `load` fires, and those are two different moments.
    e.run().await.unwrap();
    e.eval("m", r#"
        print("type=" + (typeof window.onload) + " name=" + (window.onload && window.onload.name));
        print("same=" + (window.onload === document.body.onload));
        print("ran=" + globalThis.markupRan);
        // A script's own later assignment REPLACES the markup handler rather than running beside it,
        // which is the reason the driver runs before the page's scripts.
        const f = function () {};
        window.onload = f;
        print("replaced=" + (document.body.onload === f));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "type=function name=onload",
        "same=true",
        "ran=1",
        "replaced=true",
    ]);
}
