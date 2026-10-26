//! The Web APIs the L14 findings tied to real page throws (causes C09, C11, C14, C15,
//! C17, C20), asked of the LIVE engine.
//!
//! EVERY EXPECTED STRING IS A BROWSER'S ANSWER, not this engine's: google-chrome 150.0.7871.46 on a
//! loopback HTTP page (`isSecureContext` true), driven over CDP by a launcher that adds no automation
//! flag, 2026-09-17. `serviceWorker.ready` (C15) and `FontFace` (C17) are asserted beside their
//! domains, in `service_worker_live.rs` and `font_face_live.rs`.
//!
//! C11 (`navigator.connection`) is PERSONA-SHAPED, so it is asserted per bundle against that bundle's
//! own capture rather than against one browser: the member exists exactly when the capture's
//! `Navigator` lists it, and reports exactly the capture's recorded estimate. The four installed
//! browsers answered (headless, same page): Chrome 150 / Edge 151 / Opera 135 `object`
//! (4g, rtt 100/100/200, downlink 1.5/1.45/1.55 -- estimates, which move between runs), Brave 151
//! `undefined` with `'connection' in navigator` false; `NetworkInformation` is a function in all four.

use engine::Engine;
use serde_json::Value;

fn bundles() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    out
}

fn bundle_json(id: &str, file: &str) -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles").join(id).join(file);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))).unwrap()
}

async fn page_with(p: persona::CapturedPersona) -> Engine {
    let mut e = Engine::new(persona::worn_on_host(p));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><head><title>t</title></head><body><ul id=\"l\"><li class=\"a\" n=\"1\">one</li><li class=\"b\" n=\"2\">two</li><li class=\"b\" n=\"3\">three</li></ul></body></html>").unwrap();
    e.run().await.unwrap();
    e
}

async fn page() -> Engine {
    page_with(persona::CapturedPersona::default()).await
}

/// Run an async script that ends in `print("RESULT:" + ...)` and return what it printed.
async fn result_of(e: &mut Engine, name: &'static str, script: &str) -> String {
    e.eval(name, script.to_string()).unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) || start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NONE (output: {out:?})"))
}

// ── C11: navigator.connection, per bundle ──────────────────────────────────────────────────────────

#[tokio::test]
async fn navigator_connection_follows_each_bundles_own_capture() {
    let ids = bundles();
    assert!(!ids.is_empty(), "no persona bundle in the tree");
    for id in ids {
        let surface = bundle_json(&id, "surface.json");
        let listed = surface["interfaces"]["Navigator"]["members"]
            .as_array()
            .map(|m| m.iter().any(|x| x.as_str() == Some("connection") || x["name"].as_str() == Some("connection")))
            .unwrap_or(false);
        let values = bundle_json(&id, "values.json");
        let conn = &values["nav"]["connection"];
        let mut e = page_with(persona::CapturedPersona::select(Some(&id)).unwrap()).await;
        let got = e
            .eval_value(
                "conn",
                r#"(() => {
                    const c = navigator.connection;
                    const base = [typeof c, 'connection' in navigator, typeof NetworkInformation];
                    if (!c) return JSON.stringify(base);
                    return JSON.stringify(base.concat([c.effectiveType, c.rtt, c.downlink, c.saveData,
                        String(c), c instanceof EventTarget, navigator.connection === c,
                        JSON.stringify(Object.getOwnPropertyNames(c)),
                        JSON.stringify(Object.getOwnPropertyNames(NetworkInformation.prototype)),
                        (() => { try { Object.getOwnPropertyDescriptor(NetworkInformation.prototype, "rtt").get.call({}); return "no throw"; } catch (err) { return err.name + ": " + err.message; } })()]));
                })()"#
                    .into(),
            )
            .unwrap();
        let got: Value = serde_json::from_str(got.as_str().unwrap()).unwrap();
        if !listed {
            assert!(conn.is_null(), "{id}: the capture lists no Navigator.connection but records a value");
            assert_eq!(got, serde_json::json!(["undefined", false, "function"]), "{id}");
            continue;
        }
        // `rtt` and `downlink` are Blink's noised estimate (`network_information.rs`): the
        // capture times one of Chrome's 21 per-host multipliers 0.90..1.10 (`GetRandomMultiplier`), rounded as
        // Blink rounds -- `RoundRtt` to the nearest 50 ms under 3 s, `RoundMbps` to the nearest 50 kbit/s under
        // 10 Mbit/s. The captured pair is itself one such reading, of the capture's host in its process.
        let multipliers: Vec<f64> = (0..21).map(|i| 0.9 + i as f64 * 0.01).collect();
        let (rtt0, dl0) = (conn["rtt"].as_f64().expect("a captured rtt"), conn["downlink"].as_f64().expect("a captured downlink"));
        // And since the captured pair is a READING, the estimate behind it is the desktop NQE default it is
        // a reading of (`network_information.rs` `connection_estimate`: ethernet 90 ms / 1.456, wifi 116 / 2.658,
        // unknown 115 / 1.961), when one is; the readings are that estimate's.
        let read = |rtt: f64, dl: f64, m: f64| {
            let us = (((rtt as i64 * 1000) as f64 * m) as i64).min(3_000_000);
            (((us + 25_000) / 50_000 * 50_000 / 1000) as f64, ((dl * 1000.0 * m).min(10_000.0) / 50.0).round() * 50.0 / 1000.0)
        };
        let (rtt0, dl0) = [(90.0, 1.456), (116.0, 2.658), (115.0, 1.961)]
            .into_iter()
            .find(|&(r, d)| multipliers.iter().any(|&m| {
                let (rr, dd) = read(r, d, m);
                rr == rtt0 && (dd - dl0).abs() < 1e-9
            }))
            .unwrap_or((rtt0, dl0));
        let rtt_readings: Vec<f64> = multipliers
            .iter()
            .map(|m| {
                let us = (((rtt0 as i64 * 1000) as f64 * m) as i64).min(3_000_000);
                ((us + 25_000) / 50_000 * 50_000 / 1000) as f64
            })
            .collect();
        let dl_readings: Vec<f64> = multipliers.iter().map(|m| ((dl0 * 1000.0 * m).min(10_000.0) / 50.0).round() * 50.0 / 1000.0).collect();
        let (rtt, dl) = (got[4].as_f64().unwrap_or(-1.0), got[5].as_f64().unwrap_or(-1.0));
        assert!(rtt_readings.contains(&rtt), "{id}: rtt {rtt} is no Blink reading of the captured {rtt0}: {rtt_readings:?}");
        assert!(dl_readings.iter().any(|d| (d - dl).abs() < 1e-9), "{id}: downlink {dl} is no Blink reading of the captured {dl0}: {dl_readings:?}");
        assert_eq!(
            got,
            serde_json::json!([
                "object", true, "function",
                conn["effectiveType"], got[4], got[5], conn["saveData"],
                "[object NetworkInformation]", true, true, "[]",
                r#"["onchange","effectiveType","rtt","downlink","saveData","constructor"]"#,
                "TypeError: Illegal invocation",
            ]),
            "{id}"
        );
    }
}

// ── C14: caches ────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn window_caches_is_a_cache_storage_that_round_trips() {
    let mut e = page().await;
    let got = result_of(
        &mut e,
        "caches",
        r#"(async () => {
            const out = [typeof caches, String(caches), caches instanceof CacheStorage,
                         JSON.stringify(Object.getOwnPropertyNames(caches))];
            const d = Object.getOwnPropertyDescriptor(globalThis, "caches");
            out.push([typeof d.value, d.writable, d.enumerable, d.configurable, typeof d.get, typeof d.set, d.get.name].join(","));
            out.push(caches === caches);
            const c = await caches.open("v1");
            await c.put("/x", new Response("hello"));
            out.push(await (await c.match("/x")).text());
            out.push(await caches.has("v1"), await caches.has("nope"));
            print("RESULT:" + out.join("|"));
        })()"#,
    )
    .await;
    assert_eq!(got, "object|[object CacheStorage]|true|[]|undefined,,true,true,function,undefined,get caches|true|hello|true|false");
}

// ── C20: PBKDF2 and HKDF ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pbkdf2_and_hkdf_import_and_derive_chromes_bytes_and_errors() {
    let mut e = page().await;
    let got = result_of(
        &mut e,
        "kdf",
        r#"(async () => {
            const E = new TextEncoder(); const pw = E.encode("password"); const S = crypto.subtle;
            const hex = (b) => Array.from(new Uint8Array(b)).map((x) => x.toString(16).padStart(2, "0")).join("");
            const o = [];
            const t = async (label, f) => {
                try {
                    const r = await f();
                    o.push(label + "=" + (r instanceof ArrayBuffer ? hex(r) : (r && r.usages ? r.usages.join(",") + "/" + JSON.stringify(r.algorithm) : String(r))));
                } catch (err) { o.push(label + "=" + err.name + ": " + err.message); }
            };
            const k = await S.importKey("raw", pw, "PBKDF2", false, ["deriveBits", "deriveKey"]);
            const h = await S.importKey("raw", E.encode("secret-key"), "HKDF", false, ["deriveBits", "deriveKey"]);
            o.push([String(k), k.type, k.extractable, k.usages.join(","), JSON.stringify(k.algorithm)].join(" "));
            await t("pbkdf2", () => S.deriveBits({ name: "PBKDF2", salt: E.encode("salt"), iterations: 1000, hash: "SHA-256" }, k, 256));
            await t("hkdf", () => S.deriveBits({ name: "HKDF", salt: E.encode("salt"), info: E.encode("info"), hash: "SHA-256" }, h, 256));
            await t("aes", async () => S.exportKey("raw", await S.deriveKey({ name: "PBKDF2", salt: E.encode("salt"), iterations: 2, hash: "SHA-1" }, k, { name: "AES-GCM", length: 128 }, true, ["encrypt"])));
            await t("pb-jwk", () => S.importKey("jwk", { kty: "oct", k: "cGFzc3dvcmQ" }, "PBKDF2", false, ["deriveBits"]));
            await t("pb-usage", () => S.importKey("raw", pw, "PBKDF2", false, ["sign"]));
            await t("pb-nousage", () => S.importKey("raw", pw, "PBKDF2", false, []));
            await t("hk-ext", () => S.importKey("raw", pw, "HKDF", true, ["deriveBits"]));
            await t("pb-export", () => S.exportKey("raw", k));
            await t("pb-len7", () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 1, hash: "SHA-256" }, k, 7));
            await t("pb-len0", () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 1, hash: "SHA-256" }, k, 0));
            await t("pb-lennull", () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 1, hash: "SHA-256" }, k, null));
            await t("pb-nohash", () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 1 }, k, 256));
            await t("pb-badhash", () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 1, hash: "MD5" }, k, 256));
            await t("pb-nosalt", () => S.deriveBits({ name: "PBKDF2", iterations: 1, hash: "SHA-256" }, k, 256));
            await t("pb-noiter", () => S.deriveBits({ name: "PBKDF2", salt: pw, hash: "SHA-256" }, k, 256));
            await t("pb-iter0", () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 0, hash: "SHA-256" }, k, 256));
            await t("hk-len7", () => S.deriveBits({ name: "HKDF", salt: pw, info: pw, hash: "SHA-256" }, h, 7));
            await t("hk-lennull", () => S.deriveBits({ name: "HKDF", salt: pw, info: pw, hash: "SHA-256" }, h, null));
            await t("hk-big", () => S.deriveBits({ name: "HKDF", salt: pw, info: pw, hash: "SHA-256" }, h, 255 * 256 + 8));
            await t("hk-noinfo", () => S.deriveBits({ name: "HKDF", salt: pw, hash: "SHA-256" }, h, 256));
            await t("hk-wrongalg", () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 1, hash: "SHA-256" }, h, 256));
            await t("dk-hmac", () => S.deriveKey({ name: "PBKDF2", salt: pw, iterations: 1, hash: "SHA-256" }, k, { name: "HMAC", hash: "SHA-384" }, true, ["sign"]));
            await t("dk-aes", () => S.deriveKey({ name: "HKDF", salt: pw, info: pw, hash: "SHA-256" }, h, { name: "AES-CBC", length: 256 }, false, ["decrypt", "encrypt"]));
            await t("dk-aesbad", () => S.deriveKey({ name: "HKDF", salt: pw, info: pw, hash: "SHA-256" }, h, { name: "AES-CBC", length: 100 }, false, ["encrypt"]));
            await t("dk-pb", () => S.deriveKey({ name: "HKDF", salt: pw, info: pw, hash: "SHA-256" }, h, "PBKDF2", false, ["deriveBits"]));
            await t("db-nousage", async () => S.deriveBits({ name: "PBKDF2", salt: pw, iterations: 1, hash: "SHA-256" }, await S.importKey("raw", pw, "PBKDF2", false, ["deriveKey"]), 256));
            print("RESULT:" + o.join("\n"));
        })()"#,
    )
    .await;
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            r#"[object CryptoKey] secret false deriveKey,deriveBits {"name":"PBKDF2"}"#,
            "pbkdf2=632c2812e46d4604102ba7618e9d6d7d2f8128f6266b4a03264d2a0460b7dcb3",
            "hkdf=274d210b61ee4e53a0b98935371805a5ace78d1d1a1481da5c5209f39b19b32f",
            "aes=ea6c014dc72d6f8ccd1ed92ace1d41f0",
            "pb-jwk=NotSupportedError: Unsupported import key format for algorithm",
            "pb-usage=SyntaxError: Cannot create a key using the specified key usages.",
            "pb-nousage=SyntaxError: Usages cannot be empty when creating a key.",
            "hk-ext=SyntaxError: KDF keys must set extractable=false",
            "pb-export=InvalidAccessError: Failed to execute 'exportKey' on 'SubtleCrypto': key is not extractable",
            "pb-len7=OperationError: Length for PBKDF2 key derivation must be a multiple of 8 bits.",
            "pb-len0=",
            "pb-lennull=OperationError: No length was specified for the PBKDF2 Derive Bits operation.",
            "pb-nohash=TypeError: Failed to execute 'deriveBits' on 'SubtleCrypto': Pbkdf2Params: hash: Missing or not an AlgorithmIdentifier",
            "pb-badhash=NotSupportedError: Failed to execute 'deriveBits' on 'SubtleCrypto': Pbkdf2Params: hash: Algorithm: Unrecognized name",
            "pb-nosalt=TypeError: Failed to execute 'deriveBits' on 'SubtleCrypto': Pbkdf2Params: salt: Missing required property",
            "pb-noiter=TypeError: Failed to execute 'deriveBits' on 'SubtleCrypto': Pbkdf2Params: iterations: Missing required property",
            "pb-iter0=OperationError: PBKDF2 requires iterations > 0",
            "hk-len7=OperationError: The length provided for HKDF is not a multiple of 8 bits.",
            "hk-lennull=OperationError: No length was specified for the HKDF Derive Bits operation.",
            "hk-big=OperationError: The length provided for HKDF is too large.",
            "hk-noinfo=TypeError: Failed to execute 'deriveBits' on 'SubtleCrypto': HkdfParams: info: Missing required property",
            "hk-wrongalg=InvalidAccessError: key.algorithm does not match that of operation",
            r#"dk-hmac=sign/{"name":"HMAC","hash":{"name":"SHA-384"},"length":1024}"#,
            r#"dk-aes=encrypt,decrypt/{"name":"AES-CBC","length":256}"#,
            "dk-aesbad=OperationError: AES key length must be 128 or 256 bits",
            "dk-pb=OperationError: No length was specified for the HKDF Derive Bits operation.",
            "db-nousage=InvalidAccessError: key.usages does not permit this operation",
        ]
    );
}

// ── C09: XPath ─────────────────────────────────────────────────────────────────────────────────────

/// The page the XPath answers were recorded on: `xp.html` served over loopback to google-chrome
/// 150.0.7871.46, byte for byte the same markup (its trailing newline is the `"\n"` text node after
/// `<ul>`, which several answers count).
const XPATH_PAGE: &str = "<!doctype html><html lang=\"en\"><head><title>xp</title></head><body><!--c1--><ul id=\"l\"><li class=\"a\" n=\"1\">one</li><li class=\"b\" n=\"2\">two</li><li class=\"b\" n=\"3\">three</li></ul>\n</body></html>";

/// Every probe is one script; every expected string is what Chrome returned for it (read back through
/// `Runtime.evaluate` with `returnByValue`). The scripts cover the WebIDL half (argument and context
/// errors, every result-type getter's refusal, iterator invalidation, resolvers, the three interface
/// objects) and the language (numbers, strings, every axis, predicates, name tests in an HTML document,
/// an XML document with namespaces, inline SVG, a detached context).
const XPATH_CASES: &[(&str, &str)] = &[
        (
            r#"(() => { const o = []; const t = (l, f) => { try { const r = f(); o.push(l + '=' + (typeof r === 'object' && r !== null ? Object.prototype.toString.call(r) : String(r))); } catch (e) { o.push(l + '=' + e.name + ': ' + e.message); } };
 const d = document;
 t('ev-noctx', () => d.evaluate('//li', null));
 t('ev-noargs', () => d.evaluate('//li'));
 t('ev-0args', () => d.evaluate());
 t('ev-strctx', () => d.evaluate('//li', 'x'));
 t('ev-frag', () => d.evaluate('.', d.createDocumentFragment()));
 t('ev-doctype', () => d.evaluate('.', d.doctype));
 t('ev-type10', () => d.evaluate('//li', d, null, 10, null));
 t('ev-type-1', () => d.evaluate('//li', d, null, -1, null));
 t('ev-ns', () => d.evaluate('//x:li', d, null, 0, null));
 t('ev-ns-fn', () => d.evaluate('//x:li', d, () => null, 0, null));
 t('ev-ns-ok', () => d.evaluate('count(//x:li)', d, (p) => 'http://www.w3.org/1999/xhtml', 0, null).numberValue);
 t('ev-ns-obj', () => d.evaluate('count(//x:li)', d, {lookupNamespaceURI: (p) => 'http://www.w3.org/1999/xhtml'}, 0, null).numberValue);
 t('ev-ns-node', () => d.evaluate('count(//x:li)', d, d.documentElement, 0, null).numberValue);
 t('ce-bad', () => d.createExpression('1 +'));
 t('xe-bad', () => new XPathEvaluator().createExpression('1 +'));
 t('xe-ev-bad', () => new XPathEvaluator().evaluate('1 +', d));
 t('ce-ns', () => d.createExpression('//x:li'));
 t('iter-num', () => d.evaluate('1', d, null, 4, null));
 t('snap-num', () => d.evaluate('1', d, null, 7, null));
 t('single-str', () => d.evaluate('"a"', d, null, 9, null));
 t('num-of-set', () => d.evaluate('//li', d, null, 1, null).numberValue);
 t('str-of-set', () => d.evaluate('//li', d, null, 2, null).stringValue);
 t('bool-of-set', () => d.evaluate('//li', d, null, 3, null).booleanValue);
 t('stringValue-of-num', () => d.evaluate('1', d, null, 0, null).stringValue);
 t('booleanValue-of-num', () => d.evaluate('1', d, null, 0, null).booleanValue);
 t('single-of-num', () => d.evaluate('1', d, null, 0, null).singleNodeValue);
 t('snapLen-of-num', () => d.evaluate('1', d, null, 0, null).snapshotLength);
 t('iterNext-of-num', () => d.evaluate('1', d, null, 0, null).iterateNext());
 t('snapItem-of-num', () => d.evaluate('1', d, null, 0, null).snapshotItem(0));
 t('snapItem-oob', () => d.evaluate('//li', d, null, 7, null).snapshotItem(9));
 t('iter-snapshotLength', () => d.evaluate('//li', d, null, 4, null).snapshotLength);
 t('iter-mutate', () => { const r = d.evaluate('//li', d, null, 5, null); r.iterateNext(); d.body.appendChild(d.createElement('p')); const s = r.invalidIteratorState; return s + ' ' + (() => { try { r.iterateNext(); return 'no'; } catch (e) { return e.name + ': ' + e.message; } })(); });
 t('iter-attrmutate', () => { const r = d.evaluate('//li', d, null, 5, null); d.body.setAttribute('data-x', '1'); return r.invalidIteratorState; });
 t('snap-mutate', () => { const r = d.evaluate('//li', d, null, 7, null); d.body.appendChild(d.createElement('p')); return r.invalidIteratorState + ' ' + r.snapshotLength; });
 t('resultType-any-set', () => d.evaluate('//li', d, null, 0, null).resultType);
 t('reuse-result', () => { const r1 = d.evaluate('//li', d, null, 7, null); const r2 = d.evaluate('count(//li)', d, null, 0, r1); return (r1 === r2) + ' ' + r2.resultType; });
 t('nsresolver', () => d.createNSResolver(d.body) === d.body);
 t('xe-nsresolver', () => new XPathEvaluator().createNSResolver(d.body) === d.body);
 t('expr-eval-noargs', () => d.createExpression('//li').evaluate());
 t('expr-eval-null', () => d.createExpression('//li').evaluate(null));
 t('expr-eval-ok', () => d.createExpression('count(//li)').evaluate(d).numberValue);
 t('expr-own', () => JSON.stringify(Object.getOwnPropertyNames(d.createExpression('//li'))) + JSON.stringify(Object.getOwnPropertyNames(d.evaluate('//li', d))) + JSON.stringify(Object.getOwnPropertyNames(new XPathEvaluator())));
 t('new-expr', () => new XPathExpression());
 t('new-result', () => new XPathResult());
 t('call-eval', () => XPathEvaluator());
 t('unknown-fn', () => d.evaluate('foo()', d, null, 0, null));
 t('arity', () => d.evaluate('concat("a")', d, null, 0, null));
 t('var', () => d.evaluate('$x', d, null, 0, null));
 t('pred-on-num', () => d.evaluate('1[1]', d, null, 0, null));
 t('path-on-num', () => d.evaluate('(1)/a', d, null, 0, null));
 t('union-num', () => d.evaluate('1 | //li', d, null, 0, null));
 t('attr-ctx', () => { const a = d.evaluate('//li/@class', d, null, 9, null).singleNodeValue; return a.name + '=' + a.value + ' ' + d.evaluate('string(..)', a, null, 2, null).stringValue + ' ' + d.evaluate('count(following::*)', a, null, 1, null).numberValue; });
 t('text-ctx', () => d.evaluate('name(..)', d.querySelector('li').firstChild, null, 2, null).stringValue);
 t('nums', () => ['1 div 0', '-1 div 0', '0 div 0', '1 div 3', '1e3', '1000000 * 1000000 * 1000000 * 1000', '0.0000001', '-0', '- 0.5', 'round(-0.5)', 'round(2.5)', 'round(-2.5)', 'number(" 12 ")', 'number("1 2")', 'number("1e2")', 'number(".5")', 'number("5.")', 'number("")', 'number("-.5")', '7 mod -3', '-7 mod 3', '5.5 mod 2'].map((x) => x + '->' + d.evaluate('string(' + x + ')', d, null, 2, null).stringValue).join(' ; '));
 t('strs', () => ['substring("12345", 1.5, 2.6)', 'substring("12345", 0, 3)', 'substring("12345", 0 div 0, 3)', 'substring("12345", -42, 1 div 0)', 'substring("12345", -1 div 0, 1 div 0)', 'translate("bar","abc","ABC")', 'translate("--aaa--","abc-","ABC")', 'normalize-space("  a   b ")', 'string-length("a\uD83D\uDE00")', 'substring-before("1999/04/01","/")', 'substring-after("1999/04/01","19")', 'name(//li)', 'local-name(//li)', 'namespace-uri(//li)', 'name(//li/@class)', 'name(/)', 'string(true())', 'string(//li/@n)', 'lang("en")', 'string(id("l")/li[2])', 'string(//ul/li[last()-1])', 'count(//li[@n > 1])', 'count(//*[@class = "b"])', 'string(//LI)', 'count(//html:li)', 'count(descendant::li)', 'count(//li/ancestor::*)', 'string(//li[3]/preceding-sibling::li[1])', 'string((//li[3]/preceding-sibling::li)[1])', 'count(//li/preceding::*)', 'count(/descendant::text())', 'string(//comment())', 'count(//node())', '//li[2] = "two"', '//li = //li', '//li != //li', '//li < 2', 'true() = "x"', '"1" = 1', '2 > "10"'].map((x) => x + '->' + (() => { try { return d.evaluate('string(' + x + ')', d, (p) => 'http://www.w3.org/1999/xhtml', 2, null).stringValue; } catch (e) { return e.name; } })()).join(' ; '));
 return o.join('\n'); })()"#,
            r#"ev-noctx=TypeError: Failed to execute 'evaluate' on 'Document': parameter 2 is not of type 'Node'.
ev-noargs=TypeError: Failed to execute 'evaluate' on 'Document': 2 arguments required, but only 1 present.
ev-0args=TypeError: Failed to execute 'evaluate' on 'Document': 2 arguments required, but only 0 present.
ev-strctx=TypeError: Failed to execute 'evaluate' on 'Document': parameter 2 is not of type 'Node'.
ev-frag=NotSupportedError: Failed to execute 'evaluate' on 'Document': The node provided is '#document-fragment', which is not a valid context node type.
ev-doctype=NotSupportedError: Failed to execute 'evaluate' on 'Document': The node provided is 'html', which is not a valid context node type.
ev-type10=[object XPathResult]
ev-type-1=[object XPathResult]
ev-ns=NamespaceError: Failed to execute 'evaluate' on 'Document': The string '//x:li' contains unresolvable namespaces.
ev-ns-fn=NamespaceError: Failed to execute 'evaluate' on 'Document': The string '//x:li' contains unresolvable namespaces.
ev-ns-ok=3
ev-ns-obj=3
ev-ns-node=NamespaceError: Failed to execute 'evaluate' on 'Document': The string 'count(//x:li)' contains unresolvable namespaces.
ce-bad=SyntaxError: Failed to execute 'createExpression' on 'Document': The string '1 +' is not a valid XPath expression.
xe-bad=SyntaxError: Failed to execute 'createExpression' on 'XPathEvaluator': The string '1 +' is not a valid XPath expression.
xe-ev-bad=SyntaxError: Failed to execute 'evaluate' on 'XPathEvaluator': The string '1 +' is not a valid XPath expression.
ce-ns=NamespaceError: Failed to execute 'createExpression' on 'Document': The string '//x:li' contains unresolvable namespaces.
iter-num=TypeError: Failed to execute 'evaluate' on 'Document': The result is not a node set, and therefore cannot be converted to the desired type.
snap-num=TypeError: Failed to execute 'evaluate' on 'Document': The result is not a node set, and therefore cannot be converted to the desired type.
single-str=TypeError: Failed to execute 'evaluate' on 'Document': The result is not a node set, and therefore cannot be converted to the desired type.
num-of-set=NaN
str-of-set=one
bool-of-set=true
stringValue-of-num=TypeError: Failed to read the 'stringValue' property from 'XPathResult': The result type is not a string.
booleanValue-of-num=TypeError: Failed to read the 'booleanValue' property from 'XPathResult': The result type is not a boolean.
single-of-num=TypeError: Failed to read the 'singleNodeValue' property from 'XPathResult': The result type is not a single node.
snapLen-of-num=TypeError: Failed to read the 'snapshotLength' property from 'XPathResult': The result type is not a snapshot.
iterNext-of-num=TypeError: Failed to execute 'iterateNext' on 'XPathResult': The result type is not an iterator.
snapItem-of-num=TypeError: Failed to execute 'snapshotItem' on 'XPathResult': The result type is not a snapshot.
snapItem-oob=null
iter-snapshotLength=TypeError: Failed to read the 'snapshotLength' property from 'XPathResult': The result type is not a snapshot.
iter-mutate=true InvalidStateError: Failed to execute 'iterateNext' on 'XPathResult': The document has mutated since the result was returned.
iter-attrmutate=true
snap-mutate=false 3
resultType-any-set=4
reuse-result=false 1
nsresolver=true
xe-nsresolver=true
expr-eval-noargs=TypeError: Failed to execute 'evaluate' on 'XPathExpression': 1 argument required, but only 0 present.
expr-eval-null=TypeError: Failed to execute 'evaluate' on 'XPathExpression': parameter 1 is not of type 'Node'.
expr-eval-ok=3
expr-own=[][][]
new-expr=TypeError: Failed to construct 'XPathExpression': Illegal constructor
new-result=TypeError: Failed to construct 'XPathResult': Illegal constructor
call-eval=TypeError: Failed to construct 'XPathEvaluator': Please use the 'new' operator, this DOM object constructor cannot be called as a function.
unknown-fn=SyntaxError: Failed to execute 'evaluate' on 'Document': The string 'foo()' is not a valid XPath expression.
arity=SyntaxError: Failed to execute 'evaluate' on 'Document': The string 'concat("a")' is not a valid XPath expression.
var=[object XPathResult]
pred-on-num=SyntaxError: Failed to execute 'evaluate' on 'Document': Type conversion failed while evaluating the expression.
path-on-num=SyntaxError: Failed to execute 'evaluate' on 'Document': Type conversion failed while evaluating the expression.
union-num=SyntaxError: Failed to execute 'evaluate' on 'Document': Type conversion failed while evaluating the expression.
attr-ctx=class=a one 4
text-ctx=li
nums=SyntaxError: Failed to execute 'evaluate' on 'Document': The string 'string(1e3)' is not a valid XPath expression.
strs=substring("12345", 1.5, 2.6)->234 ; substring("12345", 0, 3)->12 ; substring("12345", 0 div 0, 3)-> ; substring("12345", -42, 1 div 0)->12345 ; substring("12345", -1 div 0, 1 div 0)-> ; translate("bar","abc","ABC")->BAr ; translate("--aaa--","abc-","ABC")->AAA ; normalize-space("  a   b ")->a b ; string-length("a😀")->3 ; substring-before("1999/04/01","/")->1999 ; substring-after("1999/04/01","19")->99/04/01 ; name(//li)->li ; local-name(//li)->li ; namespace-uri(//li)->http://www.w3.org/1999/xhtml ; name(//li/@class)->class ; name(/)-> ; string(true())->true ; string(//li/@n)->1 ; lang("en")->false ; string(id("l")/li[2])->two ; string(//ul/li[last()-1])->two ; count(//li[@n > 1])->2 ; count(//*[@class = "b"])->2 ; string(//LI)->one ; count(//html:li)->3 ; count(descendant::li)->3 ; count(//li/ancestor::*)->3 ; string(//li[3]/preceding-sibling::li[1])->two ; string((//li[3]/preceding-sibling::li)[1])->one ; count(//li/preceding::*)->4 ; count(/descendant::text())->5 ; string(//comment())->c1 ; count(//node())->17 ; //li[2] = "two"->true ; //li = //li->true ; //li != //li->true ; //li < 2->false ; true() = "x"->true ; "1" = 1->true ; 2 > "10"->false"#,
        ),
        (
            r#"['1 div 0', '-1 div 0', '0 div 0', '1 div 3', '1000000 * 1000000 * 1000000 * 1000', '1000000 * 1000000 * 1000000 * 10000', '0.0000001', '0.000001', '-0', '- 0.5', 'round(-0.5)', 'round(2.5)', 'round(-2.5)', 'round(-0.2)', 'number(" 12 ")', 'number("1 2")', 'number("1e2")', 'number(".5")', 'number("5.")', 'number("")', 'number("-.5")', 'number("+5")', 'number(".")', '7 mod -3', '-7 mod 3', '5.5 mod 2', '123456789012345678', '0.1 + 0.2', '1.0', '-3.50', 'floor(-1.5)', 'ceiling(-0.5)', 'sum(//li/@n) div 7', '1 = 1.0', 'string(1 div 0 > 0)'].map((x) => x + '->' + (() => { try { return document.evaluate('string(' + x + ')', document, null, 2, null).stringValue; } catch (e) { return e.name; } })()).join(' ; ')"#,
            r#"1 div 0->Infinity ; -1 div 0->-Infinity ; 0 div 0->NaN ; 1 div 3->0.333333 ; 1000000 * 1000000 * 1000000 * 1000->1.00000e+21 ; 1000000 * 1000000 * 1000000 * 10000->1.00000e+22 ; 0.0000001->1.00000e-7 ; 0.000001->0.000001 ; -0->0 ; - 0.5->-0.5 ; round(-0.5)->0 ; round(2.5)->3 ; round(-2.5)->-2 ; round(-0.2)->0 ; number(" 12 ")->12 ; number("1 2")->NaN ; number("1e2")->NaN ; number(".5")->0.5 ; number("5.")->5 ; number("")->NaN ; number("-.5")->-0.5 ; number("+5")->NaN ; number(".")->NaN ; 7 mod -3->1 ; -7 mod 3->-1 ; 5.5 mod 2->1.5 ; 123456789012345678->1.23457e+17 ; 0.1 + 0.2->0.3 ; 1.0->1 ; -3.50->-3.5 ; floor(-1.5)->-2 ; ceiling(-0.5)->0 ; sum(//li/@n) div 7->0.857143 ; 1 = 1.0->true ; string(1 div 0 > 0)->true"#,
        ),
        (
            r#"['//li[2]/following::node()', '//li[1]/following-sibling::*', '//ul/preceding::node()', '/html/body/ul/li[1]/ancestor-or-self::node()', '//li[1]/@*', '//li[1]/attribute::n', 'id("l")', 'id("l x")', '//@*', '//text()', '//*[local-name()="LI"]', '//*[name()="li"]', '//li[position()=last()]', '(//li)[last()]', '//li[@class="b"][2]', '//li[@class="b"][position()=2]', '//li[.="two"]', '//li[contains(., "t")]', '//li[starts-with(@class,"a")]', '/descendant-or-self::node()/child::li[1]', '//li[1]', '(//li)[1]', '//ul/li[last()]/preceding-sibling::li[last()]', '/html/body/ul/..', '//li/self::li', '//li/self::node()[@n>2]', '.', '..', '/', '//body/comment()', '//processing-instruction()', '//node()[not(self::*)]'].map((x) => { try { const r = document.evaluate(x, document, null, 7, null); const o = []; for (let i = 0; i < r.snapshotLength; i++) { const n = r.snapshotItem(i); o.push(n.nodeType === 1 ? n.localName : n.nodeType === 2 ? '@' + n.name : n.nodeType === 3 ? '#' + JSON.stringify(n.data) : n.nodeName); } return x + '->' + o.join(','); } catch (e) { return x + '->' + e.name + ': ' + e.message; } }).join('\n')"#,
            r#"//li[2]/following::node()->li,#"three",#"\n"
//li[1]/following-sibling::*->li,li
//ul/preceding::node()->html,head,title,#"xp",#comment
/html/body/ul/li[1]/ancestor-or-self::node()->#document,html,body,ul,li
//li[1]/@*->@class,@n
//li[1]/attribute::n->@n
id("l")->ul
id("l x")->ul
//@*->@lang,@id,@class,@n,@class,@n,@class,@n
//text()->#"xp",#"one",#"two",#"three",#"\n"
//*[local-name()="LI"]->
//*[name()="li"]->li,li,li
//li[position()=last()]->li
(//li)[last()]->li
//li[@class="b"][2]->li
//li[@class="b"][position()=2]->li
//li[.="two"]->li
//li[contains(., "t")]->li,li
//li[starts-with(@class,"a")]->li
/descendant-or-self::node()/child::li[1]->li
//li[1]->li
(//li)[1]->li
//ul/li[last()]/preceding-sibling::li[last()]->li
/html/body/ul/..->body
//li/self::li->li,li,li
//li/self::node()[@n>2]->li
.->#document
..->
/->#document
//body/comment()->#comment
//processing-instruction()->
//node()[not(self::*)]->html,#"xp",#comment,#"one",#"two",#"three",#"\n""#,
        ),
        (
            r#"(() => { const doc = new DOMParser().parseFromString('<r xmlns:p="urn:p"><p:a x="1">t</p:a><A/><a/><b xmlns="urn:d"><c/></b></r>', 'application/xml'); const q = (x, res) => { try { const r = doc.evaluate(x, doc, res || null, 7, null); const o = []; for (let i = 0; i < r.snapshotLength; i++) o.push(r.snapshotItem(i).nodeName); return x + '->' + o.join(','); } catch (e) { return x + '->' + e.name; } }; const R = (p) => ({p: 'urn:p', d: 'urn:d'})[p] || null; return [q('//a'), q('//A'), q('//p:a', R), q('//d:c', R), q('//c'), q('//*'), q('name(//p:a)', R), q('//p:*', R)].join(' ; ') + ' | ' + ['name(//*[2])', 'local-name(//*[2])', 'namespace-uri(//*[2])', 'name(/r)'].map((x) => doc.evaluate('string(' + x + ')', doc, null, 2, null).stringValue).join(',') })()"#,
            r#"//a->a ; //A->A ; //p:a->p:a ; //d:c->c ; //c-> ; //*->r,p:a,A,a,b,c ; name(//p:a)->TypeError ; //p:*->p:a | A,A,,r"#,
        ),
        (
            r#"(() => { const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); s.appendChild(document.createElementNS('http://www.w3.org/2000/svg', 'rect')); document.body.appendChild(s); const c = (x, r) => { try { return x + '->' + document.evaluate('count(' + x + ')', document, r || null, 1, null).numberValue; } catch (e) { return x + '->' + e.name; } }; const out = [c('//svg'), c('//rect'), c('//*[local-name()="rect"]'), c('//s:rect', () => 'http://www.w3.org/2000/svg'), c('//h:body', () => 'http://www.w3.org/1999/xhtml'), c('//BODY'), c('//body/@*'), c('//*[@CLASS]'), c('//li[@class]')]; s.remove(); const f = document.createDocumentFragment(); const d = document.createElement('div'); d.innerHTML = '<p>a</p><p>b</p>'; f.appendChild(d); out.push('detached:' + document.evaluate('count(.//p)', d, null, 1, null).numberValue + ',' + document.evaluate('count(//p)', d, null, 1, null).numberValue + ',' + document.evaluate('name(/*)', d, null, 2, null).stringValue); return out.join(' ; '); })()"#,
            r#"//svg->0 ; //rect->0 ; //*[local-name()="rect"]->1 ; //s:rect->1 ; //h:body->1 ; //BODY->1 ; //body/@*->0 ; //*[@CLASS]->3 ; //li[@class]->3 ; detached:2,2,div"#,
        ),
];

#[tokio::test]
async fn xpath_answers_every_probe_the_way_chrome_150_did() {
    let mut failures = Vec::new();
    for (i, (script, want)) in XPATH_CASES.iter().enumerate() {
        // A fresh page per probe: Chrome answered each on a page no earlier probe had mutated.
        let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
        e.run().await.unwrap();
        e.load_html(XPATH_PAGE).unwrap();
        e.run().await.unwrap();
        let got = e.eval_value("xpath-probe", format!("(() => {{ const v = ({script}); return typeof v === 'string' ? v : JSON.stringify(v); }})()")).unwrap();
        let got = got.as_str().unwrap_or("<not a string>").to_string();
        let want_s = want.to_string();
        if got != want_s {
            failures.push(format!("probe {i}:\n  script {script}\n  chrome {want_s}\n  korvus {got}"));
        }
    }
    assert!(failures.is_empty(), "{} of {} XPath probes differ from Chrome:\n{}", failures.len(), XPATH_CASES.len(), failures.join("\n"));
}
