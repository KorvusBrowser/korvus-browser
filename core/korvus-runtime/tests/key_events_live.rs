//! LIVE gate: a typed sequence -- Shift, Ctrl, `'@'`, Enter in a form -- produces the
//! event rows stock Chrome 150 produces for the same REAL keys, field for field.
//!
//! # The oracle
//!
//! `fixtures/key-events/page.html` is a form with one text field `#t` (focused at load) and, by its
//! `?v=` query, a submit `<button id=go>` (`button`), nothing else (`single`), or a second text field
//! (`two`). A capturing listener on `window` records one row per `keydown` / `keypress` / `keyup` /
//! `beforeinput` / `input` / `change` / `click` / `submit`: type, target, constructor, `key`, `code`,
//! `keyCode`, `which`, `charCode`, the four modifier booleans, `getModifierState` for Shift, Control,
//! Alt, Meta, CapsLock, NumLock, AltGraph and Accel, `repeat`, `location`, `isComposing`, `isTrusted`,
//! `bubbles`, `cancelable`, `composed`, `detail`, `view === window`, `data`, `inputType`, the pointer
//! fields, `submitter`, and the field's value and selection at that instant.
//!
//! **Expectations recorded from Google Chrome 150.0.7871.46 on 2026-10-11** with REAL keys: Chrome
//! headed on a private Xvfb (no window manager), `--ozone-platform=x11`, NO DevTools client at any
//! point; X11 XTest pressed Shift+a, b, Shift+2, Ctrl+a, x, Return into the field
//! (`fixtures/key-events/record-x11.py`). Three runs of each variant, byte-identical; the fixture is
//! the first. What they say, beyond the per-key numbers:
//!
//! ```text
//! Shift's keydown          shiftKey true, keyCode 16, location 1     (the press already counts)
//! Shift's keyup            shiftKey false
//! 'A' / '@' with Shift     shiftKey true on keydown, keypress and keyup; '@' is Digit2, keyCode 50
//! Ctrl+a                   keydown + keyup, NO keypress, no edit; the selection is 0..3 at the keyup
//! 'x' after Ctrl+a         replaces the selection: value "x"
//! Enter (button)           keydown, keypress 13/13, beforeinput insertLineBreak, change,
//!                          click on #go (PointerEvent, pointerId -1, pointerType ""), submit (submitter #go)
//! Enter (single)           ... change, submit (no submitter)
//! Enter (two)              ... change, and nothing else
//! every key event          composed, view === window;  input events composed, view null
//! ```
//!
//! # The drivers
//!
//! The same sequence reaches the engine two ways, and both must produce Chrome's rows:
//!
//! * [`Engine::dispatch_key_event`] -- what CDP `Input.dispatchKeyEvent` calls -- with the parameters a
//!   client sends for those keys (`modifiers`, `windowsVirtualKeyCode`, `location`, `text`), every one
//!   of which the CDP arm used to drop;
//! * [`Engine::dispatch_key`] -- the PLATFORM keyboard: only `(type, key, code)`, the engine supplying
//!   the virtual key, the location, the held modifiers and what the key types, as Chrome's X11 input does.

use engine::Engine;
use webapi::input::KeyInput;

const CHROME_BUNDLE: &str = "chrome-152-linux-20261016T1022";
const PAGE: &str = include_str!("fixtures/key-events/page.html");

async fn page(variant: &str) -> Engine {
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the bundle");
    let mut e = Engine::new(Box::new(p));
    let url = format!("http://127.0.0.1/keys?v={variant}");
    e.set_url(&url);
    e.load_page_response(PAGE.as_bytes(), &url, &[], &[]).await.expect("load");
    e
}

async fn rows(e: &mut Engine) -> Vec<serde_json::Value> {
    let got = e
        .eval_value_settled("rows", "JSON.stringify(window.__rows)".to_string(), std::time::Duration::from_secs(5))
        .await
        .expect("eval");
    serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list")
}

fn want(variant: &str) -> Vec<serde_json::Value> {
    let src = match variant {
        "button" => include_str!("fixtures/key-events/chrome-150-button.json"),
        "single" => include_str!("fixtures/key-events/chrome-150-single.json"),
        _ => include_str!("fixtures/key-events/chrome-150-two.json"),
    };
    serde_json::from_str(src).expect("the recorded Chrome rows")
}

fn compare(variant: &str, driver: &str, got: &[serde_json::Value]) {
    let want = want(variant);
    let differ: Vec<String> = got
        .iter()
        .zip(&want)
        .enumerate()
        .filter(|(_, (g, w))| g != w)
        .map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}"))
        .collect();
    assert!(
        differ.is_empty() && got.len() == want.len(),
        "{variant} via {driver}: {} rows vs Chrome's {}, {} differ:\n{}",
        got.len(),
        want.len(),
        differ.len(),
        differ.join("\n")
    );
}

/// The CDP parameters a client sends for one key: `(type, key, code, text, windowsVirtualKeyCode,
/// modifiers, location)`. Shift = 8 and Ctrl = 2 in CDP's bitmask.
const CDP_SEQUENCE: &[(&str, &str, &str, &str, i64, i64, i64)] = &[
    ("keyDown", "Shift", "ShiftLeft", "", 16, 8, 1),
    ("keyDown", "A", "KeyA", "A", 65, 8, 0),
    ("keyUp", "A", "KeyA", "", 65, 8, 0),
    ("keyUp", "Shift", "ShiftLeft", "", 16, 0, 1),
    ("keyDown", "b", "KeyB", "b", 66, 0, 0),
    ("keyUp", "b", "KeyB", "", 66, 0, 0),
    ("keyDown", "Shift", "ShiftLeft", "", 16, 8, 1),
    ("keyDown", "@", "Digit2", "@", 50, 8, 0),
    ("keyUp", "@", "Digit2", "", 50, 8, 0),
    ("keyUp", "Shift", "ShiftLeft", "", 16, 0, 1),
    ("keyDown", "Control", "ControlLeft", "", 17, 2, 1),
    ("keyDown", "a", "KeyA", "", 65, 2, 0),
    ("keyUp", "a", "KeyA", "", 65, 2, 0),
    ("keyUp", "Control", "ControlLeft", "", 17, 0, 1),
    ("keyDown", "x", "KeyX", "x", 88, 0, 0),
    ("keyUp", "x", "KeyX", "", 88, 0, 0),
    ("keyDown", "Enter", "Enter", "\r", 13, 0, 0),
    ("keyUp", "Enter", "Enter", "", 13, 0, 0),
];

/// The same keys from the platform keyboard: `(type, key, code)` and nothing else.
const PLATFORM_SEQUENCE: &[(&str, &str, &str)] = &[
    ("keyDown", "Shift", "ShiftLeft"),
    ("keyDown", "A", "KeyA"),
    ("keyUp", "A", "KeyA"),
    ("keyUp", "Shift", "ShiftLeft"),
    ("keyDown", "b", "KeyB"),
    ("keyUp", "b", "KeyB"),
    ("keyDown", "Shift", "ShiftLeft"),
    ("keyDown", "@", "Digit2"),
    ("keyUp", "@", "Digit2"),
    ("keyUp", "Shift", "ShiftLeft"),
    ("keyDown", "Control", "ControlLeft"),
    ("keyDown", "a", "KeyA"),
    ("keyUp", "a", "KeyA"),
    ("keyUp", "Control", "ControlLeft"),
    ("keyDown", "x", "KeyX"),
    ("keyUp", "x", "KeyX"),
    ("keyDown", "Enter", "Enter"),
    ("keyUp", "Enter", "Enter"),
];

async fn drive_cdp(e: &mut Engine) {
    for (ty, key, code, text, vk, mods, loc) in CDP_SEQUENCE {
        let input = KeyInput::from_cdp(key, code, text, Some(*vk), Some(*mods), false, false, Some(*loc));
        e.dispatch_key_event(ty, input).await.expect("dispatch").expect("a CDP key type");
    }
}

async fn drive_platform(e: &mut Engine) {
    for (ty, key, code) in PLATFORM_SEQUENCE {
        e.dispatch_key(ty, key, code, "").await.expect("dispatch");
    }
}

#[tokio::test]
async fn cdp_keys_in_a_form_with_a_button_are_chromes_rows() {
    let mut e = page("button").await;
    drive_cdp(&mut e).await;
    let got = rows(&mut e).await;
    compare("button", "CDP", &got);
}

#[tokio::test]
async fn cdp_keys_in_a_form_with_one_field_are_chromes_rows() {
    let mut e = page("single").await;
    drive_cdp(&mut e).await;
    let got = rows(&mut e).await;
    compare("single", "CDP", &got);
}

#[tokio::test]
async fn cdp_keys_in_a_form_with_two_fields_are_chromes_rows() {
    let mut e = page("two").await;
    drive_cdp(&mut e).await;
    let got = rows(&mut e).await;
    compare("two", "CDP", &got);
}

#[tokio::test]
async fn platform_keys_in_a_form_with_a_button_are_chromes_rows() {
    let mut e = page("button").await;
    drive_platform(&mut e).await;
    let got = rows(&mut e).await;
    compare("button", "the platform keyboard", &got);
}

#[tokio::test]
async fn platform_keys_in_a_form_with_one_field_are_chromes_rows() {
    let mut e = page("single").await;
    drive_platform(&mut e).await;
    let got = rows(&mut e).await;
    compare("single", "the platform keyboard", &got);
}

/// CDP's absent fields are Chrome's DEFAULTS, not values derived from the other fields. Measured with a
/// raw CDP client on stock Chrome 150 (2026-10-11, `fixtures/key-events/cdp-defaults.py` ->
/// `cdp-defaults-chrome-150.json`, which also holds the two refusals the CDP arm answers): `keyDown{key:"a",code:"KeyA",text:"a"}` with no
/// `windowsVirtualKeyCode` fires a `keydown` with `keyCode` 0 (its `keypress` still 97, from the text);
/// `keyDown{key:"Backspace"}` with no virtual key deletes NOTHING (Blink's key bindings read the virtual
/// key); `keyDown{key:"b"}` with no `text` types nothing and fires no `keypress`.
#[tokio::test]
async fn absent_cdp_fields_are_chromes_defaults() {
    let mut e = page("two").await;
    let k = |key: &str, code: &str, text: &str| KeyInput::from_cdp(key, code, text, None, None, false, false, None);
    e.dispatch_key_event("keyDown", k("a", "KeyA", "a")).await.expect("dispatch");
    e.dispatch_key_event("keyUp", k("a", "KeyA", "")).await.expect("dispatch");
    e.dispatch_key_event("keyDown", k("Backspace", "Backspace", "")).await.expect("dispatch");
    e.dispatch_key_event("keyDown", k("b", "KeyB", "")).await.expect("dispatch");
    let got: Vec<String> = rows(&mut e)
        .await
        .iter()
        .map(|r| format!("{} {} {} {}", r[0].as_str().unwrap_or(""), r[5], r[7], r[38].as_str().unwrap_or("")))
        .collect();
    assert_eq!(
        got,
        [
            "keydown 0 0 ",
            "keypress 97 97 ",
            "beforeinput null null ",
            "input null null a",
            "keyup 0 0 a",
            "keydown 0 0 a",
            "keydown 0 0 a",
        ]
    );
}

/// Chrome refuses a key event type it has none of, before anything happens: no event fires.
#[tokio::test]
async fn an_unknown_key_event_type_dispatches_nothing() {
    let mut e = page("button").await;
    let input = KeyInput::from_cdp("a", "KeyA", "a", Some(65), None, false, false, None);
    assert_eq!(e.dispatch_key_event("keyPress", input).await.expect("dispatch"), None);
    assert!(rows(&mut e).await.is_empty());
}

/// User activation: a page nobody touched is not active; a modifier key alone does not
/// activate it (`keyboard_event_manager.cc:249-259`); any other key does, before its `keydown`
/// listener runs, and a navigation the page asks for inside the window is user-activated -- the
/// request carries `Sec-Fetch-User: ?1` (measured on Chrome 150: a trusted click that navigates at once
/// sends it, a script navigation with no input does not).
#[tokio::test]
async fn a_key_press_is_user_activation_and_its_navigation_is_user_activated() {
    let mut e = page("two").await;
    let state = |e: &mut Engine| {
        e.eval_value("ua", "[navigator.userActivation.hasBeenActive, navigator.userActivation.isActive].join()".to_string())
            .expect("eval")
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(state(&mut e), "false,false", "an untouched page");
    // A script navigation with no input: not user-activated.
    e.eval_value("nav0", "location.href = '/script'; 0".to_string()).expect("eval");
    let nav = e.take_nav_request().expect("the script's navigation");
    assert!(!nav.user_activated, "a script navigation with no input is not user-activated");
    assert!(!engine::navigation_request(&nav, e.persona().as_ref()).user_activated);

    let shift = KeyInput::from_cdp("Shift", "ShiftLeft", "", Some(16), Some(8), false, false, Some(1));
    e.dispatch_key_event("keyDown", shift.clone()).await.expect("dispatch");
    assert_eq!(state(&mut e), "false,false", "a modifier key is not an activation");
    e.dispatch_key_event("keyUp", KeyInput { mods: Default::default(), ..shift }).await.expect("dispatch");

    e.eval_value(
        "seen",
        "window.__seen = null; addEventListener('keydown', () => { window.__seen = navigator.userActivation.isActive; }, {once: true}); 0"
            .to_string(),
    )
    .expect("eval");
    let b = KeyInput::from_cdp("b", "KeyB", "b", Some(66), None, false, false, None);
    e.dispatch_key_event("keyDown", b).await.expect("dispatch");
    assert_eq!(state(&mut e), "true,true");
    let seen = e.eval_value("seen?", "String(window.__seen)".to_string()).expect("eval");
    assert_eq!(seen.as_str(), Some("true"), "the keydown listener already sees the activation");

    e.eval_value("nav1", "location.href = '/after-key'; 0".to_string()).expect("eval");
    let nav = e.take_nav_request().expect("the navigation");
    assert!(nav.user_activated, "asked for inside the activation window");
    assert!(engine::navigation_request(&nav, e.persona().as_ref()).user_activated, "Sec-Fetch-User: ?1");
}

/// Enter on a focused link follows it (`html_anchor_element.cc:193-197`) with a trusted click -- and
/// the navigation it asks for is user-activated.
#[tokio::test]
async fn enter_on_a_focused_link_follows_it() {
    let mut e = page("two").await;
    e.eval_value(
        "link",
        "const a = document.createElement('a'); a.id = 'l'; a.href = '/linked'; a.textContent = 'l';\
         document.body.appendChild(a); a.focus(); window.__clicks = [];\
         a.addEventListener('click', (ev) => window.__clicks.push([ev.isTrusted, ev.constructor.name, ev.pointerId]));\
         a.addEventListener('click', (ev) => ev.preventDefault(), {once: true}); 0"
            .to_string(),
    )
    .expect("eval");
    let enter = KeyInput::from_cdp("Enter", "Enter", "\r", Some(13), None, false, false, None);
    e.dispatch_key_event("keyDown", enter.clone()).await.expect("dispatch");
    let clicks = e.eval_value("clicks", "JSON.stringify(window.__clicks)".to_string()).expect("eval");
    assert_eq!(clicks.as_str(), Some("[[true,\"PointerEvent\",-1]]"));
    // The first press was cancelled by the page; the second follows the link.
    e.dispatch_key_event("keyDown", enter).await.expect("dispatch");
    let nav = e.take_nav_request().expect("the link's navigation");
    assert!(nav.url.ends_with("/linked"), "{}", nav.url);
    assert!(nav.user_activated);
}
