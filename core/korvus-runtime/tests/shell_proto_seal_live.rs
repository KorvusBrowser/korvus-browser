//! The interface-prototype SEAL: a page cannot choose what the engine mints on.
//!
//! # What this gate stands over
//!
//! `binding::protos::shell_proto` is how ~24 sites in `core/**` find the prototype to MINT an
//! instance on, and it used to find it at `globalThis[Iface].prototype` — a property the page can
//! write. A census measured 71 of 82 page-reachable mints
//! landing on a prototype the page supplied: wrong `[[Prototype]]`, wrong `Symbol.toStringTag`, no
//! interface member on the object, and an engine-minted object in page-controlled hands.
//!
//! `webapi::seal_interface_protos` snapshots the realm's finished globals into a V8 Private at the
//! end of each realm's install block, and `shell_proto` reads that. This file is what keeps it true.
//!
//! # Every expectation here is Chrome's measured answer
//!
//! Headless `Google Chrome 150.0.7871.46`, `--headless=new --dump-dom`, over a loopback server,
//! 2026-09-12. Chrome ACCEPTS `globalThis.MediaList = function Hijacked(){}` — the global really is
//! writable — and mints on the realm's own prototype regardless. The same probe page run in Chrome
//! and in this engine now agree on all 82 rows of the census.
//!
//! # The four failure modes of a table cutover
//!
//! 1. **An instance on the class template's prototype** — this unit's subject, and the reason the
//!    NON-hijacked assertions below are as load-bearing as the hijacked ones: changing which
//!    prototype an instance lands on must change NOTHING when no page is interfering.
//! 2. **A row accepting a foreign receiver** — no row body and no brand check is touched here; the
//!    seal only changes where a prototype is read from. `cssom_live.rs` and friends still hold that
//!    line.
//! 3. **A body reachable where it was a silent no-op** — this one DOES move, in the direction Chrome
//!    sets: `delete globalThis.MediaList` used to make `shell_proto` answer `None`, so `sheet.media`
//!    came back `undefined`. Chrome answers a real `MediaList`. `a_deleted_global_still_mints` is
//!    that measurement.
//! 4. **An install ORDER change discarding a decorator** — the seal is one statement appended to the
//!    end of each realm's install block. Nothing was reordered, and the seal installs nothing.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// One fresh `<style>`, so the mint is a real one and not a `[SameObject]` cache hit. The census
/// records that reusing an element is exactly how the defect hid: a baseline-first probe warms the
/// cache and the second read never reaches `shell_proto` at all.
const FRESH_SHEET: &str = r#"
function freshSheet(css) {
  const s = document.createElement("style");
  s.textContent = css;
  document.head.appendChild(s);
  return s.sheet;
}
function freshCanvas() {
  const c = document.createElement("canvas");
  c.width = 8; c.height = 8;
  document.body.appendChild(c);
  return c;
}
"#;

/// The brief's own reproduction, as a gate. Chrome: `[object MediaList]` on the real prototype, with
/// and without the reassignment.
#[tokio::test]
async fn a_hijacked_global_does_not_choose_the_minted_prototype() {
    let out = probe(&format!(
        r#"{FRESH_SHEET}
        const real = MediaList.prototype;
        globalThis.MediaList = function Hijacked() {{}};
        const m = freshSheet("@media screen {{ .a {{ color: red }} }}").media;
        print("wrote=" + (globalThis.MediaList.name === "Hijacked"));
        print("tag=" + Object.prototype.toString.call(m));
        print("proto=" + (Object.getPrototypeOf(m) === real));
        print("pageProto=" + (Object.getPrototypeOf(m) === globalThis.MediaList.prototype));
        print("member=" + (typeof m.mediaText));
        "#
    ))
    .await;
    assert_eq!(
        out,
        vec!["wrote=true", "tag=[object MediaList]", "proto=true", "pageProto=false", "member=string"],
        "a page that reassigns globalThis.MediaList must not receive an engine-minted object on its own prototype"
    );
}

/// One representative mint per family the census covers, all hijacked at once. Every tag is Chrome's.
#[tokio::test]
async fn every_family_mints_on_the_realms_own_prototype() {
    let out = probe(&format!(
        r#"{FRESH_SHEET}
        const names = ["CSSStyleRule", "CSSStyleDeclaration", "TextMetrics", "DOMMatrix",
                       "CanvasRenderingContext2D", "ValidityState", "TimeRanges", "TextTrackList",
                       "MessagePort", "MediaQueryList", "FragmentDirective"];
        const real = {{}};
        for (const n of names) real[n] = globalThis[n].prototype;
        for (const n of names) globalThis[n] = function Hijacked() {{}};

        const sheet = freshSheet(".a {{ color: red }}");
        const ctx = freshCanvas().getContext("2d");
        const input = document.createElement("input");
        document.body.appendChild(input);
        const video = document.createElement("video");
        document.body.appendChild(video);

        const minted = {{
          CSSStyleRule: sheet.cssRules[0],
          CSSStyleDeclaration: sheet.cssRules[0].style,
          TextMetrics: ctx.measureText("x"),
          DOMMatrix: ctx.getTransform(),
          CanvasRenderingContext2D: ctx,
          ValidityState: input.validity,
          TimeRanges: video.buffered,
          TextTrackList: video.textTracks,
          MessagePort: new MessageChannel().port1,
          MediaQueryList: matchMedia("(min-width: 1px)"),
          FragmentDirective: document.implementation.createHTMLDocument("x").fragmentDirective,
        }};
        for (const n of names) {{
          print(n + "=" + Object.prototype.toString.call(minted[n]) + " " + (Object.getPrototypeOf(minted[n]) === real[n]));
        }}
        "#
    ))
    .await;
    assert_eq!(
        out,
        vec![
            "CSSStyleRule=[object CSSStyleRule] true",
            "CSSStyleDeclaration=[object CSSStyleDeclaration] true",
            "TextMetrics=[object TextMetrics] true",
            "DOMMatrix=[object DOMMatrix] true",
            "CanvasRenderingContext2D=[object CanvasRenderingContext2D] true",
            "ValidityState=[object ValidityState] true",
            "TimeRanges=[object TimeRanges] true",
            "TextTrackList=[object TextTrackList] true",
            "MessagePort=[object MessagePort] true",
            "MediaQueryList=[object MediaQueryList] true",
            "FragmentDirective=[object FragmentDirective] true",
        ],
    );
}

/// The worst row in the census: `ensure_validity_flags` is a LAZY install, so with the global
/// hijacked it used to define twelve NATIVE getters onto the page's own object. The page's
/// prototype must come back untouched.
#[tokio::test]
async fn a_lazy_install_does_not_write_onto_the_pages_object() {
    let out = probe(
        r#"
        const Hijacked = function Hijacked() {};
        globalThis.ValidityState = Hijacked;
        const input = document.createElement("input");
        input.required = true;
        document.body.appendChild(input);
        const v = input.validity;
        print("pageProtoKeys=" + Object.getOwnPropertyNames(Hijacked.prototype).sort().join(","));
        print("valueMissing=" + v.valueMissing);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["pageProtoKeys=constructor", "valueMissing=true"],
        "the engine must not define its native validity getters onto a prototype the page authored"
    );
}

/// Failure mode 3, and the one behaviour that legitimately moves. Chrome's answers, measured
/// 2026-09-12 with `deleted.html`; this engine now matches them byte for byte.
#[tokio::test]
async fn a_deleted_global_still_mints() {
    let out = probe(&format!(
        r#"{FRESH_SHEET}
        const realML = MediaList.prototype, realTM = TextMetrics.prototype, realTR = TimeRanges.prototype;
        delete globalThis.MediaList;
        delete globalThis.TextMetrics;
        delete globalThis.TimeRanges;
        print("gone=" + (typeof globalThis.MediaList) + "," + (typeof globalThis.TextMetrics) + "," + (typeof globalThis.TimeRanges));
        const m = freshSheet("@media screen {{ .a {{ color: red }} }}").media;
        print("media=" + Object.prototype.toString.call(m) + " " + (Object.getPrototypeOf(m) === realML));
        const tm = freshCanvas().getContext("2d").measureText("x");
        print("textMetrics=" + Object.prototype.toString.call(tm) + " " + (Object.getPrototypeOf(tm) === realTM) + " " + (typeof tm.width));
        const video = document.createElement("video");
        document.body.appendChild(video);
        print("timeRanges=" + Object.prototype.toString.call(video.buffered) + " " + (Object.getPrototypeOf(video.buffered) === realTR) + " " + video.buffered.length);
        "#
    ))
    .await;
    assert_eq!(
        out,
        vec![
            "gone=undefined,undefined,undefined",
            "media=[object MediaList] true",
            "textMetrics=[object TextMetrics] true number",
            "timeRanges=[object TimeRanges] true 0",
        ],
    );
}

/// Failure mode 1, stated positively: with NOBODY interfering, every mint is exactly where it was.
/// This is the half of the gate that would catch a seal recording a stale prototype.
#[tokio::test]
async fn the_uninterfered_case_is_unchanged() {
    let out = probe(&format!(
        r#"{FRESH_SHEET}
        const sheet = freshSheet(".a {{ color: red }}");
        const ctx = freshCanvas().getContext("2d");
        const cases = [
          ["CSSStyleSheet", sheet],
          ["MediaList", sheet.media],
          ["CSSStyleRule", sheet.cssRules[0]],
          ["CSSStyleDeclaration", sheet.cssRules[0].style],
          ["CanvasRenderingContext2D", ctx],
          ["TextMetrics", ctx.measureText("x")],
          ["DOMMatrix", ctx.getTransform()],
          ["MessagePort", new MessageChannel().port1],
          ["URL", new URL("http://a/")],
          ["XMLHttpRequest", new XMLHttpRequest()],
        ];
        for (const [n, v] of cases) {{
          const ok = Object.getPrototypeOf(v) === globalThis[n].prototype
                  && Object.prototype.toString.call(v) === "[object " + n + "]"
                  && v.constructor === globalThis[n];
          print(n + "=" + ok);
        }}
        "#
    ))
    .await;
    assert_eq!(
        out,
        vec![
            "CSSStyleSheet=true", "MediaList=true", "CSSStyleRule=true", "CSSStyleDeclaration=true",
            "CanvasRenderingContext2D=true", "TextMetrics=true", "DOMMatrix=true", "MessagePort=true",
            "URL=true", "XMLHttpRequest=true",
        ],
    );
}

// ── the worker realm ────────────────────────────────────────────────────────────────────────────

async fn worker_probe(body: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(body).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
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
        .unwrap_or_else(|| format!("no result; output={out:?}"))
}

/// The seal is one function called from BOTH realms (invariant I8), so the worker gets the same
/// answer. Measured against Chrome with the same worker script over a loopback server: Chrome says
/// `[object MessagePort]` on the real prototype, and so does this.
///
/// This one is worth its runtime: with the worker seal removed and everything else in place, the
/// same script answers `{"tag":"[object Object]","protoIsHijack":true}`.
#[tokio::test]
async fn the_worker_realm_is_sealed_too() {
    let got = worker_probe(
        r#"
        const real = MessagePort.prototype;
        const Hijacked = function Hijacked() {};
        globalThis.MessagePort = Hijacked;
        const port = new MessageChannel().port1;
        self.postMessage(
          Object.prototype.toString.call(port) + " " +
          (Object.getPrototypeOf(port) === real) + " " +
          (Object.getPrototypeOf(port) === Hijacked.prototype));
        "#,
    )
    .await;
    assert_eq!(got, "[object MessagePort] true false");
}
