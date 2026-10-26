//! `:hover` end to end, in a live engine: the pointer moves, the chain moves with it,
//! computed style follows, and the four boundary events fire in the order a browser fires them.
//!
//! # The oracle
//!
//! Every expectation here was taken from **stock Chrome 150.0.7871.46**, driven over CDP
//! (`Input.dispatchMouseEvent` `mouseMoved`), headed under Xvfb at 1920x1080, launched
//! by us with `--remote-debugging-port` rather than by Playwright's launcher. None of it is read off
//! a specification, and the project's standing rule is why: a brief is not evidence.
//!
//! The fixture mirrors the one Chrome answered on -- `div#outer > div#mid > div#inner`, each with
//! its own `:hover` rule, plus `#outer:hover > #sub` for the menu case. Chrome's answers, verbatim
//! from the run that settled each question:
//!
//! ```text
//! entering the nest from <body>:                leaving #leaf for #outer:
//!   mouseout    on body     related=#inner        mouseout    on #leaf  related=#outer
//!   mouseover   on #inner   related=body          mouseleave  on #leaf  related=#outer
//!   mouseenter  on #outer   related=body          mouseleave  on #inner related=#outer
//!   mouseenter  on #mid     related=body          mouseleave  on #mid   related=#outer
//!   mouseenter  on #inner   related=body          mouseover   on #outer related=#leaf
//!   mousemove   on #inner   related=null          mousemove   on #outer related=null
//!
//! querySelectorAll(":hover")  ==  [html, body, #outer, #mid, #inner]
//! ```
//!
//! and, inside the FIRST event of a move (the `mouseout` on the element being left), Chrome's
//! `querySelectorAll(":hover")` already reported the NEW chain -- the state is committed before
//! dispatch, not after.
//!
//! # What this asks that the unit tests cannot
//!
//! `korvus-dom` proves the selector reads one chain and `korvus-page` proves the cascade consults
//! it, but both set the chain by hand. This drives a real `Engine` through `move_mouse` -- the same
//! entry point CDP's `Input.dispatchMouseEvent mouseMoved` reaches -- so the hit test, the
//! invalidation, the event construction and `getComputedStyle` are all in the loop.

use engine::Engine;

/// The Chrome fixture. Geometry is in the stylesheet, never inline: an inline `style` outranks every
/// `:hover` rule, so an inline width would make a green test prove nothing. `body` has no margin so
/// the boxes sit at known viewport coordinates.
const PAGE: &str = r#"<!doctype html><html><head><style>
  body { margin: 0 }
  #outer { width: 400px; height: 400px }
  #mid   { width: 300px; height: 300px }
  #inner { width: 200px; height: 200px }
  #sub   { width: 5px; height: 10px }
  #outer:hover { background-color: rgb(1, 1, 1) }
  #mid:hover   { background-color: rgb(2, 2, 2) }
  #inner:hover { background-color: rgb(3, 3, 3) }
  #outer:hover > #sub { width: 77px }
</style></head><body>
  <div id="outer"><div id="mid"><div id="inner"></div></div><div id="sub"></div></div>
</body></html>"#;

/// The listener the event-order assertions read. Records every mouse event of the family as
/// `type@target(related)`, so one string carries the target AND the `relatedTarget` that tells a
/// handler where the pointer came from.
const LISTEN: &str = r##"
globalThis.__log = [];
const label = (n) => n === null || n === undefined ? "null"
  : (n === document ? "#document" : (n === window ? "#window"
  : (n.id ? "#" + n.id : n.tagName.toLowerCase())));
globalThis.__label = label;
for (const el of [document, ...document.querySelectorAll("*")]) {
  for (const type of ["mouseover", "mouseout", "mouseenter", "mouseleave"]) {
    el.addEventListener(type, (ev) => {
      // At-target only: a bubbling event would otherwise be recorded once per ancestor listener,
      // which measures the listener set rather than the dispatch order.
      if (ev.eventPhase !== 2) return;
      globalThis.__log.push(ev.type + "@" + label(ev.target) + "(" + label(ev.relatedTarget) + ")");
    });
  }
}
globalThis.__reset = () => { globalThis.__log = []; };
globalThis.__hovered = () =>
  Array.from(document.querySelectorAll(":hover")).map(label).join(",");
globalThis.__bg = (id) => getComputedStyle(document.getElementById(id)).backgroundColor;
globalThis.__w  = (id) => document.getElementById(id).getBoundingClientRect().width;
"##;

/// Three nested boxes of the SAME size, so they coincide exactly: every point inside one is inside
/// all three, and every point outside one is outside all three.
///
/// This exists because `Engine::move_mouse` plays a human TRAJECTORY (WindMouse + Fitts + tremor --
/// `input.rs`'s whole reason for existing), not the single jump Chrome's CDP `mouseMoved` performs.
/// A trajectory across [`PAGE`]'s telescoping nest crosses one boundary at a time, so it produces a
/// run of one-element transitions and never the multi-element one this file has to pin. With
/// coincident boxes the transition is ATOMIC whatever the trajectory does, so the ordering WITHIN
/// one transition -- which is what Chrome was asked about -- becomes observable.
///
/// Each one-element transition a trajectory produces is itself in Chrome's order, and that is
/// asserted mechanically over a whole trajectory by `every_transition_is_in_chromes_order`.
const COINCIDENT: &str = r#"<!doctype html><html><head><style>
  body { margin: 0 }
  #a, #b, #c { width: 400px; height: 400px }
  #a:hover { background-color: rgb(1, 1, 1) }
  #b:hover { background-color: rgb(2, 2, 2) }
  #c:hover { background-color: rgb(3, 3, 3) }
</style></head><body><div id="a"><div id="b"><div id="c"></div></div></div></body></html>"#;

/// A booted engine on `html` with [`LISTEN`] installed, its cursor parked outside every box.
async fn page_of(html: &str) -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_html(html).expect("load page");
    e.eval("hover-listen", LISTEN.to_string()).expect("eval listen");
    e.run().await.expect("run listen");
    e.take_output();
    e
}

async fn page() -> Engine {
    page_of(PAGE).await
}

/// Evaluate `expr` and return the single line it printed.
async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("hover-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert_eq!(out.len(), 1, "one line expected from {expr:?}, got {out:?}");
    out.into_iter().next().unwrap()
}

/// Move the pointer to `(x, y)` and return the events that fired, in order.
async fn move_to(e: &mut Engine, x: f64, y: f64) -> Vec<String> {
    e.eval("hover-reset", "globalThis.__reset()".to_string()).expect("reset");
    e.run().await.expect("run reset");
    e.take_output();
    e.move_mouse(x, y, 20.0).await.expect("move");
    e.run().await.expect("run move");
    e.take_output();
    let joined = ask(e, "globalThis.__log.join(\"|\")").await;
    if joined.is_empty() { Vec::new() } else { joined.split('|').map(String::from).collect() }
}

// ---------------------------------------------------------------------------------------------
// 1. The state: `:hover` follows the pointer, over the whole chain, both directions.
// ---------------------------------------------------------------------------------------------

/// Chrome: `querySelectorAll(":hover")` is `[html, body, #outer, #mid, #inner]` with the pointer in
/// the innermost box, and empties again when the pointer leaves every box.
#[tokio::test(start_paused = true)]
async fn the_pointer_moves_the_hover_chain_and_computed_style_follows_it() {
    let mut e = page().await;
    assert_eq!(ask(&mut e, "globalThis.__hovered()").await, "", "nothing hovered before a move");
    assert_eq!(ask(&mut e, "globalThis.__bg('inner')").await, "rgba(0, 0, 0, 0)");

    move_to(&mut e, 10.0, 10.0).await;
    assert_eq!(
        ask(&mut e, "globalThis.__hovered()").await,
        "html,body,#outer,#mid,#inner",
        "the hit element AND its ancestors, in document order"
    );
    // The whole chain restyles, not only the element under the pointer.
    assert_eq!(ask(&mut e, "globalThis.__bg('outer')").await, "rgb(1, 1, 1)");
    assert_eq!(ask(&mut e, "globalThis.__bg('mid')").await, "rgb(2, 2, 2)");
    assert_eq!(ask(&mut e, "globalThis.__bg('inner')").await, "rgb(3, 3, 3)");

    // Off every box entirely: 5000 is past the viewport, so the hit test finds nothing.
    move_to(&mut e, 5000.0, 5000.0).await;
    assert_eq!(ask(&mut e, "globalThis.__hovered()").await, "", "and it empties again");
    assert_eq!(ask(&mut e, "globalThis.__bg('outer')").await, "rgba(0, 0, 0, 0)");
    assert_eq!(ask(&mut e, "globalThis.__bg('inner')").await, "rgba(0, 0, 0, 0)");
}

/// `Element.matches(":hover")` and `querySelectorAll(":hover")` read the SAME state -- the thing the
/// brief forbids from being two sources. Asked of a live engine, of every element on the page.
#[tokio::test(start_paused = true)]
async fn matches_and_query_selector_all_never_disagree() {
    let mut e = page().await;
    let probe = r#"(() => {
        const all = Array.from(document.querySelectorAll("*"));
        const byQuery = new Set(Array.from(document.querySelectorAll(":hover")));
        const disagree = all.filter((el) => el.matches(":hover") !== byQuery.has(el));
        return disagree.map(globalThis.__label).join(",") || "agree";
    })()"#;
    for (x, y) in [(10.0, 10.0), (350.0, 350.0), (5000.0, 5000.0), (250.0, 10.0)] {
        move_to(&mut e, x, y).await;
        assert_eq!(ask(&mut e, probe).await, "agree", "at ({x}, {y})");
    }
}

/// The menu case, live: `#outer:hover > #sub` widens a box that is NOT under the pointer. This is
/// the one a single-element hover implementation fails silently.
#[tokio::test(start_paused = true)]
async fn a_hover_menu_opens_and_shuts() {
    let mut e = page().await;
    assert_eq!(ask(&mut e, "globalThis.__w('sub')").await, "5", "shut");
    move_to(&mut e, 10.0, 10.0).await;
    assert_eq!(ask(&mut e, "globalThis.__w('sub')").await, "77", "open: the ancestor is hovered");
    move_to(&mut e, 5000.0, 5000.0).await;
    assert_eq!(ask(&mut e, "globalThis.__w('sub')").await, "5", "shut again");
}

// ---------------------------------------------------------------------------------------------
// 2. The events, in Chrome's order.
// ---------------------------------------------------------------------------------------------

/// Entering the nest from nothing. Chrome fired `mouseout` on what was left (nothing, on the first
/// move of the document's life), `mouseover` on the element arrived at, then `mouseenter` OUTERMOST
/// FIRST on every element that joined the chain.
#[tokio::test(start_paused = true)]
async fn entering_fires_mouseover_then_mouseenter_outermost_first() {
    let mut e = page().await;
    let log = move_to(&mut e, 10.0, 10.0).await;
    assert_eq!(
        log,
        vec![
            "mouseover@#inner(null)",
            // FIXER C6: the DOCUMENT is the outermost element of the chain Blink walks
            // (`BoundaryEventDispatcher` / `FlatTreeTraversal::Parent`), and with a `mouseenter`
            // listener of its own it gets one: stock Chrome 150 under REAL X input fired
            // `pointerenter` and `mouseenter` at a listening document on the first move into the page
            // (`fixtures/mouse-input/chrome-150.json`, the `D` rows).
            "mouseenter@#document(null)",
            "mouseenter@html(null)",
            "mouseenter@body(null)",
            "mouseenter@#outer(null)",
            "mouseenter@#mid(null)",
            "mouseenter@#inner(null)",
        ],
        "no mouseout on the first move ever (nothing was left); enter runs outermost -> innermost"
    );
}

/// Leaving several elements at once. Chrome fired `mouseout` on the element left, then `mouseleave`
/// INNERMOST FIRST on everything that dropped out, and only THEN the `mouseover` on the arrival --
/// every leave precedes the first over, and the two halves never interleave.
#[tokio::test(start_paused = true)]
async fn leaving_fires_mouseout_then_mouseleave_innermost_first_then_mouseover() {
    let mut e = page_of(COINCIDENT).await;
    move_to(&mut e, 10.0, 10.0).await;
    assert_eq!(ask(&mut e, "globalThis.__hovered()").await, "html,body,#a,#b,#c");

    // (600, 100) is outside all three coincident boxes (0..400) and still inside <body>, so the
    // whole nest drops out in ONE transition however the trajectory got there.
    let log = move_to(&mut e, 600.0, 100.0).await;
    assert_eq!(
        log,
        vec![
            "mouseout@#c(body)",
            "mouseleave@#c(body)",
            "mouseleave@#b(body)",
            "mouseleave@#a(body)",
            "mouseover@body(#c)",
        ],
        "out, then every leave innermost -> outermost, then the over -- no interleaving"
    );
    assert_eq!(ask(&mut e, "globalThis.__hovered()").await, "html,body");
}

/// The mirror image: entering several at once puts every `mouseenter` AFTER the `mouseover`, and
/// runs them OUTERMOST first.
#[tokio::test(start_paused = true)]
async fn entering_several_at_once_fires_mouseover_then_enters_outermost_first() {
    let mut e = page_of(COINCIDENT).await;
    move_to(&mut e, 600.0, 100.0).await; // on <body>, off the nest
    let log = move_to(&mut e, 10.0, 10.0).await;
    assert_eq!(
        log,
        vec![
            "mouseout@body(#c)",
            "mouseover@#c(body)",
            "mouseenter@#a(body)",
            "mouseenter@#b(body)",
            "mouseenter@#c(body)",
        ],
        "no leave (<body> stayed in the chain); enter runs outermost -> innermost"
    );
}

/// A human trajectory crosses boundaries one at a time, and EVERY transition it produces must
/// itself be in Chrome's order. Asserted mechanically rather than by pinning a trajectory-dependent
/// list: a `mouseout` is followed only by `mouseleave`s until the `mouseover`, that `mouseover`
/// only by `mouseenter`s until the next `mouseout`, and the two ends point at each other.
#[tokio::test(start_paused = true)]
async fn every_transition_is_in_chromes_order() {
    let mut e = page().await;
    let mut log = Vec::new();
    for (x, y) in [(10.0, 10.0), (350.0, 350.0), (10.0, 10.0), (5000.0, 5000.0)] {
        log.extend(move_to(&mut e, x, y).await);
    }
    assert!(log.len() >= 8, "the trajectories must have crossed something: {log:?}");

    let split = |row: &str| {
        let (kind, rest) = row.split_once('@').expect("kind@target(related)");
        let (target, related) = rest.split_once('(').expect("target(related)");
        (kind.to_string(), target.to_string(), related.trim_end_matches(')').to_string())
    };

    let mut i = 0;
    let mut transitions = 0;
    while i < log.len() {
        // A transition opens with `mouseout` -- unless there was no old chain at all, which is the
        // first move of the document's life. Chrome fires no `mouseout` and no `mouseleave` there
        // and goes straight to the `mouseover` with `relatedTarget === null`; so does this.
        let (mut from, to) = {
            let (kind, t, r) = split(&log[i]);
            if kind == "mouseover" {
                assert_eq!(r, "null", "an opening mouseover means nothing was left: {:?}", &log[i..]);
                ("null".to_string(), t)
            } else {
                assert_eq!(kind, "mouseout", "a transition opens with mouseout: {:?}", &log[i..]);
                i += 1;
                (t, r)
            }
        };
        let mut leaves = Vec::new();
        while i < log.len() && split(&log[i]).0 == "mouseleave" {
            let (_, t, r) = split(&log[i]);
            assert_eq!(r, to, "every leave points at where the pointer went: {:?}", &log[i..]);
            leaves.push(t);
            i += 1;
        }
        if !leaves.is_empty() {
            assert_eq!(leaves[0], from, "the first leave is the element the out fired on");
        }
        if to != "null" {
            // On an opening `mouseover` the cursor is already at `i`; `from` stays "null".
            if from == "null" {
                let (kind, t, _) = split(&log[i]);
                assert_eq!(kind, "mouseover");
                assert_eq!(t, to);
            }
            let (kind, t, r) = split(&log[i]);
            assert_eq!(kind, "mouseover", "every leave precedes the over: {:?}", &log[i..]);
            assert_eq!(t, to, "the over fires on where the pointer went");
            assert_eq!(r, from, "and points back at where it came from");
            from = r;
            i += 1;
            let mut enters = Vec::new();
            while i < log.len() && split(&log[i]).0 == "mouseenter" {
                let (_, t, r) = split(&log[i]);
                assert_eq!(r, from, "every enter points back at where the pointer came from");
                enters.push(t);
                i += 1;
            }
            if !enters.is_empty() {
                assert_eq!(enters.last().unwrap(), &to, "the last enter is the element arrived at");
            }
        }
        transitions += 1;
    }
    assert!(transitions >= 4, "several boundaries were crossed: {transitions}");
}

/// `relatedTarget` is the other end of the move, and it is what lets a handler tell "the pointer
/// went to a child of mine" from "the pointer left my subtree". Chrome pointed every leave at the
/// element being arrived at, every enter at the element being left -- and `null` when there was no
/// other end, which is a `mouseout` with nowhere to go.
#[tokio::test(start_paused = true)]
async fn related_target_points_at_the_other_end_of_the_move() {
    let mut e = page_of(COINCIDENT).await;
    move_to(&mut e, 10.0, 10.0).await;

    // Arriving somewhere: every row of the transition names <body>, on both sides.
    let log = move_to(&mut e, 600.0, 100.0).await;
    assert!(
        log.iter().take(4).all(|r| r.ends_with("(body)")),
        "the out and every leave point at where the pointer went: {log:?}"
    );
    assert_eq!(log[4], "mouseover@body(#c)", "and the over points back at where it came from");

    // Arriving nowhere: what is left of the chain drops out with no destination at all.
    //
    // FIXER C6: the trajectory to (5000, 5000) leaves <body>'s box (400 px tall here) INSIDE the viewport
    // before it leaves the viewport, and a point in the viewport that no box covers is <html>'s: stock
    // Chrome 150 with real X input targets `html` there (`fixtures/mouse-input/chrome-150.json`, its first
    // move), so the body -> html step is a real transition of this trajectory, not an error. What this pins
    // is the END: one `mouseout` to null, then a `mouseleave` to null for each element still in the chain,
    // the listening document last (Blink's chain reaches it; see the first-move test).
    let log = move_to(&mut e, 5000.0, 5000.0).await;
    let nulls: Vec<&str> = log.iter().map(String::as_str).skip_while(|r| !r.ends_with("(null)")).collect();
    assert!(nulls.first().is_some_and(|r| r.starts_with("mouseout@")), "the way out opens with a mouseout to null: {log:?}");
    assert!(nulls.iter().all(|r| r.ends_with("(null)")), "and nothing after it points anywhere: {log:?}");
    assert!(nulls.ends_with(&["mouseleave@html(null)", "mouseleave@#document(null)"]), "html, then the document, leave last: {log:?}");
    assert_eq!(ask(&mut e, "globalThis.__hovered()").await, "", "and the chain is empty");
}

/// Chrome's bubbling contract: `mouseover`/`mouseout` bubble, `mouseenter`/`mouseleave` do not. A
/// listener on `document` therefore sees exactly the two bubbling members.
#[tokio::test(start_paused = true)]
async fn only_mouseover_and_mouseout_bubble() {
    let mut e = page().await;
    e.eval(
        "hover-bubble",
        r#"
        globalThis.__doc = [];
        for (const type of ["mouseover", "mouseout", "mouseenter", "mouseleave"]) {
          document.addEventListener(type, (ev) => {
            if (ev.eventPhase === 2) return;  // at-target on document is not bubbling
            globalThis.__doc.push(ev.type + ":" + ev.bubbles + ":" + ev.cancelable);
          });
        }"#
        .to_string(),
    )
    .expect("eval");
    e.run().await.expect("run");
    e.take_output();

    move_to(&mut e, 10.0, 10.0).await;
    move_to(&mut e, 5000.0, 5000.0).await;
    let seen = ask(&mut e, "Array.from(new Set(globalThis.__doc)).sort().join(\",\")").await;
    assert_eq!(
        seen, "mouseout:true:true,mouseover:true:true",
        "only the bubbling pair reaches a document listener, and both are cancelable"
    );
}

/// A pointer moving WITHIN one element changes nothing, so no boundary event may fire -- Chrome
/// emits the bare `mousemove` there. This is also what keeps a mouse move from re-laying out the
/// document on every sample.
#[tokio::test(start_paused = true)]
async fn a_move_inside_one_element_fires_no_boundary_event() {
    let mut e = page().await;
    move_to(&mut e, 10.0, 10.0).await;
    let log = move_to(&mut e, 20.0, 20.0).await;
    assert!(log.is_empty(), "still inside #inner, so nothing to report: {log:?}");
    assert_eq!(
        ask(&mut e, "globalThis.__hovered()").await,
        "html,body,#outer,#mid,#inner",
        "and the chain is unchanged"
    );
}

/// The state is committed BEFORE the events, which is what a handler reading `getComputedStyle`
/// from a `mouseout` sees. Chrome: inside the `mouseout` on the element being LEFT,
/// `querySelectorAll(":hover")` already reported the new chain and the new computed style.
#[tokio::test(start_paused = true)]
async fn a_handler_sees_the_new_chain_from_inside_the_very_first_event() {
    let mut e = page_of(COINCIDENT).await;
    move_to(&mut e, 10.0, 10.0).await;
    assert_eq!(ask(&mut e, "globalThis.__bg('c')").await, "rgb(3, 3, 3)", "hovered to begin with");
    e.eval(
        "hover-snap",
        r#"
        globalThis.__snap = [];
        document.addEventListener("mouseout", () => {
          globalThis.__snap.push("out:" + globalThis.__hovered() + ":" + globalThis.__bg("c"));
        });"#
            .to_string(),
    )
    .expect("eval");
    e.run().await.expect("run");
    e.take_output();

    move_to(&mut e, 600.0, 100.0).await;
    assert_eq!(
        ask(&mut e, "globalThis.__snap[0]").await,
        "out:html,body:rgba(0, 0, 0, 0)",
        "the first event already sees the post-move chain AND the post-move style"
    );
}

/// The events are genuinely trusted, like every other engine-dispatched event -- a page scoring
/// input authenticity must not be able to tell a hover boundary from a real one.
#[tokio::test(start_paused = true)]
async fn the_boundary_events_are_trusted() {
    let mut e = page().await;
    e.eval(
        "hover-trust",
        r#"
        globalThis.__t = [];
        for (const type of ["mouseover", "mouseenter"]) {
          document.addEventListener(type, (ev) => { globalThis.__t.push(ev.isTrusted); }, true);
        }"#
        .to_string(),
    )
    .expect("eval");
    e.run().await.expect("run");
    e.take_output();

    move_to(&mut e, 10.0, 10.0).await;
    assert_eq!(ask(&mut e, "globalThis.__t.length > 0").await, "true");
    assert_eq!(ask(&mut e, "globalThis.__t.every(Boolean)").await, "true");
}
