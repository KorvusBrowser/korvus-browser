//! `WebSocket` against stock Chrome 150, row for row -- and the handshake and frames it put on the wire.
//!
//! THE FIXTURE (`fixtures/websocket/`):
//!   * `ws_server.py` -- a WebSocket echo + INSPECTION server (stdlib Python): it logs every handshake
//!     request's header lines in wire order and every frame the client sent (opcode, FIN, RSV1, MASK,
//!     length, close code/reason), and has one route per behaviour (`/echo`, `/deflate`, `/proto`,
//!     `/hdr`, `/cookie`, `/close`, `/drop`, `/status`, `/badaccept`, `/frag`, `/badutf8`, `/ping`,
//!     `/slowclose`);
//!   * `ws-probe.js` -- constructor errors, URL normalisation, every send type, close validation,
//!     bufferedAmount, binaryType, events and their order/fields, failure timing (refused, unsafe
//!     port, close in CONNECTING), the worker realm;
//!   * `chrome-150.run{1,2,3}.json` -- stock Chrome 150.0.7871.46, headed under Xvfb, NO DevTools
//!     client, three recordings (`record_chrome.sh` through abkit `live_slot.sh`, 2026-10-11),
//!     identical but for milliseconds; `chrome-150.run1-server.txt` -- the server's log of run 1.
//!
//! THE COMPARISON: every row's fields exactly, except an event's milliseconds, which are a BOUND (the
//! three Chrome runs' maximum plus slack -- a loopback connect is 0-12 ms in Chrome, the closing
//! handshake's 2 s wait is 2002 ms). The server log is compared as a multiset of connections: the
//! request line, the header NAMES in wire order, and each client frame's flags and length (a
//! compressed frame's length is the deflater's business and is not compared).
//!
//! Needs `python3` on PATH (the gate box has it).
//!
//! ITS OWN HOST. The transport's cookie jar is process-wide and cookies are scoped by HOST, not port,
//! so a page on `127.0.0.1` in this binary sees every cookie any other test set on `127.0.0.1`
//! (`client_hints_live`'s `k=1` arrived in this handshake). The server binds [`HOST`], a loopback
//! address no other test uses, and the rows and the server log are written back to Chrome's
//! `127.0.0.1` before they are compared.
use engine::Engine;

/// The loopback address this test (and no other) serves from. Linux answers all of 127.0.0.0/8.
const HOST: &str = "127.0.0.77";

const PROBE: &str = include_str!("fixtures/websocket/ws-probe.js");
const CHROME_RUNS: [&str; 3] = [
    include_str!("fixtures/websocket/chrome-150.run1.json"),
    include_str!("fixtures/websocket/chrome-150.run2.json"),
    include_str!("fixtures/websocket/chrome-150.run3.json"),
];
const CHROME_LOG: &str = include_str!("fixtures/websocket/chrome-150.run1-server.txt");

/// The fixture server, killed and its directory removed on drop.
struct Server {
    child: std::process::Child,
    port: u16,
    log: std::path::PathBuf,
    dir: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start_server() -> Server {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/websocket/ws_server.py");
    let dir = std::env::temp_dir().join(format!("korvus-ws-fixture-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&dir).expect("make the fixture server's directory");
    let port_file = dir.join("port");
    let log = dir.join("server.log");
    std::fs::write(&log, "").unwrap();
    let child = std::process::Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg(&port_file)
        .arg(&log)
        .env("WS_FIXTURE_HOST", HOST)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn python3 ws_server.py (python3 must be on PATH)");
    let start = std::time::Instant::now();
    let port = loop {
        if let Ok(s) = std::fs::read_to_string(&port_file) {
            if let Ok(p) = s.trim().parse::<u16>() {
                break p;
            }
        }
        if start.elapsed() > std::time::Duration::from_secs(10) {
            panic!("ws_server.py never wrote its port");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    Server { child, port, log, dir }
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

async fn pump_for(e: &mut Engine, tag: &str, limit: std::time::Duration) -> String {
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with(tag)) {
            return line[tag.len()..].to_string();
        }
        if start.elapsed() > limit {
            panic!("{tag} never arrived within {limit:?}; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// Run the probe in a page served from the fixture's origin, as Chrome ran it; rows with the port
/// written `PORT`, as `record_chrome.sh` wrote Chrome's.
async fn korvus_rows(server: &Server) -> Vec<serde_json::Value> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    let url = format!("http://{HOST}:{}/", server.port);
    e.load_page("<!doctype html><html><head><meta charset=utf-8></head><body></body></html>", &url, &[])
        .await
        .expect("load_page");
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), e.run()).await;
    // The probe's socket URLs on the fixture server all spell `127.0.0.1:' + P` (Chrome's recording);
    // this one replacement moves exactly those three to HOST.
    assert_eq!(PROBE.matches("127.0.0.1:' + P").count(), 3, "the probe's fixture-host URLs moved; update this test");
    let probe = PROBE.replace("127.0.0.1:' + P", &format!("{HOST}:' + P")).replace("__WSPORT__", &server.port.to_string());
    e.eval(
        "ws-probe",
        format!(
            "({probe}).then(r => print('WSROWS:' + JSON.stringify(r)), x => print('WSROWS:' + JSON.stringify([['FATAL', String(x)]])));"
        ),
    )
    .unwrap();
    let s = pump_for(&mut e, "WSROWS:", std::time::Duration::from_secs(120)).await;
    if let Ok(d) = std::env::var("KORVUS_WS_OUT") {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(format!("{d}/korvus.json"), &s);
        let _ = std::fs::copy(&server.log, format!("{d}/korvus.log"));
    }
    let s = to_chrome_host(&s, server.port);
    serde_json::from_str(&s).expect("the probe's rows are JSON")
}

/// `HOST:<port>` written as Chrome's recording writes it, `127.0.0.1:PORT`.
fn to_chrome_host(s: &str, port: u16) -> String {
    // The probe already writes some ports as `PORT`, so the host is rewritten in that spelling too.
    s.replace(&format!("{HOST}:{port}"), "127.0.0.1:PORT")
        .replace(&format!("{HOST}:PORT"), "127.0.0.1:PORT")
        .replace(&format!(":{port}"), ":PORT")
}

/// A row without its milliseconds (an `ev` row's index 3), and the milliseconds.
fn split_ms(row: &serde_json::Value) -> (serde_json::Value, Option<f64>) {
    let mut r = row.clone();
    let a = r.as_array_mut().expect("a row is an array");
    // The `/hdr` echo is the raw handshake as JSON, so its LENGTH carries the device's own
    // User-Agent / Accept-Language values; the rows after it compare what is not device-specific.
    if a.len() > 4 && a[0] == "handshake" && a[2] == "message" {
        a[4] = serde_json::Value::from("string(handshake echo)");
    }
    if a.len() > 3 && a[1] == "ev" {
        let ms = a[3].as_f64();
        a[3] = serde_json::Value::from("MS");
        return (r, ms);
    }
    (r, None)
}

/// One connection of the server log, as compared: request line, header names, client frames.
fn conn_sig(line: &str) -> String {
    let r: serde_json::Value = serde_json::from_str(line).expect("a server log line is JSON");
    let names: Vec<&str> = r["headers"].as_array().unwrap().iter().map(|h| h[0].as_str().unwrap()).collect();
    let frames: Vec<String> = r["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let rsv1 = f["rsv1"].as_bool().unwrap_or(false);
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                f["op"],
                f["fin"],
                rsv1,
                f["masked"],
                if rsv1 { "*".to_string() } else { f["len"].to_string() },
                f.get("code").map(|c| c.to_string()).unwrap_or_default(),
                f.get("reason").and_then(|c| c.as_str()).unwrap_or("")
            )
        })
        .collect();
    format!("{} | {} | {}", r["request_line"].as_str().unwrap_or(""), names.join(","), frames.join(" "))
}

#[tokio::test]
async fn websocket_matches_chrome_150_row_for_row_and_on_the_wire() {
    // The three Chrome recordings must agree with each other first, or the fixture is noise.
    let runs: Vec<Vec<serde_json::Value>> = CHROME_RUNS.iter().map(|s| serde_json::from_str(s).unwrap()).collect();
    let shapes: Vec<Vec<serde_json::Value>> = runs.iter().map(|r| r.iter().map(|x| split_ms(x).0).collect()).collect();
    assert_eq!(shapes[0], shapes[1], "Chrome runs 1 and 2 disagree beyond milliseconds");
    assert_eq!(shapes[0], shapes[2], "Chrome runs 1 and 3 disagree beyond milliseconds");
    let chrome_max_ms: Vec<Option<f64>> = (0..runs[0].len())
        .map(|i| runs.iter().filter_map(|r| split_ms(&r[i]).1).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))))
        .collect();

    let server = start_server();
    let ours = korvus_rows(&server).await;
    let ours_shape: Vec<serde_json::Value> = ours.iter().map(|x| split_ms(x).0).collect();

    // Row for row. On a mismatch, print both sequences whole: an order difference is invisible in a
    // single-row diff.
    let mut bad: Vec<String> = Vec::new();
    for i in 0..shapes[0].len().max(ours_shape.len()) {
        let c = shapes[0].get(i);
        let k = ours_shape.get(i);
        if c != k {
            bad.push(format!("row {i}: chrome {}  korvus {}", c.map(|v| v.to_string()).unwrap_or("-".into()), k.map(|v| v.to_string()).unwrap_or("-".into())));
        }
    }
    // One line per differing row on STDERR first, each self-contained, so a gate log that truncates
    // the panic message still names every row.
    for b in &bad {
        eprintln!("websocket_live ROW-DIFF {b}");
    }
    assert!(bad.is_empty(), "{} row(s) differ from Chrome 150:\n{}", bad.len(), bad.join("\n"));

    // Timing: a bound, not a value. Chrome's slowest of three plus 150 ms (a loopback connect, a
    // refused port and a failed handshake are all 0-12 ms in Chrome; the closing handshake's
    // underlying-connection wait is 2002 ms), and never EARLIER than Chrome's by more than the same
    // slack -- a close that came at 0 ms where Chrome waits 2 s is as wrong as one that never comes.
    let mut slow: Vec<String> = Vec::new();
    for (i, row) in ours.iter().enumerate() {
        let (_, ms) = split_ms(row);
        let (Some(k), Some(c)) = (ms, chrome_max_ms[i]) else { continue };
        let c_min = runs.iter().filter_map(|r| split_ms(&r[i]).1).fold(f64::MAX, f64::min);
        if k > c + 150.0 || k + 150.0 < c_min {
            slow.push(format!("row {i} {}: korvus {k} ms, chrome {c_min}..{c} ms", ours_shape[i]));
        }
    }
    for s in &slow {
        eprintln!("websocket_live TIMING {s}");
    }
    assert!(slow.is_empty(), "event timing outside Chrome's bound:\n{}", slow.join("\n"));

    // The wire: the same connections, the same header names in the same order, the same frames. The
    // server writes a connection's line when it ends; give the last close a moment to land.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let ours_log = std::fs::read_to_string(&server.log).expect("read the server log");
    let ours_log = to_chrome_host(&ours_log, server.port);
    // A socket `close()`d while CONNECTING RACES its own handshake: Blink's `Fail` only posts the
    // teardown, so the network side keeps connecting until that task runs, and a handshake that wins
    // is dropped with `Close 1001` (Chrome reached the server with the first of the four URL cases in
    // all three recordings, and with none of the others). Those sockets have paths of their own
    // (`/`, `/norm*`, `/a%20b`, `/race/*`); whether one reached the server is timing, so they are
    // left out of the multiset -- and whichever did must have said goodbye the way Chrome does.
    let racy = |line: &str| -> bool {
        let r: serde_json::Value = serde_json::from_str(line).expect("a server log line is JSON");
        let route = r["route"].as_str().unwrap_or("").to_string();
        let is_racy = route == "/" || route.starts_with("/norm") || route == "/a%20b" || route.starts_with("/race/");
        if is_racy {
            let frames = r["frames"].as_array().cloned().unwrap_or_default();
            let ok = frames.is_empty() || (frames.len() == 1 && frames[0]["op"] == 8 && frames[0]["code"] == 1001);
            assert!(ok, "a socket closed while CONNECTING reached the server and did not leave as Chrome does (nothing, or Close 1001): {line}");
        }
        is_racy
    };
    let mut chrome_conns: Vec<String> =
        CHROME_LOG.lines().filter(|l| !l.trim().is_empty() && !racy(l)).map(conn_sig).collect();
    let mut our_conns: Vec<String> = ours_log.lines().filter(|l| !l.trim().is_empty() && !racy(l)).map(conn_sig).collect();
    chrome_conns.sort();
    our_conns.sort();
    let missing: Vec<&String> = chrome_conns.iter().filter(|c| !our_conns.contains(c)).collect();
    let extra: Vec<&String> = our_conns.iter().filter(|c| !chrome_conns.contains(c)).collect();
    for m in &missing {
        eprintln!("websocket_live WIRE chrome-only {m}");
    }
    for x in &extra {
        eprintln!("websocket_live WIRE korvus-only {x}");
    }
    if chrome_conns.len() != our_conns.len() {
        eprintln!("websocket_live WIRE count chrome {} korvus {}", chrome_conns.len(), our_conns.len());
    }
    assert!(
        missing.is_empty() && extra.is_empty() && chrome_conns.len() == our_conns.len(),
        "the wire differs from Chrome 150's.\nChrome only:\n{}\nkorvus only:\n{}",
        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"),
        extra.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    );
}
