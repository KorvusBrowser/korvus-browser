//! THE ENGINE'S OWN LOOKUPS DO NOT READ A GLOBAL THE PAGE CAN REASSIGN.
//!
//! When the engine constructs a Web API object for itself -- the `PointerEvent` `element.click()`
//! dispatches, a `MessageEvent`, a `DOMException` it throws -- it takes the interface object from
//! `binding::protos::iface_object`, the realm's interface object as its install left it (sealed with the
//! prototypes), not from `globalThis[name]`, which a page can overwrite. It used to read the global
//! (`global_get`, 71 sites until `93de4814`), so a page that reassigned `PointerEvent` silenced every
//! `click()` in it.
//!
//! MEASURED, not assumed: Google Chrome 150.0.7871.46 `--headless=new --dump-dom` over a 127.0.0.1 page,
//! 2026-09-28, runs the probe below and answers `true:PointerEvent` -- the click is dispatched, and it is
//! a real `PointerEvent`. The pre-migration binary (`f1681626`) answered `no event`.
use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

const PROBE: &str = r#"(() => {
  const Orig = PointerEvent;
  let got = 'no event';
  document.body.addEventListener('click', (e) => { got = (e instanceof Orig) + ':' + e.constructor.name; });
  window.PointerEvent = function Fake() {};
  try { document.body.click(); } catch (x) { got = 'throw:' + x.message; }
  return got;
})()"#;

#[tokio::test]
async fn a_reassigned_interface_global_does_not_steer_the_engines_own_events() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.load_document(PAGE).unwrap();
    let v = e.eval_value("iface-object", PROBE.to_string()).unwrap();
    assert_eq!(v.as_str(), Some("true:PointerEvent"), "Chrome 150 dispatches a real PointerEvent after `window.PointerEvent` is reassigned");
}
