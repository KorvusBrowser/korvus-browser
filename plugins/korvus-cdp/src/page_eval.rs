//! The page-eval primitive (plugin ABI 3, B1): the page-process side of `host->page_eval`.
//!
//! A plugin callback that runs OFF the page's thread (an `on_request`/`on_response` on a network thread, or the
//! plugin's own worker) may ask to run a script on the page and read its result. The host call reaches
//! [`plugin_host::PageExecutor`], which this module implements with a channel to the page loop: the callback's
//! thread sends the script and blocks on a reply; the page loop drains the channel at a task boundary, runs the
//! script through the engine on its own thread, and sends the JSON back.
//!
//! A callback ON the page's own thread (an `on_call` exit) cannot be served this way -- the page thread is the one
//! that would have to run the script, and it is busy in the callback -- so [`Executor::eval`] returns `None` there
//! rather than deadlock. The header documents that restriction.

use engine::Engine;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::ThreadId;
use std::time::Duration;

/// How long a plugin's `page_eval` waits for the page loop to run its script before giving up.
const DEADLINE: Duration = Duration::from_secs(5);

/// One request waiting on the page thread, and where its answer goes. `Eval` runs a script in a world and returns
/// its JSON; `SpawnBackground` spawns a WebExtension MV3 background realm and returns its worker id (0 = failed).
enum Req {
    Eval {
        world: String,
        script: String,
        reply: mpsc::Sender<Option<String>>,
    },
    SpawnBackground {
        kind: String,
        url: String,
        src: String,
        reply: mpsc::Sender<u32>,
    },
}

/// The `PageExecutor` a plugin reaches through `host->page_eval`. Holds the sender into the page loop and the id of
/// the page's own thread (to refuse a re-entrant call from it).
struct Executor {
    tx: mpsc::Sender<Req>,
    page_thread: ThreadId,
}

impl plugin_host::PageExecutor for Executor {
    fn eval(&self, world: &str, script: &str) -> Option<String> {
        // A call from the page's own thread would block waiting for that same thread to drain it: refuse instead.
        if std::thread::current().id() == self.page_thread {
            return None;
        }
        let (reply, answer) = mpsc::channel();
        self.tx.send(Req::Eval { world: world.to_string(), script: script.to_string(), reply }).ok()?;
        answer.recv_timeout(DEADLINE).ok().flatten()
    }

    fn spawn_background(&self, kind: &str, url: &str, src: &str) -> u32 {
        // Like `eval`, cross to the page thread (a call from the page thread itself would deadlock): the page loop
        // drains the request with the engine and spawns the background realm, returning its worker id (0 on failure
        // / unsupported kind / before a page exists).
        if std::thread::current().id() == self.page_thread {
            return 0;
        }
        let (reply, answer) = mpsc::channel();
        if self
            .tx
            .send(Req::SpawnBackground { kind: kind.to_string(), url: url.to_string(), src: src.to_string(), reply })
            .is_err()
        {
            return 0;
        }
        answer.recv_timeout(DEADLINE).ok().unwrap_or(0)
    }
}

/// The page loop's end of the channel: [`Pump::drain`] runs the scripts waiting on it.
pub struct Pump {
    rx: mpsc::Receiver<Req>,
}

/// Install the process's page executor and return the pump the page loop drains. Call once as a page process's
/// loop begins, on the page's own thread (its id is captured to guard against a re-entrant call).
pub fn install() -> Pump {
    let (tx, rx) = mpsc::channel();
    plugin_host::set_page_executor(Arc::new(Executor { tx, page_thread: std::thread::current().id() }));
    Pump { rx }
}

impl Pump {
    /// Answer every script waiting on the channel, on this (the page's) thread. Cheap when the channel is empty.
    /// With a page (`Some`), a script in the page's own world (`world` == "") runs through the engine, and a named
    /// world runs in that document world when the current document has one of that name -- resolved by NAME, so an
    /// unknown name answers `None` and never falls through to the page's own world (the I9 self-gate: a document
    /// with no such world, e.g. any page with no plugin that made one, answers `None`). With no page yet (`None`),
    /// every waiting call is answered `None` at once, so a plugin that calls `page_eval` before a page exists is
    /// not left blocking for the timeout.
    pub fn drain(&self, engine: Option<&mut Engine>) {
        match engine {
            Some(engine) => {
                while let Ok(req) = self.rx.try_recv() {
                    match req {
                        Req::Eval { world, script, reply } => {
                            let result = if world.is_empty() {
                                engine.eval_value("korvus:page_eval", script).ok().map(|v| v.to_string())
                            } else {
                                // A named world: resolve its index for THIS document (an owned name->index list; the
                                // &self borrow ends before the &mut eval below). A miss short-circuits to `None` -- it
                                // must never fall through to the page's own world (plugin JS in the main world).
                                engine
                                    .new_document_worlds()
                                    .into_iter()
                                    .find(|(name, _)| *name == world)
                                    .and_then(|(_, index)| {
                                        let out = engine.eval_remote_details_in(Some(index), script, true);
                                        match (out.exception, out.result) {
                                            (None, engine::RemoteResult::Value(v)) => Some(v.to_string()),
                                            _ => None,
                                        }
                                    })
                            };
                            let _ = reply.send(result);
                        }
                        // A WebExtension MV3 background realm: spawn it with the engine's op_state (persona,
                        // location) and return its worker id. The realm then runs on its own isolate + thread.
                        Req::SpawnBackground { kind, url, src, reply } => {
                            let _ = reply.send(engine.spawn_background(&kind, &url, &src));
                        }
                    }
                }
            }
            None => {
                while let Ok(req) = self.rx.try_recv() {
                    match req {
                        Req::Eval { reply, .. } => {
                            let _ = reply.send(None);
                        }
                        Req::SpawnBackground { reply, .. } => {
                            let _ = reply.send(0);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_host::PageExecutor;

    #[test]
    fn a_call_from_the_page_thread_is_refused_not_deadlocked() {
        // page_thread is THIS thread, so a direct eval must return None at once rather than block for a drain that
        // can never come (the thread that would drain is the one calling).
        let (tx, _rx) = mpsc::channel();
        let ex = Executor { tx, page_thread: std::thread::current().id() };
        assert_eq!(ex.eval("", "document.title"), None);
    }

    #[test]
    fn an_off_thread_call_crosses_the_channel_and_its_answer_returns() {
        // page_thread is THIS (main) thread, which stands in for the page loop and drains; the caller runs on a
        // spawned thread, so it is "off the page thread" and passes the guard.
        let (tx, rx) = mpsc::channel::<Req>();
        let ex = Arc::new(Executor { tx, page_thread: std::thread::current().id() });
        let caller = {
            let ex = ex.clone();
            std::thread::spawn(move || ex.eval("", "document.title"))
        };
        let Req::Eval { world, script, reply } = rx.recv().unwrap() else { panic!("expected an Eval request") };
        assert_eq!((world.as_str(), script.as_str()), ("", "document.title"));
        reply.send(Some("\"hi\"".to_string())).unwrap();
        assert_eq!(caller.join().unwrap(), Some("\"hi\"".to_string()));
    }

    #[test]
    fn a_named_world_call_before_a_page_exists_is_answered_none() {
        // The no-page drain answers every waiting call `None` at once -- named world or not -- so a plugin's worker
        // that posts into its isolated world before the page's engine is up is not left blocking for the deadline.
        // (The named-world-resolves-to-None-on-a-miss path with a live engine is covered end-to-end by
        // tools/abi3/firstparty/webext/webext_probe.py's no-plugin control.)
        let (tx, rx) = mpsc::channel::<Req>();
        let (reply, answer) = mpsc::channel();
        tx.send(Req::Eval {
            world: "__korvus_webext".to_string(),
            script: "__kwx_dispatch('{}')".to_string(),
            reply,
        })
        .unwrap();
        drop(tx);
        Pump { rx }.drain(None);
        assert_eq!(answer.recv().unwrap(), None);
    }
}
