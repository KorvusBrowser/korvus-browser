//! When a listener's microtasks run: after it returns to an EMPTY stack, and not before. Blink invokes every callback
//! inside a `v8::MicrotasksScope` and runs script inside one too, so a listener that returns into a script, or into a
//! callback the engine invoked, leaves its microtasks for that outer scope's end.
//!
//! The page is evaluated UNMODIFIED in stock Chrome 152.0.7977.82 headless (`--dump-dom`, three runs alike); the
//! expected object is its output. Two listeners each queue a microtask; every case dispatches to both. A bound
//! `dispatchEvent` or `click` run as a timer's handler, as a frame callback, or as another event's listener ran each
//! listener's microtask after it here (`L1 M1 L2 M2`): the check for an empty stack walked the stack, and a native
//! function the engine calls has no script frame under it.

use std::time::{Duration, Instant};

const PAGE: &str = r#"<!doctype html><html><body><div id=d></div><script>
const d = document.getElementById('d'); const res = {}; let log = [];
d.addEventListener('x', () => { log.push('L1'); queueMicrotask(() => log.push('M1')); });
d.addEventListener('x', () => { log.push('L2'); queueMicrotask(() => log.push('M2')); });
const wait = ms => new Promise(r => setTimeout(r, ms));
const steps = [
  ['script', () => { d.dispatchEvent(new Event('x')); log.push('S'); }],
  ['timer_bound', () => { setTimeout(d.dispatchEvent.bind(d, new Event('x')), 0); }],
  ['nested_bound', () => { const t = new EventTarget(); t.addEventListener('y', d.dispatchEvent.bind(d, new Event('x')));
      t.addEventListener('y', () => { log.push('T2'); queueMicrotask(() => log.push('TM2')); });
      setTimeout(EventTarget.prototype.dispatchEvent.bind(t, new Event('y')), 0); }],
  ['raf_bound', () => { requestAnimationFrame(d.dispatchEvent.bind(d, new Event('x'))); }],
  ['promise_bound', () => { Promise.resolve(new Event('x')).then(d.dispatchEvent.bind(d)); }],
  ['click_script', () => { d.addEventListener('click', () => { log.push('C1'); queueMicrotask(() => log.push('CM1')); }, {once: true});
      d.addEventListener('click', () => { log.push('C2'); queueMicrotask(() => log.push('CM2')); }, {once: true}); d.click(); log.push('S'); }],
  ['click_timer_bound', () => { d.addEventListener('click', () => { log.push('C1'); queueMicrotask(() => log.push('CM1')); }, {once: true});
      d.addEventListener('click', () => { log.push('C2'); queueMicrotask(() => log.push('CM2')); }, {once: true}); setTimeout(HTMLElement.prototype.click.bind(d), 0); }],
];
(async () => {
  for (const [name, f] of steps) { log = []; f(); await wait(60); res[name] = log.join(' '); }
  window.__r = JSON.stringify(res);
})();
</script></body></html>"#;

/// Chrome 152's output for [`PAGE`].
const CHROME_152: &str = r#"{"script":"L1 L2 S M1 M2","timer_bound":"L1 L2 M1 M2","nested_bound":"L1 L2 T2 M1 M2 TM2","raf_bound":"L1 L2 M1 M2","promise_bound":"L1 L2 M1 M2","click_script":"C1 C2 S CM1 CM2","click_timer_bound":"C1 C2 CM1 CM2"}"#;

#[tokio::test]
async fn a_listeners_microtasks_wait_for_the_script_or_callback_below_it() {
    let mut e = engine::Engine::new(Box::new(persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle")));
    e.set_url("https://example.test/");
    e.load_page_response(PAGE.as_bytes(), "https://example.test/", &[], &[]).await.expect("load");
    let start = Instant::now();
    let got = loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break s.to_string();
        }
    };
    let got: serde_json::Value = serde_json::from_str(&got).expect("the page's object");
    let want: serde_json::Value = serde_json::from_str(CHROME_152).unwrap();
    assert_eq!(got, want, "the order of each case's listeners and their microtasks");
}
