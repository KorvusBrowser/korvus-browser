//! A dispatch runs the listeners registered when it reached the target, minus the ones removed since: DOM's "removed"
//! flag, Blink's `RegisteredEventListener::Removed()`. A listener removed and added again during the dispatch is a NEW
//! registration -- the old one is removed, the new one is not in the dispatch's snapshot -- so it does not run.
//!
//! The page is evaluated UNMODIFIED in stock Chrome 152.0.7977.82 headless (`--dump-dom`); the expected object is its
//! output. korvus asked the live list "is an equal listener still registered?" and ran the re-added one
//! (`readd_next`: `a b | a b`).

use std::time::{Duration, Instant};

const PAGE: &str = r#"<!doctype html><html><body><div id=d><span id=s></span></div><script>
const d = document.getElementById('d'), s = document.getElementById('s'); const res = {};
{ const log = []; const b = () => log.push('b');
  const a = () => { log.push('a'); d.removeEventListener('x', b); d.addEventListener('x', b); };
  d.addEventListener('x', a); d.addEventListener('x', b);
  d.dispatchEvent(new Event('x')); log.push('|'); d.dispatchEvent(new Event('x')); res.readd_next = log.join(' ');
  d.removeEventListener('x', a); d.removeEventListener('x', b); }
{ const log = []; const a = () => { log.push('a'); d.removeEventListener('y', a); d.addEventListener('y', a); };
  const c = () => log.push('c');
  d.addEventListener('y', a); d.addEventListener('y', c);
  d.dispatchEvent(new Event('y')); log.push('|'); d.dispatchEvent(new Event('y')); res.readd_self = log.join(' ');
  d.removeEventListener('y', a); d.removeEventListener('y', c); }
{ const log = []; const t = () => log.push('t');
  d.addEventListener('z', () => { log.push('cap'); s.removeEventListener('z', t); }, true); s.addEventListener('z', t);
  s.dispatchEvent(new Event('z')); res.remove_ahead = log.join(' '); }
{ const log = []; const o = () => { log.push('o'); d.addEventListener('w', o, {once: true}); };
  d.addEventListener('w', o, {once: true}); d.addEventListener('w', () => log.push('p'));
  d.dispatchEvent(new Event('w')); log.push('|'); d.dispatchEvent(new Event('w')); res.once_readd = log.join(' '); }
{ const log = []; const b = () => log.push('b');
  const a = () => { log.push('a'); d.removeEventListener('v', b); d.addEventListener('v', b, true); };
  d.addEventListener('v', a); d.addEventListener('v', b);
  d.dispatchEvent(new Event('v')); log.push('|'); d.dispatchEvent(new Event('v')); res.readd_capture = log.join(' '); }
{ const log = []; d.onclick = () => log.push('h1');
  d.addEventListener('click', () => { log.push('a'); d.onclick = () => log.push('h2'); });
  d.onclick = () => log.push('h0');
  d.dispatchEvent(new Event('click')); res.handler_swap = log.join(' '); }
window.__r = JSON.stringify(res);
</script></body></html>"#;

/// Chrome 152's output for [`PAGE`].
const CHROME_152: &str = r#"{"readd_next":"a | a","readd_self":"a c | c a","remove_ahead":"cap","once_readd":"o p | p o","readd_capture":"a | b a","handler_swap":"h0 a"}"#;

#[tokio::test]
async fn a_dispatch_skips_the_listeners_removed_since_it_reached_the_target() {
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
    assert_eq!(got, want, "which listeners each dispatch ran");
}
