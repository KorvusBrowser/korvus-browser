//! A media element LOADS: the resource selection algorithm runs, the resource is fetched
//! and its header read, `readyState` and `networkState` move, the media events fire, and `play()`
//! follows Chrome's autoplay policy.
//!
//! The page and Chrome's answer are fixture `M-media-play`'s (`tools/realweb/seed/media/`, measured on
//! Chrome 150 with no user activation): four `<video>` elements -- a muted and an unmuted playable
//! mp4, one with no source, one whose source is not media -- each `play()`ed right after its setup.
//! The answer is read from the manifest, so this test and the fixture cannot drift apart.
use engine::Engine;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PAGE: &str = include_str!("../../../tools/realweb/seed/media/index.html");
const MP4: &[u8] = include_bytes!("../../../tools/realweb/seed/media/v.mp4");
const NOT_MEDIA: &[u8] = include_bytes!("../../../tools/realweb/seed/media/bad.xyz");

fn chrome_expected() -> String {
    let m: serde_json::Value = serde_json::from_str(include_str!("../../../tools/realweb/seed/manifest.json")).unwrap();
    m["M-media-play"]["chrome_expected"].as_str().unwrap().to_string()
}

/// Serve the fixture's three files; anything else is a 404.
async fn serve(listener: TcpListener) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
            let (status, ctype, body): (&str, &str, &[u8]) = match path.as_str() {
                "/index.html" => ("200 OK", "text/html", PAGE.as_bytes()),
                "/v.mp4" => ("200 OK", "video/mp4", MP4),
                "/bad.xyz" => ("200 OK", "application/octet-stream", NOT_MEDIA),
                _ => ("404 Not Found", "text/plain", b"missing"),
            };
            let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body).await;
            let _ = s.flush().await;
        });
    }
}

/// Load `html` as the loopback server's `/index.html`, let it run for `settle`, then evaluate `read`.
async fn run_page(html: &str, settle: Duration, read: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, &format!("{origin}/index.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    let until = tokio::time::Instant::now() + settle;
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let v = e.eval_value("read", read.to_string()).expect("eval");
    v.as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn the_four_videos_load_play_and_fail_as_chrome_measured() {
    let got = run_page(PAGE, Duration::from_millis(2500), "JSON.stringify(window.R)").await;
    let got: serde_json::Value = serde_json::from_str(&got).unwrap_or_else(|e| panic!("{e}: {got}"));
    let want: serde_json::Value = serde_json::from_str(&chrome_expected()).unwrap();
    assert_eq!(got, want);
}

/// A parsed `<video autoplay muted src>` loads and plays with no script at all -- the parser's
/// element runs the same load as a script's -- and a `<source>` whose `type` the browser cannot play
/// is passed over with an `error` at that `<source>`, not at the video.
#[tokio::test]
async fn a_parsed_autoplay_video_plays_and_an_unplayable_source_is_skipped() {
    let page = r##"<!doctype html><html><body>
        <video id=a autoplay muted src="v.mp4"></video>
        <video id=b muted><source src="x.mkv" type="video/x-nothing"><source src="v.mp4" type="video/mp4"></video>
        <script>
        window.L = [];
        for (const id of ["a", "b"]) {
          const v = document.getElementById(id);
          for (const t of ["play", "playing", "loadedmetadata", "error"]) v.addEventListener(t, () => L.push(id + ":" + t));
        }
        document.querySelector("#b source").addEventListener("error", () => L.push("b-source-0:error"));
        </script></body></html>"##;
    let got = run_page(
        page,
        Duration::from_millis(800),
        r#"JSON.stringify({log: L.slice().sort(), a: [a.paused, a.readyState, a.currentSrc.split('/').pop()], b: [b.readyState, b.networkState, b.currentSrc.split('/').pop(), b.error]})"#,
    )
    .await;
    assert_eq!(
        got,
        r#"{"log":["a:loadedmetadata","a:play","a:playing","b-source-0:error","b:loadedmetadata"],"a":[true,4,"v.mp4"],"b":[4,1,"v.mp4",null]}"#
    );
}
