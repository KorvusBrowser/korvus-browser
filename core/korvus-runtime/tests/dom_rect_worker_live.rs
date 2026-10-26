//! LIVE one-table gate for the DOMRect family INSIDE a worker isolate
//! (Track A worker half).
//!
//! DOMRect is [Exposed=(Window,Worker)]. It used to be a stub shell in the worker
//! (generated_worker_shells.js); the cutover removes that shell, assigns the native cppgc class onto
//! the worker global (worker-host.js), and reconciles it to the `interface!` table in worker.rs. This
//! proves the running worker DOMRect is now byte-identical to brave-151.worker.surface.json AND real
//! (a getter returns the constructed value, not the shell's 0): the same reconcile the window gets,
//! in the other realm.

use engine::Engine;

/// Spawn a worker that probes DOMRect and posts a pipe-joined summary back; pump until RESULT:.
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
async fn worker_dom_rect_family_is_byte_identical_and_real() {
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return (d.get || d.set) ? `${n}:a:s=${+!!d.set}` : `${n}:d:w=${+d.writable}`;
        }).join(",");
        const rro = DOMRectReadOnly, r = DOMRect;
        const parts = [
            desc(rro.prototype),
            desc(r.prototype),
            Object.prototype.toString.call(new rro(1,2,3,4)),
            Object.prototype.toString.call(new r(1,2,3,4)),
            Object.getPrototypeOf(r.prototype) === rro.prototype,
            Object.getPrototypeOf(r) === rro,
            r.fromRect.length, rro.fromRect.length,
            new r(7, 8, 9, 10).width,          // 9 -- a REAL native getter, not the shell's 0
            // WebIDL `unrestricted double`, IN THIS REALM TOO. The ESM layer that
            // used to wrap these setters never ran in a worker, so `rect.x = "5"` threw here and
            // stored 5 in the window -- one member, two answers. The conversion is the ROW's now,
            // and the row is the same row in both realms.
            (() => { const q = new r(0,0,0,0); try { q.x = "5"; } catch (e) { return "THREW"; } return typeof q.x + ":" + q.x; })(),
            (() => { const q = new r(0,0,0,0); try { q.height = "abc"; } catch (e) { return "THREW"; } return String(Number.isNaN(q.height)); })()
        ];
        self.postMessage(parts.join("|"));
    "#;
    let got = worker_probe(body).await;
    let rro = "x:a:s=0,y:a:s=0,width:a:s=0,height:a:s=0,top:a:s=0,right:a:s=0,bottom:a:s=0,left:a:s=0,toJSON:d:w=1,constructor:d:w=1";
    let r = "x:a:s=1,y:a:s=1,width:a:s=1,height:a:s=1,constructor:d:w=1";
    let expected = format!("{rro}|{r}|[object DOMRectReadOnly]|[object DOMRect]|true|true|0|0|9|number:5|true");
    assert_eq!(got, expected, "worker DOMRect family != capture (byte-identical + real native getter)");
}
