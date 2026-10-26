//! performance / matchMedia / AbortController.
use engine::Engine;
#[tokio::test]
async fn perf_media_abort() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("p", r#"
        print("perfNow=" + (typeof performance.now() === "number" && performance.now() >= 0));
        print("origin=" + (typeof performance.timeOrigin === "number"));
        const mm = matchMedia("(max-width: 600px)");
        print("mm=" + mm.matches + "," + mm.media);
        const ac = new AbortController();
        let aborted = 0;
        ac.signal.addEventListener("abort", () => { aborted++; });
        print("before=" + ac.signal.aborted);
        ac.abort("stop");
        print("after=" + ac.signal.aborted + "," + aborted + "," + ac.signal.reason);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "perfNow=true",
        "origin=true",
        "mm=false,(max-width: 600px)",
        "before=false",
        "after=true,1,stop",
    ]);
}
