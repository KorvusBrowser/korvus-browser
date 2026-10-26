//! The screenshot primitive (plugin ABI 3, wave 1, C6): the page-process side of `host->capture_png`.
//!
//! A plugin callback that runs OFF the page's thread (an `on_request`/`on_response` on a network thread, or
//! the plugin's own worker) may ask for the page as a PNG. The host call reaches
//! [`plugin_host::Screenshotter`], which this module implements with a channel to the page loop -- exactly as
//! `korvus-cdp`'s `page_eval` does for `host->page_eval`: the callback's thread asks and blocks on a reply;
//! the page loop drains the channel at a task boundary, renders through the engine on its own thread
//! ([`Engine::render_png`]), and sends the PNG bytes back.
//!
//! A callback ON the page's own thread (an `on_call` exit) cannot be served this way -- the page thread is
//! the one that would have to render, and it is busy in the callback -- so [`Executor::png`] returns `None`
//! there rather than deadlock. The header documents that `capture_png` answers 0 when nothing can be
//! captured, which covers that case and the no-page / nothing-rendered-yet cases below.

use crate::Engine;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::ThreadId;
use std::time::Duration;

/// How long a plugin's `capture_png` waits for the page loop to render before giving up.
const DEADLINE: Duration = Duration::from_secs(5);

/// One capture request, and where its PNG bytes (or `None`) go.
struct Req {
    reply: mpsc::Sender<Option<Vec<u8>>>,
}

/// The `Screenshotter` a plugin reaches through `host->capture_png`. Holds the sender into the page loop and
/// the id of the page's own thread (to refuse a re-entrant call from it).
struct Executor {
    tx: mpsc::Sender<Req>,
    page_thread: ThreadId,
}

impl plugin_host::Screenshotter for Executor {
    fn png(&self) -> Option<Vec<u8>> {
        // A call from the page's own thread would block waiting for that same thread to drain it: refuse
        // instead (the plugin then gets 0 from capture_png, as the header allows).
        if std::thread::current().id() == self.page_thread {
            return None;
        }
        let (reply, answer) = mpsc::channel();
        self.tx.send(Req { reply }).ok()?;
        answer.recv_timeout(DEADLINE).ok().flatten()
    }
}

/// The page loop's end of the channel: [`Pump::drain`] renders the captures waiting on it.
pub struct Pump {
    rx: mpsc::Receiver<Req>,
}

/// Install the process's screenshotter and return the pump the page loop drains. Call once as a page
/// process's loop begins, on the page's own thread (its id is captured to guard against a re-entrant call) --
/// the same place and shape as `page_eval::install`.
//
// TODO(1021-verify): the korvus-cdp page loop must call `screenshot::install()` once as it begins (next to
// `page_eval::install()`) and `pump.drain(engine)` each turn (next to `page_eval_pump.drain(...)`); that
// wiring lands in korvus-cdp (another crate) in the integration pass.
pub fn install() -> Pump {
    let (tx, rx) = mpsc::channel();
    plugin_host::set_screenshotter(Arc::new(Executor { tx, page_thread: std::thread::current().id() }));
    Pump { rx }
}

impl Pump {
    /// Answer every capture waiting on the channel, on this (the page's) thread. Cheap when the channel is
    /// empty. With a page (`Some`), each render goes through [`Engine::render_png`] (`None` until there is a
    /// laid-out tree and a render backend); with no page yet (`None`), every waiting call is answered `None`
    /// at once, so a plugin that calls `capture_png` before a page exists is not left blocking for the
    /// timeout.
    pub fn drain(&self, engine: Option<&mut Engine>) {
        match engine {
            Some(engine) => {
                while let Ok(req) = self.rx.try_recv() {
                    let png = engine.render_png();
                    let _ = req.reply.send(png);
                }
            }
            None => {
                while let Ok(req) = self.rx.try_recv() {
                    let _ = req.reply.send(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_host::Screenshotter;

    #[test]
    fn a_call_from_the_page_thread_is_refused_not_deadlocked() {
        // page_thread is THIS thread, so a direct png() must return None at once rather than block for a
        // drain that can never come (the thread that would drain is the one calling).
        let (tx, _rx) = mpsc::channel();
        let ex = Executor { tx, page_thread: std::thread::current().id() };
        assert_eq!(ex.png(), None);
    }

    #[test]
    fn an_off_thread_call_crosses_the_channel_and_its_answer_returns() {
        // page_thread is THIS (main) thread, which stands in for the page loop and drains; the caller runs
        // on a spawned thread, so it is "off the page thread" and passes the guard.
        let (tx, rx) = mpsc::channel::<Req>();
        let ex = Arc::new(Executor { tx, page_thread: std::thread::current().id() });
        let caller = {
            let ex = ex.clone();
            std::thread::spawn(move || ex.png())
        };
        let req = rx.recv().unwrap();
        req.reply.send(Some(vec![1, 2, 3])).unwrap();
        assert_eq!(caller.join().unwrap(), Some(vec![1, 2, 3]));
    }
}
