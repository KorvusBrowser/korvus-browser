//! G1.4: non-DOM Web APIs as native #[op2] classes. Proves native URL parses + native getters.
use engine::Engine;
#[tokio::test]
async fn native_url() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("init");
    e.eval("url", r#"
        const u = new URL("https://ex.com:8443/a/b?x=1&y=2#frag");
        print("proto=" + u.protocol);
        print("host=" + u.host);
        print("hostname=" + u.hostname);
        print("path=" + u.pathname);
        print("search=" + u.search);
        print("hash=" + u.hash);
        print("x=" + u.searchParams.get("x"));
        const g = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(u), "href").get;
        print("hrefNative=" + g.toString().includes("[native code]"));
        try { new URL("not a url"); print("throw=no"); } catch (e) { print("throw=yes"); }
    "#.into()).expect("eval");
    e.run().await.expect("loop");
    assert_eq!(e.take_output(), vec![
        "proto=https:",
        "host=ex.com:8443",
        "hostname=ex.com",
        "path=/a/b",
        "search=?x=1&y=2",
        "hash=#frag",
        "x=1",
        "hrefNative=true",
        "throw=yes",
    ]);
}

#[tokio::test]
async fn native_text_encoding() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("init");
    e.eval("text", r#"
        const enc = new TextEncoder();
        const bytes = enc.encode("hi!");
        print("enc=" + enc.encoding + " " + Array.from(bytes).join(","));
        const dec = new TextDecoder();
        print("dec=" + dec.decode(new Uint8Array([104, 105, 33])));
    "#.into()).expect("eval");
    e.run().await.expect("loop");
    assert_eq!(e.take_output(), vec!["enc=utf-8 104,105,33", "dec=hi!"]);
}
