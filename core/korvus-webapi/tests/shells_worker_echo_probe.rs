//! Phase 4 worker cutover behaviour gate: with the Rust worker installer owning the worker shell
//! surface, a Worker can still receive a message and post one back -- i.e. self.onmessage +
//! self.postMessage + MessageEvent.data all work. This is a BEHAVIOUR check the surface scoreboard
//! (worker_surface.rs, present>=10) is far too loose to make: the worker cutover once passed the surface
//! gate while `MessageEvent.data` was undefined and messaging was dead (the create loop had shelled
//! DedicatedWorkerGlobalScope, which worker-finalize.js reparents globalThis onto). Boots a real Worker
//! that echoes "echo:" + the message data and asserts the round-trip.

use engine::Engine;

async fn worker_echo() -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = r#"self.onmessage = (ev) => { self.postMessage("echo:" + ev.data); };"#;
    let reg = format!(
        "globalThis.__workerScripts = {{ './echo.js': {} }};",
        serde_json::to_string(worker_src).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const w = new Worker('./echo.js'); w.onmessage = (ev) => print('ECHO:' + ev.data); w.postMessage('ping');".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("ECHO:")) {
            return line["ECHO:".len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(15) {
            return format!("TIMEOUT (output: {out:?})");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn worker_echo_survives_the_rust_shell_installer() {
    let result = worker_echo().await;
    assert_eq!(
        result, "echo:ping",
        "worker messaging is broken (want \"echo:ping\"; \"echo:undefined\" = MessageEvent.data broken; \
         TIMEOUT = onmessage/postMessage broken). The Rust worker installer must not touch the worker \
         globalThis chain that worker-finalize.js reparents onto (WorkerGlobalScope family) -- see \
         install::installer::worker_js_owned."
    );
}
