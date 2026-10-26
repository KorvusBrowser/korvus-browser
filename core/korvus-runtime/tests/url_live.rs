//! LIVE one-table gate for `URL` and `URLSearchParams`, in BOTH realms.
//!
//! Every expected answer below was measured against headless Chrome 150.0.7871.46 over a loopback
//! server on 2026-09-11 by running THIS EXACT `PROBE` string in a page (`file://` is refused), never
//! copied from this engine. The two prototype shapes come from the shipped capture bundle, the same
//! derivation `dom_rect_live.rs` uses.
//!
//! What this gate is for. The lane collapsed FOUR definition sites into two `interface!` tables in
//! `core/korvus-webapi/src/modules/url/`, mounted by `install::live::install_window_url` /
//! `install_worker_url`: the cppgc class `JsUrl` in the KERNEL, its `Proxy`/statics/blob-Map block in
//! `korvus-runtime/src/js/host.js`, the four statics' bodies in `korvus-runtime/src/natives.rs`, and
//! `dom/08g-observers.js`'s URL + URLSearchParams blocks. **There was no behaviour to fix** -- probes
//! `p01`..`p12` are the brief's twelve, and all twelve answered exactly this before the lane -- so the
//! gate exists to catch regression, plus the shape and reach the lane DID move:
//!
//!   * `constructor` now enumerates LAST on both prototypes (the cppgc class templates put it FIRST)
//!     and the interface objects carry the capture's WebIDL `length`s, `URL` 1 and `URLSearchParams` 0
//!     (the templates reported deno_ops' Rust parameter count, 2 for both);
//!   * `URL.prototype` no longer carries an own `Symbol.toPrimitive` -- Chrome's does not (`s13`), and
//!     `String(url)` reaches the same answer through the `toString` row (`c26`);
//!   * the four failure modes of a table cutover:
//!     instance prototype identity (`f1*`), `Illegal invocation` on a foreign receiver (`f2*`), no
//!     body reachable that was not reachable before, and nothing installed after the table (`f4*`);
//!   * the WORKER realm, where BOTH interfaces were the generated worker registry's D2 stubs --
//!     measured at master: `new URL("https://a.example/").href` and every `URLSearchParams` read
//!     answered `undefined` -- and are now the same tables the window gets (invariant I8).

use engine::Engine;

/// The probe script, run verbatim in both realms.
const PROBE: &str = r#"(() => {
  const out = [];
  const T = (label, fn) => { try { out.push(label + " = " + JSON.stringify(fn())); } catch (e) { out.push(label + " = THROW " + (e && e.constructor ? e.constructor.name : "?") + ": " + (e && e.message)); } };
  const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
      ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
      : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
  });
  const FP_TS = Function.prototype.toString;
  const native = (f) => { try { return FP_TS.call(f).includes("[native code]"); } catch (e) { return false; } };
  // -- THE BRIEF'S TWELVE BEHAVIOUR PROBES, in order. They answered exactly this before the lane and
  //    must answer exactly this after it; `u` is mutated in sequence, so the order is part of them.
  const u = new URL("https://a.example/p/q?x=1&y=2#h");
  T("p01", () => u.href);
  T("p02", () => u.origin);
  T("p03", () => String(u.searchParams));
  T("p04", () => { u.searchParams.set("x","9"); return u.href; });
  T("p05", () => { u.search = "?z=3"; return String(u.searchParams); });
  T("p06", () => new URL(u).href);
  T("p07", () => ANCHOR === null ? "https://b.example/x" : new URL(ANCHOR).href);
  T("p08", () => String(URL.parse("::bad::")));
  T("p09", () => URL.canParse("::bad::"));
  T("p10", () => { u.href = "not a url"; return "NO THROW -> " + u.href; });
  T("p11", () => Object.getOwnPropertyNames(URL.prototype).length);
  T("p12", () => { const s = new URLSearchParams("b=2&a=1"); s.sort(); return String(s); });
  // -- URL: failure modes, shape, and the coercion the deleted Proxy used to do.
  T("f1a", () => Object.getPrototypeOf(new URL("https://x.example/")) === URL.prototype);
  T("f1c", () => new URL("https://x.example/") instanceof URL);
  T("f1e", () => Object.prototype.toString.call(new URL("https://x.example/")));
  T("f1g", () => { class S extends URL {}; const s = new S("https://a.example/"); return [Object.getPrototypeOf(s) === S.prototype, s.href].join("|"); });
  T("f2a", () => Object.getOwnPropertyDescriptor(URL.prototype,"href").get.call({}));
  T("f2b", () => Object.getOwnPropertyDescriptor(URL.prototype,"href").set.call({}, "https://q.example/"));
  T("f2c", () => URL.prototype.toString.call({}));
  T("f2d", () => URL.prototype.toJSON.call({}));
  T("f2e", () => Object.getOwnPropertyDescriptor(URL.prototype,"searchParams").get.call({}));
  T("f2h", () => Object.getOwnPropertyDescriptor(URL.prototype,"protocol").get.call({}));
  T("f2k", () => Object.getOwnPropertyDescriptor(URL.prototype,"protocol").set.call({}, "http:"));
  T("s01", () => desc(URL.prototype).join(","));
  T("s03", () => Object.getOwnPropertySymbols(URL.prototype).map(String).join(","));
  T("s06", () => Object.getOwnPropertyNames(URL).join(","));
  T("s08", () => [URL.length, URL.name, URL.canParse.length, URL.parse.length, URL.createObjectURL.length, URL.revokeObjectURL.length].join(","));
  T("s09", () => [URL.canParse.name, URL.parse.name, URL.createObjectURL.name, URL.revokeObjectURL.name].join(","));
  T("s10", () => JSON.stringify(Object.getOwnPropertyDescriptor(URL.prototype, Symbol.toStringTag)));
  T("s13", () => typeof URL.prototype[Symbol.toPrimitive]);
  T("s22", () => URL.toString());
  T("s23", () => Object.getPrototypeOf(URL.prototype) === Object.prototype);
  T("s24", () => Object.getOwnPropertyNames(URL.prototype).map(n => { const d = Object.getOwnPropertyDescriptor(URL.prototype, n); return n + ":" + (d.get ? d.get.name + "/" + d.get.length + (d.set ? "|" + d.set.name + "/" + d.set.length : "") : (typeof d.value === "function" ? d.value.name + "/" + d.value.length : "?")); }).join(","));
  T("s25", () => Object.getOwnPropertyNames(URL.prototype).every(n => { const d = Object.getOwnPropertyDescriptor(URL.prototype, n); if (d.get && !native(d.get)) return false; if (d.set && !native(d.set)) return false; if (typeof d.value === "function" && !native(d.value)) return false; return true; }) && ["canParse","parse","createObjectURL","revokeObjectURL"].every(n => native(URL[n])));
  T("c01", () => new URL());
  T("c02", () => URL("https://a.example/"));
  T("c03", () => new URL(1));
  T("c04", () => new URL("/a","https://b.example/c/d").href);
  T("c05", () => new URL("a", {toString(){return "https://c.example/";}}).href);
  T("c06", () => new URL({toString(){return "https://d.example/";}}).href);
  T("c07", () => new URL("https://e.example/x", undefined).href);
  T("c08", () => new URL("https://e.example/x", null).href);
  T("c09", () => URL.canParse());
  T("c12", () => { const z = new URL("https://a.example/?q=1"); return z.searchParams === z.searchParams; });
  T("c13", () => URL.createObjectURL(new Blob(["x"])).slice(0,5));
  T("c14", () => { const z = new URL("https://a.example/"); z.searchParams.append("k","v"); return z.href; });
  T("c20", () => new URL("/z", new URL("https://b.example/c/d")).href);
  T("c21", () => { const z = new URL("https://u:p@h.example:8080/a?b=c#d"); return [z.protocol,z.username,z.password,z.hostname,z.port,z.host,z.pathname,z.search,z.hash,z.origin].join("|"); });
  T("c22", () => { const z = new URL("https://h.example/a"); z.protocol="http:"; z.hostname="o.example"; z.port="9000"; z.pathname="/n"; z.search="q=1"; z.hash="top"; z.username="u"; z.password="p"; return z.href; });
  T("c23", () => { const z = new URL("https://h.example:8080/"); z.port=""; return z.host; });
  T("c25", () => new URL("https://h.example/").toJSON());
  T("c26", () => String(new URL("https://h.example/x")));
  T("c27", () => { const z = new URL("https://h.example/"); z.searchParams.append("a","1"); z.search=""; return [z.href, String(z.searchParams)].join("|"); });
  // -- URLSearchParams.
  T("u01", () => { const s = new URLSearchParams("b=2&a=1"); s.sort(); return String(s); });
  T("u02", () => new URLSearchParams("a=1&b=2").get("b"));
  T("u03", () => { const s = new URLSearchParams(); return [String(s), s.size].join("|"); });
  T("u04", () => [...new URLSearchParams("a=1&b=2")].map(p => p.join(":")).join(","));
  T("u05", () => { const s = new URLSearchParams("a=1"); s.append("a","2"); return [s.getAll("a").join("/"), s.has("a"), s.toString()].join("|"); });
  T("u06", () => { const s = new URLSearchParams([["k","v"],["k2","v2"]]); s.delete("k"); return s.toString(); });
  T("u07", () => { const s = new URLSearchParams({a:"1",b:"2"}); s.set("a","9"); return s.toString(); });
  T("u08", () => { const o = []; new URLSearchParams("a=1&b=2").forEach((v,k) => o.push(k+"="+v)); return o.join(","); });
  T("u09", () => [[...new URLSearchParams("a=1").keys()].join(","), [...new URLSearchParams("a=1").values()].join(",")].join("|"));
  // -- failure mode 1: the instance is on the TABLE's prototype, not the class template's.
  T("f1b", () => Object.getPrototypeOf(new URLSearchParams("a=1")) === URLSearchParams.prototype);
  T("f1d", () => new URLSearchParams("a=1") instanceof URLSearchParams);
  T("f1f", () => Object.prototype.toString.call(new URLSearchParams("a=1")));
  T("f1h", () => { class S extends URLSearchParams {}; const s = new S("a=1"); return [Object.getPrototypeOf(s) === S.prototype, s.get("a")].join("|"); });
  // -- failure mode 2: a foreign receiver is Chrome's `Illegal invocation`, never a silent answer.
  T("f2f", () => URLSearchParams.prototype.get.call({}, "a"));
  T("f2g", () => Object.getOwnPropertyDescriptor(URLSearchParams.prototype,"size").get.call({}));
  T("f2i", () => URLSearchParams.prototype.toString.call({}));
  T("f2j", () => URLSearchParams.prototype.append.call({}, "a", "b"));
  // -- shape.
  T("s02", () => desc(URLSearchParams.prototype).join(","));
  T("s04", () => Object.getOwnPropertySymbols(URLSearchParams.prototype).map(String).join(","));
  T("s05", () => [URLSearchParams.length, URLSearchParams.name].join(","));
  T("s07", () => Object.getOwnPropertyNames(URLSearchParams).join(","));
  T("s11", () => JSON.stringify(Object.getOwnPropertyDescriptor(URLSearchParams.prototype, Symbol.toStringTag)));
  T("s12", () => URLSearchParams.prototype[Symbol.iterator] === URLSearchParams.prototype.entries);
  T("s16", () => { const d = Object.getOwnPropertyDescriptor(URLSearchParams.prototype, Symbol.iterator); return [d.writable, d.enumerable, d.configurable].join(","); });
  T("s17", () => URLSearchParams.toString());
  T("s18", () => Object.getPrototypeOf(URLSearchParams.prototype) === Object.prototype);
  T("s19", () => Object.getPrototypeOf(URLSearchParams) === Function.prototype);
  // Function names and lengths of every own member -- the fingerprint the capture records per member.
  T("s20", () => Object.getOwnPropertyNames(URLSearchParams.prototype).map(n => { const d = Object.getOwnPropertyDescriptor(URLSearchParams.prototype, n); return n + ":" + (d.get ? d.get.name + "/" + d.get.length : (typeof d.value === "function" ? d.value.name + "/" + d.value.length : "?")); }).join(","));
  // Every own member reports [native code] (PRINCIPLES: no JS body wearing a native mask).
  T("s21", () => Object.getOwnPropertyNames(URLSearchParams.prototype).every(n => { const d = Object.getOwnPropertyDescriptor(URLSearchParams.prototype, n); if (d.get && !native(d.get)) return false; if (d.set && !native(d.set)) return false; if (typeof d.value === "function" && !native(d.value)) return false; return true; }));
  // -- constructor behaviour.
  T("c11", () => URLSearchParams("a=1"));
  // The MESSAGE diverges (Chrome doubles its own prefix: "Failed to construct 'URLSearchParams':
  // Failed to construct 'URLSearchParams': Sequence initializer must only contain pair elements"),
  // so only the error TYPE is asserted -- a pre-existing gap this lane neither causes nor closes.
  T("c16", () => { try { new URLSearchParams([[1]]); return "NO THROW"; } catch (e) { return e.constructor.name; } });
  T("c17", () => new URLSearchParams(new URLSearchParams("a=1&b=2")).toString());
  T("c18", () => new URLSearchParams("?a=1").toString());
  T("c19", () => new URLSearchParams("a+b=c+d").get("a b"));
  // -- failure mode 4: nothing re-defines a member after the table. The bridge slot the deleted JS
  //    parked here is gone; a non-null answer means something outside the table is still writing.
  T("f4a", () => { const KB = globalThis[Symbol.for("korvus.bridge")]; return KB ? [typeof KB.__uspBind, typeof KB.__urlHrefNativeSet, typeof KB.__blobUrlContents].join(",") : "no-bridge"; });
  // The global is the table's own native constructor, not a Proxy over a cppgc class template. A
  // Proxy is not detectable directly, but `URL.toString()` (s22) reporting the NAME rather than
  // `function () { [native code] }` is, and so is the WebIDL `length` of 1 rather than op2's 2 (s08).
  T("f4b", () => { const KB = globalThis[Symbol.for("korvus.bridge")]; return KB ? Object.keys(KB).filter(k => k.toLowerCase().includes("url")).join(",") : "no-bridge"; });
  return out.join("\n");
})()"#;

/// Every expected answer. The `u`/`f1`/`f2`/`s`/`c` rows were measured against headless Chrome
/// 150.0.7871.46 over a loopback server on 2026-09-11 by running THIS EXACT `PROBE` string in a page
/// (`file://` is refused); `f4a` is the one engine-internal row -- the bridge slots this lane deletes,
/// which no browser has.
fn expected_answers() -> Vec<(&'static str, &'static str)> {
    vec![
        // THE BRIEF'S TWELVE.
        ("p01", r#""https://a.example/p/q?x=1&y=2#h""#),
        ("p02", r#""https://a.example""#),
        ("p03", r#""x=1&y=2""#),
        ("p04", r#""https://a.example/p/q?x=9&y=2#h""#),
        ("p05", r#""z=3""#),
        ("p06", r#""https://a.example/p/q?z=3#h""#),
        ("p07", r#""https://b.example/x""#),
        ("p08", r#""null""#),
        ("p09", "false"),
        ("p10", "THROW TypeError: Failed to set the 'href' property on 'URL': Invalid URL"),
        ("p11", "15"),
        ("p12", r#""a=1&b=2""#),
        // URL.
        ("f1a", "true"),
        ("f1c", "true"),
        ("f1e", r#""[object URL]""#),
        ("f1g", r#""true|https://a.example/""#),
        ("f2a", "THROW TypeError: Illegal invocation"),
        ("f2b", "THROW TypeError: Illegal invocation"),
        ("f2c", "THROW TypeError: Illegal invocation"),
        ("f2d", "THROW TypeError: Illegal invocation"),
        ("f2e", "THROW TypeError: Illegal invocation"),
        ("f2h", "THROW TypeError: Illegal invocation"),
        ("f2k", "THROW TypeError: Illegal invocation"),
        ("s03", r#""Symbol(Symbol.toStringTag)""#),
        ("s06", r#""length,name,prototype,canParse,parse,createObjectURL,revokeObjectURL""#),
        ("s08", r#""1,URL,1,1,1,1""#),
        ("s09", r#""canParse,parse,createObjectURL,revokeObjectURL""#),
        ("s10", r#""{\"value\":\"URL\",\"writable\":false,\"enumerable\":false,\"configurable\":true}""#),
        ("s13", r#""undefined""#),
        ("s22", r#""function URL() { [native code] }""#),
        ("s23", "true"),
        (
            "s24",
            r#""origin:get origin/0,protocol:get protocol/0|set protocol/1,username:get username/0|set username/1,password:get password/0|set password/1,host:get host/0|set host/1,hostname:get hostname/0|set hostname/1,port:get port/0|set port/1,pathname:get pathname/0|set pathname/1,search:get search/0|set search/1,searchParams:get searchParams/0,hash:get hash/0|set hash/1,href:get href/0|set href/1,toJSON:toJSON/0,toString:toString/0,constructor:URL/1""#,
        ),
        ("s25", "true"),
        ("c01", "THROW TypeError: Failed to construct 'URL': 1 argument required, but only 0 present."),
        ("c02", "THROW TypeError: Failed to construct 'URL': Please use the 'new' operator, this DOM object constructor cannot be called as a function."),
        ("c03", "THROW TypeError: Failed to construct 'URL': Invalid URL"),
        ("c04", r#""https://b.example/a""#),
        ("c05", r#""https://c.example/a""#),
        ("c06", r#""https://d.example/""#),
        ("c07", r#""https://e.example/x""#),
        ("c08", "THROW TypeError: Failed to construct 'URL': Invalid base URL"),
        ("c09", "THROW TypeError: Failed to execute 'canParse' on 'URL': 1 argument required, but only 0 present."),
        ("c12", "true"),
        ("c13", r#""blob:""#),
        ("c14", r#""https://a.example/?k=v""#),
        ("c20", r#""https://b.example/z""#),
        ("c21", r#""https:|u|p|h.example|8080|h.example:8080|/a|?b=c|#d|https://h.example:8080""#),
        ("c22", r#""http://u:p@o.example:9000/n?q=1#top""#),
        ("c23", r#""h.example""#),
        ("c25", r#""https://h.example/""#),
        ("c26", r#""https://h.example/x""#),
        ("c27", r#""https://h.example/|""#),
        // URLSearchParams.
        ("u01", r#""a=1&b=2""#),
        ("u02", r#""2""#),
        ("u03", r#""|0""#),
        ("u04", r#""a:1,b:2""#),
        ("u05", r#""1/2|true|a=1&a=2""#),
        ("u06", r#""k2=v2""#),
        ("u07", r#""a=9&b=2""#),
        ("u08", r#""a=1,b=2""#),
        ("u09", r#""a|1""#),
        ("f1b", "true"),
        ("f1d", "true"),
        ("f1f", r#""[object URLSearchParams]""#),
        ("f1h", r#""true|1""#),
        ("f2f", "THROW TypeError: Illegal invocation"),
        ("f2g", "THROW TypeError: Illegal invocation"),
        ("f2i", "THROW TypeError: Illegal invocation"),
        ("f2j", "THROW TypeError: Illegal invocation"),
        ("s04", r#""Symbol(Symbol.toStringTag),Symbol(Symbol.iterator)""#),
        ("s05", r#""0,URLSearchParams""#),
        ("s07", r#""length,name,prototype""#),
        ("s11", r#""{\"value\":\"URLSearchParams\",\"writable\":false,\"enumerable\":false,\"configurable\":true}""#),
        ("s12", "true"),
        ("s16", r#""true,false,true""#),
        ("s17", r#""function URLSearchParams() { [native code] }""#),
        ("s18", "true"),
        ("s19", "true"),
        (
            "s20",
            r#""size:get size/0,append:append/2,delete:delete/1,get:get/1,getAll:getAll/1,has:has/1,set:set/2,sort:sort/0,toString:toString/0,entries:entries/0,forEach:forEach/1,keys:keys/0,values:values/0,constructor:URLSearchParams/0""#,
        ),
        ("s21", "true"),
        ("c11", "THROW TypeError: Failed to construct 'URLSearchParams': Please use the 'new' operator, this DOM object constructor cannot be called as a function."),
        ("c16", r#""TypeError""#),
        ("c17", r#""a=1&b=2""#),
        ("c18", r#""a=1""#),
        ("c19", r#""c d""#),
        // The three bridge slots this lane deleted: `__uspBind` (08g's binder), `__urlHrefNativeSet`
        // (08g's captured native `href` setter) and `__blobUrlContents` (host.js's blob-text Map).
        // Nothing writes any of them any more, in either realm -- failure mode 4, checked rather than
        // asserted by grep. `f4b` is the same check by shape: no `url`-ish key is left on the bridge.
        //
        // NO REALM HAS A HOLDER, so the probe takes its `: "no-bridge"` arm in both -- the stronger answer
        // to this question: not "the three names are absent from the holder" but "there is no holder".
        // The WINDOW's last four slots went (a V8 Private, an `OpState` field and two
        // deletions); a WORKER's two -- the native functions its appended boot script called -- at
        // When `worker.rs` made those calls from Rust (`tests/bridge_slots.rs`).
        ("f4a", r#""no-bridge""#),
        ("f4b", r#""no-bridge""#),
    ]
}

/// The ordered `name:kind:flags` describe lines an interface's own prototype must produce, derived
/// from the shipped capture's member list plus the trailing non-enumerable `constructor` every Chrome
/// interface prototype ends with.
fn expected_proto(artifact: &str, iface: &str) -> String {
    let fx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact(artifact)).unwrap()).unwrap();
    let mut out: Vec<String> = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{artifact} has no member list for {iface}"))
        .iter()
        .map(|m| {
            let n = m["name"].as_str().unwrap();
            match m["kind"].as_str().unwrap() {
                "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
                "method" => format!("{n}:data:e=1:c=1:w=1"),
                k => panic!("unexpected kind {k}"),
            }
        })
        .collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out.join(",")
}

fn parse(dump: &str) -> std::collections::BTreeMap<String, String> {
    dump.lines().filter_map(|l| l.split_once(" = ").map(|(k, v)| (k.to_string(), v.to_string()))).collect()
}

/// Compare one realm's dump against Chrome's answers plus the capture-derived prototype shape.
fn assert_realm(realm: &str, dump: &str, artifact: &str) {
    let got = parse(dump);
    assert!(!got.is_empty(), "{realm}: the probe produced nothing: {dump}");
    let mut bad = Vec::new();
    for (key, want) in expected_answers() {
        match got.get(key) {
            Some(v) if v == want => {}
            Some(v) => bad.push(format!("  {realm} {key}: chrome {want}   korvus {v}")),
            None => bad.push(format!("  {realm} {key}: MISSING (chrome {want})")),
        }
    }
    for (key, iface) in [("s01", "URL"), ("s02", "URLSearchParams")] {
        let want = format!("{:?}", expected_proto(artifact, iface));
        match got.get(key) {
            Some(v) if *v == want => {}
            Some(v) => bad.push(format!("  {realm} {key} ({iface}.prototype): capture {want}   korvus {v}")),
            None => bad.push(format!("  {realm} {key}: MISSING")),
        }
    }
    assert!(bad.is_empty(), "{realm}: {} probe(s) disagree with Chrome/the capture:\n{}", bad.len(), bad.join("\n"));
}

async fn window_dump() -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body><a id=\"a\" href=\"https://b.example/x\">a</a></body></html>").unwrap();
    e.run().await.unwrap();
    let src = PROBE.replace("ANCHOR", "document.getElementById(\"a\")");
    e.eval_value("url-live-window", src).unwrap().as_str().unwrap_or_default().to_string()
}

async fn worker_dump() -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    // No document in a worker, so probe p07 (the anchor stringifier) resolves to its own answer.
    let body = format!("self.postMessage({});", PROBE.replace("ANCHOR", "null"));
    let reg = format!(
        "globalThis.__workerScripts = {{ './url-probe.js': {} }};",
        serde_json::to_string(&body).unwrap()
    );
    e.eval("url-live-worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "url-live-worker-driver",
        "const w = new Worker('./url-probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(l) = out.iter().find(|l| l.starts_with("RESULT:")) {
            return l["RESULT:".len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("the worker did not post its URL probe within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn url_and_url_search_params_answer_what_chrome_answers_in_the_window() {
    let dump = window_dump().await;
    assert_realm("window", &dump, "surface");
}

/// The two SYNCHRONOUS readers of the blob-URL registry, which moved from two JS
/// `Map`s in `host.js` into Rust (`modules/url/url.rs::BlobUrlRegistry`) because the statics became
/// table rows and there was no JS left to own the maps.
///
///   * `host.js`'s `resolveWorkerSource` hands a `blob:` Worker its script. It read
///     `KB.__blobUrlContents` and now calls `blob_url_text` from Rust
///     (`core/workers/worker.rs::worker_source`); the answer must be the same, so a Worker built
///     from a Blob still runs.
///   * `core/html/browsing_context.rs::blob_text` gives an `<iframe src="blob:...">` its markup. This
///     one was BROKEN and the move repaired it: the map stored the decoded TEXT while the Rust reader
///     looked for an object with `_bytes`, so every lookup failed the cast and the iframe got `""`.
///     Measured at master (`inner: ""`); Chrome renders the document, so the repaired answer is the
///     correct one. Recorded here so the change is a gate, not a surprise.
#[tokio::test]
async fn the_blob_url_registry_still_answers_its_two_synchronous_readers() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    let src = r#"(() => {
      const b = new Blob(["<!doctype html><p id=q>HELLO</p>"], {type:"text/html"});
      const u = URL.createObjectURL(b);
      const f = document.createElement("iframe");
      document.body.appendChild(f);
      f.src = u;
      let inner = "<none>";
      try { inner = f.contentDocument ? (f.contentDocument.body ? f.contentDocument.body.innerHTML : "<no body>") : "<no doc>"; } catch (err) { inner = "THROW " + err.message; }
      const w = new Blob(["self.postMessage('from-blob-worker');"]);
      const wu = URL.createObjectURL(w);
      URL.revokeObjectURL(u);
      const f2 = document.createElement("iframe");
      document.body.appendChild(f2);
      f2.src = u;
      let afterRevoke = "<none>";
      try { afterRevoke = f2.contentDocument && f2.contentDocument.body ? f2.contentDocument.body.innerHTML : "<no body>"; } catch (err) { afterRevoke = "THROW " + err.message; }
      return JSON.stringify({ id: u, inner, workerUrl: wu, afterRevoke });
    })()"#;
    let got = e.eval_value("blob-registry", src.to_string()).unwrap();
    let v: serde_json::Value = serde_json::from_str(got.as_str().expect("the probe returned a string")).unwrap();
    // Chrome's shape, `blob:<origin>/<v4 uuid>` -- `blob:null/` for this opaque-origin page. It was a
    // serial, `blob:nodedata:<n>`, a string no browser mints.
    let uuid = |s: &str| {
        let t = s.strip_prefix("blob:null/").unwrap_or("");
        t.len() == 36 && t.chars().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() && !c.is_ascii_uppercase() })
            && t.as_bytes()[14] == b'4'
    };
    assert!(uuid(v["id"].as_str().unwrap()), "blob:null/<v4 uuid>, got {}", v["id"]);
    assert!(uuid(v["workerUrl"].as_str().unwrap()), "blob:null/<v4 uuid>, got {}", v["workerUrl"]);
    assert_ne!(v["id"], v["workerUrl"], "every mint is a new URL");
    // The repair: an iframe whose src is a blob: URL gets the Blob's markup. `""` here is the master
    // behaviour, and means the Rust reader and the Rust writer have gone out of step again.
    assert_eq!(v["inner"], "<p id=\"q\">HELLO</p>", "blob: iframe markup (was \"\" at master)");
    // revokeObjectURL really removes it: the same URL now resolves to nothing.
    assert_eq!(v["afterRevoke"], "", "a revoked blob: URL resolves to no document");
}

/// A `blob:` Worker gets its source through the same registry, read synchronously by
/// `host.js`'s `resolveWorkerSource`. This is the path the two JS Maps existed for.
#[tokio::test]
async fn a_blob_url_worker_still_gets_its_source() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    e.eval(
        "blob-worker",
        "const b = new Blob([\"self.postMessage('from-blob-worker:' + (typeof URL) + ':' + new URL('https://a.example/x').pathname);\"]);\
         const w = new Worker(URL.createObjectURL(b)); w.onmessage = (ev) => print('RESULT:' + ev.data);"
            .into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    let got = loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(l) = out.iter().find(|l| l.starts_with("RESULT:")) {
            break l["RESULT:".len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("the blob: worker never posted; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    // The worker ran its Blob source AND its own realm's `URL` table works there (invariant I8).
    assert_eq!(got, "from-blob-worker:function:/x");
}

#[tokio::test]
async fn url_and_url_search_params_answer_what_chrome_answers_in_a_worker() {
    // Invariant I8: the SAME two tables, installed by `install_worker_url`. Every one of these
    // answers was `undefined` at master, where the worker's `URL` and `URLSearchParams` were both the
    // generated worker registry's D2 stubs.
    let dump = worker_dump().await;
    assert_realm("worker", &dump, "worker-surface");
}
