//! Event/CustomEvent init dict + bubbling + preventDefault/stopPropagation.
use engine::Engine;
#[tokio::test]
async fn event_model() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="outer"><button id="btn">go</button></div></body></html>"#).unwrap();
    e.eval("a", r#"
        const outer = document.getElementById("outer");
        const btn = document.getElementById("btn");
        const seen = [];
        outer.addEventListener("custom", (e) => { seen.push("outer:" + e.detail.n + ":" + (e.target.id)); });
        btn.addEventListener("custom", (e) => { seen.push("btn"); });
        const ev = new CustomEvent("custom", { bubbles: true, detail: { n: 42 } });
        btn.dispatchEvent(ev);
        print("bubble=" + seen.join(","));
        // Non-bubbling event should not reach outer.
        seen.length = 0;
        btn.dispatchEvent(new CustomEvent("custom", { bubbles: false, detail: { n: 1 } }));
        print("nobubble=" + seen.join(","));
        // stopPropagation halts bubbling.
        seen.length = 0;
        btn.addEventListener("stop", (e) => { seen.push("btn"); e.stopPropagation(); });
        outer.addEventListener("stop", () => seen.push("outer"));
        btn.dispatchEvent(new CustomEvent("stop", { bubbles: true }));
        print("stopped=" + seen.join(","));
        // preventDefault + cancelable.
        const c = new Event("x", { cancelable: true });
        btn.addEventListener("x", (e) => e.preventDefault());
        const ret = btn.dispatchEvent(c);
        print("prevented=" + (ret === false) + "," + c.defaultPrevented);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "bubble=btn,outer:42:btn",
        "nobubble=btn",
        "stopped=btn",
        "prevented=true,true",
    ]);
}
