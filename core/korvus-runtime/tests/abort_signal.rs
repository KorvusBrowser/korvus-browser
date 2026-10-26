//! AbortController + AbortSignal.abort/timeout/any + DOMException.
use engine::Engine;
#[tokio::test]
async fn abort_signal() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        const ac = new AbortController();
        let fired = false;
        ac.signal.addEventListener("abort", () => { fired = true; });
        print("before=" + ac.signal.aborted);
        ac.abort();
        print("after=" + ac.signal.aborted + ",fired=" + fired + ",name=" + ac.signal.reason.name);
        const s = AbortSignal.abort("boom");
        print("static=" + s.aborted + "," + s.reason);
        const any = AbortSignal.any([new AbortController().signal, s]);
        print("any=" + any.aborted);
        const de = new DOMException("nope", "NotFoundError");
        print("dom=" + de.name + "," + de.code + "," + (de instanceof Error) + "," + (de.toString().includes("nope")));
        // signal is a [SameObject] readonly attribute: prototype accessor (get-only, native),
        // identity across reads, prototype member order signal,abort, and a strict-mode setter throw.
        const d = Object.getOwnPropertyDescriptor(AbortController.prototype, "signal");
        print("sig=" + (ac.signal === ac.signal) + "," + (ac.signal instanceof AbortSignal)
            + "," + d.get.name + "," + d.get.length + "," + (d.set === undefined)
            + "," + d.enumerable + "," + d.configurable
            + "," + d.get.toString().includes("[native code]"));
        print("order=" + Object.getOwnPropertyNames(AbortController.prototype).filter((n) => n !== "constructor").join(","));
        let strictThrow = "no-throw";
        try { (function () { "use strict"; new AbortController().signal = null; })(); } catch (err) { strictThrow = err.name; }
        print("readonly=" + strictThrow + ",own=" + Object.keys(new AbortController()).length);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "before=false",
        "after=true,fired=true,name=AbortError",
        "static=true,boom",
        "any=true",
        "dom=NotFoundError,8,true,true",
        "sig=true,true,get signal,0,true,true,true,true",
        "order=signal,abort",
        "readonly=TypeError,own=0",
    ]);
}
