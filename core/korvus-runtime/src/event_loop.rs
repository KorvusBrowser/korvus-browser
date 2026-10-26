//! THE EVENT LOOP — the pump, the module drain, and the two deadlines that bound them.
//!
//! Moved out of `lib.rs` VERBATIM. Nothing here was edited in the move.
//!
//! # Why a "pump" and not just `run()`
//!
//! `JsRuntime::run_event_loop` resolves only when NOTHING is pending. A real web page keeps perpetual
//! timers alive — `setInterval` carousels, analytics heartbeats, polling, rAF loops — so the loop is
//! never fully idle and draining it hangs forever. [`Engine::pump`] is the bounded version, and every
//! caller in this tree goes through it.
//!
//! # The two bounds
//!
//! * **The load-event starvation guard.** A document's `load` waits on HTML's "delay the load event"
//!   set for as long as its subresources take -- Chrome has no deadline there. The one
//!   thing that cannot wait is a page that holds the isolate and never lets the loop run the release:
//!   the pump stamps the loop's turns and a watchdog interrupt breaks a spin that stopped them. The
//!   limit is `navigation::LOAD_EVENT_CAP_MS`.
//! * **The module drain budget.** `MODULE_DRAIN_BUDGET` bounds one drain of the deferred module queue,
//!   including every static import it fetches. The drain issues every queued
//!   module's imports TOGETHER, so what those 2,000 ms bound is the slowest graph and no longer the
//!   sum of them; the number, and therefore the worst case a drain costs its caller, is unchanged.
//!
//! [`Engine::pump_until_loaded`] is the pump the navigation path uses: it returns as soon as the page
//! fires `load` instead of always burning the full budget, so a fast page navigates fast.

use crate::{modules, script_exec, worker, Engine, PendingModule, PendingModules, MODULE_DRAIN_BUDGET};
use deno_core::{ModuleSpecifier, PollEventLoopOptions};

/// Why an [`Engine::pump`] returned -- the distinction a polling caller needs.
///
/// A caller that pumps in a loop is waiting for the page to reach a state only the event loop can
/// produce (`load` fired, a result published, a challenge solved). `BudgetElapsed` says work is still
/// scheduled, so another slice can change the answer. `Drained` says the loop has nothing left --
/// no timer, no in-flight op, no unresolved promise -- so nothing can change it without new input.
/// Treating the two as one is how both polling loops in this workspace came to spin a core until
/// their deadline on any page that settled early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpEnd {
    /// The event loop ran to completion; re-polling it cannot produce more work.
    Drained,
    /// The budget ran out first; work is still scheduled.
    BudgetElapsed,
}

/// How one wait on the event loop inside [`Engine::pump_slice`] ended.
enum Turn {
    /// The loop drained (or failed): nothing it can run without new work.
    Drained(anyhow::Result<()>),
    /// The slice is over: its budget ran out, the host's interrupt came, or the page asked to navigate.
    Stopped,
    /// The display's vsync, while a plugin wants frames: a rendering opportunity is due.
    Vsync,
    /// The loop has kept itself busy for [`BUSY_YIELD`] without the runtime getting a turn: the host's IO is owed one.
    Busy,
}

/// How long the page's loop may keep itself busy before it hands the runtime a turn. A loop that wakes itself --
/// a `requestAnimationFrame` animation painting each frame, a chain of ready tasks -- is never parked, and tokio's
/// current-thread scheduler then polls its IO driver only every 61 polls, so the DevTools socket waits. (minea.com's
/// screencast answered a `Runtime.evaluate` after 11 s, 2026-10-23; most of that was frames run back to back inside one
/// poll, which `scheduler::FrameCap` ends.) Chrome's renderer runs a DevTools command as its next task.
const BUSY_YIELD: std::time::Duration = std::time::Duration::from_millis(10);

/// An opaque, thread-safe handle to interrupt an [`Engine`]'s V8 isolate. `terminate()` breaks the
/// isolate out of a long-running SYNCHRONOUS script -- V8 honors the interrupt even inside a tight loop
/// that never yields to the event loop, which a cooperative `tokio::time::timeout` around the async
/// pump cannot (a Rust timer only fires at an await point). A harness uses this to bound a pathological
/// page script. `IsolateHandle` is Send+Sync by design (built for cross-thread termination); the same
/// mechanism interrupts a busy Worker (see worker.rs). The wrapped `v8` type stays inside this crate,
/// preserving the workspace invariant that only korvus-runtime imports rusty_v8.
pub struct IsolateInterrupt(deno_core::v8::IsolateHandle);

impl IsolateInterrupt {
    /// Request termination of the isolate's current execution. Returns once the request is posted;
    /// V8 unwinds at its next interrupt check. Idempotent and safe to call from another thread.
    pub fn terminate(&self) {
        self.0.terminate_execution();
    }
}

/// Whether the rendering opportunity paints a document frame. Opt-in via
/// `KORVUS_COMPOSITOR=page`, read once (the per-frame paint is behavior-changing, so it stays off by
/// default until its cost is verified against Brave).
fn paint_each_frame() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("KORVUS_COMPOSITOR").map(|v| v == "page").unwrap_or(false))
}

impl Engine {
    /// Drive the event loop to completion (microtasks, resolved promises, elapsed timers). First
    /// evaluates any deferred `<script type="module">` scripts.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        self.loop_resumed();
        let out = self.run_loop().await;
        self.loop_stopped();
        out
    }

    /// [`Engine::run`]'s loop, without telling the page the host stopped running it ([`Engine::loop_stopped`]):
    /// what a pump runs inside its slice.
    pub(crate) async fn run_loop(&mut self) -> anyhow::Result<()> {
        loop {
            // A RENDERING OPPORTUNITY FOR THE TIMELINE, AND ONLY FOR THE TIMELINE.
            // `pump` opens the observers' one too; this driver deliberately does not -- its contract
            // is deno_core's, not HTML's, and arming an observer frame here would move the P12c task
            // order. But the animation timeline must still MOVE: it is sampled once per opportunity
            // and held, so a driver that never offered one would freeze every animation for as long
            // as a caller stayed inside `run()`. This arms nothing and queues nothing; it says only
            // that the next observation may read the clock again.
            self.advance_frame_time();
            self.drain_load_tasks().await?;
            self.runtime
                .run_event_loop(PollEventLoopOptions::default())
                .await
                .map_err(|e| anyhow::anyhow!("run_event_loop failed: {e}"))?;
            // A SCRIPT THAT SETTLED DURING THAT TURN HAS NOT RUN YET. Its task was
            // parked by the fetch completion, and the completion happened INSIDE the loop we just
            // drove -- so returning here would hand the caller a page with its own script pending,
            // and a caller that calls `run()` once (many tests do) would never run it at all. Round
            // again: the drain is synchronous, so each turn consumes the queue, and a page whose
            // scripts insert scripts forever is bounded by the caller's timeout exactly as it was
            // when the execution lived inside `run_event_loop`.
            if !webapi::page::settled_scripts_pending(&self.runtime.op_state()) {
                return Ok(());
            }
        }
    }

    /// Every task the LOAD PIPELINE has ready, run from Rust with an empty JavaScript stack: the
    /// settled `<script src>` tasks, then the deferred module scripts.
    ///
    /// THE ORDER IS NOT ARBITRARY. A settled script that turns out to be a module is queued by
    /// `finish_script` into the module queue, and running the script tasks first lets that module
    /// evaluate in the SAME turn -- which is the turn it evaluated in before this queue existed, when
    /// the completion ran inside `run_event_loop` and the module drain caught it on the next one.
    pub(crate) async fn drain_load_tasks(&mut self) -> anyhow::Result<()> {
        self.with_scope(webapi::page::run_settled_scripts);
        // the module fetches the loader finished during the last turn, on the timeline (and to its observers)
        self.with_scope(webapi::drain_module_fetches);
        self.evaluate_pending_modules().await
    }

    /// Evaluate deferred `<script type="module">` scripts (queued by `load_html`/`load_page`) as ES
    /// modules, in document order. Imports resolve through the runtime's module loader against each
    /// module's OWN url, so `import "./helpers.mjs"` means what it says on the page.
    /// Idempotent: the queue is drained on the first call and empty thereafter.
    ///
    /// EVERY MODULE SCRIPT IS A SIDE MODULE. There is no "main module" in HTML: a document may carry
    /// any number of `<script type="module">` elements and they are peers, and `import.meta` in a
    /// browser exposes `url` and `resolve` and no `main`, so the only thing deno_core's main slot
    /// bought us was `import.meta.main === true` on whichever module happened to be first — a Deno
    /// concept the recorded capture has no trace of.
    ///
    /// What it COST was the whole rest of the document. deno_core permits exactly one main module per
    /// runtime and registers it the moment the entry compiles, which is BEFORE its imports are
    /// fetched. So a first module whose own body parsed but whose dependency did not — the ordinary
    /// case while `.json` and `.css` bodies were going to the JS parser — occupied the main slot and
    /// then returned `Err`, the drain kept `is_first` set because the load had failed, and every
    /// later module script in that document died on `MainModuleAlreadyExists`: an error about
    /// bookkeeping, in a file that was testing something else. It was measurable — 20 files in the
    /// pre-run carried a `Trying to create "main" module` error, and in `json-module/charset.html`
    /// (five inline modules) it took four of the five. Never taking the slot cannot leave it in that
    /// state, which is a stronger guarantee than clearing the flag more carefully would have been.
    ///
    /// ONE MODULE'S FAILURE IS ONE MODULE'S FAILURE. A module whose import 404s, whose body does not
    /// parse, or which throws at top level is reported like any other uncaught page error and the next
    /// module still runs.
    ///
    /// ONE MODULE'S FAILURE IS ONE MODULE'S FAILURE. A module whose import 404s, whose body does not
    /// parse, or which throws at top level is reported -- an external module's failed fetch as its
    /// element's `error`, everything else like any other uncaught page error -- and the
    /// next module still runs — the `?` chain this replaced abandoned the rest of the queue and returned an
    /// `Err` that `pump`'s caller discards, so a single bad import silently cost the document every
    /// module after it. That is the same shape of defect as the driver-level throw in
    /// RUN_PAGE_SCRIPTS_EXT, one layer down.
    pub(crate) async fn evaluate_pending_modules(&mut self) -> anyhow::Result<()> {
        // while the parser waits for a script (`script_exec::ParserWaiting`), the modules it found
        // are deferred to the parse's end: only the page's own insertions run, each a task of its own
        let parsing = self.runtime.op_state().borrow().try_borrow::<crate::script_exec::ParserWaiting>().is_some();
        self.drain_modules(if parsing { Drain::InsertedOnly } else { Drain::All }, None).await
    }

    /// The parse-end drain: HTML's deferred module scripts, and only those. See
    /// [`Drain::ParserDeferred`].
    pub(crate) async fn evaluate_parser_modules(&mut self) -> anyhow::Result<()> {
        self.drain_modules(Drain::ParserDeferred, None).await
    }

    /// The first `n` parser modules still queued, in document order (a deferred classic
    /// script runs between the modules around it).
    pub(crate) async fn evaluate_parser_modules_first(&mut self, n: usize) -> anyhow::Result<()> {
        self.drain_modules(Drain::ParserDeferred, Some(n)).await
    }

    /// THE QUEUE IS NOT EMPTIED UP FRONT. This used to `std::mem::take` the whole of
    /// `PendingModules` and then `await` each module in turn, and a drain runs inside its CALLER's
    /// future — the harness's `timeout(_, engine.run())`, `Engine::pump`'s budget, CDP's
    /// `Page.navigate`. Drop that future while the drain is awaiting a module's imports and the
    /// module is already off the queue: it is never evaluated, its element never fires, and nothing
    /// is left anywhere to retry it. Measured, at 8 concurrent test threads on a box at load 12–25:
    /// `<script type=module src=/graph.mjs>` (one static import) produced an empty page log 4 times
    /// in 40.
    ///
    /// So an entry is read from the queue and REMOVED only at the instant its evaluation begins, and
    /// there is no `await` between the two. A drain cut short before that leaves the entry queued,
    /// in order, for the next turn of the loop.
    async fn drain_modules(&mut self, which: Drain, limit: Option<usize>) -> anyhow::Result<()> {
        // This drain owns the modules that were queued when it started and no more: one queued BY a
        // module's own body belongs to the next turn of the loop, which is what taking the queue in
        // one go used to say and what Chrome does with a task that queues a task.
        let owned = {
            let os = self.runtime.op_state();
            let st = os.borrow();
            match st.try_borrow::<PendingModules>() {
                Some(q) => q.0.iter().filter(|m| which.takes(m)).count(),
                None => 0,
            }
        };
        let owned = limit.map_or(owned, |n| owned.min(n));
        if owned == 0 {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + MODULE_DRAIN_BUDGET;
        // INDEPENDENT GRAPHS ARE FETCHED TOGETHER. Everything below this line still
        // runs one module at a time -- it has to, because `load_side_es_module_from_code` takes the
        // whole `JsRuntime` -- but the NETWORK no longer waits its turn: every queued module's
        // static imports are issued now, into the document's own module map, and the serial loop
        // then finds each one already in flight or already answered. `warming` is the future that
        // drives them, and it is polled beside the loop rather than before it, so the first module
        // does not wait for the sixth module's import before it starts.
        //
        // This is what the budget above was actually measuring. It bounded the SUM of the modules'
        // import fetches because they were issued in series; with them issued together it bounds
        // the SLOWEST GRAPH, which is what a browser's one-navigation wall clock bounds too. The
        // constant is unchanged, and so is the worst case a single drain can cost its caller.
        let warm = self.warm_queued_module_imports(which, owned);
        let warming = async move {
            // A fetch's failure is the loader's to report when the import asks for it; here the
            // only job is to poll them. Then park: a `select!` arm that COMPLETES would end the
            // drain, and the drain ends when the modules are done, not when the network is.
            let _ = deno_core::futures::future::join_all(warm).await;
            std::future::pending::<()>().await
        };
        let in_order = self.drain_modules_in_order(which, owned, deadline);
        tokio::pin!(in_order);
        tokio::pin!(warming);
        tokio::select! {
            r = &mut in_order => r,
            _ = &mut warming => unreachable!("the warming future parks rather than completing"),
        }
    }

    /// The drain's SERIAL half: each owned module in queue order, one at a time, because
    /// `load_side_es_module_from_code` takes the whole `JsRuntime` and two of them cannot be in
    /// flight together. Its fetching is not serial — `drain_modules` polls the warmed imports
    /// beside this — and its ordering guarantees are the ones a page can observe:
    /// module 2 evaluates after module 1 however fast module 2's bytes arrived.
    async fn drain_modules_in_order(&mut self, which: Drain, mut owned: usize, deadline: tokio::time::Instant) -> anyhow::Result<()> {
        while owned > 0 {
            owned -= 1;
            let Some(pm) = self.peek_pending_module(which) else { break };
            // The module's real URL when it has one; `about:blank` only for the URL-less `load_html`
            // path. `parse` cannot fail on `about:blank`, and a page URL that does not parse falls
            // back to it rather than dropping the module.
            //
            // THIS NAME IS PAGE-VISIBLE, which is why it is not `korvus:module/{i}` any more:
            // deno_core builds the V8 `ScriptOrigin` from it and V8 puts the origin in every
            // stack frame, so the old form put the engine's name in `Error.stack`. It is not reachable
            // from page script -- the only two producers of a `PendingModule` are `queue_module`,
            // which always passes a URL derived from the document, and `Engine::load_html`, which
            // passes the empty string and has no caller outside this crate's tests (CDP navigates
            // through `load_html_injected`, which runs the real driver at the real URL). `about:blank`
            // is nevertheless what a browser calls a document with no URL, so if it ever does fire it
            // reads like one: Chrome reports `about:srcdoc` for a `srcdoc` document and `<anonymous>`
            // for a script written into an `about:blank` frame -- measured -- and never an engine name.
            // deno_core cannot take the empty name that produces `<anonymous>`, since a
            // `ModuleSpecifier` is a `Url`.
            //
            // The numbering went with it. Two modules at one name are two modules
            // (`tests/inline_module_url.rs`), so there is nothing left for a counter to disambiguate.
            let spec = ModuleSpecifier::parse(&pm.url)
                .or_else(|_| ModuleSpecifier::parse("about:blank"))
                .map_err(|e| anyhow::anyhow!("module specifier: {e}"))?;
            // What this module's own relative `import`s resolve against, which `<base href>` makes
            // different from the name above. Recorded before the load because static imports resolve
            // during compilation, and left in place afterwards because `import.meta.resolve` resolves
            // whenever page script calls it -- arbitrarily later.
            //
            // Only an INLINE module writes it, and every inline module of one document writes the same
            // pair. An external one in the same queue must NOT clear it on its way past, or an inline
            // module earlier in the document loses its base the moment a `<script type=module src>`
            // follows it.
            if let Some(base) = pm.import_base {
                self.module_fetch.borrow_mut().inline_base = Some((spec.to_string(), base));
            }
            // A REFUSED EXTERNAL MODULE: its own response was a 404, a 500, a network
            // error or a non-JavaScript Content-Type. Nothing is compiled -- the body is an error page,
            // not the page's code -- and the element fires `error`. Not an uncaught error: Chrome
            // raises no exception for it, only the element's event.
            if pm.refused {
                let taken = self.take_pending_module(pm.idx);
                let (element, delay_token) = split(taken);
                if let Some(el) = element.as_ref() {
                    self.fire_module_element(el, "error");
                }
                self.undelay_module(delay_token);
                continue;
            }
            // THE ONE CANCELLABLE STEP, and the entry is still on the queue for the whole of it.
            let loaded = self.load_one_module(&spec, pm.source, deadline).await;
            // From here to the events there is no `await`, so nothing can fall between taking the
            // entry off the queue and firing what it owes the page.
            let taken = self.take_pending_module(pm.idx);
            let (element, delay_token) = split(taken);
            match loaded {
                // The element's `load` and the load-delay release are NOT fired here:
                // `run_started_module` fires them at the instant HTML's "run the script" returns —
                // which for a module with a top-level `await` is the FIRST AWAIT, not the settling of
                // its evaluation promise. See that function for the Chrome 150 reading.
                Ok(id) => self.run_started_module(&spec, id, element.as_ref(), delay_token),
                Err(f) => {
                    match f {
                        // An IMPORT that could not be fetched. For an external module that is the element's
                        // `error` and nothing else, measured on Chrome 150 (`import` of a 404 -> `onerror`, no
                        // `Runtime.exceptionThrown`). An inline module has no event to carry it, and keeps the
                        // uncaught report it had.
                        ModuleFailure::Fetch(e) => match element.as_ref() {
                            Some(el) => self.fire_module_element(el, "error"),
                            None => self.record_module_error(&spec, &e),
                        },
                        // The module did not PARSE (it, or something it imports): an uncaught error, and
                        // then `load` -- a module that failed to compile still loaded. Chrome 150, measured
                        // for a syntax error. A module that parsed and then threw never reaches here: its
                        // body RAN, so "run the script" returned and the events are the evaluator's.
                        ModuleFailure::Threw(e) => {
                            // Surface it the way a page error is surfaced, so the file's `errors` names the
                            // cause instead of the file simply registering nothing.
                            self.record_module_error(&spec, &e);
                            if let Some(el) = element.as_ref() {
                                self.fire_module_element(el, "load");
                            }
                        }
                        // Out of budget while fetching imports: neither loaded nor failed yet, so no event.
                        //
                        // THE ENTRY IS STILL TAKEN OFF THE QUEUE, and that is a deliberate refusal.
                        // Leaving it queued would be the obvious reading of "neither
                        // loaded nor failed" — the next turn of the caller's loop would hand it a
                        // fresh budget, and the real bound would become the caller's own deadline,
                        // which is the layer that actually owns the wall clock. It is not done
                        // because of what it would do to the WORST CASE: a drain costs its caller
                        // at most `MODULE_DRAIN_BUDGET` today, and a module that never answers
                        // would then cost `caller's budget + one more drain` instead, in 2 s
                        // chunks, on every turn. The WPT runner pumps in 50 ms slices to a 2,500 ms
                        // loop deadline under an 8,000 ms per-file watchdog; turning one bounded
                        // failure into a repeated one is how a sweep's tail gets long enough to
                        // start tripping it. A bound that moves with the caller is not a bound this
                        // lane is willing to claim, so the module fails once, here, and says so.
                        ModuleFailure::TimedOut(e) => self.record_module_error(&spec, &e),
                    }
                    // THE DOCUMENT'S `load` WAITED FOR THIS MODULE, and it waited for the
                    // EVALUATION, not for the bytes: the element loader took the token when it issued the
                    // fetch and handed it over with the queued module rather than releasing it on arrival.
                    // Measured on Chrome 150 over loopback, a module served 600 ms late and inserted from
                    // script: `ran:slow-mod`, the element's `load`, the document's `load` -- in that order.
                    // Released on every ending, including a timeout, so a module that never finishes costs
                    // the page its `load` no more than a `<script src>` that never finishes does. The
                    // ENDINGS THAT GOT AS FAR AS RUNNING THE BODY release it inside `run_started_module`;
                    // this releases the three that never did.
                    self.undelay_module(delay_token);
                }
            }
        }
        Ok(())
    }

    /// ISSUE EVERY QUEUED MODULE'S STATIC IMPORTS NOW, and answer the futures that drive them.
    /// This is the half of the drain that Chrome does in parallel and korvus did in
    /// series.
    ///
    /// # What Chrome does, measured rather than assumed
    ///
    /// Chrome 150.0.7871.46 over a loopback server that delays every import by 700 ms, a page with
    /// six module scripts and one import apiece (`tests/fixtures/module-drain-probe.corpus.txt`):
    /// **all six requests leave within 7 ms of the document arriving**, in one round, and the page
    /// is complete in 0.93 s rather than 4.2 s. A three-deep chain inside ONE graph is served one
    /// hop at a time (300 ms apart, measured on the same server), because a chain genuinely cannot
    /// be parallelised: the second URL is not known until the first body is parsed. So the rule is
    /// not "fetch everything", it is **independent graphs are independent** — and the drain's own
    /// queue is exactly a list of independent graph roots.
    ///
    /// # Why this is not a preload scanner, and not a second parser
    ///
    /// The specifiers come from **V8**, not from a regular expression: the source is compiled as a
    /// module and `get_module_requests()` is asked what it imports, which is the same answer
    /// deno_core's own load will get from the same compiler. Each specifier is then resolved by
    /// [`modules::resolve_from_base`] — the function the loader itself resolves with, import map
    /// and all — so a warmed URL is a URL the loader would have asked for, not a guess at one.
    ///
    /// **Nothing speculative is fetched.** A module on this queue WILL be loaded by this drain or
    /// the next turn of the loop; Chrome has already requested its imports by the time korvus even
    /// reaches the queue. A `refused` entry (its own response was a 404 or a bad MIME type) is
    /// skipped, because nothing will ever compile it. A compile that fails is skipped in silence:
    /// the real load reports the syntax error, and a module that does not parse has no imports.
    ///
    /// # Cost
    ///
    /// One extra V8 module compile per queued module, and only when the queue holds MORE THAN ONE —
    /// with a single module there are no independent graphs to overlap and the scan would buy
    /// nothing. The compile is discarded; `mod_evaluate` never sees it.
    fn warm_queued_module_imports(&mut self, which: Drain, owned: usize) -> Vec<modules::SharedModuleFetch> {
        // ONE module is the common case and has nothing to overlap with.
        if owned < 2 {
            return Vec::new();
        }
        // A fetch minted before the navigation named its persona would be refused for naming none,
        // and it would be refused ONCE, into the module map, where the real import would find the
        // refusal instead of the network. The drain is always inside a navigation that has set
        // this; if it somehow is not, warming nothing is the safe answer.
        if self.module_fetch.borrow().identity.is_none() {
            return Vec::new();
        }
        let queued: Vec<(String, String)> = {
            let os = self.runtime.op_state();
            let st = os.borrow();
            let Some(q) = st.try_borrow::<PendingModules>() else { return Vec::new() };
            q.0.iter()
                // The drain's OWN set first, exactly as `owned` counted it, and only then the
                // entries worth warming: taking `owned` from an already-thinned list would reach
                // past the modules this drain owns.
                .filter(|m| which.takes(m))
                .take(owned)
                .filter(|m| !m.refused)
                // The base a module's OWN imports resolve against: its `import_base` when the
                // parser recorded one (an inline module under `<base href>`), its name otherwise.
                // The same pair `drain_modules` installs when it reaches the module.
                .map(|m| (m.source.clone(), m.import_base.clone().unwrap_or_else(|| m.url.clone())))
                .collect()
        };
        let fetch = self.module_fetch.clone();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (source, base) in &queued {
            for spec in self.static_import_specifiers(source, base) {
                let Ok(url) = modules::resolve_from_base(&fetch, &spec, base, modules::Worded::StaticImport) else { continue };
                if !seen.insert(url.as_str().to_string()) {
                    continue;
                }
                if let Some(s) = modules::module_map_entry(&fetch, &url) {
                    out.push(s);
                }
            }
        }
        out
    }

    /// The specifiers a module source statically imports, as V8's own parser reports them. Empty
    /// for a source that does not compile — see [`Engine::warm_queued_module_imports`].
    ///
    /// `name` is the module's import base, used only for the `ScriptOrigin` so that a compile error
    /// (which is swallowed here) is attributed the way it would be anywhere else.
    fn static_import_specifiers(&mut self, source: &str, name: &str) -> Vec<String> {
        use deno_core::v8;
        self.with_scope(|scope| {
            let Some(code) = v8::String::new(scope, source) else { return Vec::new() };
            let Some(res) = v8::String::new(scope, name) else { return Vec::new() };
            // The last of the three trailing flags is `is_module`, which is what makes V8 accept
            // `import` at all; everything else matches the classic-script origin in `script_exec`.
            let origin = v8::ScriptOrigin::new(scope, res.into(), 0, 0, false, 0, None, false, false, true, None);
            v8::tc_scope!(let tc, scope);
            let mut src = v8::script_compiler::Source::new(code, Some(&origin));
            let Some(module) = v8::script_compiler::compile_module(tc, &mut src) else {
                // A syntax error, reported by the real load and not by this scan. The `TryCatch`
                // swallows it here so the pending exception does not leak into the drain.
                return Vec::new();
            };
            let requests = module.get_module_requests();
            let mut out = Vec::with_capacity(requests.length());
            for i in 0..requests.length() {
                let Some(item) = requests.get(tc, i) else { continue };
                let Ok(req) = v8::Local::<v8::ModuleRequest>::try_from(item) else { continue };
                out.push(req.get_specifier().to_rust_string_lossy(tc));
            }
            out
        })
    }

    /// The next module this drain owns, READ OUT of the queue with the entry left in place. See
    /// [`Engine::drain_modules`] for why it stays there until its evaluation begins.
    fn peek_pending_module(&mut self, which: Drain) -> Option<NextModule> {
        let os = self.runtime.op_state();
        let st = os.borrow();
        let q = st.try_borrow::<PendingModules>()?;
        let idx = q.0.iter().position(|m| which.takes(m))?;
        let pm = q.0.get(idx)?;
        Some(NextModule {
            idx,
            url: pm.url.clone(),
            source: pm.source.clone(),
            import_base: pm.import_base.clone(),
            refused: pm.refused,
        })
    }

    /// Take the entry at `idx` off the queue. Called at the instant the module's evaluation begins
    /// (or its ending is decided), never before: an entry off the queue is an entry nothing can retry.
    fn take_pending_module(&mut self, idx: usize) -> Option<PendingModule> {
        let os = self.runtime.op_state();
        let mut st = os.borrow_mut();
        let q = st.try_borrow_mut::<PendingModules>()?;
        (idx < q.0.len()).then(|| q.0.remove(idx))
    }

    /// Release a queued module's "delay the load event" token, if it carried one. An inline module and
    /// every parser-inserted module carry `None`: the runtime's own fetch pass already had their bytes.
    fn undelay_module(&mut self, token: Option<u32>) {
        let Some(token) = token else { return };
        self.with_scope(|scope| crate::navigation::quietly(scope, |scope| webapi::page::undelay_load(scope, token)));
    }

    /// Is a `<script type=module>` still waiting for the drain? [`Engine::pump`] asks before it reports
    /// `Drained`: a module the PAGE inserted is queued from a fetch completion INSIDE the loop, and
    /// `run()` drains the queue on the way in, so the turn that queued it can never be the turn that
    /// runs it. Without this a page whose whole behaviour is in an inserted module settled with the
    /// module still on the queue and the caller stopped pumping, which is the same silence P15c was
    /// sent to remove, one layer further out.
    fn has_pending_modules(&mut self) -> bool {
        let state = self.runtime.op_state();
        let st = state.borrow();
        // while the parser waits (`script_exec::ParserWaiting`) its own modules wait for the parse's end: no turn runs them
        let parsing = st.try_borrow::<crate::script_exec::ParserWaiting>().is_some();
        st.try_borrow::<PendingModules>().is_some_and(|q| q.0.iter().any(|m| !parsing || m.inserted))
    }

    /// FETCH AND LINK one module's graph, bounded by `deadline` — everything HTML does before it runs
    /// the body. This is the drain's ONE cancellable step, and the reason it is a step of its own:
    /// the queue entry stays in `PendingModules` for the whole of it, so a caller whose future is
    /// dropped here costs the page nothing but a retry on the next turn of the loop. See
    /// [`Engine::drain_modules`].
    ///
    /// Loading is where the imports are FETCHED, so it gets its own share of the drain's budget; a
    /// helper that never answers must not eat the whole navigation. Timing out is not losing the
    /// module either — the entry is still queued.
    async fn load_one_module(
        &mut self,
        spec: &ModuleSpecifier,
        code: String,
        deadline: tokio::time::Instant,
    ) -> Result<deno_core::ModuleId, ModuleFailure> {
        let load = self.runtime.load_side_es_module_from_code(spec, code);
        tokio::time::timeout_at(deadline, load)
            .await
            .map_err(|_| ModuleFailure::TimedOut(anyhow::anyhow!("load module: timed out fetching imports")))?
            .map_err(|e| {
                // A JavaScript exception out of LOADING is a parse error (of this module or of one it
                // imports); anything else is the loader refusing a fetch.
                let threw = matches!(e.0.as_ref(), deno_core::error::CoreErrorKind::Js(_));
                let e = anyhow::anyhow!("load module: {e}");
                if threw { ModuleFailure::Threw(e) } else { ModuleFailure::Fetch(e) }
            })
    }

    /// RUN a linked module's body, and fire what its element is owed. **Synchronous on purpose**: from
    /// the first line to the last there is no `await`, so no caller can be cancelled part-way through
    /// and leave a module evaluated but its element silent.
    ///
    /// # WHERE THE ELEMENT'S `load` FIRES
    ///
    /// At the moment HTML's "run the script" RETURNS. For a module with no top-level `await` that is
    /// the end of its body; for one WITH a top-level await it is the FIRST AWAIT, with the rest of the
    /// module still to run.
    ///
    /// Chrome 150.0.7871.46, headless, over loopback, 2026-09-21 (loadavg 35.83). A page inserts
    /// `<script type=module src>` whose body is
    /// `P('tla:before'); await new Promise(r => setTimeout(r, 400)); P('tla:after');`:
    ///
    /// ```text
    ///   inserted:        DCL @1.8   tla:before @8.0   onload:tla @8.2   doc:load @8.2   tla:after @411.7
    ///   parser-inserted: tla:before @0.7  onload:ptla @1.8  DCL @1.9  doc:load @2.6  tla:after @403.2
    ///   with an import:  DCL @5.7  ran:dep @13.3  tla:before @13.3  onload:tla @13.4  doc:load @13.4  tla:after @415.4
    /// ```
    ///
    /// The element's `load` and the DOCUMENT's `load` both land AT the first await — 400 ms before the
    /// module finishes — on the inserted path and on the parser-inserted path alike. korvus fired both
    /// once the evaluation promise SETTLED, which is the wrong moment and was also an unreachable one:
    /// the drain awaited that promise in 25 ms slices with the module already off its queue, so a
    /// caller whose future was dropped in that window took the element's `load` with it —
    /// permanently, since nothing was left to retry. Traced at 0c2a34b2, a failing run logged the
    /// drain taking `…/tla.mjs` and no matching event, while the page's own log still showed
    /// `tla:before`, `tla:after` and the document's `load`: V8 kept evaluating, only the event was
    /// lost.
    ///
    /// A module suspended at a top-level await is LEFT suspended. The rest of its body runs on the
    /// event loop every caller already drives — which is also what stops the PARSE waiting for it:
    /// korvus used to put `DOMContentLoaded` and the document's `load` on the far side of the await,
    /// where Chrome puts them 400 ms before.
    fn run_started_module(
        &mut self,
        spec: &ModuleSpecifier,
        id: deno_core::ModuleId,
        element: Option<&deno_core::v8::Global<deno_core::v8::Value>>,
        delay_token: Option<u32>,
    ) {
        // A module's own throw is re-raised by deno_core through a promise it derives here; the
        // hook around this one call is how the realm tells that promise from a page's own
        // rejection (`webapi::core::events::report::ModuleRethrows`).
        self.with_scope(|scope| webapi::begin_module_evaluation(scope));
        let eval = self.runtime.mod_evaluate(id);
        self.with_scope(|scope| webapi::end_module_evaluation(scope));
        let mut eval = std::pin::pin!(eval);
        // ONE poll, and the answer is already in: `mod_evaluate` ran the body and drained microtasks
        // before it returned, so the evaluation promise of a module with NO top-level await has
        // already settled and that of a module WITH one is pending. (One poll, because a `Map`
        // future panics if it is polled again after returning `Ready`.)
        match deno_core::futures::FutureExt::now_or_never(eval.as_mut()) {
            // It threw before reaching any top-level await -- it had none, or it threw first. Chrome
            // reports the throw and fires `load` anyway: a module that threw still loaded.
            Some(Err(e)) => self.record_module_error(spec, &anyhow::anyhow!("evaluate module: {e}")),
            // It ran to the end.
            Some(Ok(())) => {}
            // It is SUSPENDED at a top-level `await`. A rejection after this point is reported
            // through the realm's uncaught path, where deno_core re-throws it.
            None => {}
        }
        if let Some(el) = element {
            self.fire_module_element(el, "load");
        }
        self.undelay_module(delay_token);
    }

    /// Fire `load` or `error` at an external module script's element.
    fn fire_module_element(&mut self, el: &deno_core::v8::Global<deno_core::v8::Value>, ty: &str) {
        self.with_scope(|scope| {
            crate::navigation::quietly(scope, |scope| {
                let el = deno_core::v8::Local::new(scope, el);
                webapi::page::fire_script_event(scope, el, ty);
            });
        });
    }

    /// Report a failed module script TO THE PAGE, through the same uncaught path a throwing classic
    /// script goes through — which tallies it and fires an `ErrorEvent` at the global, so
    /// `window.onerror` and the WPT report hook's listener both see it.
    ///
    /// Without this a module failure is invisible: it is not a page-script throw, nothing on the JS
    /// side ever hears about it, and the file reaches no verdict with an EMPTY `errors` array — the
    /// exact silence that unit was sent to remove, reintroduced one layer down.
    fn record_module_error(&mut self, spec: &ModuleSpecifier, err: &anyhow::Error) {
        let msg = format!("module {spec}: {err}");
        self.with_scope(|scope| script_exec::report_module_failure(scope, &msg));
    }

    /// Pump the JS event loop, returning when it genuinely settles OR `budget` elapses -- whichever
    /// is first.
    ///
    /// `run()` drains the loop unconditionally: it resolves only when NOTHING is pending. But a real
    /// web page keeps perpetual timers alive (setInterval carousels, analytics heartbeats, polling,
    /// requestAnimationFrame loops), so the loop is never fully idle -- draining it hangs forever.
    /// That is exactly what stalls `Page.navigate` (and every later `Runtime.evaluate`, which also
    /// pumps first) on modern-JS sites: webscraper.io's app.js, React/Vue SPAs, npm, and friends.
    ///
    /// `tokio::time::timeout` gives the behaviour a live browser has: a page whose scripts finish and
    /// leave nothing scheduled settles and returns instantly (the inner future completes early); a
    /// page with long-lived timers runs its ready microtasks + due timers for up to `budget`, then we
    /// hand control back with those timers still pending. Dropping the half-polled event-loop future
    /// is safe -- deno_core is built to be polled incrementally, and the runtime state persists in
    /// `self.runtime` for the next pump.
    ///
    /// THE RETURN VALUE IS WHY A CALLER MAY STOP. A caller that pumps in a loop waiting for the page
    /// to reach some state -- `load` fired, a result published -- has to tell the two endings apart.
    /// `BudgetElapsed` means work is still scheduled and another slice can change the answer;
    /// `Drained` means nothing is left to run and the answer cannot change without new input, so
    /// polling again until a deadline is a spin that burns a core and learns nothing. Both loops in
    /// this workspace did exactly that: a settled page cost 5.9 s of CPU for a 5 s budget.
    ///
    /// IT ALSO KEEPS THE LOAD-DELAY SET HONEST, in two ways, neither a
    /// deadline -- Chrome's `load` waits for its subresources however long they take:
    ///
    /// * it stamps the event loop's TURNS (`webapi::EventLoopTurns`) from a beat future polled beside
    ///   the loop. A page that holds the isolate in a synchronous spin stops the stamps, and the
    ///   load-delay watchdog thread reads that -- and only that -- as the starvation it must break.
    /// * a loop that DRAINS with the load step still parked behind a non-empty set can never release
    ///   it (every release is a completion on this loop), so the step runs then rather than never.
    pub async fn pump(&mut self, budget: std::time::Duration) -> anyhow::Result<PumpEnd> {
        self.pump_inner(budget, std::future::pending::<()>(), false).await
    }

    /// [`Engine::pump`] for a HOST THAT HAS ITS OWN INPUT TO DELIVER (FIXER C11): it also returns, with
    /// `BudgetElapsed`, at the first boundary between two turns of the loop where `interrupt` has completed
    /// or the page has asked for a cross-document navigation.
    ///
    /// `korvus serve` used to run the page 25 ms out of every 50 (an idle tick that pumped a fixed slice and
    /// then left the loop alone), so a `setTimeout(f, 10)` fired up to 32 ms late and `requestAnimationFrame`
    /// intervals spread from 3.5 to 36.8 ms, where Chrome 150 and `korvus fetch` hold 10.0-10.2 and 16.6-16.8.
    /// A pump that runs all the time needs a way to hand the thread back the moment the DevTools socket has a
    /// command, which is what `interrupt` is: Chrome's renderer runs an input event (and a DevTools command)
    /// as its next task, between two of the page's tasks, never in the middle of one -- and a turn boundary
    /// is exactly that point here, since a turn runs its JavaScript to completion inside one poll. The
    /// interrupt is polled BEFORE the next turn, so a ready command waits for the task already running and
    /// for nothing else.
    ///
    /// A navigation the page asked for stops the slice for the same reason: the driver performs it (Chrome's
    /// browser process starts it at once -- 2 ms from the ask to the request, measured), and a document that
    /// is being left has nothing left to do that the driver should wait for.
    pub async fn pump_until<F: std::future::Future>(&mut self, budget: std::time::Duration, interrupt: F) -> anyhow::Result<PumpEnd> {
        self.pump_inner(budget, interrupt, true).await
    }

    /// The one pump behind [`Engine::pump`] and [`Engine::pump_until`]: with `interrupt` pending forever and
    /// `stop_on_navigation` off it is `pump` exactly as it always was.
    async fn pump_inner<F: std::future::Future>(
        &mut self,
        budget: std::time::Duration,
        interrupt: F,
        stop_on_navigation: bool,
    ) -> anyhow::Result<PumpEnd> {
        self.loop_resumed();
        let out = self.pump_slice(budget, interrupt, stop_on_navigation).await;
        self.loop_stopped();
        out
    }

    /// The host runs the page's loop again ([`Engine::run`], a pump), and stops running it. Between the two
    /// no frame can run: a CSS animation made before such a gap is started where Chrome's next frame would have
    /// started it, not at the late frame after it (`webapi::css_loop_resumed`).
    fn loop_resumed(&mut self) {
        self.with_scope(webapi::css_loop_resumed);
    }

    fn loop_stopped(&mut self) {
        self.with_scope(webapi::css_loop_stopped);
    }

    /// When the page's next CSS animation or transition event falls due, with the frame that sends it: a host
    /// that runs no loop while the page has nothing else to do -- `korvus serve`'s settled page -- wakes it then, so
    /// `transitionend` and the rest are sent from the page's own frames and not at its next command. `None` when no
    /// event is to come.
    pub fn css_wake_due(&mut self) -> Option<std::time::Instant> {
        let ms = self.with_scope(webapi::css_next_event_in)?;
        Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(ms.max(0.0) / 1000.0))
    }

    /// One pump: [`Engine::pump_inner`] without the host's start and stop.
    async fn pump_slice<F: std::future::Future>(
        &mut self,
        budget: std::time::Duration,
        interrupt: F,
        stop_on_navigation: bool,
    ) -> anyhow::Result<PumpEnd> {
        let deadline = tokio::time::Instant::now() + budget;
        let turns = self.event_loop_turns();
        // Read between turns, while the loop's future holds the runtime: the realm's state is its own `Rc`.
        let op_state = self.runtime.op_state();
        let asked = || stop_on_navigation && crate::navigation::navigation_requested(&op_state.borrow());
        tokio::pin!(interrupt);
        // Polls still owed after a worker woke this loop (see the park below).
        let mut woken_polls = 0u8;
        loop {
            // THE HOST'S RENDERING OPPORTUNITY: a scroll or mutation made since the last
            // frame -- by a CDP command, or by the task that drained the loop -- arms one.
            self.rendering_opportunity();
            let now = tokio::time::Instant::now();
            if now >= deadline || asked() {
                return Ok(PumpEnd::BudgetElapsed);
            }
            // THE MODULE DRAIN IS NOT INSIDE THE SLICE, and this is the line that makes
            // an inserted module RUN rather than vanish.
            //
            // `run()` drains the queue on the way in, and the drain is `await`ed: a module whose import
            // is still being fetched, or whose top-level `await` has not settled, is parked there. Put
            // that under `timeout(deadline - now, run)` and the slice DROPS the half-finished future --
            // and the queue was emptied by `mem::take` before the first await, so the module is gone,
            // silently, with no event and no error. `pump_until_loaded`'s slice is 25 ms, so every
            // inserted module with a static import died that way: measured on the loopback fixture,
            // `import {d} from './dep.mjs'` FETCHED the dependency and then produced nothing at all --
            // no `ran:dep`, no `ran:graph`, no `load` at the element, an empty `uncaught`.
            //
            // It is bounded by MODULE_DRAIN_BUDGET from the inside, which is the bound that belongs to
            // it, and it beats the loop's turn stamps while it runs so the load-delay watchdog does not
            // read a drain as a synchronous spin.
            turns.enter();
            let drained = {
                let beat = async {
                    loop {
                        turns.beat();
                        tokio::time::sleep(webapi::EventLoopTurns::BEAT).await;
                    }
                };
                let drain = self.drain_load_tasks();
                tokio::pin!(drain);
                tokio::pin!(beat);
                tokio::select! {
                    r = &mut drain => r,
                    _ = &mut beat => unreachable!("the beat future never completes"),
                }
            };
            turns.leave();
            drained?;
            // The drain may have spent the budget; re-read the clock rather than handing `timeout` a
            // duration computed before it.
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(PumpEnd::BudgetElapsed);
            }
            turns.enter();
            // `timeout(deadline - now, run)` as a hand-written poll, so that the interrupt is asked first and
            // the navigation check runs between two turns (see `pump_until`). With the interrupt pending and
            // no navigation check it is that `timeout`: the loop first, then the deadline.
            // THE VSYNC, for a plugin that wants frames (plugin ABI 3, C1). The frame tap paints at the host's
            // rendering opportunity, which the top of this loop offers -- and a running page (a timer, a fetch in
            // flight) returns there only when the slice runs out, 50 ms in `korvus serve`: a page that changed
            // its picture from a timer or a command reached the plugin at 20 frames a second (measured
            // 2026-10-22, D72). So while a plugin wants frames the slice also wakes at the display's next vsync
            // and offers the opportunity there (`webapi::deliver_frame` hands on only a changed picture). No such
            // plugin: no vsync, the slice as it always was (I9/J2).
            let vsync = if plugin_host::has_frames() {
                let ms = self.with_scope(webapi::next_frame_in_ms);
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs_f64(ms.max(0.0) / 1000.0))
            } else {
                None
            };
            let ran = {
                let beat = async {
                    loop {
                        turns.beat();
                        tokio::time::sleep(webapi::EventLoopTurns::BEAT).await;
                    }
                };
                let run = self.run_loop();
                let sleep = tokio::time::sleep_until(deadline);
                let frame = vsync.map(tokio::time::sleep_until);
                tokio::pin!(run);
                tokio::pin!(beat);
                tokio::pin!(sleep);
                tokio::pin!(frame);
                let mut busy = std::time::Duration::ZERO;
                std::future::poll_fn(|cx| {
                    use std::future::Future;
                    use std::task::Poll;
                    if interrupt.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Turn::Stopped);
                    }
                    let polled = std::time::Instant::now();
                    crate::scheduler::arm_frame_cap(&op_state, true);
                    let ran = run.as_mut().poll(cx);
                    crate::scheduler::arm_frame_cap(&op_state, false);
                    if let Poll::Ready(r) = ran {
                        return Poll::Ready(Turn::Drained(r));
                    }
                    busy += polled.elapsed();
                    // a turn of the loop may be what completes the interrupt (the parser's own end, a task of its own)
                    if asked() || interrupt.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Turn::Stopped);
                    }
                    if beat.as_mut().poll(cx).is_ready() {
                        unreachable!("the beat future never completes");
                    }
                    if sleep.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Turn::Stopped);
                    }
                    if frame.as_mut().as_pin_mut().is_some_and(|f| f.poll(cx).is_ready()) {
                        return Poll::Ready(Turn::Vsync);
                    }
                    if busy >= BUSY_YIELD {
                        return Poll::Ready(Turn::Busy);
                    }
                    Poll::Pending
                })
                .await
            };
            turns.leave();
            match ran {
                // A vsync while a plugin wants frames: back to the top, whose rendering opportunity paints -- after
                // one yield, as after a busy stretch (`BUSY_YIELD`). A frame that takes longer than a refresh finds
                // the next vsync already due, and the loop went from paint to paint without ever handing the thread
                // back: the IO driver never ran, so the DevTools socket never read ready (the interrupt) and the
                // screencast's writer never wrote.
                Turn::Vsync | Turn::Busy => {
                    tokio::task::yield_now().await;
                    continue;
                }
                // The loop genuinely drained: no timer, no in-flight op, no unresolved promise.
                Turn::Drained(r) => {
                    r?;
                    // A DRAINED LOOP WITH THE LOAD STEP PARKED can never release it: run the step, and
                    // poll again for whatever the `load` handlers scheduled.
                    // A DRAINED LOOP WITH A MODULE STILL QUEUED IS NOT IDLE. `run()`
                    // drains the module queue on the way IN, so a module the page inserted -- queued
                    // by a fetch completion inside the loop this call just drained -- is still there.
                    // Another turn runs it; the budget above still bounds how many turns there are.
                    //
                    // BEFORE the load-delay release below, and that order is the measured behaviour,
                    // not tidiness: the queued module still holds its element's "delay the load event"
                    // token, and `release_unreleasable_load_delays` exists to fire `load` for a set
                    // that can never empty on a drained loop. Asked first, it fired the document's
                    // `load` BEFORE the module it was waiting for had run -- `DCL, load, ran:mod`
                    // where Chrome 150 has `DCL, ran:mod, onload, load`.
                    if self.has_pending_modules() {
                        continue;
                    }
                    if self.release_unreleasable_load_delays() {
                        continue;
                    }
                    // NOR IS A DRAINED LOOP WITH A LIVE WORKER IDLE. `Drained` promises "re-polling
                    // cannot produce more work", and while a `Worker` is alive that is simply false:
                    // its four receive promises (host.js's pumpMsg/pumpBc/pumpPort/pumpErr) are
                    // deliberately UNREF'd so a parked receive does not pin the page loop, so the loop
                    // reports idle while a message from another thread is still on its way. Nothing
                    // used to notice, because `setTimeout` was a promise chain that left a pending
                    // `op_sleep` behind for every armed timer and the loop was therefore never idle on
                    // a real page. Once the timer task source made "idle" mean idle, the
                    // lie became reachable: a caller that stops pumping on `Drained` (the WPT runner
                    // does) gave a page waiting on a worker ONE 50 ms slice, and a spin of
                    // instantly-returning polls never yields to the tokio runtime the local server and
                    // the worker bridge live on either. Measured on
                    // `html/webappapis/.../event-handler-processing-algorithm-error/worker.html`:
                    // 3/3 in 296 ms before, 0/3 in 103 ms after, back to 3/3 with this.
                    //
                    // So: sleep a short quantum (which DOES yield) and poll again, exactly as a page
                    // with a pending op does. The budget still bounds it, and `run()` is untouched --
                    // its contract is deno_core's, not HTML's.
                    if !self.has_live_worker() && !self.rendering_opportunity() {
                        return Ok(PumpEnd::Drained);
                    }
                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        return Ok(PumpEnd::BudgetElapsed);
                    }
                    // Woken the moment a worker sends this page a frame; the quantum stays as
                    // the ceiling for everything else a live worker can make ready.
                    // The poll a wake buys completes the unref'd receive op, and its promise resolves on the
                    // NEXT poll: a loop that parked straight after it sat out the whole quantum per frame.
                    // So a woken loop polls twice more before it parks again.
                    if woken_polls > 0 {
                        woken_polls -= 1;
                        tokio::task::yield_now().await;
                        continue;
                    }
                    tokio::select! {
                        biased;
                        _ = &mut interrupt => return Ok(PumpEnd::BudgetElapsed),
                        _ = crate::worker::PAGE_WAKE.notified() => woken_polls = 2,
                        _ = tokio::time::sleep((deadline - now).min(std::time::Duration::from_millis(5))) => {}
                    }
                }
                // Budget elapsed with work still scheduled -- fine, like a live page. Or the host's interrupt
                // came, or the page asked to navigate: work is still scheduled either way.
                Turn::Stopped => return Ok(PumpEnd::BudgetElapsed),
            }
        }
    }

    /// Run everything the event loop can run RIGHT NOW, then return -- never wait for work that is
    /// not ready. `budget` is a ceiling on a loop that keeps making itself ready, not a duration to
    /// spend.
    ///
    /// WHY THIS IS NOT [`Engine::pump`] WITH A SMALL BUDGET. A reader that has to
    /// observe the page's state first has to flush what the page has already finished: settled
    /// promises, ready microtasks, a fetch whose response has landed. `pump` does that -- and then,
    /// on any page that does not drain, it goes on waiting until its wall clock runs out, because a
    /// page with a `setInterval` heartbeat or a rAF loop never drains. Measured 2026-09-18 over the
    /// replay tier: every `Runtime.evaluate` cost a flat 100 ms of wall clock before the expression
    /// ran, on a probe whose own work is 3 ms on webscraper.io and 15 ms on prestashop.com. That is
    /// not a cost that scales with the page; it is a constant per CDP round trip, and it was the
    /// whole of the engine's wall-clock deficit against Chrome on the load axis.
    ///
    /// HOW "READY" IS DECIDED, AND WHY IT IS NOT A TIMER. The loop is polled with a waker of our
    /// own. A poll that returns `Pending` WITHOUT having signalled that waker has parked: everything
    /// runnable has run and the next thing to happen is a timer falling due or an op completing on
    /// another thread -- neither of which a reader should block on, and both of which the CDP idle
    /// tick already advances between commands (see korvus-cdp's `message_loop`). A poll that DID
    /// signal it has more ready work queued behind it, so it is polled again. deno_core is built to
    /// be polled incrementally and `pump` already drops half-polled loop futures, so stopping here
    /// leaves exactly the state the next pump continues from.
    ///
    /// The result is the same as `pump`'s on a page that settles -- both return the instant the loop
    /// drains -- and differs only on the page `pump`'s budget was written for.
    pub async fn flush_ready(&mut self, budget: std::time::Duration) -> anyhow::Result<PumpEnd> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        /// A waker that only records that it was woken. See [`Engine::flush_ready`].
        struct Signal(AtomicBool);
        impl std::task::Wake for Signal {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let deadline = tokio::time::Instant::now() + budget;
        let turns = self.event_loop_turns();
        let op_state = self.runtime.op_state();
        loop {
            // The same rendering opportunity `pump` opens: a scroll or a mutation made by the last
            // command arms a frame, and a reader must see it.
            self.rendering_opportunity();
            if tokio::time::Instant::now() >= deadline {
                return Ok(PumpEnd::BudgetElapsed);
            }
            // THE MODULE DRAIN IS AWAITED, exactly as in `pump` and for P15c's reason: a module
            // parked on an import that is still being fetched must not be dropped half-evaluated.
            // `MODULE_DRAIN_BUDGET` bounds it from the inside.
            turns.enter();
            let drained = {
                let beat = async {
                    loop {
                        turns.beat();
                        tokio::time::sleep(webapi::EventLoopTurns::BEAT).await;
                    }
                };
                let drain = self.drain_load_tasks();
                tokio::pin!(drain);
                tokio::pin!(beat);
                tokio::select! {
                    r = &mut drain => r,
                    _ = &mut beat => unreachable!("the beat future never completes"),
                }
            };
            turns.leave();
            drained?;

            let signal = Arc::new(Signal(AtomicBool::new(false)));
            let waker = std::task::Waker::from(signal.clone());
            let mut cx = std::task::Context::from_waker(&waker);
            // ONE YIELD IS ALLOWED, AND IT IS WHAT MAKES "READY" MEAN READY. A timer whose deadline
            // has already passed is ready work, but tokio only moves it out of its timer wheel when
            // the runtime gets a turn -- and polling deno_core with a waker of our own never gives
            // it one. So a `setTimeout(fn, 0)` armed microseconds ago read as "not ready" and a
            // reader saw the page before its own due task had run, which is not what any of the
            // callers of this function mean. `yield_now` costs a scheduler tick, not a wall-clock
            // wait, and it is spent ONCE: the second time the loop parks with nothing to say, it is
            // genuinely waiting on something that has not happened.
            let mut yielded = false;
            turns.enter();
            let ran = {
                let run = self.runtime.run_event_loop(PollEventLoopOptions::default());
                tokio::pin!(run);
                loop {
                    turns.beat();
                    signal.0.store(false, Ordering::SeqCst);
                    crate::scheduler::arm_frame_cap(&op_state, true);
                    let polled = std::future::Future::poll(run.as_mut(), &mut cx);
                    crate::scheduler::arm_frame_cap(&op_state, false);
                    match polled {
                        std::task::Poll::Ready(r) => break Some(r),
                        std::task::Poll::Pending => {
                            // Parked: nothing is runnable now. Or it queued more for itself, in
                            // which case poll again -- until the ceiling, so a loop that makes
                            // itself ready forever still hands control back.
                            if tokio::time::Instant::now() >= deadline {
                                break None;
                            }
                            if !signal.0.load(Ordering::SeqCst) {
                                if yielded {
                                    break None;
                                }
                                yielded = true;
                                tokio::task::yield_now().await;
                            }
                        }
                    }
                }
            };
            turns.leave();
            match ran {
                None => return Ok(PumpEnd::BudgetElapsed),
                Some(r) => {
                    r.map_err(|e| anyhow::anyhow!("run_event_loop failed: {e}"))?;
                    // The same two "a drained loop is not finished" cases `pump` handles, in the
                    // same order and for the same measured reasons (P15c, then the load-delay
                    // release).
                    if self.has_pending_modules() {
                        continue;
                    }
                    if self.release_unreleasable_load_delays() {
                        continue;
                    }
                    return Ok(PumpEnd::Drained);
                }
            }
        }
    }

    /// Does this page still own a `Worker` that has not been terminated? See [`Engine::pump`]: while one
    /// is alive, another thread can still deliver into this realm, so the loop is not idle even when
    /// deno_core says it has nothing scheduled.
    fn has_live_worker(&mut self) -> bool {
        let state = self.runtime.op_state();
        let st = state.borrow();
        st.try_borrow::<worker::WorkerRegistry>().is_some_and(|r| r.has_live_worker())
            // A remote frame is another thread that can still send this page a task.
            || crate::oopif::live_remote_frames(&st) > 0
    }

    /// The event loop's turn record the load-delay watchdog reads. See [`Engine::pump`].
    fn event_loop_turns(&mut self) -> std::sync::Arc<webapi::EventLoopTurns> {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::event_loop_turns(scope)
    }

    /// Offer the page's geometry observers a rendering opportunity; `true` if it armed a frame.
    ///
    /// It also moves the animation timeline on: a rendering opportunity is exactly where
    /// HTML samples it, and between two of them every observation must see the same moment -- which
    /// is what Chrome does and what keeps a style read on an animating page from re-sampling, and
    /// therefore re-laying-out, the whole document per call.
    fn rendering_opportunity(&mut self) -> bool {
        // a render-blocked document has no rendering opportunity (`webapi::page::render_blocked`)
        if webapi::page::render_blocked(&self.runtime.op_state().borrow()) {
            return false;
        }
        self.advance_frame_time();
        use deno_core::v8;
        // RENDER #6: a frame is also due when an image finished loading since our last paint (the
        // `FetchedImages` generation advanced past `last_painted_img_gen`). Read it before entering the
        // v8 scope, which borrows the runtime; compositor-only, so the default path pays nothing.
        let img_gen = if paint_each_frame() {
            let os = self.runtime.op_state();
            let st = os.borrow();
            st.try_borrow::<spec::kernel::Context>()
                .and_then(|c| c.get::<spec::FetchedImages>())
                .map(|i| i.generation())
                .unwrap_or(self.last_painted_img_gen)
        } else {
            self.last_painted_img_gen
        };
        let img_loaded = img_gen != self.last_painted_img_gen;
        // SESSION 2b-repaint: a frame is ALSO due when a plugin fed a new camera frame since our last paint
        // (`fed_frame_generation` advanced). Unlike img_gen this is read UNCONDITIONALLY -- the whole point is the
        // DEFAULT no-compositor path where a fed <video> would otherwise never paint -- and it is only a cheap
        // atomic load. It stays equal forever on any no-plugin / no-fed run, so the gate below is unchanged then.
        let fed_gen = plugin_host::fed_frame_generation();
        let fed_pending = fed_gen != self.last_painted_fed_gen;
        let armed = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            let armed = webapi::rendering_opportunity(scope);
            // long animation frames whose rendering update is due, onto their realms' timelines
            webapi::loaf_rendering_opportunity(scope);
            // Paint the document to an offscreen frame when a frame is due. A frame
            // is due when a geometry observer armed one (`armed`) OR a rAF animation is producing frames
            // (`has_pending_animation_frame`). Without the rAF term a rAF-only animation never repainted,
            // because `armed` is observer-geometry-change only (RC2.b.0). An idle/static page (no observer,
            // no rAF) still costs ~0 — render_document_frame does the first paint then skips. Opt-in
            // (KORVUS_COMPOSITOR=page) until cost-verified; default unchanged.
            // C1 (plugin ABI 3 wave 1, the D29 streaming core): a plugin that wants frames
            // (`want_frames` + `on_frame`) forces the per-frame paint even without the compositor opt-in,
            // so its `on_frame` sees every painted frame; a frame no plugin wants is never captured.
            let want_frames = plugin_host::has_frames();
            if paint_each_frame() || want_frames || fed_pending {
                // render_document_frame itself always does the FIRST paint and then gates on the flag (the
                // initial render is a paint even when nothing armed a frame). `fed_pending` is OR'd into the flag
                // too: FirstPaintDone is set after load, so without it cssom's `if !first && !armed { return }`
                // would skip the fed repaint. A fed-only paint delivers no on_frame to a plugin that never opted
                // into frames.
                let animating = webapi::has_pending_animation_frame(scope);
                let mut owed = false;
                if !want_frames {
                    webapi::render_document_frame(scope, armed || animating || img_loaded || fed_pending);
                } else if !animating {
                    // THE FRAME TAP: paint, and hand the frame to every frame-wanting plugin if its pixels changed
                    // since the last one they were handed (`webapi::deliver_frame`). A page whose animation frame is
                    // pending is left to that frame, which paints right after its callbacks (`rendering::cb_frame`):
                    // painted here first, the plugin would be handed the picture from before them. A frame owed (too
                    // soon after the last) keeps the page running until the next vsync offers it.
                    owed = webapi::deliver_frame(scope);
                }
                armed || owed
            } else {
                webapi::note_paint_without_compositor(scope);
                armed
            }
        };
        // RENDER #6: record the generation we just offered a frame at, so the next loaded image (which
        // bumps the generation) arms exactly one repaint — and not a repeat for one already painted.
        self.last_painted_img_gen = img_gen;
        // SESSION 2b-repaint: catch the fed-frame cursor up, so the next opportunity sees `fed_pending == false`
        // (exactly one paint per fed frame) and a settled no-fed page parks at zero cost.
        self.last_painted_fed_gen = fed_gen;
        // Offer the SAME rendering opportunity to the document compositor.
        // Under `NullCompositor` -- the default provided by `PagePlugin` --
        // `update_rendering` paints nothing, so this is the pre-RC0 behaviour (R5). RC1 makes a real
        // compositor paint the box tree here. Clone the handle out of op_state before calling, the
        // canvas idiom, so the borrow is dropped first (RC1's paint walk will itself read op_state).
        let comp = {
            let os = self.runtime.op_state();
            let st = os.borrow();
            st.try_borrow::<spec::kernel::Context>()
                .and_then(|c| c.get::<spec::CompositorHandle>())
                .map(|h| h.0.clone())
        };
        if let Some(comp) = comp {
            comp.update_rendering();
        }
        // RENDER #6: report "work pending" when an image load caused a repaint this turn, so the pump
        // runs one more turn and DELIVERS the largest-contentful-paint notification the repaint queued
        // (otherwise the emit lands on the loop's final turn and its observer task is never pumped).
        // One-shot: `last_painted_img_gen` is now caught up, so next turn `img_loaded` is false.
        // `fed_pending` is likewise one-shot (the cursor is caught up above) -- it reports work this turn so the
        // pump runs once more and dispatches the media events fed_metadata queued, before the loop settles.
        armed || img_loaded || fed_pending
    }

    /// Move the animation timeline on to a fresh moment.
    ///
    /// The timeline is sampled ONCE per rendering opportunity and held for everything that observes
    /// between two of them -- which is what Chrome does (`document.timeline.currentTime` read five
    /// times in one task is one value, measured on Chrome 150) and what keeps a style read on an
    /// animating page from re-sampling, and therefore re-laying-out, the whole document per call.
    /// It reads no clock: the sample is taken lazily by the first observation that wants one.
    fn advance_frame_time(&mut self) {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::advance_frame_time(scope);
    }

    /// Run a load step a drained loop can never release. `true` if it ran.
    fn release_unreleasable_load_delays(&mut self) -> bool {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::release_unreleasable_load_delays(scope)
    }

    /// A thread-safe handle to interrupt this engine's isolate (see [`IsolateInterrupt`]). Hand it to a
    /// watchdog thread that calls `terminate()` if a page script runs synchronously past a deadline; the
    /// pump/eval in progress then returns an error instead of hanging the process forever.
    pub fn isolate_interrupt(&mut self) -> IsolateInterrupt {
        IsolateInterrupt(self.runtime.v8_isolate().thread_safe_handle())
    }

    /// Clear a pending termination posted by [`IsolateInterrupt::terminate`], so the isolate can execute
    /// again -- call after the watchdog fired to salvage state captured before the hang (e.g. read back
    /// an incremental result). Without this, every subsequent script on the isolate re-terminates.
    pub fn clear_termination(&mut self) {
        self.runtime.v8_isolate().cancel_terminate_execution();
    }

    /// Pump the event loop until the page fires its `load` event (recorded in `OpState` by
    /// `navigation.rs::set_load_fired`), then briefly settle -- OR until `budget` elapses. This lets
    /// navigation return as soon as the page is loaded instead of always burning the full budget on a
    /// page whose perpetual timers keep the loop non-idle. A page that settles with no timers still
    /// returns even earlier (each pump slice completes instantly).
    pub async fn pump_until_loaded(&mut self, budget: std::time::Duration) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + budget;
        let slice = std::time::Duration::from_millis(25);
        loop {
            let end = self.pump(slice).await;
            // A RUST READ, NOT A PAGE EVAL. This was the only caller of `eval_bool` and
            // the only thing that kept `__loadFired` on the page's bridge; it asked a running realm a
            // question the host itself had already answered, once per 25 ms slice, in JavaScript.
            if self.load_fired() {
                // A short post-load settle so load-handler microtasks/immediate work flush.
                let _ = self.pump(std::time::Duration::from_millis(50)).await;
                return Ok(());
            }
            // `load` fires from work the event loop has to run -- a delayed-load entry releasing, a
            // subresource settling, a timer. A DRAINED loop has none of that pending, so the answer
            // cannot change and the remaining budget would be spent re-asking the same question at
            // full CPU. Return with `load` unfired rather than spin: the caller's budget is a
            // ceiling on waiting, not a quota to burn.
            if matches!(end, Ok(PumpEnd::Drained)) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(());
            }
        }
    }
}

/// Which queued module scripts a drain takes.
///
/// They share one queue and one evaluator, and the split is WHEN, not how: HTML defers a module
/// script the PARSER found to the end of the parse -- it runs before `DOMContentLoaded`, in document
/// order -- while a module the PAGE inserted is a task of its own and runs after. Chrome 150,
/// measured over loopback on 2026-09-18: `<script type=module>` in the markup gives
/// `ran:parser-module, DCL`; a script that inserts an inline module gives `after-insert, DCL,
/// ran:inline`. One queue drained indiscriminately at the end of the parse ran the second one first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drain {
    /// Everything queued -- the event loop's own drain, which is what finally runs the page's.
    All,
    /// Only the modules the parser found. `navigation.rs` asks for this at "the end", before
    /// `DOMContentLoaded`.
    ParserDeferred,
    /// Only the page's own insertions: the event loop's drain while the parser waits for a script, its own modules
    /// being deferred to the parse's end.
    InsertedOnly,
}

impl Drain {
    /// Does this drain take `m`?
    fn takes(self, m: &webapi::page::PendingModule) -> bool {
        match self {
            Drain::All => true,
            // The page's own insertions stay queued, in order, for the next turn of the loop.
            Drain::ParserDeferred => !m.inserted,
            Drain::InsertedOnly => m.inserted,
        }
    }
}

/// What a drain needs to START the next queued module, read out of `PendingModules` with the entry
/// LEFT IN PLACE. `idx` is where that entry sits; modules are only ever appended, so
/// it stays valid across the one await the drain makes before removing it.
struct NextModule {
    idx: usize,
    url: String,
    source: String,
    import_base: Option<String>,
    refused: bool,
}

/// The two things a `PendingModule` still owes the page once the drain has finished with it: the
/// element that fires `load` or `error`, and the "delay the load event" token to release. `None`
/// for both if the entry was gone -- which only a second drain of the same index could do, and there
/// is no second drain.
fn split(pm: Option<PendingModule>) -> (Option<deno_core::v8::Global<deno_core::v8::Value>>, Option<u32>) {
    match pm {
        Some(pm) => (pm.element, pm.delay_token),
        None => (None, None),
    }
}

/// Why one queued module did not finish, in the three shapes a page can tell apart.
enum ModuleFailure {
    /// The loader refused a fetch (an import answered 404, or a non-JavaScript Content-Type).
    Fetch(anyhow::Error),
    /// A JavaScript exception: a parse error, or a throw during evaluation.
    Threw(anyhow::Error),
    /// The drain's budget ran out while imports were still being fetched.
    TimedOut(anyhow::Error),
}
