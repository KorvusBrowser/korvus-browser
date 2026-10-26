//! IntersectionObserver/ResizeObserver/PerformanceObserver + Request.
use engine::Engine;
#[tokio::test]
async fn observers_and_request() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    // A document, because `observe()` needs a target and a target is an element.
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("o", r#"
        const io = new IntersectionObserver(() => {}, { rootMargin: "10px" });
        // `observe()` takes a required target now, so this passes one. It used to be a bare call, and
        // a bare call reaching the Rust body with an undefined target is what the WebIDL arity guard
        // exists to stop -- Chrome throws there and this asserted that it did not.
        io.observe(document.createElement("div"));
        print("io=" + (io.disconnect() === undefined) + "," + (io instanceof IntersectionObserver)
              + "," + io.rootMargin + "," + io.scrollMargin + "," + io.delay + "," + io.trackVisibility);
        // The options dictionary is optional -- this threw "Cannot convert undefined or null to
        // object" until the constructor stopped calling `to_object` on `undefined`.
        print("bare=" + (new IntersectionObserver(() => {}).rootMargin));
        // ...and the callback is not optional.
        const noCb = (C) => { try { new C(); return "no-throw"; } catch (err) { return err.constructor.name; } };
        print("ctor=" + [noCb(IntersectionObserver), noCb(ResizeObserver), noCb(MutationObserver)].join(","));
        const ro = new ResizeObserver(() => {});
        print("ro=" + (ro instanceof ResizeObserver));
        const po = new PerformanceObserver(() => {});
        print("po=" + (po instanceof PerformanceObserver) + "," + po.takeRecords().length);
        // A Request's `body` is a ReadableStream of the bytes, not the string it was given (Chrome 150,
        // `tests/fixtures/bodies-probe.chrome-150.txt` "Request body tag").
        const req = new Request("https://x.com/api", { method: "POST", body: "d" });
        print("req=" + req.method + "," + req.url + "," + req.body);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        // `rootMargin` reads back as four components, which is how every browser serializes it; it
        // used to hand back whatever the page wrote, so a default observer reported "0px".
        "io=true,true,10px 10px 10px 10px,0px 0px 0px 0px,0,false",
        "bare=0px 0px 0px 0px",
        "ctor=TypeError,TypeError,TypeError",
        "ro=true",
        "po=true,0",
        "req=POST,https://x.com/api,[object ReadableStream]",
    ]);
}
