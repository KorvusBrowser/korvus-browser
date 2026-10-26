//! history.pushState/replaceState update location; getComputedStyle on the root element.
//!
//! This passed `document` to getComputedStyle and expected `""` back. Chrome 151 throws
//! `TypeError: parameter 1 is not of type 'Element'` -- checked directly -- and so does the engine
//! now; the assertion was pinning the old stub's silence.
use engine::Engine;
#[tokio::test]
async fn history_and_style() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.set_url("https://ex.com/a/b");
    e.run().await.unwrap();
    // A document, so there is a root element to measure. Nothing above needed one.
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("h", r#"
        print("len0=" + history.length);
        history.pushState({n:1}, "", "/c/d");
        print("href=" + location.href);
        print("len1=" + history.length);
        print("state=" + JSON.stringify(history.state));
        history.replaceState(null, "", "?q=1");
        print("search=" + location.search);
        let threw = "no";
        try { getComputedStyle(document); } catch (e) { threw = e.name; }
        print("nonElement=" + threw);
        const cs = getComputedStyle(document.documentElement);
        print("style=" + JSON.stringify(cs.getPropertyValue("color")));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "len0=1",
        "href=https://ex.com/c/d",
        "len1=2",
        "state={\"n\":1}",
        "search=?q=1",
        "nonElement=TypeError",
        "style=\"rgb(0, 0, 0)\"",
    ]);
}

/// A `History` method from another same-origin realm acts on the history it is called on: YouTube binds a hidden
/// `about:blank` frame's `replaceState` and `pushState` to the page's `history` (its `bindHistoryStateFunctions`), and
/// every search it starts commits through them. Chrome 150 rewrites the page's URL; korvus refused the page's object
/// with `Illegal invocation` (the frame realm's brand check) and the search never left the home page. A receiver that
/// is no `History` at all is still refused.
#[tokio::test]
async fn a_frames_history_method_bound_to_the_pages_history_acts_on_the_page() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.set_url("https://ex.com/a");
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("h", r#"
        const f = document.createElement("iframe");
        f.sandbox = "allow-same-origin";
        f.style.display = "none";
        document.body.appendChild(f);
        const replace = f.contentWindow.history.replaceState.bind(window.history);
        const push = f.contentWindow.history.pushState.bind(window.history);
        let threw = "no";
        try { replace({ a: 1 }, "", "/results?search_query=x"); } catch (e) { threw = e.name; }
        print("threw=" + threw + " href=" + location.href + " state=" + JSON.stringify(history.state));
        push(null, "", "/next");
        print("len=" + history.length + " href=" + location.href + " frame=" + f.contentWindow.location.href);
        let brand = "no";
        try { f.contentWindow.History.prototype.replaceState.call({}, null, ""); } catch (e) { brand = e.name; }
        print("brand=" + brand);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "threw=no href=https://ex.com/results?search_query=x state={\"a\":1}",
        "len=2 href=https://ex.com/next frame=about:blank",
        "brand=TypeError",
    ]);
}
