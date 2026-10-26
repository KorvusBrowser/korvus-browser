//! A nested browsing context is a SEPARATE document, not a second view of the page's.
//!
//! Every assertion here failed before `JsDocument` carried a root: an `<iframe>`'s
//! `contentDocument` answered every document-level query against the one arena root, so it found
//! the parent page's element by id, the parent page's `<body>`, and the parent page's collections.
use engine::Engine;
/// The page has `#only-parent`; the frame has `#only-frame` and a same-named `#shared`.
const PAGE: &str = r#"<!doctype html><html><head><title>parent page</title></head>
<body>
  <p id="only-parent">p</p>
  <div id="shared">parent-shared</div>
  <form id="pform"></form>
  <a href="/x">parent link</a>
  <iframe id="f"></iframe>
</body></html>"#;

const FRAME_HTML: &str = "<!doctype html><html><head><title>frame title</title></head>\
<body><span id=\"only-frame\">s</span><div id=\"shared\">frame-shared</div>\
<form id=\"fform\"></form></body></html>";

/// `<iframe srcdoc>` is a navigation to `about:srcdoc` whose document IS the attribute.
///
/// It reflected as a plain string and nothing else, so the markup sat on the element and the frame
/// held an empty document. Both spellings are covered: the one the parser writes and the one a
/// script assigns. The navigation commits a task later, as Chrome 150's does:
/// right after the assignment the frame still holds its initial `about:blank` document.
#[tokio::test]
async fn srcdoc_builds_the_frames_document() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body>
        <iframe id="a" srcdoc="<p id='inside'>from srcdoc</p>"></iframe>
        <div id="only-parent"></div>
      </body></html>"#).unwrap();
    // the parsed frame's navigation commits a task later
    e.run().await.unwrap();
    e.eval("b", r#"
        const a = document.getElementById("a").contentDocument;
        print("parsed=" + (a.getElementById("inside") || {textContent:"null"}).textContent);
        print("scoped=" + a.getElementById("only-parent"));
        print("loc=" + document.getElementById("a").contentWindow.location.href);

        const b = document.createElement("iframe");
        document.body.appendChild(b);
        b.srcdoc = "<span id='later'>assigned</span>";
        print("before=" + b.contentDocument.getElementById("later") + " " + b.contentWindow.location.href);
        b.onload = () => {
            print("assigned=" + b.contentDocument.getElementById("later").textContent);
            print("attr=" + b.getAttribute("srcdoc").slice(0, 5));
            // The parent is untouched by either.
            print("parent=" + document.getElementById("only-parent").tagName
                  + " " + document.getElementById("inside"));
        };
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "parsed=from srcdoc",
        "scoped=null",
        "loc=about:srcdoc",
        "before=null about:blank",
        "assigned=assigned",
        "attr=<span",
        "parent=DIV null",
    ]);
}

/// A FRAME'S WINDOW HAS THE REALM'S INTERFACE OBJECTS.
///
/// It used to have SEVEN globals, by a hand list -- `BroadcastChannel`, `MessageChannel`, `URL`,
/// `Blob`, `Worker`, `navigator`, `crypto` -- so every other constructor read off a `contentWindow`
/// was `undefined`. A real browser's nested browsing context is a whole realm with its own copy of
/// every one, and so is this engine's (by default).
///
/// FOUND BY THE CORPUS, not by a test. `grok.com`'s session-replay chunk does
/// `iframeEl.contentWindow.Element.prototype.attachShadow` inside its `onIframeLoad` and answered
/// `TypeError: Cannot read properties of undefined (reading 'prototype')`. It had never reached that
/// line before -- the page's own bootstrap stopped earlier -- so the gap only became visible when
/// W4.f's CSSOM cutover let the script run through to it.
#[tokio::test]
async fn a_frames_window_has_the_realms_interface_objects() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><iframe id="f"></iframe></body></html>"#).unwrap();
    e.eval(
        "t",
        r#"
        const w = document.getElementById("f").contentWindow;
        // The exact read that failed, and the kinds of interface object around it.
        print("attachShadow=" + typeof w.Element.prototype.attachShadow);
        for (const k of ["Element", "HTMLElement", "Node", "EventTarget", "ShadowRoot", "Document",
                         "CSSStyleSheet", "StyleSheet", "CSSRule", "MediaList", "PromiseRejectionEvent",
                         "Promise", "Object"]) {
          print(k + "=" + (typeof w[k] === "function" && w[k] === globalThis[k]));
        }
        // The JAVASCRIPT intrinsics are the frame's own: Chrome answers `false` for
        // `Array === parent.Array` (fixture F-realm-globals). So are the Web API interface objects
        // (a realm per frame, built from templates): Chrome 150 answers
        // `false` for `w.Element === Element` and `w instanceof Window` (measured, headless, 2026-09-26),
        // and `true` for `w instanceof w.Window` (2026-09-28).
        // A METHOD is not an interface object and is not copied: the frame keeps its OWN timers, its
        // own `location`, and its own `window`/`parent`.
        print("ownTimers=" + (w.setTimeout !== globalThis.setTimeout));
        print("ownWindow=" + (w.window === w) + " parent=" + (w.parent === globalThis));
        print("ownLocation=" + (w.location !== globalThis.location));
        print("brand=" + Object.prototype.toString.call(w) + "/" + (w instanceof Window) + "/" + (w instanceof w.Window));
        "#
        .to_string(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec![
            "attachShadow=function",
            "Element=false", "HTMLElement=false", "Node=false", "EventTarget=false", "ShadowRoot=false",
            "Document=false", "CSSStyleSheet=false", "StyleSheet=false", "CSSRule=false", "MediaList=false",
            "PromiseRejectionEvent=false", "Promise=false", "Object=false",
            "ownTimers=true",
            "ownWindow=true parent=true",
            "ownLocation=true",
            "brand=[object Window]/false/true",
        ]
    );
}

/// A frame's window is a real event target and `contentWindow.postMessage` actually delivers.
///
/// It used to be `function () {}` -- a silent drop -- and `addEventListener` reached the reparented
/// `Window.prototype` with the PAGE's handler store behind it, so a listener added on the frame
/// heard the page's events. Cross-document messaging is the only channel a page has to hear back
/// from inside a frame.
#[tokio::test]
async fn frame_window_receives_messages() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.eval("b", r#"
        const w = document.getElementById("f").contentWindow;
        const seen = [];
        w.onmessage = (ev) => seen.push("on:" + ev.data);
        w.addEventListener("message", (ev) => seen.push("li:" + ev.data + ":" + (ev.source === window)));
        // A listener on the frame must NOT hear the page's own events.
        window.addEventListener("message", (ev) => seen.push("page:" + ev.data));
        w.postMessage("to-frame", "*");
        globalThis.setTimeout(() => {
            print(seen.join("|"));
            print("dropped=" + (function () {
                const before = seen.length;
                // A targetOrigin that is not the frame's drops the message.
                w.postMessage("nope", "https://elsewhere.invalid");
                return before === seen.length;
            })());
        }, 0);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "on:to-frame|li:to-frame:true",
        "dropped=true",
    ]);
}

#[tokio::test]
async fn frame_document_is_its_own_document() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.eval("b", format!(r#"
        const f = document.getElementById("f");
        // open/write/close REPLACES the frame's document, so the one to look at is the one the
        // frame holds afterwards -- exactly as in a browser.
        f.contentDocument.open(); f.contentDocument.write({frame:?}); f.contentDocument.close();
        const d = f.contentDocument;

        // 1. getElementById is scoped to the frame's tree, in both directions.
        print("frame-own=" + (d.getElementById("only-frame") || {{tagName:"null"}}).tagName);
        print("frame-sees-parent=" + d.getElementById("only-parent"));
        print("parent-sees-frame=" + document.getElementById("only-frame"));
        print("shared=" + d.getElementById("shared").textContent
              + "/" + document.getElementById("shared").textContent);

        // 2. body/head/documentElement/title are the frame's, and are not the page's objects.
        print("body-differs=" + (d.body !== document.body));
        print("head-differs=" + (d.head !== document.head));
        print("de-differs=" + (d.documentElement !== document.documentElement));
        print("titles=" + d.title + "|" + document.title);

        // 3. The collections and the tag/class queries are scoped too.
        print("frame-forms=" + Array.from(d.forms).map((x) => x.id).join(","));
        print("parent-forms=" + Array.from(document.forms).map((x) => x.id).join(","));
        print("frame-links=" + d.links.length + " parent-links=" + document.links.length);
        print("frame-spans=" + d.getElementsByTagName("span").length
              + " parent-spans=" + document.getElementsByTagName("span").length);
        print("frame-qsa=" + d.querySelectorAll("div").length
              + " parent-qsa=" + document.querySelectorAll("div").length);
        print("frame-count=" + d.childElementCount + " parent-count=" + document.childElementCount);
        // getElementsByTagNameNS builds a LIVE HTMLCollection through the collection factory, which
        // is keyed by a root node id and was handed the page's for every document.
        const HTML_NS = "http://www.w3.org/1999/xhtml";
        print("frame-ns-span=" + d.getElementsByTagNameNS(HTML_NS, "span").length
              + " parent-ns-span=" + document.getElementsByTagNameNS(HTML_NS, "span").length);

        // 4. It is a real Document over arena nodes, not a plain object.
        // Its OWN realm's Document: Chrome 150 answers `false` for the parent's
        // `Document` and `true` for `d.defaultView.Document` (measured, headless, 2026-09-26).
        print("isDoc=" + (d instanceof d.defaultView.Document) + " nodeType=" + d.nodeType);
        print("toString=" + Object.prototype.toString.call(d));

        // 5. The parent's own queries are unchanged by the frame existing.
        print("parent-body=" + document.body.tagName
              + " parent-de=" + document.documentElement.tagName
              + " parent-title=" + document.title);
    "#, frame = FRAME_HTML)).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "frame-own=SPAN",
        "frame-sees-parent=null",
        "parent-sees-frame=null",
        "shared=frame-shared/parent-shared",
        "body-differs=true",
        "head-differs=true",
        "de-differs=true",
        "titles=frame title|parent page",
        "frame-forms=fform",
        "parent-forms=pform",
        "frame-links=0 parent-links=1",
        "frame-spans=1 parent-spans=0",
        "frame-qsa=1 parent-qsa=1",
        "frame-count=1 parent-count=1",
        "frame-ns-span=1 parent-ns-span=0",
        "isDoc=true nodeType=9",
        "toString=[object HTMLDocument]",
        "parent-body=BODY parent-de=HTML parent-title=parent page",
    ]);
}

// ---------------------------------------------------------------------------------------------
// Track 4.2b: the context LOADS. Track 4.2 gave a frame a document of its own and a `src` that
// fetched; what it explicitly left open was that "a frame's inline scripts run, its `<script src>`
// ones do not", and that `window.open` had no context to return. Both are closed below, against a
// real HTTP server rather than a stand-in, because a fetch that does not happen IS the defect.

/// A tiny multi-route HTTP/1.1 server. These cases need TWO resources -- a document and the external
/// script it pulls -- served from ONE origin, which the single-body helpers elsewhere cannot do.
async fn serve(listener: tokio::net::TcpListener, routes: Vec<(&'static str, &'static str, String)>) {
    loop {
        let (mut s, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let routes = routes.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let n = match s.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let resp = match routes.iter().find(|(p, _, _)| *p == path) {
                Some((_, ct, body)) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
            };
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

const FRAME_PAGE: &str = "<!doctype html><html><head><title>framed page</title>\
<script src=\"ext.js\"></script></head><body><p id=\"only-in-frame\">inside</p>\
<script>\
window.__order = (window.__fromExternal === 'external-ran') ? 'external-first' : 'inline-first';\
parent.postMessage({ t: document.title, e: window.__fromExternal, h: location.href }, '*');\
</script></body></html>";

/// An `<iframe src>` pointing at a real HTTP page loads it, runs BOTH its inline and its external
/// `<script src>` (the external one first, because a classic external script blocks the ones after
/// it), and `contentDocument` is that page's document -- title and all.
#[tokio::test]
async fn a_frame_loads_its_src_and_runs_its_external_script() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(serve(
        l,
        vec![
            ("/frame.html", "text/html", FRAME_PAGE.to_string()),
            ("/ext.js", "application/javascript", "window.__fromExternal = 'external-ran';".to_string()),
        ],
    ));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    let base = format!("http://{addr}/");
    let page = "<!doctype html><html><head><title>parent page</title><script>\
        window.addEventListener('message', (ev) => {\
          const f = document.getElementById('f');\
          print('msg=' + ev.data.t + '|' + ev.data.e);\
          print('msg-src-is-frame=' + (ev.source === f.contentWindow));\
          print('msg-origin-matches=' + (ev.origin === location.origin));\
          print('msg-href=' + ev.data.h.endsWith('/frame.html'));\
        });\
        </script></head><body>\
        <iframe id=\"f\" src=\"/frame.html\"></iframe>\
        <script>\
        document.getElementById('f').addEventListener('load', () => {\
          const f = document.getElementById('f');\
          print('title=' + f.contentDocument.title);\
          print('external=' + f.contentWindow.__fromExternal);\
          print('order=' + f.contentWindow.__order);\
          print('scoped=' + (f.contentDocument.getElementById('only-in-frame') || {textContent:'null'}).textContent);\
          print('href=' + f.contentWindow.location.href.endsWith('/frame.html'));\
          print('is-real-doc=' + (f.contentDocument instanceof f.contentWindow.Document) + ' ' + f.contentDocument.nodeType);\
          print('parent-untouched=' + document.title + '/' + document.getElementById('only-in-frame'));\
        });\
        </script></body></html>";
    e.load_page(page, &base, &[]).await.unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "msg=framed page|external-ran",
        "msg-src-is-frame=true",
        "msg-origin-matches=true",
        "msg-href=true",
        "title=framed page",
        "external=external-ran",
        "order=external-first",
        "scoped=inside",
        "href=true",
        "is-real-doc=true 9",
        "parent-untouched=parent page/null",
    ]);
}

const POPUP_PAGE: &str = "<!doctype html><html><head><title>popup page</title>\
<script src=\"p.js\"></script></head><body><div id=\"only-in-popup\">popped</div>\
<script>opener.postMessage({ t: document.title, e: window.__popupExternal, h: location.href }, '*');\
</script></body></html>";

/// `window.open(url)` returns a REAL auxiliary browsing context: the document is the page that was
/// actually fetched, its external and inline scripts ran, `location`/`closed` answer, `load` fires on
/// it, a message posted back through `opener` arrives with `source` and `origin` set, and `close()`
/// sets `closed`. The two cases where the SPEC returns null still return null.
#[tokio::test]
async fn window_open_returns_a_real_auxiliary_context() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(serve(
        l,
        vec![
            ("/popup.html", "text/html", POPUP_PAGE.to_string()),
            ("/p.js", "application/javascript", "window.__popupExternal = 'popup-external-ran';".to_string()),
        ],
    ));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    let base = format!("http://{addr}/");
    let page = "<!doctype html><html><head><title>opener page</title></head><body>\
        <script>\
        const w = window.open('/popup.html');\
        print('not-null=' + (w !== null));\
        print('brand=' + Object.prototype.toString.call(w));\
        print('closed-before=' + w.closed);\
        print('opener=' + (w.opener === window) + ' top=' + (w.top === w) + ' parent=' + (w.parent === w));\
        print('frameElement=' + w.frameElement);\
        print('noopener=' + window.open('about:blank', '_blank', 'noopener'));\
        print('self-target=' + (window.open('', '_self') === window));\
        window.addEventListener('message', (ev) => {\
          if (ev.source !== w) return;\
          print('msg=' + ev.data.t + '|' + ev.data.e);\
          print('msg-src-is-popup=' + (ev.source === w));\
          print('msg-origin-matches=' + (ev.origin === location.origin));\
          print('msg-href=' + ev.data.h.endsWith('/popup.html'));\
        });\
        w.addEventListener('load', () => {\
          print('title=' + w.document.title);\
          print('external=' + w.__popupExternal);\
          print('scoped=' + (w.document.getElementById('only-in-popup') || {textContent:'null'}).textContent);\
          print('href=' + w.location.href.endsWith('/popup.html'));\
          print('is-real-doc=' + (w.document instanceof w.Document) + ' ' + w.document.nodeType);\
          print('opener-untouched=' + document.title + '/' + document.getElementById('only-in-popup'));\
          w.close();\
          print('closed-after=' + w.closed);\
        });\
        </script></body></html>";
    e.load_page(page, &base, &[]).await.unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "not-null=true",
        "brand=[object Window]",
        "closed-before=false",
        "opener=true top=true parent=true",
        "frameElement=null",
        "noopener=null",
        "self-target=true",
        "msg=popup page|popup-external-ran",
        "msg-src-is-popup=true",
        "msg-origin-matches=true",
        "msg-href=true",
        "title=popup page",
        "external=popup-external-ran",
        "scoped=popped",
        "href=true",
        "is-real-doc=true 9",
        "opener-untouched=opener page/null",
        "closed-after=true",
    ]);
}

/// A second context's `location` NAVIGATES it. All four spellings a page uses -- `location.href = x`,
/// `location = x` (`[PutForwards=href]`), `location.replace(x)` and `location.assign(x)` -- fetch the
/// target, replace the document, and fire `load` again. It was a `URL` instance: every component read
/// correctly and not one of them did anything.
#[tokio::test]
async fn a_contexts_location_navigates_it() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(serve(
        l,
        vec![
            ("/a.html", "text/html", "<!doctype html><title>A</title><body>a".to_string()),
            ("/b.html", "text/html", "<!doctype html><title>B</title><body>b".to_string()),
            ("/c.html", "text/html", "<!doctype html><title>C</title><body>c".to_string()),
        ],
    ));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    let base = format!("http://{addr}/");
    let page = "<!doctype html><html><head><title>driver</title></head><body>\
        <iframe id=\"f\"></iframe>\
        <script>\
        const w = window.open('/a.html');\
        let n = 0;\
        w.addEventListener('load', () => {\
          n++;\
          print(n + '=' + w.document.title + '|' + w.location.pathname);\
          if (n === 1) { print('is-location=' + (w.location instanceof w.Location)); w.location.href = 'b.html'; }\
          else if (n === 2) { w.location = 'c.html'; }\
          else if (n === 3) { w.location.replace('a.html'); }\
          else if (n === 4) {\
            print('origin=' + (w.location.origin === location.origin) + ' search=' + JSON.stringify(w.location.search));\
            const f = document.getElementById('f');\
            f.addEventListener('load', () => {\
              print('frame=' + f.contentDocument.title + '|' + f.contentWindow.location.pathname);\
            });\
            f.contentWindow.location.assign('/c.html');\
          }\
        });\
        </script></body></html>";
    e.load_page(page, &base, &[]).await.unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "1=A|/a.html",
        "is-location=true",
        "2=B|/b.html",
        "3=C|/c.html",
        "4=A|/a.html",
        "origin=true search=\"\"",
        "frame=C|/c.html",
    ]);
}
