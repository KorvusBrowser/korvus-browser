//! `img.decode()` waits for the request it was called for (issue #26).
//!
//! HTML's `decode()` resolves once the element's current request is completely available and decoded, rejects with
//! EncodingError when that request breaks, and rejects when the current request changes first. HyperFrames injects
//! every video frame as `img.src = next; await img.decode()` and captures the page straight after; korvus answered that
//! `decode()` from the PREVIOUS frame -- the "available at once" test read the image under the element's current
//! request, not the URL just selected -- so `complete` read false, `naturalWidth` 0, and half of v152.0.8's captures
//! were blank where the video should be.
//!
//! THE ORACLE IS CHROME. Chrome 155.0.8059.39 headless-shell, 2026-11-14, over a loopback server answering after the
//! delays below, gave exactly the answers asserted here (the probe: `next_frame`, `src_change_midflight`,
//! `src_change_same_task`, `http_404`, `http_junk`, `data_after_http`, `blob`).
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A 20x10 PNG.
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAABQAAAAKCAIAAAA7N+mxAAAAF0lEQVR4nGM8ISfHQC5gIlvnqOYRoxkAhH4BGBokPGwAAAAASUVORK5CYII=";

/// `/img/<name>?d=<ms>`: the PNG after `d` ms; `/404?d=` a 404; `/junk?d=` bytes that are no image.
async fn serve(listener: TcpListener) {
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.decode(PNG_B64).unwrap();
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let png = png.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let delay = path.split("d=").nth(1).and_then(|d| d.parse::<u64>().ok()).unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let (status, ctype, body) = if path.starts_with("/img/") {
                (200, "image/png", png)
            } else if path.starts_with("/junk") {
                (200, "image/png", b"not an image".to_vec())
            } else {
                (404, "text/plain", b"no".to_vec())
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

/// Run `script` (an async function body that returns a value) in a page served from the loopback server, and pump
/// until it has answered (or 10 s).
async fn answer(script: &str) -> serde_json::Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = "<!doctype html><html><body><img id=i></body></html>";
    e.load_page(doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump");
    let run = format!(
        r#"window.settle = (p) => p.then(() => "resolved", (e) => "rejected " + e.name);
        window.st = (i) => [i.complete, i.naturalWidth];
        window.ev = (i) => {{ const log = []; i.onload = () => log.push("load " + i.naturalWidth); i.onerror = () => log.push("error"); return log; }};
        window.__r = undefined;
        (async () => {{ {script} }})().then((v) => {{ window.__r = JSON.stringify(v); }}, (e) => {{ window.__r = JSON.stringify("threw " + e); }});
        0"#
    );
    e.eval_value("decode-run", run).expect("eval");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let r = e.eval_value("decode-answer", "window.__r === undefined ? null : window.__r".to_string()).expect("eval");
        if let Some(s) = r.as_str() {
            return serde_json::from_str(s).unwrap();
        }
        assert!(std::time::Instant::now() < deadline, "the page never answered: a decode() promise never settled");
        e.pump(Duration::from_millis(20)).await.expect("pump");
    }
}

/// THE HYPERFRAMES SHAPE: one `<img>`, a new `http:` frame each time, `await decode()`, then read it. Every frame is
/// there when `decode()` resolves (Chrome: 20 of 20; korvus 152.0.8: 1 of 20 -- the first, which had no image before).
#[tokio::test]
async fn decode_waits_for_the_next_frame_over_http() {
    let got = answer(
        r#"const i = document.getElementById("i"); const out = [];
        for (let n = 0; n < 6; n++) { i.src = `/img/f${n}.png?d=${n % 2 ? 40 : 0}`; const r = await settle(i.decode()); out.push([r, ...st(i)]); }
        return out;"#,
    )
    .await;
    let want: Vec<serde_json::Value> = (0..6).map(|_| serde_json::json!(["resolved", true, 20])).collect();
    assert_eq!(got, serde_json::Value::Array(want), "decode() must resolve only once the new frame is loaded");
}

/// A `src` that moves on while the first request is in flight: the first `decode()` rejects (EncodingError), the second
/// resolves with the second image, and the abandoned request fires nothing (Chrome: one `load`).
#[tokio::test]
async fn a_src_change_in_flight_rejects_the_earlier_decode() {
    let got = answer(
        r#"const i = new Image(); const log = ev(i);
        i.src = "/img/a.png?d=150"; const p1 = settle(i.decode());
        await new Promise((r) => setTimeout(r, 20));
        i.src = "/img/b.png?d=30"; const p2 = settle(i.decode());
        const r1 = await p1, r2 = await p2;
        await new Promise((r) => setTimeout(r, 250));
        return [r1, r2, ...st(i), log];"#,
    )
    .await;
    assert_eq!(got, serde_json::json!(["rejected EncodingError", "resolved", true, 20, ["load 20"]]));
}

/// The same change within one task: `src = a; decode(); src = b; decode()` rejects the first, resolves the second.
#[tokio::test]
async fn a_src_change_in_the_same_task_rejects_the_earlier_decode() {
    let got = answer(
        r#"const i = new Image();
        i.src = "/img/c.png?d=60"; const p1 = settle(i.decode());
        i.src = "/img/d.png?d=20"; const p2 = settle(i.decode());
        return [await p1, await p2, ...st(i)];"#,
    )
    .await;
    assert_eq!(got, serde_json::json!(["rejected EncodingError", "resolved", true, 20]));
}

/// A failed load rejects: a 404 and bytes that are no image, each EncodingError, with `complete` true after it.
#[tokio::test]
async fn decode_rejects_a_failed_load() {
    let got = answer(
        r#"const out = [];
        for (const u of ["/404?d=30", "/junk?d=30"]) { const i = new Image(); i.src = u; out.push([await settle(i.decode()), ...st(i)]); }
        return out;"#,
    )
    .await;
    assert_eq!(got, serde_json::json!([["rejected EncodingError", true, 0], ["rejected EncodingError", true, 0]]));
}

/// `data:` and `blob:` follow the same rule: after an `http:` frame, a `data:` one resolves loaded; a `blob:` one too.
#[tokio::test]
async fn data_and_blob_sources_wait_as_http_does() {
    let got = answer(&format!(
        r#"const png = "data:image/png;base64,{PNG_B64}";
        const i = new Image(); i.src = "/img/e.png?d=20"; await i.decode();
        i.src = png; const a = [await settle(i.decode()), ...st(i)];
        const bl = await (await fetch("/img/g.png?d=0")).blob();
        const j = new Image(); j.src = URL.createObjectURL(bl); const b = [await settle(j.decode()), ...st(j)];
        return [a, b];"#
    ))
    .await;
    assert_eq!(got, serde_json::json!([["resolved", true, 20], ["resolved", true, 20]]));
}
