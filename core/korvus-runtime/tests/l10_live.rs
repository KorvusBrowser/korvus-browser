//! LIVE one-table gate for the ten crypto / messaging / cookie / XHR interfaces.
//!
//! Boots the real engine and asserts, for `CryptoKey`, `SubtleCrypto`, `FileReader`, `MessagePort`,
//! `BroadcastChannel`, `CookieStore`, `XMLHttpRequest`, `XMLHttpRequestEventTarget` and
//! `XMLHttpRequestUpload` (plus the one `Crypto` row this lane owns, `subtle`):
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the captured bundle's member
//!      list for that interface, in the capture's order, with the capture's descriptors, followed by
//!      the non-enumerable `constructor` -- the hand-table shape.
//!   2. **The chains, the interface objects and newability.** `FileReader`/`BroadcastChannel`/
//!      `XMLHttpRequest` construct; the other six throw "Illegal constructor"; the WebIDL splices
//!      (prototype chain AND constructor chain) are the installer's.
//!   3. **A receiver check on every member** the tables own: `Object.create(Iface.prototype)` gets a
//!      TypeError where the deleted JS answered `undefined`.
//!   4. **Real behaviour through the real dispatch machinery**: a SHA-256 digest, a `FileReader`
//!      `readAsText` round trip delivered through `onload`, a `MessageChannel` pair delivering, two
//!      `BroadcastChannel`s delivering, `cookieStore.set`/`get` with `onchange`, and an XHR
//!      `open`/`send` against a loopback server with readystatechange/load/loadend.
//!   5. **The WORKER realm owns the same tables** (invariant I8) for the seven the worker capture
//!      lists.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The nine this lane's tables own in the WINDOW, in the order the installer builds them.
const IFACES: &[&str] = &[
    "CryptoKey",
    "SubtleCrypto",
    "MessagePort",
    "XMLHttpRequestEventTarget",
    "XMLHttpRequestUpload",
    "FileReader",
    "BroadcastChannel",
    "CookieStore",
    "XMLHttpRequest",
];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        // A WebIDL constant: non-writable, non-configurable, enumerable.
        "value" => format!(
            "{n}:data:e={}:c={}:w=0",
            m["enumerable"].as_bool().unwrap() as u8,
            m["configurable"].as_bool().unwrap() as u8
        ),
        k => panic!("unexpected kind {k}"),
    }
}

/// The `describe_own` lines the capture's member list implies, with `constructor` at its recorded seat.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx = bundle();
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the bundle surface"))
        .clone();
    let mut out: Vec<String> = members.iter().map(describe_line).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// Pump the event loop until `globalThis.__done` is true (the async cases: dispatch runs on
/// setTimeout/microtask tasks), then read `globalThis.__out`.
async fn pump_for(e: &mut Engine, name: &'static str, script: &str) -> String {
    e.eval(name, script.to_string()).unwrap();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        let done = e
            .eval_value("l10-done", "!!globalThis.__done".to_string())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if done || start.elapsed() > std::time::Duration::from_secs(15) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    str_of(e, "l10-out", "String(globalThis.__out)".to_string())
}

// ── 1. the nine prototypes, byte-identical to the capture ───────────────────────────────────────

#[tokio::test]
async fn every_l10_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l10-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l10_interface_object_carries_the_capture_tag_length_and_statics() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{ {DESCRIBE}
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, statics.join(",")].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l10-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        let want_statics: Vec<&str> = fx["interfaces"][iface]["statics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            got,
            format!("{iface}|0|0|1|{want_len}|{iface}|{}", want_statics.join(",")),
            "{iface} interface object != the capture"
        );
    }
    // The five XHR constants live on BOTH holders, with the same values and the same descriptor.
    let got = str_of(
        &mut e,
        "l10-consts",
        r#"(() => {
            const names = ["UNSENT","OPENED","HEADERS_RECEIVED","LOADING","DONE"];
            const one = (h) => names.map(n => {
                const d = Object.getOwnPropertyDescriptor(h, n);
                return n + "=" + d.value + ":e=" + (+d.enumerable) + ":c=" + (+d.configurable) + ":w=" + (+d.writable);
            }).join(",");
            return one(XMLHttpRequest) + "|" + one(XMLHttpRequest.prototype) + "|" + (XMLHttpRequest.DONE === 4) + "|" + (XMLHttpRequest.prototype.DONE === 4);
        })()"#
            .to_string(),
    );
    let want_row = "UNSENT=0:e=1:c=0:w=0,OPENED=1:e=1:c=0:w=0,HEADERS_RECEIVED=2:e=1:c=0:w=0,LOADING=3:e=1:c=0:w=0,DONE=4:e=1:c=0:w=0";
    assert_eq!(got, format!("{want_row}|{want_row}|true|true"), "the XHR constants are not on both holders");
}

// ── 2. the chains, the interface objects, newability ────────────────────────────────────────────

#[tokio::test]
async fn the_chains_and_the_interface_objects_are_chromes() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l10-chains",
        r#"(() => {
            const P = Object.getPrototypeOf;
            const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } };
            const native = (f) => Function.prototype.toString.call(f).includes("[native code]");
            return [
                P(MessagePort.prototype) === EventTarget.prototype,
                P(FileReader.prototype) === EventTarget.prototype,
                P(BroadcastChannel.prototype) === EventTarget.prototype,
                P(CookieStore.prototype) === EventTarget.prototype,
                P(XMLHttpRequestEventTarget.prototype) === EventTarget.prototype,
                P(XMLHttpRequestUpload.prototype) === XMLHttpRequestEventTarget.prototype,
                P(XMLHttpRequest.prototype) === XMLHttpRequestEventTarget.prototype,
                P(MessagePort) === EventTarget,
                P(XMLHttpRequest) === XMLHttpRequestEventTarget,
                P(XMLHttpRequestUpload) === XMLHttpRequestEventTarget,
                throws(() => new CryptoKey()),
                throws(() => new SubtleCrypto()),
                throws(() => new MessagePort()),
                throws(() => new CookieStore()),
                throws(() => new XMLHttpRequestEventTarget()),
                throws(() => new XMLHttpRequestUpload()),
                throws(() => new BroadcastChannel()),
                Object.prototype.toString.call(new FileReader()),
                Object.prototype.toString.call(new BroadcastChannel("x")),
                Object.prototype.toString.call(new XMLHttpRequest()),
                ["CryptoKey","SubtleCrypto","MessagePort","XMLHttpRequestEventTarget","XMLHttpRequestUpload",
                 "FileReader","BroadcastChannel","CookieStore","XMLHttpRequest"].every(n => native(globalThis[n])),
                cookieStore instanceof CookieStore,
                Object.prototype.toString.call(cookieStore),
                new XMLHttpRequest().upload instanceof XMLHttpRequestUpload,
                Object.prototype.toString.call(new XMLHttpRequest().upload),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        // The six non-newable ones carry Chrome's construction context, per interface --
        // which is what made `BroadcastChannel`'s prefix look like an exception rather than the rule.
        "true|true|true|true|true|true|true|true|true|true|\
TypeError:Failed to construct 'CryptoKey': Illegal constructor|\
TypeError:Failed to construct 'SubtleCrypto': Illegal constructor|\
TypeError:Failed to construct 'MessagePort': Illegal constructor|\
TypeError:Failed to construct 'CookieStore': Illegal constructor|\
TypeError:Failed to construct 'XMLHttpRequestEventTarget': Illegal constructor|\
TypeError:Failed to construct 'XMLHttpRequestUpload': Illegal constructor|\
TypeError:Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present.|\
[object FileReader]|[object BroadcastChannel]|[object XMLHttpRequest]|true|\
true|[object CookieStore]|true|[object XMLHttpRequestUpload]",
        "the L10 chains / interface objects are not Chrome's"
    );
}

// ── 3. receiver checks ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_table_owned_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    // The bodies the tables mount unwrap a cppgc receiver; before the cutover they answered
    // `undefined` (or silently did nothing) for any object at all. The `on*` handler rows are the
    // shared `event_handler` kit, which cannot brand-check -- they are excluded, as they are on every
    // other interface in the tree (W4.b's lane owns the kit).
    //
    // A REFUSAL IS NOT ALWAYS A THROW, and this probe used to assume it was. It calls every method
    // with three arguments, so for a member that needs more the ARGUMENT COUNT answers before the
    // receiver does -- and for a promise-returning member WebIDL delivers that refusal as a
    // REJECTION. Five rows land there (`SubtleCrypto.deriveKey`/`importKey`/`unwrapKey`/`verify`/
    // `wrapKey`, which need 5/5/7/4/4). Measured in Google Chrome 150.0.7871.46 `--headless=new`,
    // 2026-09-23, awaiting each promise: all five REJECT `TypeError`, none throws and none resolves.
    //
    // So a thenable is not scored here; it is stashed and AWAITED below. A promise that RESOLVES is
    // reported as the non-refusal it is, which is the hole HARNESS8 closed in `receiver_sweep.rs`
    // and the one three separate artefacts in this project fell into in one night.
    e.eval(
        "l10-recv",
        r#"(() => {
            globalThis.__bad = [];
            globalThis.__pending = [];
            const bad = globalThis.__bad;
            const probe = (label, f) => {
                let v;
                try { v = f(); }
                catch (e) { if (!(e instanceof TypeError)) bad.push(label + ":" + e.name); return; }
                if (v && typeof v.then === "function") { globalThis.__pending.push([label, v]); return; }
                bad.push(label + ":no-throw");
            };
            const IF = { CryptoKey, SubtleCrypto, MessagePort, FileReader, BroadcastChannel, CookieStore, XMLHttpRequest };
            for (const name in IF) {
                const P = IF[name].prototype, foreign = Object.create(P);
                for (const m of Object.getOwnPropertyNames(P)) {
                    if (m === "constructor" || /^on[a-z]+$/.test(m)) continue;
                    const d = Object.getOwnPropertyDescriptor(P, m);
                    if (d.get) probe(name + "." + m + ".get", () => d.get.call(foreign));
                    if (d.set) probe(name + "." + m + ".set", () => d.set.call(foreign, 1));
                    if (typeof d.value === "function") probe(name + "." + m + "()", () => d.value.call(foreign, "x", "y", "z"));
                }
            }
        })()"#
            .to_string(),
    )
    .unwrap();
    // The settlement, through a real microtask checkpoint. A rejected `TypeError` is a refusal; a
    // resolve, or a rejection with some other error, is not, and is named.
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        (async () => {
            const p = globalThis.__pending;
            const settled = await Promise.allSettled(p.map(x => x[1]));
            p.forEach((x, i) => {
                const s = settled[i];
                if (s.status !== "rejected") globalThis.__bad.push(x[0] + ":RESOLVED:" + Object.prototype.toString.call(s.value));
                else if (!(s.reason instanceof TypeError)) globalThis.__bad.push(x[0] + ":rejected-with-" + (s.reason && s.reason.name));
            });
            globalThis.__out = globalThis.__bad.join(",") + "|awaited=" + p.length;
            globalThis.__done = true;
        })();
    "#;
    let got = pump_for(&mut e, "l10-recv-settle", script).await;
    // 16 (was 5): the receiver check is the brand, which `Object.create(P)` lacks, and a
    // promise member's refusal is a REJECTION there (WebIDL; Chrome's `Failed to execute ...: Illegal
    // invocation`) -- SubtleCrypto's twelve methods, which used to throw from their bodies, now reject at the
    // check, with CookieStore's four.
    assert_eq!(
        got, "|awaited=16",
        "a member answered instead of refusing for a foreign receiver (the tail is how many refusals \
         came back as a promise and were awaited -- if that number moves, say why)"
    );
}

// ── 4. crypto.subtle ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn crypto_subtle_is_an_accessor_on_the_crypto_prototype() {
    let mut e = page().await;
    // The capture records `Crypto.prototype.subtle` as `{get, enumerable, configurable}` with a
    // `[native code]` getter named "get subtle" of length 0; `dom/08f-crypto.js` wrote it as an own
    // DATA property of the `crypto` object instead.
    let got = str_of(
        &mut e,
        "l10-subtle",
        r#"(() => {
            const d = Object.getOwnPropertyDescriptor(Crypto.prototype, "subtle");
            return [
                !!d.get, !!d.set, d.enumerable, d.configurable,
                d.get.name, d.get.length,
                Function.prototype.toString.call(d.get).includes("[native code]"),
                Object.prototype.hasOwnProperty.call(crypto, "subtle"),
                crypto.subtle instanceof SubtleCrypto,
                Object.prototype.toString.call(crypto.subtle),
                crypto.subtle === crypto.subtle,
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|false|true|true|get subtle|0|true|false|true|[object SubtleCrypto]|true",
        "crypto.subtle is not the capture's accessor on Crypto.prototype"
    );
}

#[tokio::test]
async fn subtle_crypto_digests_and_refuses_a_bare_call() {
    let mut e = page().await;
    // SHA-256 of "abc" (FIPS 180-4's own test vector).
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        crypto.subtle.digest("SHA-256", new TextEncoder().encode("abc")).then((buf) => {
            globalThis.__out = Array.from(new Uint8Array(buf)).map(b => b.toString(16).padStart(2, "0")).join("");
            globalThis.__done = true;
        }, (err) => { globalThis.__out = "REJECTED:" + err; globalThis.__done = true; });
    "#;
    let got = pump_for(&mut e, "l10-digest", script).await;
    assert_eq!(got, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "SubtleCrypto.digest");

    // A zero-argument call is the arity guard's TypeError -- REJECTED, not thrown, because `digest`
    // returns a promise and WebIDL delivers a refusal out of a promise-returning operation as a
    // rejection of the promise it already committed to returning.
    //
    // This test asserted a synchronous THROW until TY2, and it was wrong in the one direction a
    // `.then`-free probe cannot see. Measured, Google Chrome 150.0.7871.46 `--headless=new` over a
    // loopback server, 2026-09-23, AWAITING the promise:
    //
    //   crypto.subtle.digest()      REJECTED TypeError: Failed to execute 'digest' on
    //                                 'SubtleCrypto': 2 arguments required, but only 0 present.
    //   document.getElementById()   THREW    <- the control in the same probe, so it can see a throw
    //
    // `crypto.subtle.digest()` is the headline case ARITY1's own brief named and it still threw,
    // because this row's `ty` was `Object` (TY2 fixed the column; this fixes the test that pinned the
    // wrong answer). The sentence is asserted whole, because a page can read `err.message`.
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        let r;
        try { r = crypto.subtle.digest(); }
        catch (e) { globalThis.__out = "THREW:" + e.constructor.name + ":" + e.message; globalThis.__done = true; }
        if (globalThis.__out === "pending") {
            if (!r || typeof r.then !== "function") {
                globalThis.__out = "NOT-A-PROMISE:" + Object.prototype.toString.call(r); globalThis.__done = true;
            } else {
                r.then(v  => { globalThis.__out = "RESOLVED:" + Object.prototype.toString.call(v); globalThis.__done = true; },
                       err => { globalThis.__out = "REJECTED:" + err.constructor.name + ":" + err.message; globalThis.__done = true; });
            }
        }
    "#;
    let got = pump_for(&mut e, "l10-arity", script).await;
    assert_eq!(
        got,
        "REJECTED:TypeError:Failed to execute 'digest' on 'SubtleCrypto': 2 arguments required, but only 0 present.",
        "a zero-argument crypto.subtle.digest() must REJECT the arity TypeError, not throw it and not resolve"
    );

    // THE CONTROL, in the same test: a NON-promise member with a required argument still THROWS the
    // same sentence synchronously. Without it, a guard that rejected everything would pass above.
    let got = str_of(
        &mut e,
        "l10-arity-control",
        r#"(() => { try { document.getElementById(); return "no-throw"; }
                    catch (e) { return "throw:" + e.constructor.name + ":" + e.message; } })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "throw:TypeError:Failed to execute 'getElementById' on 'Document': 1 argument required, but only 0 present.",
        "the control: a non-promise arity refusal is still a synchronous throw"
    );
}

// ── 5. FileReader, MessageChannel, BroadcastChannel, cookieStore ────────────────────────────────

#[tokio::test]
async fn file_reader_reads_a_blob_and_delivers_onload_through_dispatch() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const fr = new FileReader();
        const seen = [];
        fr.addEventListener("loadstart", () => seen.push("loadstart"));
        fr.onload = function (ev) {
            seen.push("onload:" + (ev.target === fr) + ":" + fr.readyState + ":" + fr.result);
        };
        fr.onloadend = () => { globalThis.__out = seen.join(","); globalThis.__done = true; };
        fr.readAsText(new Blob(["hello l10"]));
    "#;
    let got = pump_for(&mut e, "l10-fr", script).await;
    assert_eq!(got, "loadstart,onload:true:2:hello l10", "FileReader round trip");
}

#[tokio::test]
async fn a_message_channel_pair_delivers() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const mc = new MessageChannel();
        const shapes = [
            Object.prototype.toString.call(mc.port1),
            mc.port1 instanceof MessagePort,
            mc.port1 !== mc.port2,
        ].join(":");
        mc.port2.onmessage = (ev) => { globalThis.__out = shapes + "|" + ev.data + "|" + (ev instanceof MessageEvent); globalThis.__done = true; };
        mc.port1.postMessage({ a: 1 });
    "#;
    let got = pump_for(&mut e, "l10-mc", script).await;
    assert_eq!(got, "[object MessagePort]:true:true|[object Object]|true", "MessageChannel delivery");
}

#[tokio::test]
async fn two_broadcast_channels_deliver() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const a = new BroadcastChannel("l10");
        const b = new BroadcastChannel("l10");
        b.onmessage = (ev) => { globalThis.__out = [a.name, b.name, ev.data].join("|"); globalThis.__done = true; };
        a.postMessage("ping");
    "#;
    let got = pump_for(&mut e, "l10-bc", script).await;
    assert_eq!(got, "l10|l10|ping", "BroadcastChannel delivery");
}

#[tokio::test]
async fn cookie_store_sets_gets_and_fires_onchange() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const seen = [];
        cookieStore.onchange = (ev) => seen.push("change:" + (ev.changed ? ev.changed.length : -1));
        cookieStore.set("l10cookie", "yes").then(() => cookieStore.get("l10cookie")).then((c) => {
            // `onchange` is delivered on a task the mutation scheduled, so read it a turn later --
            // the promise chain above is microtasks and would otherwise win the race.
            setTimeout(() => {
                globalThis.__out = [c ? c.name : "none", c ? c.value : "none", seen.join(",")].join("|");
                globalThis.__done = true;
            }, 0);
        }, (err) => { globalThis.__out = "REJECTED:" + err; globalThis.__done = true; });
    "#;
    let got = pump_for(&mut e, "l10-cs", script).await;
    assert_eq!(got, "l10cookie|yes|change:1", "cookieStore set/get/onchange");
}

// ── 6. XMLHttpRequest against a loopback server ─────────────────────────────────────────────────

/// A one-shot loopback HTTP server: answers every request with `body` and closes. Returns its port.
fn loopback(body: &'static str, hits: usize) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for _ in 0..hits {
            let Ok((mut sock, _)) = listener.accept() else { return };
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
        }
    });
    port
}

#[tokio::test]
async fn xhr_open_send_drives_the_event_model_against_a_loopback_server() {
    let port = loopback("l10-xhr-body", 4);
    let mut e = page().await;
    let script = format!(
        r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const x = new XMLHttpRequest();
        const seen = [];
        seen.push("state0:" + x.readyState);
        seen.push("upload:" + (x.upload instanceof XMLHttpRequestUpload));
        x.onreadystatechange = function () {{ if (x.readyState === 4) seen.push("rsc4"); }};
        x.addEventListener("load", () => seen.push("load:" + x.status + ":" + x.responseText));
        x.onloadend = () => {{ globalThis.__out = seen.join(","); globalThis.__done = true; }};
        x.open("GET", "http://127.0.0.1:{port}/l10");
        seen.push("state1:" + x.readyState);
        x.send();
    "#
    );
    let got = pump_for(&mut e, "l10-xhr", &script).await;
    assert_eq!(
        got, "state0:0,upload:true,state1:1,rsc4,load:200:l10-xhr-body",
        "the XHR event model over a loopback server"
    );
}

// ── 7. the worker realm ─────────────────────────────────────────────────────────────────────────

async fn worker_probe(worker_body: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document(PAGE).unwrap();
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
async fn the_worker_realm_owns_the_same_tables() {
    // The worker's seven come from the SAME tables (invariant I8): same prototypes, same member order,
    // the same chains, the same "Illegal constructor" and the same real bodies. `CookieStore` and
    // `cookieStore` are NOT there -- `worker.surface.json` has neither, and the bundle is the
    // denominator. `XMLHttpRequest` is still `worker_js_owned` (worker-native.js) until W6, but its
    // BASE is this lane's table and the JS-owned prototype is re-chained onto it.
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return (d.get || d.set) ? `${n}:a:s=${+!!d.set}` : `${n}:d:w=${+d.writable}`;
        }).join(",");
        const names = ["CryptoKey","SubtleCrypto","MessagePort","BroadcastChannel","FileReader",
                       "XMLHttpRequestEventTarget","XMLHttpRequestUpload"];
        const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name; } };
        const parts = [
            names.every((n) => typeof globalThis[n] === "function"),
            names.every((n) => globalThis[n].prototype[Symbol.toStringTag] === n),
            desc(MessagePort.prototype),
            desc(CryptoKey.prototype),
            Object.getPrototypeOf(MessagePort.prototype) === EventTarget.prototype,
            Object.getPrototypeOf(FileReader.prototype) === EventTarget.prototype,
            Object.getPrototypeOf(XMLHttpRequestUpload.prototype) === XMLHttpRequestEventTarget.prototype,
            Object.getPrototypeOf(XMLHttpRequest.prototype) === XMLHttpRequestEventTarget.prototype,
            throws(() => new MessagePort()),
            Object.prototype.toString.call(new FileReader()),
            Object.prototype.toString.call(new BroadcastChannel("w")) + ":" + new BroadcastChannel("w").name,
            Object.prototype.toString.call(crypto.subtle),
            typeof globalThis.cookieStore,
            typeof globalThis.CookieStore,
            Object.prototype.toString.call(new MessageChannel().port1),
        ];
        self.postMessage(parts.join("|"));
    "#;
    let got = worker_probe(body).await;
    let mp = "onmessage:a:s=1,onmessageerror:a:s=1,close:d:w=1,postMessage:d:w=1,start:d:w=1,constructor:d:w=1";
    let ck = "type:a:s=0,extractable:a:s=0,algorithm:a:s=0,usages:a:s=0,constructor:d:w=1";
    assert_eq!(
        got,
        format!("true|true|{mp}|{ck}|true|true|true|true|TypeError|[object FileReader]|[object BroadcastChannel]:w|[object SubtleCrypto]|undefined|undefined|[object MessagePort]"),
        "the worker realm's L10 interfaces are not the window's tables"
    );
}
