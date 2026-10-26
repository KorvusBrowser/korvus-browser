//! Typing end to end, in a live engine: the keystroke reaches the FOCUSED element, the
//! field's value changes, and the events fire on it in the order a browser fires them.
//!
//! # The oracle
//!
//! Every expectation here was taken from **stock Chrome 150.0.7871.46**, driven over CDP
//! (`Input.insertText`, `Input.dispatchKeyEvent`, `Input.dispatchMouseEvent`), headed under Xvfb at
//! 1920x1080, launched by us with `--remote-debugging-port` rather than by Playwright's
//! launcher. None of it is read off a specification.
//!
//! A capturing listener on `window` for every keyboard / input / composition / focus event recorded
//! `type`, `target`, `constructor.name`, `keyCode`, `charCode`, `data`, `inputType` and the field's
//! `value` at that instant, **with a fresh navigation before each case**. The fresh navigation is
//! not hygiene: the first run of that probe answered "nothing focused" wrongly, because a selection
//! the previous case had left inside the field made Chrome re-focus it, and the contaminated
//! reading said `insertText` types at the body. It does not — it types nothing at all.
//!
//! Chrome's answers, verbatim from the run that settled each one:
//!
//! ```text
//! Input.insertText "ab" into a focused <input id=f>
//!   beforeinput  on #f   InputEvent  data="ab"  inputType="insertText"   cancelable   value=""
//!   textInput    on #f   TextEvent   data="ab"                           cancelable   value=""
//!   input        on #f   InputEvent  data="ab"  inputType="insertText"                value="ab"
//!
//! Input.dispatchKeyEvent keyDown{key:"a",text:"a"} then keyUp, same field
//!   keydown      on #f   KeyboardEvent  key="a"  keyCode=65  charCode=0
//!   keypress     on #f   KeyboardEvent  key="a"  keyCode=97  charCode=97
//!   beforeinput  on #f   InputEvent     data="a" inputType="insertText"
//!   textInput    on #f   TextEvent      data="a"
//!   input        on #f   InputEvent     data="a" inputType="insertText"  value="a"
//!   keyup        on #f   KeyboardEvent  key="a"  keyCode=65  charCode=0
//!
//! with NOTHING focused (activeElement === document.body, fresh page)
//!   Input.insertText        -> no event of any kind, no value change
//!   Input.dispatchKeyEvent  -> keydown / keypress / keyup on <body>, no beforeinput, no input
//!
//! Input.dispatchMouseEvent mousePressed/mouseReleased on #g while #f was focused
//!   mousedown  on #g   activeElement=#f
//!   blur       on #f   relatedTarget=#g   activeElement=BODY
//!   focusout   on #f   relatedTarget=#g   activeElement=BODY
//!   focus      on #g   relatedTarget=#f   activeElement=#g
//!   focusin    on #g   relatedTarget=#f   activeElement=#g
//!   mouseup    on #g
//!   click      on #g
//!
//! `change`, on a field whose value started as "seed" (frame focused by a pointer move first, or
//! the focus events do not dispatch at all -- which is how the first attempt at these four cases
//! produced four empty logs and proved nothing):
//!   type "XY" then blur                                    -> change, then blur, then focusout
//!   type "XY", Backspace twice (back to "seed"), then blur  -> NO change
//!   focus and blur with no typing                           -> NO change
//!   el.value = "PROG" from script, then blur                -> NO change
//!   type "XY", then el.value = "seed" from script, blur     -> NO change
//!   the change listener's phases                            -> 2 at the control, 3 at document
//! ```
//!
//! # The one declared difference: `textInput`
//!
//! Chrome fires a `textInput` between `beforeinput` and `input`, and this engine does not. It is a
//! `TextEvent`, an interface Chrome itself declares `newable: false` (the capture bundle records
//! that) and that this tree carries as a D2 stub row in the generated registry — `data` and
//! `initTextEvent` are both `BodyKind::Stub`. Firing a `textInput` whose object was an `InputEvent`
//! or a bare `Event` would be a forgery a page reads off `e.constructor.name` in one line, so the
//! difference is DECLARED here, asserted as a difference by
//! [`the_sequence_is_chromes_minus_exactly_one_declared_event`], and left for whoever implements
//! `TextEvent`. It is not filtered out of the comparison; it is the comparison's only residue.
//!
//! # What this asks that a unit test cannot
//!
//! `core/events/typing.rs`'s own tests pin the key-code table and the command mapping. This drives a
//! real `Engine` through the same entry points CDP reaches — `Engine::insert_text`,
//! `Engine::dispatch_key`, `Engine::click_at`, `Engine::type_into` — so the focus resolution, the
//! layout hit test, the event construction, the trusted flag, the selection and the `value` setter
//! are all in the loop.

use engine::Engine;

/// The fixture. Geometry is in the stylesheet because the elements need real boxes for the mouse
/// press to hit them, and `body { margin: 0 }` puts those boxes at known viewport coordinates.
const PAGE: &str = r#"<!doctype html><html><head><style>
  body { margin: 0 }
  #f, #g, #ro { display: block; width: 300px; height: 40px }
  #plain { display: block; width: 300px; height: 40px }
  #btn { display: block; width: 300px; height: 40px }
</style></head><body>
<input id="f" type="text">
<input id="g" type="text">
<input id="ro" type="text" readonly>
<div id="plain">not focusable</div>
<button id="btn">button</button>
</body></html>"#;

/// The listener every assertion reads. At-target only (`eventPhase === 2`), so one dispatch is
/// recorded once with its real target rather than once per ancestor listener — the same rule
/// `hover_live.rs` uses, and for the same reason.
const LISTEN: &str = r##"
globalThis.__log = [];
globalThis.__n = 0;
const label = (n) => n === null || n === undefined ? "null"
  : (n === document ? "#document" : (n === window ? "#window"
  : (n.id ? "#" + n.id : n.tagName.toLowerCase())));
globalThis.__label = label;
const TYPES = ["keydown", "keypress", "keyup", "beforeinput", "input", "textInput",
               "change", "focus", "blur", "focusin", "focusout"];
for (const el of [document, ...document.querySelectorAll("*")]) {
  for (const type of TYPES) {
    el.addEventListener(type, (ev) => {
      if (ev.eventPhase !== 2) return;
      globalThis.__n++;
      globalThis.__log.push({
        t: ev.type,
        target: label(ev.target),
        rel: ev.relatedTarget === undefined ? "-" : label(ev.relatedTarget),
        trusted: ev.isTrusted,
        ctor: ev.constructor.name,
        key: ev.key === undefined ? null : ev.key,
        keyCode: ev.keyCode === undefined ? null : ev.keyCode,
        charCode: ev.charCode === undefined ? null : ev.charCode,
        which: ev.which === undefined ? null : ev.which,
        data: ev.data === undefined ? null : ev.data,
        inputType: ev.inputType === undefined ? null : ev.inputType,
        cancelable: ev.cancelable,
        bubbles: ev.bubbles,
        value: document.getElementById("f").value
      });
    }, true);
  }
}
globalThis.__reset = () => { globalThis.__log = []; globalThis.__n = 0; };
globalThis.__seq = () => globalThis.__log.map((r) => r.t + "@" + r.target).join("|");
globalThis.__box = (id) => {
  const r = document.getElementById(id).getBoundingClientRect();
  return [Math.round(r.left + r.width / 2), Math.round(r.top + r.height / 2),
          Math.round(r.width), Math.round(r.height)].join(",");
};
globalThis.__active = () => globalThis.__label(document.activeElement);
"##;

/// A booted engine on [`PAGE`] with [`LISTEN`] installed and the log reset.
async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_html(PAGE).expect("load page");
    e.eval("typing-listen", LISTEN.to_string()).expect("eval listen");
    e.run().await.expect("run listen");
    e.take_output();
    e
}

/// Evaluate `expr` and return the single line it printed.
async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("typing-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert_eq!(out.len(), 1, "one line expected from {expr:?}, got {out:?}");
    out.into_iter().next().unwrap()
}

async fn run_js(e: &mut Engine, src: &str) {
    e.eval("typing-do", src.to_string()).expect("eval");
    e.run().await.expect("run");
    e.take_output();
}

/// Clear the log and the fields, then focus `id` (or nothing, for `""`).
async fn reset(e: &mut Engine, id: &str) {
    let focus = if id.is_empty() {
        "document.activeElement && document.activeElement.blur && document.activeElement.blur();".to_string()
    } else {
        format!("document.getElementById('{id}').focus();")
    };
    run_js(e, &format!(
        "for (const el of document.querySelectorAll('input')) el.value = '';\
         {focus} globalThis.__reset();"
    ))
    .await;
}

/// The log as `type@target` in order.
async fn seq(e: &mut Engine) -> Vec<String> {
    let joined = ask(e, "globalThis.__seq()").await;
    if joined.is_empty() { Vec::new() } else { joined.split('|').map(String::from).collect() }
}

/// One recorded row's field, JSON-encoded — so a `null` and the STRING `"null"` are told apart,
/// which is exactly the distinction an `InputEvent.data` of a deletion turns on.
async fn field(e: &mut Engine, i: usize, name: &str) -> String {
    ask(e, &format!("JSON.stringify(globalThis.__log[{i}] && globalThis.__log[{i}].{name})")).await
}

/// One recorded row's `relatedTarget`, as the listener's own label (`"null"` for none).
async fn rel(e: &mut Engine, i: usize) -> String {
    ask(e, &format!("globalThis.__log[{i}].rel")).await
}

async fn value_of(e: &mut Engine, id: &str) -> String {
    ask(e, &format!("document.getElementById('{id}').value")).await
}

/// The number of events the engine REPORTED dispatching, checked against what listeners actually
/// saw. Every assertion below that counts events goes through this, because an engine that reports
/// more than it delivers is the failure mode a tally can hide.
async fn reported(e: &mut Engine) -> usize {
    ask(e, "globalThis.__n").await.parse().expect("__n is a number")
}

// ================================================================================================
// 1. `Input.insertText` -- the path the interactivity sweep's typing step actually uses.
// ================================================================================================

/// Chrome: `beforeinput` then `input` on the focused field, both trusted, both carrying the WHOLE
/// string as `data` with `inputType: "insertText"` — and the value changes BETWEEN them.
#[tokio::test(start_paused = true)]
async fn insert_text_into_a_focused_input_changes_the_value_and_fires_chromes_events() {
    let mut e = page().await;
    reset(&mut e, "f").await;

    let n = e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run insert");

    // BOTH halves. Either one alone is a fake the brief names by name.
    assert_eq!(value_of(&mut e, "f").await, "parity", "the focused field's value must change");
    assert_eq!(
        seq(&mut e).await,
        vec!["beforeinput@#f", "input@#f"],
        "Chrome's sequence and targets, minus the declared `textInput`"
    );
    assert_eq!(n, 2, "the engine reports the two events it dispatched");
    assert_eq!(reported(&mut e).await, 2, "and a listener saw exactly those two");

    // The fields on each, as Chrome reported them.
    assert_eq!(field(&mut e, 0, "ctor").await, "\"InputEvent\"");
    assert_eq!(field(&mut e, 0, "data").await, "\"parity\"");
    assert_eq!(field(&mut e, 0, "inputType").await, "\"insertText\"");
    assert_eq!(field(&mut e, 0, "cancelable").await, "true", "beforeinput is the cancellation point");
    assert_eq!(field(&mut e, 0, "trusted").await, "true");
    assert_eq!(field(&mut e, 0, "value").await, "\"\"", "the value has NOT changed yet at beforeinput");
    assert_eq!(field(&mut e, 1, "ctor").await, "\"InputEvent\"");
    assert_eq!(field(&mut e, 1, "data").await, "\"parity\"");
    assert_eq!(field(&mut e, 1, "inputType").await, "\"insertText\"");
    assert_eq!(field(&mut e, 1, "cancelable").await, "false", "input is not cancelable");
    assert_eq!(field(&mut e, 1, "trusted").await, "true");
    assert_eq!(field(&mut e, 1, "value").await, "\"parity\"", "and HAS changed by input");
    assert_eq!(field(&mut e, 1, "bubbles").await, "true");
}

/// Chrome inserts the whole string in ONE edit: `"hello"` is one `beforeinput`/`input` pair with
/// `data: "hello"`, never five of anything. Humanising this method into a keystroke stream would
/// make this the only browser that types when asked to paste.
#[tokio::test(start_paused = true)]
async fn insert_text_is_one_edit_for_the_whole_string_not_a_keystroke_stream() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    e.insert_text("hello").await.expect("insert");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, vec!["beforeinput@#f", "input@#f"]);
    assert_eq!(field(&mut e, 0, "data").await, "\"hello\"");
    assert_eq!(value_of(&mut e, "f").await, "hello");
}

/// A second insert appends, because the selection advanced to the end of the first one.
#[tokio::test(start_paused = true)]
async fn consecutive_inserts_append_because_the_caret_moved() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    e.insert_text("ab").await.expect("insert");
    e.run().await.expect("run");
    e.insert_text("cd").await.expect("insert");
    e.run().await.expect("run");
    assert_eq!(value_of(&mut e, "f").await, "abcd");
}

/// Chrome, on a freshly navigated page with nothing ever focused: `Input.insertText` fires NOTHING
/// and changes nothing. It is not "types at the body" — an editing command with no editable target
/// has nowhere to go.
#[tokio::test(start_paused = true)]
async fn insert_text_with_nothing_focused_does_nothing_at_all() {
    let mut e = page().await;
    reset(&mut e, "").await;
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "body", "the ring is empty");

    let n = e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run");

    assert_eq!(n, 0);
    assert_eq!(seq(&mut e).await, Vec::<String>::new());
    assert_eq!(value_of(&mut e, "f").await, "");
}

/// Same for a focused element that is not an editable control: the `<button>` holds the ring and
/// the editing command still has nowhere to go.
#[tokio::test(start_paused = true)]
async fn insert_text_into_a_focused_non_editable_element_does_nothing() {
    let mut e = page().await;
    reset(&mut e, "btn").await;
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "#btn");
    let n = e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run");
    assert_eq!(n, 0);
    assert_eq!(seq(&mut e).await, Vec::<String>::new());
}

/// A readonly field ANNOUNCES the edit and refuses it: `beforeinput` fires, `input` does not, the
/// value is unchanged. Measured — this is not the same as "not editable".
#[tokio::test(start_paused = true)]
async fn a_readonly_field_announces_the_edit_and_refuses_it() {
    let mut e = page().await;
    reset(&mut e, "ro").await;
    e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, vec!["beforeinput@#ro"]);
    assert_eq!(value_of(&mut e, "ro").await, "");
}

// ================================================================================================
// 2. `Input.dispatchKeyEvent` -- one key, and the default action it runs.
// ================================================================================================

/// Chrome's six for one printable key, minus the declared `textInput`: `keydown`, `keypress`,
/// `beforeinput`, `input`, `keyup`, every one of them on the focused field.
#[tokio::test(start_paused = true)]
async fn a_printable_key_fires_keydown_keypress_beforeinput_input_keyup_on_the_field() {
    let mut e = page().await;
    reset(&mut e, "f").await;

    let down = e.dispatch_key("keyDown", "a", "KeyA", "a").await.expect("down");
    e.run().await.expect("run down");
    let up = e.dispatch_key("keyUp", "a", "KeyA", "").await.expect("up");
    e.run().await.expect("run up");

    assert_eq!(value_of(&mut e, "f").await, "a");
    assert_eq!(
        seq(&mut e).await,
        vec!["keydown@#f", "keypress@#f", "beforeinput@#f", "input@#f", "keyup@#f"]
    );
    assert_eq!(down, 4, "keydown + keypress + beforeinput + input");
    assert_eq!(up, 1);
    assert_eq!(reported(&mut e).await, 5);

    // keydown carries the VIRTUAL KEY code; the keypress between carries the CHARACTER, in both
    // `keyCode` and `charCode`. They differ, and a page that branches on the legacy numbers reads
    // the difference.
    assert_eq!(field(&mut e, 0, "keyCode").await, "65");
    assert_eq!(field(&mut e, 0, "charCode").await, "0");
    assert_eq!(field(&mut e, 0, "which").await, "65");
    assert_eq!(field(&mut e, 1, "keyCode").await, "97");
    assert_eq!(field(&mut e, 1, "charCode").await, "97");
    assert_eq!(field(&mut e, 4, "keyCode").await, "65");
    assert_eq!(field(&mut e, 4, "charCode").await, "0");
    for i in 0..5 {
        assert_eq!(field(&mut e, i, "trusted").await, "true", "event {i} must be trusted");
    }
}

/// `rawKeyDown` + `char` + `keyUp` is the same six events as `keyDown{text}` + `keyUp` — measured,
/// and the reason the CDP arm maps all four types rather than two.
#[tokio::test(start_paused = true)]
async fn raw_key_down_plus_char_is_the_same_sequence_as_key_down_with_text() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    e.dispatch_key("rawKeyDown", "b", "KeyB", "").await.expect("raw");
    e.run().await.expect("run");
    e.dispatch_key("char", "b", "", "b").await.expect("char");
    e.run().await.expect("run");
    e.dispatch_key("keyUp", "b", "KeyB", "").await.expect("up");
    e.run().await.expect("run");
    assert_eq!(
        seq(&mut e).await,
        vec!["keydown@#f", "keypress@#f", "beforeinput@#f", "input@#f", "keyup@#f"]
    );
    assert_eq!(value_of(&mut e, "f").await, "b");
}

/// Chrome with nothing focused: the key events fire on `<body>`, and nothing else does. Not on
/// `document` — that is where this engine used to put them, and `e.target` is observable.
#[tokio::test(start_paused = true)]
async fn a_key_with_nothing_focused_targets_the_body_and_edits_nothing() {
    let mut e = page().await;
    reset(&mut e, "").await;
    e.dispatch_key("keyDown", "a", "KeyA", "a").await.expect("down");
    e.run().await.expect("run");
    e.dispatch_key("keyUp", "a", "KeyA", "").await.expect("up");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, vec!["keydown@body", "keypress@body", "keyup@body"]);
    assert_eq!(value_of(&mut e, "f").await, "");
}

/// A key that produces no character fires no `keypress`. Chrome fired none for ArrowLeft and none
/// for Backspace.
#[tokio::test(start_paused = true)]
async fn a_key_that_types_nothing_fires_no_keypress() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    e.dispatch_key("keyDown", "ArrowLeft", "ArrowLeft", "").await.expect("down");
    e.run().await.expect("run");
    e.dispatch_key("keyUp", "ArrowLeft", "ArrowLeft", "").await.expect("up");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, vec!["keydown@#f", "keyup@#f"]);
    assert_eq!(field(&mut e, 0, "keyCode").await, "37");
}

/// Backspace: `keydown`, then `beforeinput`/`input` with `inputType: "deleteContentBackward"` and
/// `data: null`, then `keyup`. No `keypress`. Measured with the caret at the end of `"abc"`.
#[tokio::test(start_paused = true)]
async fn backspace_deletes_the_character_before_the_caret() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    run_js(&mut e, "const f = document.getElementById('f'); f.value = 'abc'; \
                    f.setSelectionRange(3, 3); globalThis.__reset();").await;

    e.dispatch_key("keyDown", "Backspace", "Backspace", "").await.expect("down");
    e.run().await.expect("run");
    e.dispatch_key("keyUp", "Backspace", "Backspace", "").await.expect("up");
    e.run().await.expect("run");

    assert_eq!(value_of(&mut e, "f").await, "ab");
    assert_eq!(seq(&mut e).await, vec!["keydown@#f", "beforeinput@#f", "input@#f", "keyup@#f"]);
    assert_eq!(field(&mut e, 1, "inputType").await, "\"deleteContentBackward\"");
    assert_eq!(field(&mut e, 1, "data").await, "null");
    assert_eq!(field(&mut e, 0, "keyCode").await, "8");
}

/// Backspace with the caret at 0 deletes nothing, so `input` does not fire — the pair "value
/// changed" / "`input` fired" never comes apart in either direction.
#[tokio::test(start_paused = true)]
async fn backspace_at_the_start_announces_and_changes_nothing() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    e.dispatch_key("keyDown", "Backspace", "Backspace", "").await.expect("down");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, vec!["keydown@#f", "beforeinput@#f"]);
    assert_eq!(value_of(&mut e, "f").await, "");
}

// ================================================================================================
// 3. The two cancellation points, both measured.
// ================================================================================================

/// `beforeinput` cancelled: no `input`, value unchanged. Chrome's cancellation point for an edit.
#[tokio::test(start_paused = true)]
async fn a_cancelled_beforeinput_leaves_the_value_alone() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    run_js(&mut e, "document.getElementById('f').addEventListener('beforeinput', \
                    (ev) => ev.preventDefault()); globalThis.__reset();").await;
    e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, vec!["beforeinput@#f"]);
    assert_eq!(value_of(&mut e, "f").await, "");
}

/// `keydown` cancelled: no `keypress`, no `beforeinput`, no `input`, value unchanged — and the
/// `keyup` still fires, because it is a separate dispatch.
#[tokio::test(start_paused = true)]
async fn a_cancelled_keydown_suppresses_everything_but_the_keyup() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    run_js(&mut e, "document.getElementById('f').addEventListener('keydown', \
                    (ev) => ev.preventDefault()); globalThis.__reset();").await;
    e.dispatch_key("keyDown", "a", "KeyA", "a").await.expect("down");
    e.run().await.expect("run");
    e.dispatch_key("keyUp", "a", "KeyA", "").await.expect("up");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, vec!["keydown@#f", "keyup@#f"]);
    assert_eq!(value_of(&mut e, "f").await, "");
}

// ================================================================================================
// 4. The focus ring -- what puts the caret in the field in the first place.
// ================================================================================================

/// `el.focus()` from a page script fires `focus` then `focusin`, and moving the ring fires
/// `blur`/`focusout` on whatever held it first, with `relatedTarget` pointing the other way.
#[tokio::test(start_paused = true)]
async fn moving_the_focus_ring_fires_chromes_four_events_with_their_related_targets() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    run_js(&mut e, "document.getElementById('g').focus();").await;

    assert_eq!(seq(&mut e).await, vec!["blur@#f", "focusout@#f", "focus@#g", "focusin@#g"]);
    assert_eq!(rel(&mut e, 0).await, "#g", "blur points at where the focus went");
    assert_eq!(rel(&mut e, 2).await, "#f", "focus points at where it came from");
    assert_eq!(field(&mut e, 0, "bubbles").await, "false", "blur does not bubble");
    assert_eq!(field(&mut e, 1, "bubbles").await, "true", "focusout does");
    assert_eq!(field(&mut e, 2, "bubbles").await, "false");
    assert_eq!(field(&mut e, 3, "bubbles").await, "true");
    for i in 0..4 {
        assert_eq!(field(&mut e, i, "ctor").await, "\"FocusEvent\"");
        assert_eq!(field(&mut e, i, "trusted").await, "true");
    }
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "#g");
}

/// A press on a focusable element moves the ring to it. Chrome's order puts the four focus events
/// between `mousedown` and `mouseup`, and `document.activeElement` reads the BODY inside the
/// leaving pair.
#[tokio::test(start_paused = true)]
async fn a_mouse_press_moves_the_focus_ring_to_what_it_pressed() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    let (x, y) = centre(&mut e, "btn").await;

    e.click_at(x, y, 20.0).await.expect("click");
    e.run().await.expect("run click");

    assert_eq!(seq(&mut e).await, vec!["blur@#f", "focusout@#f", "focus@#btn", "focusin@#btn"]);
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "#btn");
}

/// A press on something unfocusable CLEARS the ring rather than leaving it: `blur`/`focusout` with
/// `relatedTarget: null`, no `focus` at all, and `activeElement` back at the body. Measured on a
/// `<span>` inside a plain `<div>`.
#[tokio::test(start_paused = true)]
async fn a_press_on_unfocusable_content_clears_the_ring() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    let (x, y) = centre(&mut e, "plain").await;

    e.click_at(x, y, 20.0).await.expect("click");
    e.run().await.expect("run click");

    assert_eq!(seq(&mut e).await, vec!["blur@#f", "focusout@#f"]);
    assert_eq!(rel(&mut e, 0).await, "null", "focus went nowhere, so blur points at nothing");
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "body");
}

/// Chrome commits a typed edit with `change` when the focus leaves — BEFORE `blur`, on the field,
/// bubbling and not cancelable.
#[tokio::test(start_paused = true)]
async fn a_typed_edit_is_committed_with_change_when_the_focus_leaves() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run");
    run_js(&mut e, "globalThis.__reset(); document.getElementById('f').blur();").await;

    assert_eq!(seq(&mut e).await, vec!["change@#f", "blur@#f", "focusout@#f"]);
    assert_eq!(field(&mut e, 0, "ctor").await, "\"Event\"", "a plain Event, not an InputEvent");
    assert_eq!(field(&mut e, 0, "bubbles").await, "true");
    assert_eq!(field(&mut e, 0, "cancelable").await, "false");
    assert_eq!(field(&mut e, 0, "trusted").await, "true");
}

/// An edit the user UNDOES earns no `change`: Chrome compares against the value from just before
/// the first user edit, not against "did anything happen".
#[tokio::test(start_paused = true)]
async fn an_edit_typed_and_then_undone_commits_nothing() {
    let mut e = page().await;
    reset(&mut e, "f").await;
    run_js(&mut e, "document.getElementById('f').value = 'seed'; \
                    document.getElementById('f').setSelectionRange(4, 4); globalThis.__reset();").await;

    e.insert_text("XY").await.expect("insert");
    e.run().await.expect("run");
    assert_eq!(value_of(&mut e, "f").await, "seedXY");
    for _ in 0..2 {
        e.dispatch_key("keyDown", "Backspace", "Backspace", "").await.expect("down");
        e.run().await.expect("run");
    }
    assert_eq!(value_of(&mut e, "f").await, "seed", "back where it started");

    run_js(&mut e, "globalThis.__reset(); document.getElementById('f').blur();").await;
    assert_eq!(seq(&mut e).await, vec!["blur@#f", "focusout@#f"], "no change: the value is unchanged");
}

/// Focus and blur with no typing commits nothing, and neither does a value a SCRIPT wrote — a
/// `change` is the user's, not the page's.
#[tokio::test(start_paused = true)]
async fn a_script_written_value_never_commits_a_change() {
    let mut e = page().await;

    reset(&mut e, "f").await;
    run_js(&mut e, "globalThis.__reset(); document.getElementById('f').blur();").await;
    assert_eq!(seq(&mut e).await, vec!["blur@#f", "focusout@#f"], "no typing, no change");

    reset(&mut e, "f").await;
    run_js(&mut e, "document.getElementById('f').value = 'PROG'; globalThis.__reset(); \
                    document.getElementById('f').blur();").await;
    assert_eq!(seq(&mut e).await, vec!["blur@#f", "focusout@#f"], "a script's write is not an edit");
    assert_eq!(value_of(&mut e, "f").await, "PROG");
}

/// The driver's own shape once more, end to end: press the field, type, then press elsewhere. That
/// is where a page's `change` handler actually runs, and the whole ordering has to hold across the
/// two presses.
#[tokio::test(start_paused = true)]
async fn typing_then_pressing_another_control_commits_and_moves_the_ring() {
    let mut e = page().await;
    reset(&mut e, "").await;
    let (fx, fy) = centre(&mut e, "f").await;
    e.click_at(fx, fy, 20.0).await.expect("press f");
    e.run().await.expect("run");
    e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run");

    run_js(&mut e, "globalThis.__reset();").await;
    let (gx, gy) = centre(&mut e, "g").await;
    e.click_at(gx, gy, 20.0).await.expect("press g");
    e.run().await.expect("run");

    assert_eq!(
        seq(&mut e).await,
        vec!["change@#f", "blur@#f", "focusout@#f", "focus@#g", "focusin@#g"]
    );
    assert_eq!(value_of(&mut e, "f").await, "parity");
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "#g");
}

/// The viewport centre of `id`, from the engine's own layout.
async fn centre(e: &mut Engine, id: &str) -> (f64, f64) {
    let raw = ask(e, &format!("globalThis.__box('{id}')")).await;
    let n: Vec<f64> = raw.split(',').map(|s| s.parse().expect("box number")).collect();
    assert!(n[2] > 0.0 && n[3] > 0.0, "#{id} must have a box to press: {raw}");
    (n[0], n[1])
}

// ================================================================================================
// 5. The end-to-end row: the parity driver's own sequence.
// ================================================================================================

/// **The row that was worth 169 floor rows.** `tools/parity/driver.py` focuses a text field with a
/// mouse press at its centre and then calls `Input.insertText`; before P3b the press focused
/// nothing and the insert typed at the document, so the field's value never changed on a single one
/// of them. This is that exact sequence through the exact two `Engine` entry points the CDP arms
/// call.
#[tokio::test(start_paused = true)]
async fn pressing_a_field_and_then_inserting_text_fills_it() {
    let mut e = page().await;
    reset(&mut e, "").await;
    let (x, y) = centre(&mut e, "f").await;

    e.click_at(x, y, 20.0).await.expect("press");
    e.run().await.expect("run press");
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "#f", "the press must focus the field");

    run_js(&mut e, "globalThis.__reset();").await;
    e.insert_text("parity").await.expect("insert");
    e.run().await.expect("run insert");

    assert_eq!(value_of(&mut e, "f").await, "parity");
    assert_eq!(seq(&mut e).await, vec!["beforeinput@#f", "input@#f"]);
}

/// `Engine::type_into` is now `focus` + `type_text`'s own body, so the humanised keystroke stream
/// fills the field it focused — including when the behaviour model injects a typo and a Backspace,
/// because the Backspace runs the deletion.
#[tokio::test(start_paused = true)]
async fn type_into_focuses_the_field_and_the_humanised_stream_fills_it() {
    let mut e = page().await;
    reset(&mut e, "").await;

    let n = e.type_into("#f", "hello world").await.expect("type").expect("selector matches");
    e.run().await.expect("run type");

    assert_eq!(value_of(&mut e, "f").await, "hello world");
    assert_eq!(ask(&mut e, "globalThis.__active()").await, "#f");
    assert_eq!(reported(&mut e).await, n, "every event the engine reported reached a listener");
    // Every event of the stream landed on the field, never on the document.
    let off_target: Vec<String> =
        seq(&mut e).await.into_iter().filter(|s| !s.ends_with("@#f")).collect();
    assert_eq!(off_target, Vec::<String>::new(), "no keystroke may land anywhere but the field");
}

/// A selector that matches nothing is still `None`, and nothing is typed.
#[tokio::test(start_paused = true)]
async fn type_into_a_selector_that_matches_nothing_is_none() {
    let mut e = page().await;
    reset(&mut e, "").await;
    assert!(e.type_into("#nope", "hello").await.expect("type").is_none());
}

/// `Engine::type_text` with nothing focused types at the body and changes nothing — the humanised
/// stream has the same target rule as a single CDP key, because it is the same dispatcher.
#[tokio::test(start_paused = true)]
async fn type_text_with_nothing_focused_lands_on_the_body() {
    let mut e = page().await;
    reset(&mut e, "").await;
    let n = e.type_text("ab").await.expect("type");
    e.run().await.expect("run");
    assert_eq!(value_of(&mut e, "f").await, "");
    assert_eq!(reported(&mut e).await, n);
    let off_body: Vec<String> =
        seq(&mut e).await.into_iter().filter(|s| !s.ends_with("@body")).collect();
    assert_eq!(off_body, Vec::<String>::new(), "with no focus every keystroke is on <body>");
}

// ================================================================================================
// 6. The comparison itself, with its one declared residue.
// ================================================================================================

/// Chrome's full sequence for `Input.insertText` into a focused `<input>`, verbatim from the probe.
const CHROME_INSERT_TEXT: &[&str] = &["beforeinput@#f", "textInput@#f", "input@#f"];

/// Chrome's full sequence for a printable `keyDown{text}` + `keyUp` into the same field.
const CHROME_KEY_STROKE: &[&str] =
    &["keydown@#f", "keypress@#f", "beforeinput@#f", "textInput@#f", "input@#f", "keyup@#f"];

/// The declared difference: `textInput`, and nothing else. This asserts it as a DIFFERENCE — the
/// engine's sequence is Chrome's with exactly the `textInput` rows removed and the order of what
/// remains unchanged — so the day `TextEvent` stops being a D2 stub row, this test is the thing
/// that says the residue is gone.
#[tokio::test(start_paused = true)]
async fn the_sequence_is_chromes_minus_exactly_one_declared_event() {
    let expect = |chrome: &[&str]| -> Vec<String> {
        chrome.iter().filter(|s| !s.starts_with("textInput")).map(|s| s.to_string()).collect()
    };

    let mut e = page().await;
    reset(&mut e, "f").await;
    e.insert_text("ab").await.expect("insert");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, expect(CHROME_INSERT_TEXT));

    reset(&mut e, "f").await;
    e.dispatch_key("keyDown", "a", "KeyA", "a").await.expect("down");
    e.run().await.expect("run");
    e.dispatch_key("keyUp", "a", "KeyA", "").await.expect("up");
    e.run().await.expect("run");
    assert_eq!(seq(&mut e).await, expect(CHROME_KEY_STROKE));

    // The residue is one interface, and it is a counted D2 stub rather than a silence: Chrome
    // declares `TextEvent` non-constructible too, so there is no `new TextEvent` to forge one with.
    assert_eq!(ask(&mut e, "typeof TextEvent").await, "function", "the interface is exposed");
    assert_eq!(
        ask(&mut e, "(() => { try { new TextEvent('x'); return 'constructed'; } \
                    catch (err) { return 'throws'; } })()").await,
        "throws",
        "Chrome's capture records `newable: false`, so a forged textInput has no honest constructor"
    );
}
