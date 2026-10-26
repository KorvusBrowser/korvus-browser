//! `isTrusted` as Blink makes it: `[LegacyUnforgeable]`, so each event interface's instance template carries its own
//! accessor -- an own, enumerable, non-configurable getter, one per interface and realm -- and each answers only for
//! an instance of its interface.
//!
//! The pages are evaluated UNMODIFIED in stock Chrome 152.0.7977.82 headless (`--dump-dom`); the expected objects are
//! its output. korvus gave every event ONE getter (`same`: all true) and answered `false` for any receiver at all.

use std::time::{Duration, Instant};

const SHAPE: &str = r#"<!doctype html><html><body><div id=d></div><script>
const r = {};
const e = new Event('x'), m = new MouseEvent('click'), c = new CustomEvent('c'), k = new KeyboardEvent('k');
class X extends Event {}; const x = new X('y');
const de = Object.getOwnPropertyDescriptor(e, 'isTrusted');
r.own = Object.getOwnPropertyNames(e);
r.ownM = Object.getOwnPropertyNames(m);
r.desc = [typeof de.get, de.set, de.enumerable, de.configurable, de.get.name, de.get.length, String(de.get)];
const g = de.get;
r.same = [g === Object.getOwnPropertyDescriptor(m, 'isTrusted').get, g === Object.getOwnPropertyDescriptor(c, 'isTrusted').get, g === Object.getOwnPropertyDescriptor(x, 'isTrusted').get, g === Object.getOwnPropertyDescriptor(k, 'isTrusted').get];
r.values = [e.isTrusted, m.isTrusted, x.isTrusted];
try { r.foreign = String(g.call({})); } catch (err) { r.foreign = err.name + ': ' + err.message; }
try { r.prim = String(g.call(1)); } catch (err) { r.prim = err.name + ': ' + err.message; }
r.proto = 'isTrusted' in Event.prototype;
r.keys = Object.keys(e);
r.ctorProto = Object.prototype.hasOwnProperty.call(g, 'prototype');
try { new g(); r.newg = 'ok'; } catch (err) { r.newg = err.name; }
r.created = document.createEvent('Event').isTrusted;
r.createdOwn = Object.getOwnPropertyNames(document.createEvent('MouseEvents'));
r.createdSame = Object.getOwnPropertyDescriptor(document.createEvent('Event'), 'isTrusted').get === g;
let clickTrusted = null; document.getElementById('d').addEventListener('click', ev => { clickTrusted = [ev.isTrusted, Object.getOwnPropertyDescriptor(ev, 'isTrusted').get === g]; });
document.getElementById('d').click(); r.click = clickTrusted;
window.__r = JSON.stringify(r);
</script></body></html>"#;

const SHAPE_CHROME_152: &str = r#"{"own":["isTrusted"],"ownM":["isTrusted"],"desc":["function",null,true,false,"get isTrusted",0,"function get isTrusted() { [native code] }"],"same":[false,false,true,false],"values":[false,false,false],"foreign":"TypeError: Illegal invocation","prim":"TypeError: Illegal invocation","proto":false,"keys":["isTrusted"],"ctorProto":false,"newg":"TypeError","created":false,"createdOwn":["isTrusted"],"createdSame":true,"click":[false,false]}"#;

const RECEIVERS: &str = r#"<!doctype html><html><body><script>
const r = {};
const e = new Event('x'), m = new MouseEvent('click'), p = new PointerEvent('pointerdown'), u = new UIEvent('u');
const G = o => Object.getOwnPropertyDescriptor(o, 'isTrusted').get;
const call = (g, o) => { try { return String(g.call(o)); } catch (err) { return err.name; } };
r.mOnE = call(G(m), e); r.eOnM = call(G(e), m); r.pOnM = call(G(p), m); r.mOnP = call(G(m), p); r.uOnM = call(G(u), m);
r.eOnProto = call(G(e), Event.prototype); r.eOnObjCreate = call(G(e), Object.create(Event.prototype));
r.same = [G(m) === G(new MouseEvent('a')), G(p) === G(m), G(u) === G(e)];
const f = document.createElement('iframe'); document.body.appendChild(f);
const fe = new f.contentWindow.Event('x');
r.frame = [G(fe) === G(e), call(G(fe), e), call(G(e), fe)];
r.names = [G(m).name, G(p).name];
window.__r = JSON.stringify(r);
</script></body></html>"#;

const RECEIVERS_CHROME_152: &str = r#"{"mOnE":"TypeError","eOnM":"false","pOnM":"TypeError","mOnP":"false","uOnM":"false","eOnProto":"TypeError","eOnObjCreate":"TypeError","same":[true,false,false],"frame":[false,"false","false"],"names":["get isTrusted","get isTrusted"]}"#;

async fn run(page: &str) -> serde_json::Value {
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")));
    e.set_url("https://example.test/");
    e.load_page_response(page.as_bytes(), "https://example.test/", &[], &[]).await.expect("load");
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return serde_json::from_str(s).expect("the page's object");
        }
    }
}

#[tokio::test]
async fn each_event_interface_has_its_own_is_trusted_getter() {
    let want: serde_json::Value = serde_json::from_str(SHAPE_CHROME_152).unwrap();
    assert_eq!(run(SHAPE).await, want, "isTrusted's shape");
}

#[tokio::test]
async fn an_is_trusted_getter_answers_only_for_its_interface() {
    let want: serde_json::Value = serde_json::from_str(RECEIVERS_CHROME_152).unwrap();
    assert_eq!(run(RECEIVERS).await, want, "which receivers each isTrusted getter answers for");
}
