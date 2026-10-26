//! Workers inside workers, and storage a worker makes durable (the live a challenge provider challenge, 2026-10-07).
//!
//! a managed bot challenge starts a worker in its frame, and that worker starts the workers that
//! do the work; korvus's worker realm registered no spawn ops, so `new Worker(...)` inside a worker
//! branded and returned a worker that never ran -- no message, no error. Measured on Chrome 150
//! (`--headless=new`, loopback):
//!
//! ```text
//! worker -> new Worker(blob it made)        inner workers run; their messages relay out
//! worker -> new Worker(its PAGE's blob URL) "inner from page blob ok" (korvus: TypeError: Failed to fetch)
//! sync access handle flush()                ~1.1 ms on a real profile: the bytes reach the disk
//! ```

use engine::Engine;
use std::time::Duration;

/// Run `page_js` on a blank page and pump until it prints a line starting `RESULT:`.
async fn page_result(page_js: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    // a real origin (OPFS refuses an opaque one), loopback being a secure context
    e.load_page("<!doctype html><html><head></head><body></body></html>", "http://127.0.0.1:8123/page.html", &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval("driver", page_js.to_string()).unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    while start.elapsed() < Duration::from_secs(20) {
        let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(l) = out.iter().find(|l| l.starts_with("RESULT:")) {
            return l["RESULT:".len()..].to_string();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    format!("NONE (output: {out:?})")
}

#[tokio::test]
async fn a_worker_starts_workers_of_its_own() {
    let got = page_result(
        r#"
        const inner = 'postMessage("inner ok")';
        const outer = `
            const u = URL.createObjectURL(new Blob([${JSON.stringify(inner)}], {type: 'text/javascript'}));
            let n = 0; const seen = [];
            for (const k of [1, 2]) {
                const w = new Worker(u);
                w.onmessage = e => { seen.push(k + ':' + e.data); if (++n === 2) postMessage(seen.sort().join(',')); };
                w.onerror = e => postMessage('inner error: ' + e.message);
            }`;
        const w = new Worker(URL.createObjectURL(new Blob([outer], {type: 'text/javascript'})));
        w.onmessage = e => print('RESULT:' + e.data);
        "#,
    )
    .await;
    assert_eq!(got, "1:inner ok,2:inner ok");
}

#[tokio::test]
async fn a_worker_starts_a_blob_url_its_creator_made() {
    let got = page_result(
        r#"
        const innerUrl = URL.createObjectURL(new Blob(['postMessage("inner from page blob ok")'], {type: 'text/javascript'}));
        const outer = `onmessage = e => { const w = new Worker(e.data); w.onmessage = m => postMessage(m.data); w.onerror = er => postMessage('inner error: ' + er.message); }`;
        const w = new Worker(URL.createObjectURL(new Blob([outer], {type: 'text/javascript'})));
        w.onmessage = e => print('RESULT:' + e.data);
        w.postMessage(innerUrl);
        "#,
    )
    .await;
    assert_eq!(got, "inner from page blob ok");
}

/// `flush()` makes the file durable: the process's backing file for it holds the bytes afterwards (and the
/// call takes the time a disk sync takes, which a no-op flush could not).
#[tokio::test]
async fn a_sync_access_handle_flush_reaches_the_disk() {
    let got = page_result(
        r#"
        const src = `navigator.storage.getDirectory().then(r => r.getFileHandle('nested-flush-probe', {create: true}))
            .then(f => f.createSyncAccessHandle()).then(h => {
                h.write(new TextEncoder().encode('durable-bytes'), {at: 0});
                h.flush(); h.close(); postMessage('flushed');
            }).catch(e => postMessage('error ' + e.message));`;
        const w = new Worker(URL.createObjectURL(new Blob([src], {type: 'text/javascript'})));
        w.onmessage = e => print('RESULT:' + e.data);
        "#,
    )
    .await;
    assert_eq!(got, "flushed");
    let dir = std::env::temp_dir().join(format!("korvus-opfs-{}", std::process::id()));
    let found = std::fs::read_dir(&dir)
        .map(|d| d.flatten().any(|e| std::fs::read(e.path()).map(|b| b == b"durable-bytes").unwrap_or(false)))
        .unwrap_or(false);
    assert!(found, "no backing file in {dir:?} holds the flushed bytes");
}
