//! LIVE one-table gate for the DOMPoint family INSIDE a worker isolate (Phase 5 F1a; mirrors
//! dom_rect_worker_live.rs).
//!
//! DOMPoint is [Exposed=(Window,Worker)]. The cutover removes its worker shell
//! (shells.worker.registry.json), keeps the native cppgc class assigned onto the worker global, and
//! reconciles it to the `interface!` table in worker.rs. This proves the running worker DOMPoint is
//! byte-identical to brave-151.worker.surface.json AND real (a getter returns the constructed value):
//! the same reconcile the window gets, in the other realm.

use engine::Engine;

/// Spawn a worker that probes DOMPoint and posts a pipe-joined summary back; pump until RESULT:.
async fn worker_probe(worker_body: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(worker_body).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NONE (output: {out:?})"))
}

#[tokio::test]
async fn worker_dom_point_family_is_byte_identical_and_real() {
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return (d.get || d.set) ? `${n}:a:s=${+!!d.set}` : `${n}:d:w=${+d.writable}`;
        }).join(",");
        const rro = DOMPointReadOnly, r = DOMPoint;
        const parts = [
            desc(rro.prototype),
            desc(r.prototype),
            Object.prototype.toString.call(new rro(1,2,3,4)),
            Object.prototype.toString.call(new r(1,2,3,4)),
            Object.getPrototypeOf(r.prototype) === rro.prototype,
            Object.getPrototypeOf(r) === rro,
            r.fromPoint.length, rro.fromPoint.length,
            new r(7, 8, 9, 10).z           // 9 -- a REAL native getter, not the shell's 0
        ];
        self.postMessage(parts.join("|"));
    "#;
    let got = worker_probe(body).await;
    let rro = "x:a:s=0,y:a:s=0,z:a:s=0,w:a:s=0,matrixTransform:d:w=1,toJSON:d:w=1,constructor:d:w=1";
    let r = "x:a:s=1,y:a:s=1,z:a:s=1,w:a:s=1,constructor:d:w=1";
    let expected = format!("{rro}|{r}|[object DOMPointReadOnly]|[object DOMPoint]|true|true|0|0|9");
    assert_eq!(got, expected, "worker DOMPoint family != capture (byte-identical + real native getter)");
}
