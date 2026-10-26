//! THE NAVIGATION API'S EVENTS, AS CHROME 150 FIRES THEM. Every same-document navigation a
//! page can make -- `navigation.navigate()` with each history behaviour, `state` and `info`, `location.hash` /
//! `href` / `replace`, a link, `pushState` / `replaceState`, `history.back()` / `forward()` / `go()`,
//! `navigation.back()` / `forward()` / `traverseTo()` -- and what `preventDefault()`, `intercept()` (with handlers
//! that fulfil, reject and outlive a second navigation), `scroll()`, `updateCurrentEntry()` and a disposed entry
//! do, each logged as `navigate`, `currententrychange`, `popstate`, `navigatesuccess` / `navigateerror`,
//! `hashchange` and the `committed` / `finished` promises, with the microtask and task boundaries marked.
//!
//! Korvus fired none of the Navigation API's events, `navigation.navigate("#a")` loaded the page again (the sweep
//! that found it lost the page), `history.pushState` kept the page's own object as the state, and the document's URL
//! -- what a relative URL resolves against -- stayed where the page was loaded.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11**, headed under Xvfb with NO
//! DevTools attached (with DevTools attached Blink reads a `navigateerror`'s `filename` off the inspector's stack trace
//! instead), three runs alike, from `fixtures/navigation-events/events-probe.js` in `canvas-and-errors/probe-page.html`
//! -- a classic script, so the first navigations come before the `load` event and replace (HTML: "not completely
//! loaded"). The canceled traversal is the probe's last step: after one, Chrome's `history.length` reads one short of
//! `navigation.entries()` until the next traversal commits (its browser and renderer count differently).

use crate::chrome_canvas_and_errors_live::{answers, assert_as_chrome, Mode};

#[tokio::test]
async fn the_navigation_apis_events_fire_as_chrome_150_fires_them() {
    let got = answers(Mode::Page, include_str!("fixtures/navigation-events/events-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/navigation-events/events-chrome-152.json"));
}

/// A FRAME's History used from the page (`frame.contentWindow.history`), recorded as above: it acts on the frame --
/// its state a structured clone, its URL resolved against the frame document's base URL and refused where an
/// `about:blank` document may not move (`/path`, `#frag`, `about:blank?x` alike: Blink's `CanChangeToUrlForHistoryApi`
/// with the document's own, inherited origin) -- and never on the page. It acted on whichever realm's script was
/// RUNNING, so the page's call moved the PAGE's URL and state.
#[tokio::test]
async fn a_frames_history_is_the_frames_own() {
    let got = answers(Mode::Page, include_str!("fixtures/navigation-events/frame-history-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/navigation-events/frame-history-chrome-152.json"));
}

/// WHERE A TRAVERSAL FALLS AMONG THE PAGE'S OTHER TASKS, recorded as above (three runs alike): `history.back()` goes
/// through the browser, so a message posted and a 0 ms timer armed right after it run first and its `navigate` comes
/// about half a millisecond later (Chrome 150 measured: 0.5-0.8 ms); its `popstate` comes after the commit's microtasks
/// and before what a `navigate` listener queued, and its `hashchange` -- queued at the commit -- after those; a fragment
/// navigation's `hashchange` is a DOM-manipulation task, which the timer nesting clamp does not hold back; a second
/// `history.back()` asked before the first traversal arrived goes back from where the first one arrived, after its
/// `hashchange` (HTML's session history traversal queue). Korvus ran the traversal ahead of the page's own tasks,
/// queued `hashchange` behind `popstate`, and resolved `history.go`'s delta when it was called.
#[tokio::test]
async fn a_traversal_falls_among_the_pages_tasks_as_in_chrome_150() {
    let got = answers(Mode::Page, include_str!("fixtures/navigation-events/order-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/navigation-events/order-chrome-152.json"));
}
