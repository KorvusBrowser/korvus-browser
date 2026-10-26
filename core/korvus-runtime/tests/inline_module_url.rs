//! What URL an INLINE `<script type="module">` reports, and how two of them stay two modules.
//!
//! ## What was wrong
//!
//! `script_exec.rs` named an inline module `{base}#korvus-inline-module-{i}`. That name is the V8
//! `ScriptOrigin` resource name, so **the engine's name was in every stack frame of every page that
//! throws inside an inline module** — readable from page script, with no privileged API:
//!
//! ```text
//! TypeError: Cannot read properties of null (reading 'x')
//!     at http://127.0.0.1:45999/i.html#korvus-inline-module-0:2:12
//! ```
//!
//! ## Every expectation below was measured in Chrome 150.0.7871.46
//!
//! `--headless=new` over a loopback `python3 -m http.server`, driven through CDP on 2026-09-12, on
//! eight probe pages. The three surfaces were read separately, because they are three surfaces:
//! `Error.prototype.stack`, `ErrorEvent.filename` / `window.onerror`'s second argument, and CDP
//! `Runtime.exceptionThrown`'s `exceptionDetails.url`. **All three answer the same string**, and for
//! an inline script — module or classic — that string is the DOCUMENT URL with no fragment, no
//! query, and no suffix of any kind:
//!
//! | shape                                   | Chrome's reported URL   |
//! |-----------------------------------------|-------------------------|
//! | throwing inline `<script type=module>`  | `…/m1.html`             |
//! | throwing classic inline `<script>`      | `…/c1.html`             |
//! | two inline modules, both throwing       | `…/m2.html` for BOTH    |
//! | async throw (setTimeout / rejection)    | `…/async.html`          |
//! | external `<script type=module src>`     | `…/ext.mjs`             |
//!
//! ## The crux: what Chrome does about two inline modules
//!
//! A module map is keyed by URL, so the fragment looked load-bearing. It is not. Two **byte-identical**
//! inline modules in one document (`dup.html`: same source, same URL, same V8 source hash
//! `541a0156…`) both ran — `runs` came back `2` — and `Debugger.scriptParsed` reported them as two
//! scripts with two `scriptId`s (`13` and `14`) at one URL. Chrome does not deduplicate them, because
//! per HTML an inline module script is never entered into the module map at all; only *fetched*
//! modules are. Their IMPORTS are shared: `imp.html` had two inline modules each `import`ing
//! `./helper.mjs` and calling a counter, and the answers were `1` and `2` — one helper instance, two
//! importers, exactly as a module map should behave.
//!
//! deno_core 0.410 turns out to have the same property, which is what makes the fragment removable:
//! `ModuleMap::create_module_info` assigns `data.handles.len()` as the id and `by_name.insert`
//! OVERWRITES, with no uniqueness check — so a second `load_side_es_module_from_code` at a name that
//! is already taken compiles a second `v8::Module`, gets a second `ModuleId`, and evaluates it. Only
//! the name→id lookup is last-wins, and nothing ever looks an inline module up by name (a document
//! URL is HTML, not a module; an `import` of it fetches and fails in Chrome too).
//!
//! ## The one thing that genuinely cannot match Chrome, and why
//!
//! With `<base href="/sub/">` Chrome reports TWO DIFFERENT URLs for one inline module:
//! `Error.stack` says the document URL (`…/basepage.html`) while `import.meta.url` says the base URL
//! (`…/sub/`) — measured. In deno_core both are the module's `name`: `script_origin()` builds the
//! `ScriptOrigin` from it and `host_initialize_import_meta_object_callback` writes it into
//! `import.meta.url`. One string cannot be two values, so the name is the DOCUMENT URL (the surface a
//! page reads by accident, from any throw) and the import base is restored through the loader, which
//! korvus does own. See `a_base_href_moves_the_import_base_but_not_the_stack_url`.
//!
//! ## A DIFFERENT `Error.stack` defect, measured here and deliberately left alone
//!
//! A sibling unit handed over a second page-visible `Error.stack` difference and asked whether it
//! shares a root cause with the one above. Measured on the same day, it does not, and the measurement
//! that settles it is the NO-sourceURL case:
//!
//! ```text
//! (0,eval)("throw new Error('e')\n//# sourceURL=zz.js")
//!   Chrome   at eval (zz.js:1:7)
//!   korvus   at eval (zz.js, <anonymous>:1:7)          <- differs
//!
//! (0,eval)("throw new Error('e')")                      // same call, no sourceURL comment
//!   Chrome   at eval (eval at <anonymous> (…/evalsrc.html:6:49), <anonymous>:1:7)
//!   korvus   at eval (eval at <anonymous> (korvus:run:5:49), <anonymous>:1:7)   <- SAME shape
//! ```
//!
//! Without a `//# sourceURL` the two engines agree exactly, korvus's own script name and eval position
//! included — so the script origin and V8's eval-from bookkeeping are both being threaded correctly
//! (an independent confirmation of the sibling's "it is not the script origin"). The divergence
//! appears only once the eval'd script has a NAME: Chrome's V8 then prints that name alone, and this
//! build's V8 prints it in the eval-origin slot and keeps the `, <anonymous>` tail.
//!
//! That is V8 rendering a string korvus never supplies (`zz.js` comes from the page's own comment,
//! `<anonymous>` is V8's literal for a nameless script), whereas the defect this file fixes was korvus
//! CHOOSING the wrong string and handing it to V8 as a `ScriptOrigin` resource name. Different layer,
//! different mechanism, and nothing in this commit moves it — it reproduces under `korvus run`, with
//! no document, no navigation and no module involved. Reported rather than chased.
//!
//! ## What is NOT asserted here, and is not this unit's
//!
//! Chrome's line/column for an inline script are DOCUMENT-relative (`m1.html:4:12` — line 4 of the
//! HTML file), because Blink hands V8 the script element's position as the origin's line/column
//! offset. korvus compiles the body alone at offset 0,0, so its numbers are BODY-relative. That is a
//! real difference and a different unit: it needs the parser to record each script element's source
//! position, which nothing in the tree does yet. These tests therefore assert the URL — the fact
//! under test — and say so rather than pinning a line number either engine would call wrong.

use engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const DOC: &str = "http://127.0.0.1:8731/i.html";

/// The URL out of the first `at …:line:col` frame of a stack, with the position stripped. Defined in
/// JS rather than Rust so the test reads exactly what a PAGE reads — `Error.prototype.stack`, from
/// page script, which is the surface the defect was on.
const FRAME_URL: &str = r#"
function frameUrl(stack) {
  if (typeof stack !== "string") return "NO-STACK";
  const m = /\n\s*at (?:.*\()?([^()]*?):(\d+):(\d+)\)?/.exec(stack);
  return m ? m[1] : "NO-FRAME";
}
"#;

/// Load `html` at [`DOC`], let the deferred module queue drain, then read one JSON value back.
async fn probe(html: &str, expr: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, DOC, &[]).await.expect("load_page");
    e.run().await.expect("run");
    e.eval_value("probe", format!("(() => {{ {FRAME_URL}\n return ({expr}); }})()"))
        .expect("eval")
}

/// Shape 1 — a throwing inline `<script type="module">`. Chrome: `…/m1.html`, the document URL, no
/// fragment. This is the exact page from the brief.
#[tokio::test]
async fn an_inline_modules_stack_names_the_document_url() {
    let v = probe(
        r#"<!doctype html><html><body>
           <script type="module">try { null.x } catch(e) { window.__s = e.stack; }</script>
           </body></html>"#,
        "frameUrl(window.__s)",
    )
    .await;
    assert_eq!(v.as_str(), Some(DOC), "an inline module's stack frame must name the document URL, as Chrome's does");
}

/// The same page, stated as the property that was violated: NOTHING in a stack a page can read may
/// name the engine. Separate from the equality above on purpose — this is the one that stays
/// meaningful if the URL scheme is ever reworked again.
#[tokio::test]
async fn no_stack_a_page_can_read_names_the_engine() {
    let v = probe(
        r#"<!doctype html><html><body>
           <script type="module">try { null.x } catch(e) { window.__s = e.stack; }</script>
           </body></html>"#,
        "window.__s",
    )
    .await;
    let stack = v.as_str().expect("the module must have run and caught");
    assert!(
        !stack.to_ascii_lowercase().contains("korvus"),
        "a page-readable Error.stack must not name the engine, got: {stack}"
    );
    assert!(!stack.contains('#'), "and must carry no fragment at all, got: {stack}");
}

/// Shape 2 — a throwing CLASSIC inline `<script>`, for contrast. Chrome reports the document URL here
/// too, and korvus already did: `run_classic` builds its own `ScriptOrigin` from `page_url`. The
/// regression guard for the half of the pass this unit did not touch.
#[tokio::test]
async fn a_classic_inline_scripts_stack_names_the_document_url() {
    let v = probe(
        r#"<!doctype html><html><body>
           <script>try { null.x } catch(e) { window.__s = e.stack; }</script>
           </body></html>"#,
        "frameUrl(window.__s)",
    )
    .await;
    assert_eq!(v.as_str(), Some(DOC), "a classic inline script's stack frame must name the document URL");
}

/// Shape 3, THE CRUX — two inline modules in one document are two modules.
///
/// Written before the name changed, and it passed before and after: that is the whole point of it.
/// The two bodies are BYTE-IDENTICAL apart from the slot they write, which is the sharpest form of
/// the question — if anything deduplicated by source or by URL, `runs` would be 1. Chrome's answer on
/// the same shape was 2.
#[tokio::test]
async fn two_inline_modules_are_two_modules() {
    let v = probe(
        r#"<!doctype html><html><body>
           <script>window.__runs = 0; window.__st = [];</script>
           <script type="module">window.__runs++; try { null.x } catch(e) { window.__st.push(e.stack); }</script>
           <script type="module">window.__runs++; try { null.x } catch(e) { window.__st.push(e.stack); }</script>
           </body></html>"#,
        "JSON.parse(JSON.stringify({runs: window.__runs, urls: window.__st.map(frameUrl)}))",
    )
    .await;
    assert_eq!(v["runs"].as_u64(), Some(2), "two inline modules must BOTH evaluate -- they are two module scripts, not one");
    assert_eq!(
        v["urls"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()),
        Some(vec![DOC, DOC]),
        "and both name the document URL, as Chrome's two do (one URL, two scripts)"
    );
}

/// Shape 4 — an ASYNC throw out of an inline module, through `window.onerror` and `ErrorEvent`.
/// Chrome answers the document URL on both, from the module's origin (`…/async.html`), so this is the
/// second channel the fragment leaked through and it has to be checked separately from `Error.stack`.
///
/// A SECOND, SEPARATE LEAK THIS MEASUREMENT FOUND, which is NOT this unit's and is not fixed here.
/// korvus's stack for a throwing timer callback carries a HOST frame Chrome does not have:
///
/// ```text
///     at http://127.0.0.1:8731/i.html:1:29        <- this unit fixed this line
///     at kPump (korvus:bootstrap:82:54)           <- still there; the engine's name, page-readable
/// ```
///
/// Chrome's answer on the same shape (`async.html`) is the one frame and nothing under it — a timer
/// callback is entered from the task source, not from a JS function, so there is no host frame to
/// show. `kPump` is the timer pump in `engine.rs`'s bootstrap JS string, i.e. the
/// runtime-JS surface a sibling unit owns; this unit is `script_exec.rs`/`event_loop.rs` and does not
/// reach into it. The assertions below are on the REPORTED URL — `ErrorEvent.filename`,
/// `window.onerror`'s `source`, and the first stack frame — which is what this unit changed; the
/// extra frame is recorded here so the next reader of this file cannot mistake a green run for the
/// whole stack being clean.
#[tokio::test]
async fn an_async_throw_reports_the_document_url_on_every_channel() {
    let v = probe(
        r#"<!doctype html><html><body>
           <script>
             window.__ev = []; window.__oe = [];
             window.addEventListener('error', function(e){ window.__ev.push({f: e.filename, s: e.error && e.error.stack}); }, true);
             window.onerror = function(m, src, l, c, err){ window.__oe.push({src: src, s: err && err.stack}); };
           </script>
           <script type="module">
             setTimeout(function(){ null.zzz }, 0);
             window.__rej = (function(){ try { null.q } catch(e) { return e.stack; } })();
           </script>
           </body></html>"#,
        "JSON.parse(JSON.stringify({ev: window.__ev, oe: window.__oe, rej: window.__rej}))",
    )
    .await;
    let ev = v["ev"].as_array().cloned().unwrap_or_default();
    let oe = v["oe"].as_array().cloned().unwrap_or_default();
    // An Error CONSTRUCTED inside the module carries the module's origin in its own stack, which is
    // the channel a rejected promise reports through (`unhandledrejection`'s `reason.stack` -- Chrome:
    // `at …/async.html:17:16`). Same origin string, so it is checked here rather than in its own test.
    assert_eq!(
        v["rej"].as_str().map(frame_url),
        Some(DOC.to_string()),
        "an Error built inside an inline module -- what a rejection reports -- must name the document URL"
    );
    assert!(!ev.is_empty(), "the async throw must reach an `error` event, as it does in Chrome");
    assert_eq!(ev[0]["f"].as_str(), Some(DOC), "ErrorEvent.filename must be the document URL");
    assert_eq!(
        ev[0]["s"].as_str().map(frame_url),
        Some(DOC.to_string()),
        "and the ErrorEvent's error.stack must name it too"
    );
    assert!(!oe.is_empty(), "and window.onerror must fire");
    assert_eq!(oe[0]["src"].as_str(), Some(DOC), "window.onerror's `source` argument must be the document URL");
}

/// Shape 5 — an EXTERNAL `<script type="module" src>`, for contrast. That path already went through
/// `resolve()` and reports the SCRIPT's URL, not the document's: Chrome said `…/ext.mjs`. The guard
/// that this unit did not flatten the external case into the inline one.
#[tokio::test]
async fn an_external_modules_stack_names_the_script_url() {
    let js = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = js.local_addr().unwrap();
    tokio::spawn(serve(js, "text/javascript", "try { null.x } catch(e) { globalThis.__s = e.stack; }"));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!("http://{addr}/e.html");
    e.load_page(
        "<!doctype html><html><body><script type=\"module\" src=\"ext.mjs\"></script></body></html>",
        &doc,
        &[],
    )
    .await
    .expect("load_page");
    e.run().await.expect("run");
    let v = e
        .eval_value("probe", format!("(() => {{ {FRAME_URL}\n return frameUrl(globalThis.__s); }})()"))
        .expect("eval");
    assert_eq!(
        v.as_str(),
        Some(format!("http://{addr}/ext.mjs").as_str()),
        "an EXTERNAL module's stack names the script's own URL, not the document's"
    );
}

/// The `<base href>` split, measured in Chrome and impossible to satisfy on both halves at once (see
/// the file header). Chrome: `Error.stack` → the DOCUMENT url; `import.meta.url` → the BASE url;
/// `import.meta.resolve("./x.js")` → under the BASE.
///
/// korvus matches Chrome on the stack URL and on `import.meta.resolve`, and cannot on
/// `import.meta.url`, because deno_core derives both from one string. The assertion on
/// `import.meta.url` therefore states what korvus DOES, with the Chrome value named in the message —
/// a recorded difference, not a hidden one.
#[tokio::test]
async fn a_base_href_moves_the_import_base_but_not_the_stack_url() {
    let v = probe(
        r#"<!doctype html><html><head><base href="/sub/"></head><body>
           <script type="module">
             try { null.x } catch(e) { window.__s = e.stack; }
             window.__imu = import.meta.url;
             window.__res = import.meta.resolve("./x.js");
           </script>
           </body></html>"#,
        "JSON.parse(JSON.stringify({url: frameUrl(window.__s), imu: window.__imu, res: window.__res}))",
    )
    .await;
    assert_eq!(
        v["url"].as_str(),
        Some(DOC),
        "a <base href> moves the import base, NOT the reported script URL -- Chrome reports the document URL here"
    );
    assert_eq!(
        v["res"].as_str(),
        Some("http://127.0.0.1:8731/sub/x.js"),
        "import.meta.resolve must still resolve against the BASE url"
    );
    assert_eq!(
        v["imu"].as_str(),
        Some(DOC),
        "KNOWN DIFFERENCE: Chrome answers the base url `http://127.0.0.1:8731/sub/` here. deno_core \
         derives import.meta.url and the ScriptOrigin from ONE module name, and the stack URL is the \
         surface a page reads by accident, so the name is the document URL. Recorded, not hidden."
    );
}

/// Import resolution, end to end and over the wire: a RELATIVE `import` inside an inline module must
/// still fetch, and two inline modules importing the same relative path must share ONE instance.
///
/// Chrome's answer on the same shape (`imp.html`): `count1 = 1`, `count2 = 2` — one helper module,
/// two importers. This is the test that would catch a "fix" that made the module name unique again by
/// some other means, since a per-module name changes what a relative import resolves against.
#[tokio::test]
async fn two_inline_modules_share_one_relative_import() {
    let js = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = js.local_addr().unwrap();
    tokio::spawn(serve(
        js,
        "text/javascript",
        "export const tag = 'helper-loaded';\nlet n = 0;\nexport function bump(){ n += 1; return n; }\n",
    ));

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = format!("http://{addr}/imp.html");
    e.load_page(
        "<!doctype html><html><body>\
         <script type=\"module\">import {tag, bump} from \"./helper.mjs\"; globalThis.__t1 = tag; globalThis.__c1 = bump();</script>\
         <script type=\"module\">import {tag, bump} from \"./helper.mjs\"; globalThis.__t2 = tag; globalThis.__c2 = bump();</script>\
         </body></html>",
        &doc,
        &[],
    )
    .await
    .expect("load_page");
    e.run().await.expect("run");
    let v = e
        .eval_value(
            "probe",
            "JSON.parse(JSON.stringify({t1: globalThis.__t1, t2: globalThis.__t2, c1: globalThis.__c1, c2: globalThis.__c2}))"
                .to_string(),
        )
        .expect("eval");
    assert_eq!(v["t1"].as_str(), Some("helper-loaded"), "the first inline module's relative import must resolve and fetch");
    assert_eq!(v["t2"].as_str(), Some("helper-loaded"), "and so must the second's");
    assert_eq!(v["c1"].as_u64(), Some(1), "one helper instance, first importer sees 1 (Chrome: 1)");
    assert_eq!(v["c2"].as_u64(), Some(2), "and the second sees 2 -- ONE module instance shared, not two (Chrome: 2)");
}

/// THE PRICE OF NAMING AN INLINE MODULE AFTER THE DOCUMENT, and the proof it is paid back.
///
/// Putting the document URL on an inline module puts it in deno_core's `by_name` map, where Chrome has
/// no entry at all — so `import("./self.html")` from the page would find the inline module and resolve
/// to its namespace instead of fetching. Measured on the intermediate build, before the guard in
/// `KorvusModuleLoader::resolve`: korvus answered `RESOLVED:[]` where Chrome answers
/// `REJECTED: Failed to fetch dynamically imported module: …/selfimp.html`. Both engines measured on
/// 2026-09-12 on the same page.
///
/// That it was NEW, not pre-existing, was checked rather than assumed: importing a DIFFERENT html file
/// (`import("./plain.html")`) rejects on this build and always did, since nothing registers that URL —
/// which is exactly the path the self-import took before the name changed.
#[tokio::test]
async fn a_module_cannot_import_the_document_it_is_written_in() {
    let v = probe(
        r#"<!doctype html><html><body>
           <script type="module">export const x = 1; window.__m = "inline-ran";</script>
           <script type="module">
             import("./i.html").then(
               function(m){ window.__self = "RESOLVED:" + JSON.stringify(Object.keys(m)); },
               function(e){ window.__self = "REJECTED"; });
           </script>
           </body></html>"#,
        "JSON.parse(JSON.stringify({m: window.__m, self: window.__self}))",
    )
    .await;
    assert_eq!(v["m"].as_str(), Some("inline-ran"), "the first inline module must still run");
    assert_eq!(
        v["self"].as_str(),
        Some("REJECTED"),
        "importing the document's own URL must fail, as it does in Chrome -- an inline module is not \
         something the module map may hand back"
    );
}

/// The URL-LESS path, `Engine::load_html` — the one place a module has no document URL at all, and
/// the one the `korvus:module/{i}` fallback in `event_loop.rs` was the specifier for.
///
/// Two things are asserted, and the first is why the second is allowed to be true. Those two modules
/// used to be told apart by the `{i}` in that specifier; they are now BOTH `about:blank`, which is
/// safe only because two modules at one name are two modules — so this is that proof again on the
/// path where the name is synthetic rather than a real URL, where a collision would be likeliest.
/// And nothing a page can read out of either one names the engine.
#[tokio::test]
async fn two_inline_modules_survive_the_url_less_path() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_html(
        "<!doctype html><html><body>\
         <script>globalThis.__runs = 0; globalThis.__st = [];</script>\
         <script type=\"module\">globalThis.__runs++; try { null.x } catch(e) { globalThis.__st.push(e.stack); }</script>\
         <script type=\"module\">globalThis.__runs++; try { null.x } catch(e) { globalThis.__st.push(e.stack); }</script>\
         </body></html>",
    )
    .expect("load_html");
    e.run().await.expect("run");
    let v = e
        .eval_value("probe", "JSON.parse(JSON.stringify({runs: globalThis.__runs, st: globalThis.__st}))".to_string())
        .expect("eval");
    assert_eq!(v["runs"].as_u64(), Some(2), "both URL-less inline modules must evaluate -- they share one name and are still two modules");
    let stacks: Vec<&str> = v["st"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
    assert_eq!(stacks.len(), 2, "and both must have thrown and been caught");
    for s in stacks {
        assert_eq!(frame_url(s), "about:blank", "a document with no URL names its modules `about:blank`, not the engine");
        assert!(!s.to_ascii_lowercase().contains("korvus"), "and no frame names the engine, got: {s}");
    }
}

/// The same `frameUrl` as the JS helper, for assertions that need it on the Rust side: the URL of the
/// FIRST stack frame, with `:line:col` stripped.
fn frame_url(stack: &str) -> String {
    let Some(first) = stack.lines().nth(1) else { return "NO-FRAME".to_string() };
    let f = first.trim_start().trim_start_matches("at ").trim();
    let f = f.rsplit_once(':').map(|(a, _)| a).unwrap_or(f);
    f.rsplit_once(':').map(|(a, _)| a).unwrap_or(f).to_string()
}

async fn serve(listener: TcpListener, content_type: &'static str, body: &'static str) {
    loop {
        let (mut s, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(resp.as_bytes()).await;
        let _ = s.flush().await;
    }
}
