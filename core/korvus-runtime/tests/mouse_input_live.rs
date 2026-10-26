//! FIXER C6 -- a real mouse, as stock Chrome 150 turns it into DOM events, end to end.
//!
//! # The oracle
//!
//! `fixtures/mouse-input/chrome-150.json`: stock Chrome 150.0.7871.46 (`/usr/bin/google-chrome`), headed on a
//! private Xvfb with `--ozone-platform=x11` and NO DevTools attached, driven by REAL X input through XTEST (the
//! recorder drove stock Chrome; three runs gave byte-identical rows). The page is
//! `fixtures/mouse-input/page.html`: nested boxes, a closed shadow root, a button, two checkboxes and a label, a
//! pointer-capturing box and a same-origin frame, with a CAPTURING listener on each window (and on the shadow
//! root) recording every mouse, pointer, wheel, focus, input and change event -- type, interface, target,
//! relatedTarget, toElement, fromElement, srcElement, isTrusted, composed, bubbles, cancelable, view, detail,
//! button, buttons, which, client point, screen-minus-client less the window's content origin, page and offset
//! points, movement, composedPath, activeElement, the pointer fields and the coalesced/predicted counts -- plus
//! non-capturing enter/leave listeners on the document and on `#mid`. Timing values are left out; ORDER is not.
//!
//! The recording excludes two rows that were the X server's, not the page's: the Chrome window's own `blur` at
//! load and `focus` at the first click (Xvfb has no window manager; the first click focuses the window).
//!
//! # What this drives
//!
//! The same sequence, as platform events through `Engine::mouse_event` / `Engine::mouse_wheel` -- the entry the
//! CDP `Input.dispatchMouseEvent` arm reaches -- with the click counts the X11 platform gave (a second press
//! within 500 ms and 2 px is count 2). The page is served from a loopback URL so it is a secure context, where
//! Chrome fires `pointerrawupdate`.

use engine::Engine;
use webapi::input::{held_button, Modifiers, MouseKind};

const PAGE: &str = include_str!("fixtures/mouse-input/page.html");
const CHROME: &str = include_str!("fixtures/mouse-input/chrome-150.json");

/// One step of the recorded input, in the page's client coordinates.
enum Step {
    /// A move to the step's point (the point travels beside the step).
    Move,
    Down(i32, i32),
    Up(i32, i32),
    Wheel(f64),
}

/// `drive.py`'s sequence (the calibration move first, to the client point Chrome reported for it).
fn sequence(calibration: (f64, f64)) -> Vec<(f64, f64, Step)> {
    use Step::*;
    let mut s = vec![(calibration.0, calibration.1, Move)];
    let mut at = |x: f64, y: f64, st: Step| s.push((x, y, st));
    at(300.0, 600.0, Move);
    at(150.0, 150.0, Move);
    at(100.0, 100.0, Move);
    // click, then a second click inside 500 ms and 2 px: click counts 1 and 2
    at(100.0, 100.0, Down(0, 1));
    at(100.0, 100.0, Up(0, 1));
    at(100.0, 100.0, Down(0, 2));
    at(100.0, 100.0, Up(0, 2));
    // press on #inner, release on #mid: the click goes to their common ancestor
    at(150.0, 150.0, Move);
    at(150.0, 150.0, Down(0, 1));
    at(300.0, 220.0, Move);
    at(300.0, 220.0, Up(0, 1));
    // right click on #outer
    at(400.0, 300.0, Move);
    at(400.0, 300.0, Down(2, 1));
    at(400.0, 300.0, Up(2, 1));
    // the closed shadow root, the button, the checkbox, the label
    for (x, y) in [(560.0, 70.0), (80.0, 395.0), (170.0, 395.0), (260.0, 395.0)] {
        at(x, y, Move);
        at(x, y, Down(0, 1));
        at(x, y, Up(0, 1));
    }
    // pointer capture: press on #cap, drag off it, release
    at(90.0, 470.0, Move);
    at(90.0, 470.0, Down(0, 1));
    at(300.0, 470.0, Move);
    at(300.0, 470.0, Up(0, 1));
    // into the frame and click there, then out again
    at(550.0, 250.0, Move);
    at(550.0, 250.0, Down(0, 1));
    at(550.0, 250.0, Up(0, 1));
    at(300.0, 600.0, Move);
    at(150.0, 150.0, Move);
    at(150.0, 150.0, Wheel(53.0));
    s
}

/// The page loaded at a loopback URL (a secure context), its frame loaded, and the rows the input made.
async fn drive() -> Vec<String> {
    let chrome: serde_json::Value = serde_json::from_str(CHROME).expect("fixture json");
    let cal = &chrome["calibration_client"];
    let calibration = (cal[0].as_f64().unwrap(), cal[1].as_f64().unwrap());

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.set_url("http://127.0.0.1:9/");
    e.load_html_injected(PAGE, &[]).expect("load page");
    e.run().await.expect("run page");
    let mut held: u16 = 0;
    for (x, y, step) in sequence(calibration) {
        match step {
            Step::Move => {
                e.mouse_event(MouseKind::Move, x, y, held_button(held), held, 0, Modifiers::default()).expect("move");
            }
            Step::Down(button, count) => {
                held |= webapi::input::button_bit(button);
                e.mouse_event(MouseKind::Down, x, y, button, held, count, Modifiers::default()).expect("down");
            }
            Step::Up(button, count) => {
                held &= !webapi::input::button_bit(button);
                e.mouse_event(MouseKind::Up, x, y, button, held, count, Modifiers::default()).expect("up");
            }
            Step::Wheel(dy) => {
                e.mouse_wheel(x, y, 0.0, dy, held, Modifiers::default()).expect("wheel");
            }
        }
        e.run().await.expect("run step");
    }
    e.eval("mouse-input-rows", "print(JSON.stringify(globalThis.__rows))".to_string()).expect("eval rows");
    e.run().await.expect("run rows");
    let out = e.take_output();
    let line = out.iter().rev().find(|l| l.starts_with('[')).unwrap_or_else(|| panic!("no rows printed: {out:?}"));
    serde_json::from_str(line).expect("rows json")
}

/// Every row Chrome recorded, in Chrome's order -- the whole of it, compared row by row.
#[tokio::test]
async fn a_real_mouse_makes_the_events_chrome_makes() {
    let chrome: serde_json::Value = serde_json::from_str(CHROME).expect("fixture json");
    let want: Vec<String> = chrome["rows"].as_array().unwrap().iter().map(|r| r.as_str().unwrap().to_string()).collect();
    let got = drive().await;
    let first_diff = want.iter().zip(got.iter()).position(|(w, g)| w != g);
    if let Some(i) = first_diff {
        // Both sides, row by row, from a few rows before the first difference to a few after; `>>` marks a row
        // that differs, and the field that differs is named.
        let from = i.saturating_sub(4);
        let to = (i + 8).min(want.len().max(got.len()));
        let mut report = String::new();
        for k in from..to {
            let w = want.get(k).map(String::as_str).unwrap_or("<none>");
            let g = got.get(k).map(String::as_str).unwrap_or("<none>");
            let mark = if w == g { "  " } else { ">>" };
            report.push_str(&format!("{mark} {k} chrome: {w}\n{mark} {k} korvus: {g}\n"));
            if w != g {
                let fields: Vec<String> = w
                    .split('|')
                    .zip(g.split('|'))
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(n, (a, b))| format!("field {n}: chrome {a:?} korvus {b:?}"))
                    .collect();
                report.push_str(&format!("   {}\n", fields.join("; ")));
            }
        }
        panic!("row {i} differs (of {} Chrome rows, {} korvus rows)\n{report}", want.len(), got.len());
    }
    assert_eq!(got.len(), want.len(), "korvus made {} rows where Chrome made {}; the tail: {:?}", got.len(), want.len(), got.iter().skip(want.len().min(got.len())).take(5).collect::<Vec<_>>());
}

/// The rows a behavioural sensor decides on, asserted on their own so a failure names them: a `mousemove` at the
/// element under the pointer, with `toElement` that element, `view` the window, `composed`, `which` 0, the
/// screen offset of the content area, and a whole composed path.
#[tokio::test]
async fn a_mousemove_targets_the_element_under_the_pointer() {
    let rows = drive().await;
    let mv = rows
        .iter()
        .find(|r| r.starts_with("W|mousemove|MouseEvent|#inner|"))
        .unwrap_or_else(|| panic!("no mousemove at #inner: {rows:?}"));
    let f: Vec<&str> = mv.split('|').collect();
    assert_eq!(f[5], "#inner", "toElement is the target on a mousemove (Blink mouse_event.cc:364-382)");
    assert_eq!(f[7], "#inner", "srcElement is the target");
    assert_eq!(f[8], "true", "isTrusted");
    assert_eq!(f[9], "true", "composed");
    assert_eq!(f[12], "win", "view is the window");
    assert_eq!(f[16], "0", "which is 0 with no button held");
    assert!(mv.contains("|soff=0,0|"), "screenX/Y - clientX/Y is the content area's screen origin: {mv}");
    assert!(mv.contains("|path=#inner>#mid>#outer>body>html>#document>#window|"), "the whole path: {mv}");
}

/// `toElement` / `fromElement` on SCRIPT-dispatched events (Blink `mouse_event.cc:364-382`), from the
/// a bot sensor investigator's probe (`fixtures/mouse-input/toelement-probe.js`), recorded on stock Chrome 150 through
/// `abkit/probe/rec_page.sh` three times alike (`toelement.chrome-150.json`): korvus answered null for both on all
/// 19 rows' events, where Chrome answers the target or the related target by type, null on a `PointerEvent`.
#[tokio::test]
async fn to_and_from_element_follow_the_type_on_script_events() {
    let probe = include_str!("fixtures/mouse-input/toelement-probe.js");
    let want: serde_json::Value = serde_json::from_str(include_str!("fixtures/mouse-input/toelement.chrome-150.json")).unwrap();
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_html("<!doctype html><html><body></body></html>").expect("load");
    e.run().await.expect("run");
    e.eval("toelement", format!("print(JSON.stringify({probe}))")).expect("eval probe");
    e.run().await.expect("run probe");
    let out = e.take_output();
    let line = out.iter().rev().find(|l| l.starts_with('[')).unwrap_or_else(|| panic!("no probe output: {out:?}"));
    let got: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(got, want);
}
