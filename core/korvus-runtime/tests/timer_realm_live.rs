//! A window's map of active timers is the window whose `setTimeout` a script calls -- not the realm whose event or
//! script happens to be running.
//!
//! a bot-detection script's press & hold arms its progress `setInterval` from a PAGE function that a listener of the challenge
//! FRAME's event calls, and clears it from the interval's own callback. korvus keyed handles by the ENTERED realm, so
//! the interval went into the frame's map, the clear looked in the page's, missed, and the interval ran on every 32 ms
//! into the captcha's reset (`zn.frameContentDocument` undefined there: a TypeError PX reported as a clientError, 11
//! per replayed session, 0 in Chrome). The handles are now the INCUMBENT realm's (the calling
//! function's), which is the window's own whenever a script calls its own window's method.
//!
//! Chrome 150.0.7871.46 on this machine, three runs alike: the page's two timeouts are 1
//! and 2; the frame's own bare, `window.`-qualified and strict-mode `setTimeout`s are the FRAME's 1, 2, 3; the page
//! function the frame's script calls gets the PAGE's 3; its interval stops after the third tick.

use engine::Engine;
use std::time::Duration;

const PROBE: &str = r#"<!doctype html><html><body><script>
var R = [];
var a = setTimeout(function () {}, 1000), b = setTimeout(function () {}, 1000);
R.push('page handles ' + a + ',' + b);
var ticks = 0, iv = 0;
window.arm = function () {
  iv = setInterval(function () { ticks++; if (ticks === 3) clearInterval(iv); }, 10);
  R.push('armed from frame: handle ' + iv);
};
var f = document.createElement('iframe');
document.body.appendChild(f);
var d = f.contentDocument; d.open();
d.write('<script>var x = setTimeout(function(){}, 1000); var y = window.setTimeout(function(){}, 1000); var z = (function(){ "use strict"; return setTimeout(function(){}, 1000); })(); parent.R.push("frame bare " + x + " explicit " + y + " strict " + z); parent.arm();<\/script>');
d.close();
setTimeout(function () { R.push('ticks ' + ticks); }, 400);
</script></body></html>"#;

#[tokio::test]
async fn a_page_function_called_from_a_frame_arms_and_clears_the_pages_interval() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(PROBE, "http://127.0.0.1:9/page.html", &[]).await.expect("load_page");
    for _ in 0..20 {
        let _ = e.pump(Duration::from_millis(50)).await;
    }
    let v = e.eval_value("timer-realm", "R.slice()".to_string()).expect("eval");
    let got: Vec<String> = v.as_array().map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
    assert_eq!(got, ["page handles 1,2", "frame bare 1 explicit 2 strict 3", "armed from frame: handle 3", "ticks 3"]);
}
