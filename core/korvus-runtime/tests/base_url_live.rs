//! `<base href>`: ONE document base URL, read by every call site.
//!
//! Every expected value was measured in Chrome 151 headless (`--headless=new --dump-dom`) against a
//! loopback HTTP server on 2026-09-11 -- three probe pages served from `/basetest2.html`, `/base2.html`
//! and `/base3.html`, which now answer byte-identically in both engines. Loopback, not `file://`:
//! Chrome refuses to run these probes from a file URL.
//!
//! WHAT WAS WRONG. `<base href>` was unimplemented. `Node.baseURI` honoured an ABSOLUTE href and
//! deliberately ignored a relative one ("the webapi crate has no URL resolver" -- which had stopped
//! being true), and FIVE other places reached for the DOCUMENT url on their own: the URL-valued
//! reflectors, `HTMLHyperlinkElementUtils`, `<form action>`, the element subresource loader, the
//! classic-script fetch pass and `Request`. Six copies of "what do relative URLs resolve against" and
//! nowhere to put the answer. `core/dom/document.rs::document_base_url` is now that place.

use engine::Engine;

/// `load_page` carries the document URL, so these run exactly as a navigation does -- which matters,
/// because half the point is that the document url and the BASE url are different things. None of the
/// probes has an external script or an inline one, so nothing here touches the network.
const DOC: &str = "http://127.0.0.1:8731/basetest2.html";

async fn probe(html: &str, js: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page(html, DOC, &[]).await.expect("load_page");
    e.eval("i", js.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The eight rows of the unit's table that a page can read back, with a RELATIVE `<base href>` --
/// which is exactly the half that used to be ignored.
#[tokio::test]
async fn a_relative_base_moves_every_url_on_the_page() {
    let out = probe(
        r#"<!doctype html><html><head><base href="/sub/"><link id="lnk" rel="stylesheet" href="s.css"></head>
           <body><a id="a" href="y.html">y</a><img id="im" src="q.png"></body></html>"#,
        r#"
        print("baseURI=" + document.baseURI);
        print("URL=" + document.URL);
        print("documentURI=" + document.documentURI);
        print("a=" + document.getElementById("a").href);
        print("link=" + document.getElementById("lnk").href);
        print("img=" + document.getElementById("im").src);
        print("url=" + new URL("z.js", document.baseURI).href);
        print("request=" + new Request("r.json").url);
        print("base-el=" + document.querySelector("base").href);
        print("head=" + document.head.baseURI);
        print("detached=" + document.createElement("div").baseURI);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "baseURI=http://127.0.0.1:8731/sub/",
            // The DOCUMENT url does NOT move -- only the base does.
            "URL=http://127.0.0.1:8731/basetest2.html",
            "documentURI=http://127.0.0.1:8731/basetest2.html",
            "a=http://127.0.0.1:8731/sub/y.html",
            "link=http://127.0.0.1:8731/sub/s.css",
            "img=http://127.0.0.1:8731/sub/q.png",
            "url=http://127.0.0.1:8731/sub/z.js",
            // Was the RAW string "r.json": a Request's url is a URL record, not what was passed.
            "request=http://127.0.0.1:8731/sub/r.json",
            "base-el=http://127.0.0.1:8731/sub/",
            "head=http://127.0.0.1:8731/sub/",
            "detached=http://127.0.0.1:8731/sub/",
        ]
    );
}

/// WHICH `<base>` wins, and what a broken one does. Both measured: a bare `<base>` before the first
/// one with an `href` is skipped, a relative href resolves against the DOCUMENT url, and an href that
/// does not parse makes Chrome report `about:blank` -- not "no base".
#[tokio::test]
async fn the_first_base_with_an_href_wins() {
    let out = probe(
        r#"<!doctype html><html><head><base><base href="sub/deep/"><base href="/other/"></head>
           <body><a id="a" href="y.html">y</a></body></html>"#,
        r#"
        print("baseURI=" + document.baseURI);
        print("a=" + document.getElementById("a").href);
        document.querySelectorAll("base")[1].href = "/zzz/";
        print("moved=" + document.baseURI + " " + document.getElementById("a").href);
        const bs = document.querySelectorAll("base");
        for (let i = 0; i < bs.length; i++) bs[i].remove();
        print("none=" + document.baseURI + " " + document.getElementById("a").href);
        const nb = document.createElement("base"); nb.href = "/dyn/"; document.head.appendChild(nb);
        print("dyn=" + document.baseURI + " " + document.getElementById("a").href + " " + new Request("rr.json").url);
        nb.setAttribute("href", "http://[bogus");
        print("bogus=" + document.baseURI);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            // The bare <base> is skipped; "sub/deep/" resolves against the document url.
            "baseURI=http://127.0.0.1:8731/sub/deep/",
            "a=http://127.0.0.1:8731/sub/deep/y.html",
            // Live: rewriting the winning base's href moves everything.
            "moved=http://127.0.0.1:8731/zzz/ http://127.0.0.1:8731/zzz/y.html",
            // Removing every <base> puts the document url back -- which is why the scan is scoped to
            // the document TREE and not to the arena.
            "none=http://127.0.0.1:8731/basetest2.html http://127.0.0.1:8731/y.html",
            "dyn=http://127.0.0.1:8731/dyn/ http://127.0.0.1:8731/dyn/y.html http://127.0.0.1:8731/dyn/rr.json",
            "bogus=about:blank",
        ]
    );
}

/// `<form action>` is the ONE URL reflection that still reads the document url, and only for the
/// missing/empty case -- HTML says "the element's node document's URL" there. A non-empty relative
/// action resolves against the base like everything else. Chrome shows both in one page.
#[tokio::test]
async fn form_action_splits_the_empty_case_from_the_rest() {
    let out = probe(
        r#"<!doctype html><html><head><base href="/sub/"></head><body>
           <form id="f1"></form><form id="f2" action=""></form>
           <form id="f3" action="go"></form><form id="f4" action="/abs"></form>
           <a id="a0" href="">e</a><a id="a1">n</a><img id="i0" src="">
           </body></html>"#,
        r#"
        print("f1=" + document.getElementById("f1").action);
        print("f2=" + document.getElementById("f2").action);
        print("f3=" + document.getElementById("f3").action);
        print("f4=" + document.getElementById("f4").action);
        print("a0=" + document.getElementById("a0").href);
        print("a1=" + JSON.stringify(document.getElementById("a1").href));
        print("i0=" + document.getElementById("i0").src);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            // Missing and empty `action` both report the DOCUMENT url.
            "f1=http://127.0.0.1:8731/basetest2.html",
            "f2=http://127.0.0.1:8731/basetest2.html",
            // A non-empty one resolves against the BASE url.
            "f3=http://127.0.0.1:8731/sub/go",
            "f4=http://127.0.0.1:8731/abs",
            // An EMPTY href is a relative reference, so it resolves TO the base url.
            "a0=http://127.0.0.1:8731/sub/",
            // An ABSENT one is not a URL at all.
            "a1=\"\"",
            "i0=http://127.0.0.1:8731/sub/",
        ]
    );
}

/// A MODULE resolves against the base URL too, and an INLINE one is the case that is easy to miss:
/// its own URL is synthesised, so whatever it is synthesised FROM becomes the base every relative
/// `import` in it resolves against. Chrome is unambiguous -- for an inline module on a page carrying
/// `<base href="/sub/">`, `import.meta.url` is `…/sub/` (the base, not the document) and
/// `import.meta.resolve("./x.js")` is `…/sub/x.js`; korvus synthesised from the document URL and
/// fetched `/x.js`. Measured over loopback with a real `import`: Chrome requests
/// `/sub/inline-imported.js` and so does korvus now.
#[tokio::test]
async fn an_inline_modules_imports_resolve_against_the_base() {
    let out = probe(
        r#"<!doctype html><html><head><base href="/sub/"></head><body>
           <script type="module">print("resolve=" + import.meta.resolve("./x.js"));</script>
           </body></html>"#,
        // The module is deferred, so nothing else is needed here -- `run()` evaluates it.
        r#""#,
    )
    .await;
    assert_eq!(out, vec!["resolve=http://127.0.0.1:8731/sub/x.js"]);
}

/// No `<base>` at all: nothing moves, and the document url is the base. The regression guard for the
/// 99 % of pages that have no `<base>` element.
#[tokio::test]
async fn without_a_base_the_document_url_is_the_base() {
    let out = probe(
        r#"<!doctype html><html><body><a id="a" href="y.html">y</a><img id="im" src="q.png"></body></html>"#,
        r#"
        print("baseURI=" + document.baseURI);
        print("a=" + document.getElementById("a").href);
        print("img=" + document.getElementById("im").src);
        print("request=" + new Request("r.json").url);
        print("abs=" + new Request("https://example.org/x").url);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "baseURI=http://127.0.0.1:8731/basetest2.html",
            "a=http://127.0.0.1:8731/y.html",
            "img=http://127.0.0.1:8731/q.png",
            "request=http://127.0.0.1:8731/r.json",
            "abs=https://example.org/x",
        ]
    );
}
