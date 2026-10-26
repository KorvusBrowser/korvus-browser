//! LIVE gate: a microtask checkpoint after each callback the engine calls with no script on the stack, and after each
//! task -- MEASURED, NOT YET THE ENGINE'S.
//!
//! The engine's pumps -- the timer task source, a fetch's end, a worker's, a socket's or a frame's message -- chain
//! their next step with `then`, so every task runs as a PROMISE JOB, inside V8's microtask checkpoint, where V8 makes
//! a nested checkpoint a no-op. Two listeners of one engine-fired event both run before the first one's microtasks:
//! korvus answers `L1,L2,m1,p1,m2` where Chrome answers `L1,m1,p1,L2,m2` (HTML "clean up after running a callback").
//! An earlier change made each step a task (V8TaskSpawner) and checkpointed after each listener, and this test passed; it was
//! reverted because it moved orders other Chrome fixtures pin (a page timer ahead of `hashchange`, XHR's handler
//! quirks, LoAF's task attribution). The redesign brings Chrome's per-source task queues with it.
//!
//! `fixtures/microtask-checkpoints/probe.js` puts two listeners on each kind of event; `chrome-150.json` is stock
//! Chrome 150.0.7871.46 (headed on the Wayland desktop, no DevTools, three runs alike).

#[tokio::test]
async fn each_callback_the_engine_calls_has_its_own_microtask_checkpoint() {
    let script = include_str!("fixtures/microtask-checkpoints/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/microtask-checkpoints/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    let wrong: Vec<String> = got.iter().zip(&want).filter(|(g, w)| g != w).map(|(g, w)| format!("  korvus {g}\n  chrome {w}")).collect();
    assert_eq!(got.len(), want.len(), "korvus answered {} rows, Chrome {}", got.len(), want.len());
    assert!(wrong.is_empty(), "{} case(s) order their microtasks unlike Chrome 150:\n{}", wrong.len(), wrong.join("\n"));
}

/// The handles `requestAnimationFrame` / `requestIdleCallback` answer: each document's, each kind's, from 1 (Blink
/// `FrameRequestCallbackCollection`, `ScriptedIdleTaskController`). korvus counted both kinds and every frame on one
/// counter. `fixtures/callback-handles/chrome-152.json`: stock Chrome 150, recorded as the file above.
#[tokio::test]
async fn frame_and_idle_callback_handles_are_each_documents_own() {
    let script = include_str!("fixtures/callback-handles/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/callback-handles/chrome-152.json")).expect("the recorded Chrome rows");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want, "the handles differ from Chrome 150's");
}
