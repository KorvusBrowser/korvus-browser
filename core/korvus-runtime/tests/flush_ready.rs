//! `Engine::flush_ready`, the read-side flush the CDP evaluate path uses.
//!
//! # What it is for
//!
//! A client that is about to READ the page's state (`Runtime.evaluate`, `Runtime.callFunctionOn`)
//! first has to let the page finish what it has already finished: a settled promise's continuation,
//! a microtask, a task whose turn has come. It must NOT wait for anything else, because a real page
//! keeps perpetual work alive — a `setInterval` heartbeat, a rAF loop — and waiting for THAT is
//! waiting forever, bounded only by a wall clock.
//!
//! That is the defect this file locks. `Runtime.evaluate` used to call `Engine::pump`, whose budget
//! is a duration to WAIT (it returns early only when the loop DRAINS), so every evaluate on a page
//! with a live timer cost the full budget. Measured on the replay tier, 2026-09-18: a CDP evaluate
//! of the expression `1` cost 127.7 ms in korvus against 0.7 ms in Chrome, and the §6 parity probe —
//! whose own work is 3.0 ms on webscraper.io and 14.9 ms on prestashop.com — cost 133 ms.
//!
//! # Why none of these assertions is a clock
//!
//! A wall-clock assertion in a test is forbidden here (and would be flaky on a loaded box besides).
//! The behaviour is stated STRUCTURALLY instead, and the two facts below are exactly the two halves
//! of "flush what is ready, wait for nothing":
//!
//!   * what the page had finished IS visible afterwards — the ready work ran;
//!   * what the page had NOT finished is NOT — a timer that is not due has not fired, and the
//!     return value says `BudgetElapsed`, i.e. "work is still scheduled", which is the answer a
//!     page with a live interval must produce however long anyone waits.
//!
//! The speed is not asserted because it cannot be asserted honestly; it is measured, and the
//! measurement is in FP11.1's commit body and in the parity sweep.
use engine::{Engine, PumpEnd};
use std::time::Duration;

/// A generous ceiling. It is never reached on a settled page, and on a page with a live interval it
/// is what `flush_ready` must NOT spend — which the return value, not a stopwatch, reports.
const CEILING: Duration = Duration::from_secs(5);

async fn booted(html: &str) -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_html(html).expect("load_html");
    e
}

/// The flush RUNS what the page has already made ready: a resolved promise's `.then`, a
/// `queueMicrotask`, and a `setTimeout(…, 0)` whose turn has come.
#[tokio::test]
async fn a_flush_runs_the_work_the_page_has_already_made_ready() {
    let mut e = booted(
        r#"<!doctype html><html><body><script>
             window.__ran = [];
             Promise.resolve().then(function () { window.__ran.push("promise"); });
             queueMicrotask(function () { window.__ran.push("microtask"); });
             setTimeout(function () { window.__ran.push("timeout0"); }, 0);
           </script></body></html>"#,
    )
    .await;

    e.flush_ready(CEILING).await.expect("flush_ready");

    let ran = e.eval_value("chk", "window.__ran.join(',')".to_string()).expect("eval");
    assert_eq!(
        ran.as_str(),
        Some("promise,microtask,timeout0"),
        "a flush must leave the page's already-ready work done, and in its own order"
    );
}

/// The flush does NOT run work whose time has not come, and it does not wait for it either. A page
/// with a live `setInterval` can never drain, so the honest answer is `BudgetElapsed` — "still
/// scheduled" — and it is the answer whether the ceiling is five seconds or five minutes.
#[tokio::test]
async fn a_page_that_never_drains_still_gets_an_answer_and_keeps_its_pending_work() {
    let mut e = booted(
        r#"<!doctype html><html><body><script>
             window.__ticks = 0;
             window.__late = false;
             setInterval(function () { window.__ticks++; }, 10);
             setTimeout(function () { window.__late = true; }, 600000);
           </script></body></html>"#,
    )
    .await;

    let end = e.flush_ready(CEILING).await.expect("flush_ready");
    assert_eq!(
        end,
        PumpEnd::BudgetElapsed,
        "a page with a live interval has work scheduled, so the loop has not drained"
    );
    let late = e.eval_value("chk", "window.__late".to_string()).expect("eval");
    assert_eq!(
        late.as_bool(),
        Some(false),
        "a timer that is not due must not be run by a flush -- the flush is not a fast-forward"
    );
}

/// A page with nothing left scheduled drains, and says so. This is the case where `flush_ready` and
/// `pump` have always agreed, and it is here so that a change that made the flush ALWAYS report
/// `BudgetElapsed` — which would pass the test above on its own — goes red.
///
/// The page settles first: its first paint is PRESENTED a frame later, and Blink adds the paint
/// entries in a task at that presentation (paint_timing.cc), so right after the load that task is
/// still scheduled -- as it is in Chrome. Once it has run, nothing is.
#[tokio::test]
async fn a_settled_page_drains() {
    let mut e = booted("<!doctype html><html><body><p>nothing scheduled</p></body></html>").await;
    assert_eq!(e.pump(CEILING).await.expect("pump"), PumpEnd::Drained, "the page settles once its first paint is presented");
    let painted = e.eval_value("chk", "performance.getEntriesByType('paint').length".to_string()).expect("eval");
    assert_eq!(painted.as_u64(), Some(2), "first-paint and first-contentful-paint are on the timeline");

    assert_eq!(
        e.flush_ready(CEILING).await.expect("flush_ready"),
        PumpEnd::Drained,
        "nothing is pending, so re-polling cannot produce more work"
    );
}
