//! A video's PICTURES and a `MediaSource`'s BUFFERS: the frames a `<video>` decodes (H.264, AV1) are the ones it
//! draws, and the bytes a page appends to a `SourceBuffer` buffer, play and end as Chrome's do.
//!
//! The expected values are Chrome 150's (`google-chrome` 150.0.7871.46 on this machine, headless, over loopback,
//! 2026-10-24), on WPT's own vectors (`tools/realweb/seed/media/vectors/`): the corners of each decoded frame drawn
//! on a canvas, and the event log and ranges of an append, a removal and an end of stream.
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GREEN: &[u8] = include_bytes!("../../../tools/realweb/seed/media/v.mp4");
const FOUR: &[u8] = include_bytes!("../../../tools/realweb/seed/media/vectors/four-colors.mp4");
const FRAGMENTED: &[u8] = include_bytes!("../../../tools/realweb/seed/media/vectors/test-v-128k-320x240-24fps-8kfr.mp4");

async fn serve(listener: TcpListener, page: String) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let page = page.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
            let (status, ctype, body): (&str, &str, &[u8]) = match path.as_str() {
                "/index.html" => ("200 OK", "text/html", page.as_bytes()),
                "/green.mp4" => ("200 OK", "video/mp4", GREEN),
                "/four.mp4" => ("200 OK", "video/mp4", FOUR),
                "/frag.mp4" => ("200 OK", "video/mp4", FRAGMENTED),
                _ => ("404 Not Found", "text/plain", b"missing"),
            };
            let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body).await;
            let _ = s.flush().await;
        });
    }
}

/// Load `html` as the loopback server's `/index.html`, run it until `read` gives a string (or `limit` passes).
async fn run_page(html: &str, limit: Duration, read: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, html.to_string()));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, &format!("{origin}/index.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + limit;
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
        let v = e.eval_value("read", read.to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
        if tokio::time::Instant::now() >= until {
            return String::new();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_files_frames_draw_as_chrome_150_draws_them() {
    let page = r##"<!doctype html><body><script>
        window.R = {};
        for (const f of ["green.mp4", "four.mp4"]) {
          const v = document.createElement("video");
          v.src = f;
          document.body.append(v);
          v.addEventListener("loadeddata", () => setTimeout(() => {
            const c = document.createElement("canvas");
            c.width = v.videoWidth;
            c.height = v.videoHeight;
            const k = c.getContext("2d");
            k.drawImage(v, 0, 0);
            const at = (x, y) => Array.from(k.getImageData(x, y, 1, 1).data);
            R[f] = [at(0, 0), at(c.width - 1, 0), at(0, c.height - 1), at(c.width - 1, c.height - 1)];
          }, 300));
        }
        </script></body>"##;
    let got = run_page(page, Duration::from_secs(5), r#"Object.keys(R).length == 2 ? JSON.stringify(R) : """#).await;
    let got: serde_json::Value = serde_json::from_str(&got).unwrap_or_else(|e| panic!("{e}: {got}"));
    // WPT's 2x2-green, H.264 with no colour description: Chrome draws `1, 128, 2` -- BT.601, as its FFmpeg path takes
    // an SD file's frames. One step of rounding either way is the conversion's, not the matrix's.
    for px in got["green.mp4"].as_array().unwrap() {
        let want = [1i64, 128, 2, 255];
        for (c, w) in px.as_array().unwrap().iter().zip(want) {
            assert!((c.as_i64().unwrap() - w).abs() <= 1, "green {px} (Chrome 1,128,2,255)");
        }
    }
    // WebCodecs' four-colors (AV1): yellow, red, blue, green at the corners, exactly
    assert_eq!(got["four.mp4"], serde_json::json!([[255, 255, 0, 255], [255, 0, 0, 255], [0, 0, 255, 255], [0, 255, 0, 255]]));
}

#[tokio::test]
async fn a_media_source_appends_removes_and_ends_as_chrome_150_does() {
    let page = r##"<!doctype html><body><script>
        window.L = [];
        window.done = "";
        const v = document.createElement("video");
        v.muted = true;
        document.body.append(v);
        const ms = new MediaSource();
        ["sourceopen", "sourceended", "sourceclose"].forEach(e => ms.addEventListener(e, () => L.push("ms:" + e)));
        v.addEventListener("durationchange", () => L.push("v:durationchange"));
        v.src = URL.createObjectURL(ms);
        const r = b => b.length ? Array.from({length: b.length}, (_, i) => [b.start(i), b.end(i)]) : [];
        ms.addEventListener("sourceopen", async () => {
          const sb = ms.addSourceBuffer('video/mp4;codecs="avc1.4D4001"');
          ["updatestart", "update", "updateend"].forEach(e => sb.addEventListener(e, () => L.push("sb:" + e)));
          const buf = await (await fetch("frag.mp4")).arrayBuffer();
          sb.appendBuffer(buf);
          try { sb.appendBuffer(buf); } catch (e) { L.push("again:" + e.name); }
          await new Promise(res => sb.addEventListener("updateend", res, {once: true}));
          L.push("whole=" + JSON.stringify(r(sb.buffered)) + " dur=" + ms.duration);
          sb.remove(0, 1);
          await new Promise(res => sb.addEventListener("updateend", res, {once: true}));
          L.push("removed=" + JSON.stringify(r(sb.buffered)));
          ms.endOfStream();
          L.push("eos dur=" + ms.duration + " " + ms.readyState);
          setTimeout(() => { L.push("w=" + v.videoWidth + " vb=" + JSON.stringify(r(v.buffered))); done = JSON.stringify(L); }, 300);
        });
        </script></body>"##;
    let got = run_page(page, Duration::from_secs(5), "done").await;
    let got: Vec<String> = serde_json::from_str(&got).unwrap_or_else(|e| panic!("{e}: {got}"));
    // `durationchange` is the element's task and lands among the buffer's events by timing; that it fires once (the
    // append's frames lengthen the 2 s the init segment states) is the fact
    assert_eq!(got.iter().filter(|l| *l == "v:durationchange").count(), 1, "{got:?}");
    let rest: Vec<&str> = got.iter().map(String::as_str).filter(|l| *l != "v:durationchange").collect();
    assert_eq!(
        rest,
        vec![
            "ms:sourceopen",
            "again:InvalidStateError",
            "sb:updatestart",
            "sb:update",
            "sb:updateend",
            "whole=[[0.083333,2.083332]] dur=2.083332",
            "sb:updatestart",
            "sb:update",
            "sb:updateend",
            // the removal takes the frames up to the next keyframe with it
            "removed=[[1.083333,2.083332]]",
            "eos dur=2.083332 ended",
            "ms:sourceended",
            "w=320 vb=[[1.083333,2.083332]]",
        ]
    );
}

#[tokio::test]
async fn a_media_source_video_plays_to_its_end() {
    let page = r##"<!doctype html><body><script>
        window.L = [];
        window.done = "";
        const v = document.createElement("video");
        v.muted = true;
        v.autoplay = true;
        document.body.append(v);
        for (const e of ["loadedmetadata", "playing", "pause", "ended"]) v.addEventListener(e, () => L.push(e));
        v.addEventListener("ended", () => setTimeout(() => { done = JSON.stringify({L, t: v.currentTime, ended: v.ended, paused: v.paused}); }, 0));
        const ms = new MediaSource();
        v.src = URL.createObjectURL(ms);
        ms.addEventListener("sourceopen", async () => {
          const sb = ms.addSourceBuffer('video/mp4;codecs="avc1.4D4001"');
          sb.appendBuffer(await (await fetch("frag.mp4")).arrayBuffer());
          sb.addEventListener("updateend", () => ms.endOfStream(), {once: true});
        });
        </script></body>"##;
    let got = run_page(page, Duration::from_secs(8), "done").await;
    assert_eq!(got, r#"{"L":["loadedmetadata","playing","pause","ended"],"t":2.083332,"ended":true,"paused":true}"#);
}
