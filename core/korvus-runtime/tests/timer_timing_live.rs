//! LIVE gate: timer and frame TIMING as Chrome keeps it.
//!
//! Measured side by side on this machine (stock Chrome 150.0.7871.46, the probe below, 20 samples):
//!
//! | what                                        | Chrome 150              | this engine before   |
//! |---------------------------------------------|-------------------------|----------------------|
//! | `setTimeout(f, 10)`, p50                    | 10.1 ms                 | 11.3 ms              |
//! | `setInterval(f, 10)` ticks                  | 10.1, 20.0, 30.0 ...    | 11.1, 22.2 ... 112.3 |
//! | `requestAnimationFrame` interval, p50       | 16.7 ms (60 Hz vsync)   | 17.3 ms              |
//!
//! Three causes, one fix each: tokio's millisecond timer wheel rounded every deadline up
//! (`op_timer_next_due` now sleeps to 2 ms short and yields to the deadline); an interval re-armed
//! from `now` instead of snapping to its grid as Blink's `TimerBase::RunInternal` does; and frames ran
//! on a 16 ms timer from the request instead of the display's refresh grid (`rendering::next_frame_ms`,
//! the persona's `screen.refreshRate`).
//!
//! And Chrome's nesting rule over HTML's (owner's decision, 2026-10-08): a timer armed from the
//! MICROTASKS of a deeply nested timer task is clamped to 4 ms, as Chrome 150 clamps it (an `await`
//! loop of `setTimeout(r, 0)` runs at 4.0-4.1 ms a turn; this engine ran it at 0.0). HTML says it must
//! not be and WPT `timer-nesting-not-inherited-in-microtask.html` checks that; Chrome fails it, and so
//! does this engine (`scheduler::Scheduler::after_task`).
//!
//! The bounds are loose enough for a loaded gate box and tight enough that each old behaviour fails.

const PROBE: &str = r#"(async () => {
  const med = (a) => { a.sort((x, y) => x - y); return a[a.length >> 1]; };
  const chain = []; for (let i = 0; i < 12; i++) { const t = performance.now(); await new Promise((r) => setTimeout(r, 0)); chain.push(performance.now() - t); }
  const ten = []; for (let i = 0; i < 15; i++) { const t = performance.now(); await new Promise((r) => setTimeout(r, 10)); ten.push(performance.now() - t); }
  const ticks = await new Promise((res) => { const t = performance.now(); const c = []; const iv = setInterval(() => { c.push(performance.now() - t); if (c.length === 8) { clearInterval(iv); res(c); } }, 10); });
  const raf = []; let last = 0; for (let i = 0; i < 12; i++) { const t = await new Promise((r) => requestAnimationFrame(r)); if (last) raf.push(t - last); last = t; }
  return { chainLate: med(chain.slice(6)), ten: med(ten), tickErr: Math.max(...ticks.map((v, i) => Math.abs(v - 10 * (i + 1)))), raf: med(raf) };
})()"#;

/// Up to three runs of the probe, the first that keeps all four bounds passing: a test thread descheduled for a
/// millisecond on a box running the rest of the suite beside it put one interval tick 1.1 ms off its grid (2 of 2
/// full-suite runs on 2026-10-09, alone 3 of 3 clean). Every old behaviour fails all three runs -- its error is
/// the engine's, not the box's: the interval re-armed from `now` drifted a millisecond a tick, to 12 ms by the
/// eighth.
#[tokio::test]
async fn timers_and_frames_keep_chromes_time() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got = crate::keyboard_live::answers_value(crate::keyboard_live::CHROME_BUNDLE, PROBE).await;
        let f = |k: &str| got[k].as_f64().unwrap_or(f64::NAN);
        let ok = f("chainLate") >= 3.5 && f("ten") < 10.9 && f("ten") >= 10.0 && f("tickErr") < 0.9 && f("raf") > 16.2 && f("raf") < 17.1;
        runs.push(got);
        if ok {
            return;
        }
    }
    let got = runs.last().cloned().unwrap_or_default();
    let f = |k: &str| got[k].as_f64().unwrap_or(f64::NAN);
    assert!(f("chainLate") >= 3.5, "a 0 ms timer armed from the task's microtasks is clamped from the sixth level, as Chrome 150 clamps it: {runs:?}");
    assert!(f("ten") < 10.9 && f("ten") >= 10.0, "setTimeout(10) fires on time, not a millisecond late: {runs:?}");
    assert!(f("tickErr") < 0.9, "setInterval(10) ticks stay on their 10 ms grid: {runs:?}");
    assert!(f("raf") > 16.2 && f("raf") < 17.1, "requestAnimationFrame runs at the display's 60 Hz: {runs:?}");
}

/// A same-process frame that observes and animates leaves the page's frames and observers alone: the rendering state
/// is the runtime's, and a frame's realm read the page's document as stale -- its `observe()` and its frame tasks
/// dropped every observer of the page and the pending frame's flag, and each time another chain of frames began
/// beside the first. minea.com (five frames) ran `requestAnimationFrame` every 4-7 ms, where Chrome 150 holds 16.7,
/// and the page's own observers stopped reporting.
const FRAME_PROBE: &str = r#"(async () => {
  const el = document.createElement('div');
  el.style.cssText = 'width:50px;height:50px;background:red';
  document.body.appendChild(el);
  const seen = [];
  new IntersectionObserver((es) => { for (const e of es) seen.push(e.isIntersecting); }).observe(el);
  await new Promise((r) => setTimeout(r, 100));
  const f = document.createElement('iframe');
  f.srcdoc = '<div id=t></div><script>new IntersectionObserver(() => {}).observe(document.getElementById("t")); (function g() { requestAnimationFrame(g); })()<\/script>';
  document.body.appendChild(f);
  await new Promise((r) => f.onload = r);
  await new Promise((r) => setTimeout(r, 200));
  const raf = []; let last = 0; for (let i = 0; i < 12; i++) { const t = await new Promise((r) => requestAnimationFrame(r)); if (last) raf.push(t - last); last = t; }
  el.style.marginTop = '5000px';
  await new Promise((r) => setTimeout(r, 300));
  raf.sort((x, y) => x - y);
  return { raf: raf[raf.length >> 1], seen };
})()"#;

#[tokio::test]
async fn a_frames_observers_and_frames_leave_the_pages_alone() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got = crate::keyboard_live::answers_value(crate::keyboard_live::CHROME_BUNDLE, FRAME_PROBE).await;
        let raf = got["raf"].as_f64().unwrap_or(f64::NAN);
        let seen: Vec<bool> = got["seen"].as_array().map(|a| a.iter().filter_map(|v| v.as_bool()).collect()).unwrap_or_default();
        let ok = raf > 16.2 && raf < 17.1 && seen == [true, false];
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("the page's rAF keeps its 60 Hz and its observer reports its target leaving the view (Chrome 150: 16.7, [true, false]): {runs:?}");
}
