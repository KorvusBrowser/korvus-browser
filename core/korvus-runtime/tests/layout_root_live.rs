//! LIVE gate: the root of the box tree as Chrome lays it out -- the initial containing block, the
//! root's margins, the viewport's scrolling area, `offsetLeft`/`offsetTop` from the offset parent, and
//! an absolutely positioned box's static position.
//!
//! Every expectation is Google Chrome 150.0.7871.46's answer on this machine on 2026-10-07, to the same
//! script (`fixtures/layout-root/*.js`), served from 127.0.0.1 in a `--window-size=1920,1060` window --
//! the persona's 1920x965 viewport exactly. Before this change korvus differed on 37 of the 47 rows:
//!
//! * `root`: `<html>`'s margin was ignored (its border box at 0, 0), and `documentElement.scrollHeight`
//!   ended at `<body>`'s border box (3008 where Chrome counts the root's margin box: 3016, 3086);
//! * `offset`: `offsetLeft`/`offsetTop` were measured from the canvas, never from the offset parent's
//!   padding edge (a child of a `position: relative` box: 37, 119 against 12, 36), and `<body>`'s were
//!   its margin (8, 10) where the spec and Chrome answer 0;
//! * `positioned`: an absolutely positioned or fixed box no positioned ancestor contains was laid out
//!   in `<html>`'s box, not the viewport (`height: 50%` was 58px where Chrome's is 482.5px);
//! * `static`: such a box with `auto` insets sat after `<html>`'s content, not where its own parent's
//!   flow would have put it.

use engine::Engine;

const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";

pub(crate) const FIXTURES: &[(&str, &str, &str)] = &[
    ("root", include_str!("fixtures/layout-root/root.js"), include_str!("fixtures/layout-root/root.chrome-152.json")),
    ("offset", include_str!("fixtures/layout-root/offset.js"), include_str!("fixtures/layout-root/offset.chrome-152.json")),
    ("positioned", include_str!("fixtures/layout-root/positioned.js"), include_str!("fixtures/layout-root/positioned.chrome-152.json")),
    ("static", include_str!("fixtures/layout-root/static.js"), include_str!("fixtures/layout-root/static.chrome-152.json")),
];

pub(crate) async fn answers(script: &str) -> Vec<serde_json::Value> {
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = Engine::new(Box::new(p));
    let url = "http://127.0.0.1/layout-root";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><html><head><meta charset=utf-8></head><body></body></html>", url, &[], &[]).await.expect("load");
    // a script may answer with a promise (one that waits for an animation, say): it is awaited
    let got = e
        .eval_value_settled("r", format!("Promise.resolve({script}).then(r => JSON.stringify(r))"), std::time::Duration::from_secs(10))
        .await
        .expect("eval");
    serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list")
}

#[tokio::test]
async fn the_root_lays_out_as_chrome_150_lays_it_out() {
    let mut differ = Vec::new();
    for (name, script, chrome) in FIXTURES {
        let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
        let got = answers(script).await;
        assert_eq!(got.len(), want.len(), "{name}: answer count");
        for (g, w) in got.iter().zip(&want) {
            if g != w {
                differ.push(format!("{name}: korvus {g}\n    Chrome {w}"));
            }
        }
    }
    assert!(differ.is_empty(), "{} row(s) differ from Chrome 150's:\n  {}", differ.len(), differ.join("\n  "));
}
