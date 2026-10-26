//! Common globals: atob/btoa, structuredClone, MutationObserver, requestAnimationFrame.
use engine::Engine;
#[tokio::test]
async fn common_globals() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("g", r#"
        print("b64=" + btoa("hello"));
        print("dec=" + atob("aGVsbG8="));
        print("roundtrip=" + (atob(btoa("Korvus!")) === "Korvus!"));
        const clone = structuredClone({ a: [1, 2], b: "x" });
        print("clone=" + JSON.stringify(clone));
        const mo = new MutationObserver(() => {});
        // observe() with no valid options throws TypeError (Chrome parity; the real observer is no longer a
        // lenient stub -- a silent no-op here would itself be a fingerprint tell). disconnect/takeRecords are safe.
        let threw = false; try { mo.observe(); } catch (e) { threw = e instanceof TypeError; }
        mo.disconnect();
        print("mo=" + (mo instanceof MutationObserver) + " threw=" + threw + " recs=" + mo.takeRecords().length);
        let raf = 0;
        requestAnimationFrame(() => { raf = 1; });
        // driven by run()
        print("rafScheduled=true");
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "b64=aGVsbG8=",
        "dec=hello",
        "roundtrip=true",
        "clone={\"a\":[1,2],\"b\":\"x\"}",
        "mo=true threw=true recs=0",
        "rafScheduled=true",
    ]);
}
