//! An `RTCPeerConnection`'s configuration, offer and ICE gathering, as Chrome's (a challenge provider's
//! challenge -- in the page and in its frame -- gathers against its STUN server and reads every candidate).
//!
//! `fixtures/webrtc/webrtc.html` is loaded UNMODIFIED here and in Chrome 152.0.7977.82 (headed on the Wayland
//! desktop, as the persona's capture ran), each against a STUN server that answers every Binding Request with 203.0.113.9 and the
//! request's own port (`fixtures/webrtc/stun-server.py` for Chrome, the thread below here), named by
//! `?stun=<port>`. `webrtc.chrome-152.txt` is Chrome's `window.__r`, three runs identical: the dictionary's read
//! order, `getConfiguration()`, seventeen conversion and ICE-URL errors, `setConfiguration`, the
//! `RTCPeerConnectionIceEvent` constructor, `createOffer`'s option reads and its three m-sections, and the
//! six candidates three transports gather (their strings normalized: foundation, port, mDNS name, ufrag).
//! A line that differs is a finding about the engine, never a reason to edit the file.
//!
//! Re-record: `python3 stun-server.py 34780 log` beside `python3 -m http.server` over the directory, open
//! `webrtc.html?stun=34780` in Chrome, wait 2 s, keep `JSON.stringify(window.__r, null, 1)`.
use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("fixtures/webrtc/webrtc.html");
const CHROME: &str = include_str!("fixtures/webrtc/webrtc.chrome-152.txt");
const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

/// The fixture STUN server: every Binding Request answered with XOR-MAPPED-ADDRESS 203.0.113.9:<its port>,
/// and kept.
fn stun_server() -> (u16, Arc<Mutex<Vec<Vec<u8>>>>) {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = s.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let keep = seen.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        while let Ok((n, from)) = s.recv_from(&mut buf) {
            keep.lock().unwrap().push(buf[..n].to_vec());
            if n < 20 || buf[0..2] != [0, 1] || buf[4..8] != [0x21, 0x12, 0xa4, 0x42] {
                continue;
            }
            let mut r = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
            r.extend_from_slice(&buf[8..20]);
            r.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
            r.extend_from_slice(&(from.port() ^ 0x2112).to_be_bytes());
            r.extend_from_slice(&[203 ^ 0x21, 0x12, 113 ^ 0xa4, 9 ^ 0x42]);
            let _ = s.send_to(&r, from);
        }
    });
    (port, seen)
}

#[tokio::test]
async fn a_peer_connection_configures_offers_and_gathers_as_chromes_does() {
    let want: Vec<String> = serde_json::from_str(CHROME).expect("the recorded Chrome output is a JSON list");
    let (port, seen) = stun_server();
    // Chrome 152's record, so the engine wears Chrome 152: an offer's audio and video sections are the device's.
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    e.run().await.unwrap();
    e.load_page(PAGE, &format!("http://127.0.0.1:8123/webrtc.html?stun={port}"), &[]).await.expect("load_page");
    let start = Instant::now();
    let mut got: Vec<String> = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
        e.eval("read", "print('R:' + JSON.stringify(window.__r))".to_string()).unwrap();
        if let Some(l) = e.take_output().into_iter().rev().find(|l| l.starts_with("R:")) {
            got = serde_json::from_str(&l[2..]).unwrap_or_default();
        }
        if got.last().is_some_and(|l| l == "done") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(g, w, "line #{i}: korvus vs Chrome 152 (fixtures/webrtc/webrtc.chrome-152.txt)");
    }
    assert_eq!(got.len(), want.len(), "the page's lines");
    // what the STUN server saw: libwebrtc's Binding Requests -- 20 bytes, no attributes, a transaction id of
    // 12 base64-alphabet characters (measured from Chrome on the wire) -- one per transport
    let seen = seen.lock().unwrap();
    assert!(seen.len() >= 3, "a Binding Request per transport: {}", seen.len());
    for m in seen.iter() {
        assert_eq!(m.len(), 20);
        assert_eq!(&m[..8], &[0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42]);
        assert!(m[8..20].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/'), "{:?}", &m[8..20]);
    }
}

/// A STUN server answering every Binding Request after `delay` -- each answer on its own thread, so a burst of
/// requests is answered together -- with XOR-MAPPED-ADDRESS 203.0.113.9:<its port> (`record-pool.py`'s, for Chrome).
fn slow_stun_server(delay: Duration) -> u16 {
    let s = Arc::new(std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
    let port = s.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        while let Ok((n, from)) = s.recv_from(&mut buf) {
            if n < 20 || buf[0..2] != [0, 1] || buf[4..8] != [0x21, 0x12, 0xa4, 0x42] {
                continue;
            }
            let mut r = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
            r.extend_from_slice(&buf[8..20]);
            r.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
            r.extend_from_slice(&(from.port() ^ 0x2112).to_be_bytes());
            r.extend_from_slice(&[203 ^ 0x21, 0x12, 113 ^ 0xa4, 9 ^ 0x42]);
            let s = s.clone();
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                let _ = s.send_to(&r, from);
            });
        }
    });
    port
}

/// `iceCandidatePoolSize`: the pool gathers with the CONNECTION, and the description applied later reports what it
/// gathered at once -- while `iceGatheringState` still reads "new" -- before "gathering" and the other transports
/// (`fixtures/webrtc/pool.html`, one case per page; `pool.chrome-150.json` is Chrome 150's, recorded by
/// `record-pool.py` beside it, three runs per case alike). korvus gathered nothing before `setLocalDescription`, and
/// reported the gathering before that call's promise settled.
#[tokio::test]
async fn a_candidate_pool_gathers_with_the_connection_as_chromes_does() {
    let want: Vec<String> = serde_json::from_str(include_str!("fixtures/webrtc/pool.chrome-150.json")).expect("Chrome's lines");
    let port = slow_stun_server(Duration::from_millis(80));
    let page = include_str!("fixtures/webrtc/pool.html");
    let mut got = Vec::new();
    for case in ["0,1", "0,3", "1,1", "1,3"] {
        let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
        let mut e = Engine::new(Box::new(p));
        e.run().await.unwrap();
        e.load_page(page, &format!("http://127.0.0.1:8123/pool.html?stun={port}&case={case}"), &[]).await.expect("load_page");
        let start = Instant::now();
        let line = loop {
            let _ = tokio::time::timeout(Duration::from_millis(100), e.run()).await;
            e.eval("read", "print('R:' + (window.__r || ''))".to_string()).unwrap();
            if let Some(l) = e.take_output().into_iter().rev().find(|l| l.starts_with("R:") && l.len() > 2) {
                break l[2..].to_string();
            }
            assert!(start.elapsed() < Duration::from_secs(10), "case {case}: the page never answered");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        got.push(line);
    }
    assert_eq!(got, want, "korvus vs Chrome 150 (fixtures/webrtc/pool.chrome-150.json)");
}

/// The offer's audio and video sections are the WORN device's: each bundle's `webrtc-offer.json` is its
/// browser's `createOffer({offerToReceiveAudio: 1, offerToReceiveVideo: 1})`, measured 2026-10-07 on the four
/// installed browsers -- 200 lines on Chrome 150, 244 on Brave 151 and Edge 151 (RTCP XR, video payload types
/// numbered differently), 156 on Opera 135 (no H.264). The whole offer the engine writes for the same call --
/// its session header, BUNDLE group and mids, and the sections with its own values in them -- is that browser's,
/// line for line, once each side's session id, ICE credentials and fingerprint are blanked.
#[tokio::test]
async fn the_offer_is_the_worn_devices() {
    let root = persona::shipped_bundle_dir().parent().expect("the bundles directory").to_path_buf();
    let mut ids: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("manifest.toml").exists())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    ids.sort();
    assert!(!ids.is_empty(), "no persona bundle in the tree");
    let blank = |sdp: &str| -> Vec<String> {
        sdp.trim_end_matches("\r\n")
            .split("\r\n")
            .map(|l| match l.split_once(':') {
                _ if l.starts_with("o=- ") => format!("o=- SID {}", l.splitn(3, ' ').nth(2).unwrap_or("")),
                Some((k @ ("a=ice-ufrag" | "a=ice-pwd" | "a=fingerprint"), _)) => format!("{k}:BLANK"),
                _ => l.to_string(),
            })
            .collect()
    };
    for id in &ids {
        let p = persona::CapturedPersona::select(Some(id)).unwrap_or_else(|e| panic!("select {id}: {e}"));
        let file = std::fs::read_to_string(p.artifact("webrtc-offer")).unwrap();
        let want = blank(&persona::webrtc_offer::sdp_of(&file).expect("the measured offer"));
        let mut e = Engine::new(persona::worn_on_host(p));
        e.run().await.unwrap();
        e.load_page("<!doctype html><body>", "http://127.0.0.1:8123/offer.html", &[]).await.expect("load_page");
        e.eval(
            "offer",
            "globalThis.__o = null; new RTCPeerConnection().createOffer({offerToReceiveAudio: 1, offerToReceiveVideo: 1})\
             .then(o => { globalThis.__o = o.sdp; }, err => { globalThis.__o = 'REJECTED ' + err; })"
                .to_string(),
        )
        .unwrap();
        let start = Instant::now();
        let mut got: Option<String> = None;
        while got.is_none() && start.elapsed() < Duration::from_secs(5) {
            let _ = tokio::time::timeout(Duration::from_millis(50), e.run()).await;
            e.eval("read", "print('O:' + JSON.stringify(globalThis.__o))".to_string()).unwrap();
            if let Some(l) = e.take_output().into_iter().rev().find(|l| l.starts_with("O:")) {
                got = serde_json::from_str::<Option<String>>(&l[2..]).unwrap_or_default();
            }
        }
        let got = blank(&got.unwrap_or_else(|| panic!("{id}: createOffer never settled")));
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert_eq!(g, w, "{id}: line #{i} of the offer, korvus vs the browser (webrtc-offer.json)");
        }
        assert_eq!(got.len(), want.len(), "{id}: the offer's lines");
    }
}

/// `fixtures/webrtc/turn-server.py` as a thread, on the port `turn-probe.js` names (34782): an unauthenticated
/// request is answered 401 with REALM "korvus" and a NONCE; one carrying USERNAME "u" and MESSAGE-INTEGRITY is
/// answered success, signed with the key of password "p" -- an Allocate with XOR-RELAYED-ADDRESS
/// 198.51.100.7:<source port + 1>, XOR-MAPPED-ADDRESS 203.0.113.9:<source port> and LIFETIME 600 -- and a Binding
/// Request with the mapped address. The Allocates it saw are kept, as their attribute types.
fn turn_server() -> Arc<Mutex<Vec<Vec<u16>>>> {
    use hmac::Mac;
    const KEY: [u8; 16] = [224, 219, 61, 63, 167, 98, 148, 99, 120, 160, 178, 22, 169, 32, 94, 5]; // MD5("u:korvus:p")
    const MAGIC: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
    let s = std::net::UdpSocket::bind("127.0.0.1:34782").expect("the TURN fixture's port, 34782");
    let seen: Arc<Mutex<Vec<Vec<u16>>>> = Arc::default();
    let keep = seen.clone();
    fn attr(t: u16, v: &[u8]) -> Vec<u8> {
        let mut a = t.to_be_bytes().to_vec();
        a.extend_from_slice(&(v.len() as u16).to_be_bytes());
        a.extend_from_slice(v);
        a.resize(a.len() + (4 - v.len() % 4) % 4, 0);
        a
    }
    fn xaddr(t: u16, ip: [u8; 4], port: u16) -> Vec<u8> {
        let mut v = vec![0, 1];
        v.extend_from_slice(&(port ^ 0x2112).to_be_bytes());
        v.extend(ip.iter().zip(MAGIC.iter()).map(|(a, b)| a ^ b));
        attr(t, &v)
    }
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        while let Ok((n, from)) = s.recv_from(&mut buf) {
            let m = &buf[..n];
            if n < 20 || m[4..8] != MAGIC {
                continue;
            }
            let ty = u16::from_be_bytes([m[0], m[1]]);
            let (mut at, mut types) = (20, Vec::new());
            let end = (20 + u16::from_be_bytes([m[2], m[3]]) as usize).min(n);
            let mut user = Vec::new();
            while at + 4 <= end {
                let t = u16::from_be_bytes([m[at], m[at + 1]]);
                let l = u16::from_be_bytes([m[at + 2], m[at + 3]]) as usize;
                if t == 0x0006 {
                    user = m[at + 4..(at + 4 + l).min(n)].to_vec();
                }
                types.push(t);
                at += 4 + l.div_ceil(4) * 4;
            }
            let mapped = xaddr(0x0020, [203, 0, 113, 9], from.port());
            let (rty, body, signed) = match ty {
                0x0001 => (0x0101, mapped, false),
                0x0003 => {
                    keep.lock().unwrap().push(types.clone());
                    if !types.contains(&0x0008) || user != b"u" {
                        let mut e = attr(0x0009, &[0, 0, 4, 1, b'U', b'n', b'a', b'u', b't', b'h', b'o', b'r', b'i', b'z', b'e', b'd']);
                        e.extend(attr(0x0014, b"korvus"));
                        e.extend(attr(0x0015, b"0123456789abcdef"));
                        (0x0113, e, false)
                    } else {
                        let mut b = xaddr(0x0016, [198, 51, 100, 7], from.port().wrapping_add(1));
                        b.extend(mapped);
                        b.extend(attr(0x000D, &600u32.to_be_bytes()));
                        (0x0103, b, true)
                    }
                }
                _ => continue,
            };
            let mut body = body;
            let head = |len: usize| {
                let mut h = (rty as u16).to_be_bytes().to_vec();
                h.extend_from_slice(&(len as u16).to_be_bytes());
                h.extend_from_slice(&MAGIC);
                h.extend_from_slice(&m[8..20]);
                h
            };
            if signed {
                let mut mac = <hmac::Hmac<sha1::Sha1> as Mac>::new_from_slice(&KEY).unwrap();
                mac.update(&head(body.len() + 24));
                mac.update(&body);
                let tag = mac.finalize().into_bytes();
                body.extend(attr(0x0008, &tag));
            }
            let mut r = head(body.len());
            r.extend(body);
            let _ = s.send_to(&r, from);
        }
    });
    seen
}

/// ICE gathering through a TURN server, as Chrome 150 gathers it (BrowserScan N3): the relay
/// candidate, the server-reflexive one its Binding Request gives, a relay-only connection's single relay, and a
/// wrong credential's silence. `fixtures/webrtc/turn-chrome-150.json`: `turn-probe.js` on stock Chrome 150, headed
/// on the Wayland desktop, no DevTools, against `turn-server.py 34782`, three runs alike -- which also showed
/// Chrome's Allocates: the first with REQUESTED-TRANSPORT alone, the second with REQUESTED-TRANSPORT, USERNAME,
/// REALM, NONCE and MESSAGE-INTEGRITY, no FINGERPRINT.
#[tokio::test]
async fn a_turn_server_gives_the_relay_chrome_gathers() {
    let seen = turn_server();
    let script = include_str!("fixtures/webrtc/turn-probe.js");
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/webrtc/turn-chrome-150.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers_value(CHROME_BUNDLE, script).await;
    // the recorder writes every `:<2-5 digits>` as `:PORT` -- the server's port, and the first octet of 127.0.0.1
    let got: serde_json::Value = serde_json::from_str(&got.to_string().replace(":127.", ":PORT.").replace(":34782", ":PORT")).expect("json");
    assert_eq!(got, want, "korvus's candidates differ from Chrome 150's");
    let allocates = seen.lock().unwrap().clone();
    assert!(allocates.contains(&vec![0x0019]), "an Allocate with REQUESTED-TRANSPORT alone: {allocates:x?}");
    assert!(allocates.contains(&vec![0x0019, 0x0006, 0x0014, 0x0015, 0x0008]), "a signed Allocate as Chrome's: {allocates:x?}");
}
