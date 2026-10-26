//! The kernel's [`spec::RemoteFrameHost`] -- a cross-site `<iframe>`'s
//! document in its own `Engine` on its own thread, as Chrome runs it in its own renderer process.
//!
//! The parent (`korvus-webapi`'s `core/html/remote_frame.rs`) hands over the committed response; this host
//! boots a child engine on a fresh thread, loads the response there, and runs that engine's loop until the
//! process ends. Messages cross as V8 structured-clone bytes over two unbounded channels; every frame the
//! child sends wakes the parent's loop ([`crate::worker::wake_page`]), which parks on live remote frames as
//! it does on live workers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use deno_core::{op2, OpState};
use tokio::sync::mpsc as tmpsc;
use tokio::sync::Mutex as TMutex;

use spec::{Persona, RemoteFrameEvent, RemoteFrameSpawn};

/// One remote frame, from the parent's side.
struct Link {
    to_child: tmpsc::UnboundedSender<(Vec<u8>, String)>,
    from_child: Arc<TMutex<tmpsc::UnboundedReceiver<RemoteFrameEvent>>>,
}

/// The host: one per engine (a child engine has its own, for frames nested in it).
pub(crate) struct Host {
    persona: Arc<dyn Persona>,
    next: AtomicU64,
    frames: Mutex<HashMap<u64, Link>>,
}

impl Host {
    pub(crate) fn new(persona: Arc<dyn Persona>) -> Self {
        Host { persona, next: AtomicU64::new(1), frames: Mutex::new(HashMap::new()) }
    }
}

/// The child engine's end of its link to the parent.
pub(crate) struct ChildLink {
    to_parent: tmpsc::UnboundedSender<RemoteFrameEvent>,
    from_parent: Arc<TMutex<tmpsc::UnboundedReceiver<(Vec<u8>, String)>>>,
    /// The child document's origin -- the `origin` of every message it sends.
    origin: String,
}

impl spec::RemoteFrameHost for Host {
    fn spawn(&self, s: RemoteFrameSpawn) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (to_child, from_parent) = tmpsc::unbounded_channel::<(Vec<u8>, String)>();
        let (to_parent, from_child) = tmpsc::unbounded_channel::<RemoteFrameEvent>();
        self.frames
            .lock()
            .unwrap()
            .insert(id, Link { to_child, from_child: Arc::new(TMutex::new(from_child)) });
        let persona = self.persona.clone();
        let trace = std::env::var_os("KORVUS_TRACE_OOPIF").is_some();
        // A window engine's stack, not a worker's: V8 and the page pipeline recurse as deep as the main
        // thread lets them (8 MB there).
        let spawned = std::thread::Builder::new().name(format!("korvus-oopif-{id}")).stack_size(8 << 20).spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[oopif] #{id}: no runtime: {e}");
                    return;
                }
            };
            rt.block_on(run_child(id, persona, s, to_parent, from_parent, trace));
        });
        if let Err(e) = spawned {
            eprintln!("[oopif] #{id}: cannot start a thread: {e}");
        }
        id
    }

    fn post(&self, id: u64, data: Vec<u8>, origin: String) {
        if let Some(l) = self.frames.lock().unwrap().get(&id) {
            let _ = l.to_child.send((data, origin));
        }
    }

    fn recv(&self, id: u64) -> spec::RemoteFrameFuture {
        let rx = self.frames.lock().unwrap().get(&id).map(|l| l.from_child.clone());
        Box::pin(async move {
            let rx = rx?;
            let mut g = rx.lock().await;
            g.recv().await
        })
    }

    fn terminate(&self, id: u64) {
        // Dropping the parent's sender ends the child's receive loop; its engine then drains and the thread
        // exits (P2 adds termination of a running script).
        self.frames.lock().unwrap().remove(&id);
    }

    fn live(&self) -> usize {
        self.frames.lock().unwrap().len()
    }
}

async fn run_child(
    id: u64,
    persona: Arc<dyn Persona>,
    s: RemoteFrameSpawn,
    to_parent: tmpsc::UnboundedSender<RemoteFrameEvent>,
    from_parent: tmpsc::UnboundedReceiver<(Vec<u8>, String)>,
    trace: bool,
) {
    let mut e = match crate::Engine::assemble_shared(persona) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("[oopif] #{id}: the child engine did not assemble: {err}");
            return;
        }
    };
    let origin = url::Url::parse(&s.url).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".to_string());
    {
        let st = e.runtime.op_state();
        let mut st = st.borrow_mut();
        st.put(webapi::page::FrameEmbedding {
            parent_origin: s.parent_origin.clone(),
            top_url: s.top_url.clone(),
            viewport: s.viewport,
            scrollbars_off: s.scrollbars_off,
            ancestor_origins: s.ancestor_origins.clone(),
            disabled_features: s.disabled_features.clone(),
            cluster_isolated: s.cluster_isolated,
        });
        // ONE browsing session: the child farbles from the page's seed.
        if let Some(seed) = s.session_seed {
            st.put(spec::farble::SessionSeed(seed));
        }
        webapi::page::set_pending_navigation_timing(&mut st, s.timing.clone());
        st.put(ChildLink { to_parent, from_parent: Arc::new(TMutex::new(from_parent)), origin });
    }
    e.arrive(webapi::page::Arrival { referrer: s.referrer.clone(), reload: false, ..Default::default() });
    if trace {
        eprintln!("[oopif] #{id}: child engine up, loading {}", s.url);
    }
    if let Err(err) = e.load_page_response(&s.body, &s.url, &s.injected, &s.headers).await {
        eprintln!("[oopif] #{id}: load failed: {err}");
    }
    // The child's loop: its parent-message receive is a REF'd op, so the loop never drains while the parent
    // can still talk to it; each slice returns, and the next one picks up.
    // Under KORVUS_TRACE_OOPIF the child's console reaches stderr, slice by slice (diagnosis: a probe in a
    // remote frame reports through `console.log`, and the parent's `--console` cannot see this engine).
    // Slices as short as the page's own driver runs (`korvus fetch`: 50 ms): the HOST's rendering opportunity -- paint
    // timing, long animation frames onto their timelines -- runs between slices, and a 30 s slice starved it for the
    // child's whole life (an out-of-process frame's 80 ms tasks reached its timeline after the
    // challenge had read it).
    let slice = std::time::Duration::from_millis(50);
    loop {
        let end = e.pump(slice).await;
        if trace {
            for line in e.take_output() {
                eprintln!("[oopif #{id} console] {line}");
            }
        }
        match end {
            Ok(crate::PumpEnd::Drained) => break,
            Ok(_) => {}
            Err(err) => {
                eprintln!("[oopif] #{id}: {err}");
                break;
            }
        }
    }
    if trace {
        let unc = e.uncaught_errors();
        eprintln!("[oopif] #{id}: child loop ended; uncaught {unc:?}");
    }
}

/// The child document's `load` fired: the parent's `<iframe>` fires its own.
pub(crate) fn note_child_loaded(state: &OpState) {
    if let Some(l) = state.try_borrow::<ChildLink>() {
        let _ = l.to_parent.send(RemoteFrameEvent::Loaded);
        crate::worker::wake_page();
    }
}

/// `parent.postMessage(...)` from a remote frame: the structured-clone bytes, to the parent.
#[op2(fast)]
pub(crate) fn op_oopif_post_parent(state: &mut OpState, #[buffer] bytes: &[u8]) {
    if let Some(l) = state.try_borrow::<ChildLink>() {
        let _ = l.to_parent.send(RemoteFrameEvent::Message { data: bytes.to_vec(), origin: l.origin.clone() });
        crate::worker::wake_page();
    }
}

/// The next message from the parent: `{data, origin}`, or `null` once the parent is gone. REF'd: a remote
/// frame's loop stays alive while its parent can still post to it.
#[op2(async(lazy), fast)]
#[serde]
pub(crate) async fn op_oopif_parent_recv(state: std::rc::Rc<std::cell::RefCell<OpState>>) -> Option<serde_json::Value> {
    let rx = state.borrow().try_borrow::<ChildLink>().map(|l| l.from_parent.clone())?;
    let (data, origin) = rx.lock().await.recv().await?;
    Some(serde_json::json!({"data": data, "origin": origin}))
}

/// Remote frames this engine's page is waiting on, for the loop's "is anything still coming" question.
pub(crate) fn live_remote_frames(state: &OpState) -> usize {
    state
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<spec::RemoteFrames>())
        .map(|h| h.0.live())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A loopback server answering every request with an empty page (no icon declared), and the paths it was asked.
    fn server() -> (String, Arc<Mutex<Vec<String>>>) {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let lg = log.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let lg = lg.clone();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut w = s;
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        lg.lock().unwrap().push(line.split_whitespace().nth(1).unwrap_or("").to_string());
                        loop {
                            let mut h = String::new();
                            if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
                                break;
                            }
                        }
                        let b = "<!doctype html><title>t</title>";
                        let out = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{b}", b.len());
                        if w.write_all(out.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (format!("http://127.0.0.1:{port}/"), log)
    }

    /// What a document loaded at `url` asks of the network in the 1.5 s after its load: a page's engine, or a
    /// remote frame's (a child, `FrameEmbedding`).
    async fn asked_after_load(child: bool) -> Vec<String> {
        let (url, log) = server();
        let mut e = crate::Engine::assemble_shared(Arc::new(persona::CapturedPersona::default())).expect("the engine");
        if child {
            let st = e.runtime.op_state();
            st.borrow_mut().put(webapi::page::FrameEmbedding {
                parent_origin: "http://localhost".to_string(),
                top_url: "http://localhost/".to_string(),
                ..Default::default()
            });
        }
        e.set_url(&url);
        e.load_page_response(b"<!doctype html><html><head></head><body>frame</body></html>", &url, &[], &[]).await.expect("load");
        let until = std::time::Instant::now() + Duration::from_millis(1500);
        while std::time::Instant::now() < until {
            let _ = e.pump(Duration::from_millis(50)).await;
        }
        let asked = log.lock().unwrap().clone();
        asked
    }

    /// ONLY THE PAGE'S DOCUMENT HAS ICONS. Stock Chrome 150 asked for no icon of a bot-challenge frame
    /// (NetLog) -- its favicon handler serves the tab, the main frame's -- where the frame run
    /// out of process here, a whole window engine of its own, fetched `/favicon.ico` after its `load` as a page does.
    /// The page's engine is the control: the same document, the same server, asks for the icon.
    #[tokio::test]
    async fn a_remote_frames_document_asks_for_no_icon() {
        let page = asked_after_load(false).await;
        assert!(page.iter().any(|p| p == "/favicon.ico"), "a page's document asks for /favicon.ico (the control): {page:?}");
        let frame = asked_after_load(true).await;
        assert!(!frame.iter().any(|p| p == "/favicon.ico"), "a remote frame's document asked for an icon: {frame:?}");
    }
}
